use tokio_util::sync::CancellationToken;

use crate::{
    client::{ClientCommand, ClientEvent, ClientPort, CommitBarrier, CommitCause, StateCommitted},
    model::{PreparedRun, RevisionId, ToolCall, ToolRoundAssistant, ToolRoundId},
    store::Store,
};

use super::{RunFailure, RunOutcome};

pub(super) struct ToolRound {
    pub id: ToolRoundId,
    pub assistant: ToolRoundAssistant,
    pub calls: Vec<ToolCall>,
    pub recovered_started_at_ms: Option<u64>,
}

pub(super) async fn execute(
    store: &Store,
    prepared: &PreparedRun,
    client: &mut ClientPort,
    cancellation: &CancellationToken,
    mut revision: RevisionId,
    round: ToolRound,
) -> std::result::Result<RevisionId, RunOutcome> {
    let ToolRound {
        id: round_id,
        assistant,
        calls,
        recovered_started_at_ms,
    } = round;
    store
        .create_tool_round(
            &round_id,
            &prepared.run_id,
            revision,
            &assistant,
            &calls,
            recovered_started_at_ms,
        )
        .await
        .map_err(failed)?;
    tracing::info!(
        round_id = %round_id,
        revision_id = revision.0,
        calls = calls.len(),
        "tool round started"
    );
    let (barrier, ready) = {
        let (barrier, ready) = CommitBarrier::before_continue();
        (barrier, Some(ready))
    };
    send(
        client,
        ClientEvent::StateCommitted(StateCommitted {
            revision_id: revision,
            tool_round_version: 0,
            cause: CommitCause::ToolRoundStarted {
                round_id: round_id.clone(),
                assistant: assistant.clone(),
                calls: calls.clone(),
            },
            barrier,
        }),
    )
    .await?;
    if let Some(ready) = ready {
        super::engine::wait_for_state_ready(ready, cancellation).await?;
    }
    send(
        client,
        ClientEvent::ExecuteToolRound {
            round_id: round_id.clone(),
            calls: calls.clone(),
        },
    )
    .await?;

    let mut remaining = calls.len();
    let mut pending_runtime_messages = Vec::new();
    while remaining > 0 {
        let command = tokio::select! {
            _ = cancellation.cancelled() => return Err(RunOutcome::Cancelled),
            command = client.commands.recv() => command,
        };
        match command {
            Some(ClientCommand::ToolResult(result)) => {
                let call_id = result.call_id.clone();
                let committed = store
                    .commit_tool_result(
                        &prepared.conversation_id,
                        &prepared.run_id,
                        &round_id,
                        &result,
                    )
                    .await
                    .map_err(failed)?;
                revision = committed.revision_id;
                tracing::info!(
                    round_id = %round_id,
                    call_id,
                    revision_id = revision.0,
                    tool_round_version = committed.tool_round_version,
                    completion_seq = committed.completion_seq,
                    settled = committed.settled,
                    "tool result committed"
                );
                remaining -= 1;
                let (barrier, ready) = if committed.settled {
                    let (barrier, ready) = CommitBarrier::before_continue();
                    (barrier, Some(ready))
                } else {
                    (CommitBarrier::None, None)
                };
                send(
                    client,
                    ClientEvent::StateCommitted(StateCommitted {
                        revision_id: revision,
                        tool_round_version: committed.tool_round_version,
                        cause: CommitCause::ToolResult { call_id },
                        barrier,
                    }),
                )
                .await?;
                if let Some(ready) = ready {
                    super::engine::wait_for_state_ready(ready, cancellation).await?;
                }
            }
            Some(ClientCommand::RuntimeEvent(event)) => {
                pending_runtime_messages.push(event.into_message());
            }
            Some(ClientCommand::RuntimeMessage(message)) => {
                pending_runtime_messages.push(message);
            }
            Some(ClientCommand::Cancel) => return Err(RunOutcome::Cancelled),
            Some(ClientCommand::ClientClosed { error }) => {
                return Err(RunOutcome::Failed(RunFailure::Client(error)));
            }
            None => return Err(client_failure()),
        }
    }
    for message in pending_runtime_messages {
        let event_id = message.runtime_event_id.clone().ok_or_else(|| {
            RunOutcome::Failed(RunFailure::Protocol(
                "runtime message has no event identity".into(),
            ))
        })?;
        let (next, inserted) = store
            .append_message_once(
                &prepared.conversation_id,
                &prepared.run_id,
                revision,
                &message,
            )
            .await
            .map_err(failed)?;
        revision = next;
        if inserted {
            let (barrier, ready) = CommitBarrier::before_continue();
            send(
                client,
                ClientEvent::StateCommitted(StateCommitted {
                    revision_id: revision,
                    tool_round_version: 0,
                    cause: CommitCause::RuntimeEvent { event_id },
                    barrier,
                }),
            )
            .await?;
            super::engine::wait_for_state_ready(ready, cancellation).await?;
        }
    }
    Ok(revision)
}

async fn send(client: &ClientPort, event: ClientEvent) -> std::result::Result<(), RunOutcome> {
    client
        .events
        .send(event)
        .await
        .map_err(|_| client_failure())
}

fn failed(error: crate::Error) -> RunOutcome {
    RunOutcome::Failed(error.into())
}

fn client_failure() -> RunOutcome {
    RunOutcome::Failed(RunFailure::Client("client event channel closed".into()))
}
