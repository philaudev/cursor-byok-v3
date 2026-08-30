//! Executes one Run across model cycles, Tool rounds, and message commits.
use std::collections::HashSet;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    model::{
        CanonicalMessage, ContentPart, MessageContent, Origin, PreparedRun, Role, RunAction,
        ToolRoundAssistant, ToolRoundId, Usage,
    },
    provider::Provider,
    store::{RunStatus, Store},
};

use super::{
    consume_model_cycle, CommitBarrier, CommitCause, MessagesCommitted, ModelCycleFailure,
    RunCommand, RunEvent, RunFailure, RunOutcome, RunPort,
};

pub struct RunEngine {
    store: Store,
    provider: Arc<dyn Provider>,
}

impl RunEngine {
    pub fn new(store: Store, provider: Arc<dyn Provider>) -> Self {
        Self { store, provider }
    }

    #[tracing::instrument(
        skip_all,
        fields(run_id = %prepared.run_id, conversation_id = %prepared.conversation_id)
    )]
    pub async fn run(
        &self,
        prepared: PreparedRun,
        mut client: RunPort,
        cancellation: CancellationToken,
    ) -> RunOutcome {
        let claimed = match self.store.claim_run(&prepared).await {
            Ok(claimed) => claimed,
            Err(error) => {
                let outcome = RunOutcome::Failed(error.into());
                client.phase.finish();
                let _ = client.events.send(RunEvent::Ended(outcome.clone())).await;
                tracing::info!(outcome = ?outcome, "Run claim failed");
                return outcome;
            }
        };
        let outcome = self
            .run_claimed(
                &prepared,
                claimed.head_checkpoint_id,
                &mut client,
                &cancellation,
            )
            .await;
        let usage = outcome.1;
        let outcome = outcome.0;
        let (status, failure) = match &outcome {
            RunOutcome::Completed => (RunStatus::Completed, None),
            RunOutcome::Cancelled => (RunStatus::Cancelled, None),
            RunOutcome::Failed(failure) => (
                RunStatus::Failed,
                Some((failure.category(), failure_message(failure))),
            ),
        };
        let failure_ref = failure
            .as_ref()
            .map(|(category, summary)| (*category, summary.as_str()));
        if let Err(error) = self
            .store
            .finish_run(&prepared.run_id, status, usage, failure_ref)
            .await
        {
            tracing::error!(run_id = %prepared.run_id, %error, "failed to persist Run outcome");
        }
        client.phase.finish();
        let _ = client.events.send(RunEvent::Ended(outcome.clone())).await;
        tracing::info!(outcome = ?outcome, usage = ?usage, "Run ended");
        outcome
    }

    async fn run_claimed(
        &self,
        prepared: &PreparedRun,
        mut checkpoint: crate::model::CheckpointId,
        client: &mut RunPort,
        cancellation: &CancellationToken,
    ) -> (RunOutcome, Option<Usage>) {
        let mut usage = None;
        tracing::info!(
            checkpoint_id = checkpoint.0,
            "Run claimed conversation ownership"
        );
        if !prepared.initial_messages.is_empty() {
            let mut changed = false;
            for message in &prepared.initial_messages {
                match self
                    .store
                    .append_message_once(
                        &prepared.conversation_id,
                        &prepared.run_id,
                        checkpoint,
                        message,
                    )
                    .await
                {
                    Ok((next, inserted)) => {
                        checkpoint = next;
                        changed |= inserted;
                    }
                    Err(error) => return (RunOutcome::Failed(error.into()), usage),
                }
            }
            if changed {
                let (barrier, ready) = CommitBarrier::before_continue();
                if emit(
                    client,
                    RunEvent::MessagesCommitted(MessagesCommitted {
                        checkpoint_id: checkpoint,
                        tool_round_version: 0,
                        cause: CommitCause::InitialMessages,
                        barrier,
                    }),
                )
                .await
                .is_err()
                {
                    return (client_failure(), usage);
                }
                if let Err(outcome) = wait_for_state_ready(ready, cancellation).await {
                    return (outcome, usage);
                }
            }
        }

        if let RunAction::Resume {
            pending_tool_round: Some(round),
        } = &prepared.action
        {
            checkpoint = match super::tool_round::execute(
                &self.store,
                prepared,
                client,
                cancellation,
                checkpoint,
                super::tool_round::ToolRound {
                    id: ToolRoundId::new(format!("{}:round:resume", prepared.run_id)),
                    assistant: round.assistant.clone(),
                    calls: round.calls.clone(),
                    recovered_started_at_ms: Some(round.started_at_ms),
                },
                Vec::new(),
            )
            .await
            {
                Ok(checkpoint) => checkpoint,
                Err(outcome) => return (outcome, usage),
            };
        }

        let mut auto_compacted = false;
        let mut checkpoint_context_tokens = prepared.checkpoint_context_tokens;
        'model: loop {
            if cancellation.is_cancelled() {
                return (RunOutcome::Cancelled, usage);
            }
            let messages = match self.store.load_checkpoint_messages(checkpoint).await {
                Ok(messages) => messages,
                Err(error) => return (RunOutcome::Failed(error.into()), usage),
            };
            let context_anchor = if !auto_compacted && prepared.action == RunAction::Start {
                match self
                    .store
                    .latest_llm_call_usage_anchor(
                        &prepared.conversation_id,
                        &prepared.model.model_id,
                    )
                    .await
                {
                    Ok(anchor) => {
                        anchor.and_then(super::compaction::ContextUsageAnchor::from_llm_call)
                    }
                    Err(error) => return (RunOutcome::Failed(error.into()), usage),
                }
            } else {
                None
            };
            if context_anchor.is_some() {
                checkpoint_context_tokens = None;
            }
            let history = match crate::model::project_messages(&messages) {
                Ok(history) => history,
                Err(error) => return (RunOutcome::Failed(error.into()), usage),
            };
            if !auto_compacted
                && super::compaction::should_compact(prepared, &messages, &history, context_anchor)
            {
                match self
                    .auto_compact(prepared, checkpoint, &messages, client, cancellation)
                    .await
                {
                    Ok((next_checkpoint, compaction_usage)) => {
                        checkpoint = next_checkpoint;
                        auto_compacted = true;
                        if let Some(compaction_usage) = compaction_usage {
                            accumulate_usage(&mut usage, compaction_usage);
                        }
                        continue 'model;
                    }
                    Err(outcome) => return (outcome, usage),
                }
            }
            let provider_call_index = match self.store.begin_provider_call(&prepared.run_id).await {
                Ok(index) => index,
                Err(error) => return (RunOutcome::Failed(error.into()), usage),
            };
            tracing::debug!(
                provider_call_index,
                checkpoint_id = checkpoint.0,
                "starting model call"
            );
            let mut history = if prepared.action == RunAction::Compact {
                match crate::model::project_compaction_messages(&messages) {
                    Ok(history) => history,
                    Err(error) => return (RunOutcome::Failed(error.into()), usage),
                }
            } else {
                history
            };
            crate::model::normalize_provider_tool_call_ids(&mut history);
            if let Err(error) = hydrate_tool_images(&self.store, &mut history).await {
                return (RunOutcome::Failed(error.into()), usage);
            }
            let history_fingerprint = prompt_history_fingerprint(&prepared.prompt, &history);
            let projected_message_count = history.len();
            let request = crate::model::ModelRequest {
                prompt: prepared.prompt.clone(),
                model: prepared.model.clone(),
                history,
            };
            let invocation = crate::model::ModelInvocation {
                call_id: format!("{}:{provider_call_index}", prepared.run_id),
                run_id: prepared.run_id.to_string(),
                conversation_id: prepared.conversation_id.to_string(),
                provider_call_index,
                canonical_message_count: messages.len(),
                projected_message_count,
                history_fingerprint,
                request,
            };
            let cycle_cancellation = cancellation.child_token();
            let cycle_events = client.events.clone();
            let cycle = consume_model_cycle(
                self.provider.stream(invocation, cycle_cancellation.clone()),
                &cycle_events,
                &cycle_cancellation,
            );
            tokio::pin!(cycle);
            let mut pending_insertions = Vec::new();
            let cycle = loop {
                tokio::select! {
                    biased;
                    command = client.commands.recv() => {
                        let interruption = match command {
                            Some(RunCommand::InsertMessages(insertion)) => {
                                pending_insertions.push(insertion);
                                continue;
                            }
                            Some(RunCommand::BreakMessages(messages)) => messages,
                            Some(RunCommand::Cancel) => {
                                cycle_cancellation.cancel();
                                let _ = cycle.await;
                                let _ = emit(client, RunEvent::CycleInterrupted).await;
                                return (RunOutcome::Cancelled, usage);
                            }
                            Some(RunCommand::ToolResult(_)) => {
                                cycle_cancellation.cancel();
                                let _ = cycle.await;
                                let _ = emit(client, RunEvent::CycleInterrupted).await;
                                return (
                                    RunOutcome::Failed(RunFailure::Protocol(
                                        "received a tool result while the model was running".into(),
                                    )),
                                    usage,
                                );
                            }
                            None => {
                                cycle_cancellation.cancel();
                                let _ = cycle.await;
                                let _ = emit(client, RunEvent::CycleInterrupted).await;
                                return (client_failure(), usage);
                            }
                        };
                        cycle_cancellation.cancel();
                        let interrupted = cycle.await;
                        match interrupted {
                            Ok(cycle) => {
                                if let Some(cycle_usage) = cycle.usage {
                                    accumulate_usage(&mut usage, cycle_usage);
                                }
                            }
                            Err(failure) => {
                                if let Some(cycle_usage) = failure.usage {
                                    accumulate_usage(&mut usage, cycle_usage);
                                }
                            }
                        }
                        if emit(client, RunEvent::CycleInterrupted).await.is_err() {
                            return (client_failure(), usage);
                        }
                        checkpoint = match super::messages::append_batches(
                            &self.store,
                            prepared,
                            client,
                            cancellation,
                            checkpoint,
                            std::mem::take(&mut pending_insertions),
                        )
                        .await
                        {
                            Ok((checkpoint, _)) => checkpoint,
                            Err(outcome) => return (outcome, usage),
                        };
                        checkpoint = match super::messages::append_batches(
                            &self.store,
                            prepared,
                            client,
                            cancellation,
                            checkpoint,
                            vec![interruption],
                        )
                        .await
                        {
                            Ok((checkpoint, _)) => checkpoint,
                            Err(outcome) => return (outcome, usage),
                        };
                        continue 'model;
                    },
                    result = &mut cycle => break result,
                }
            };
            let cycle = match cycle {
                Ok(cycle) => cycle,
                Err(ModelCycleFailure {
                    failure,
                    usage: cycle_usage,
                    ..
                }) => {
                    if let Some(cycle_usage) = cycle_usage {
                        accumulate_usage(&mut usage, cycle_usage);
                    }
                    if cancellation.is_cancelled() {
                        let _ = emit(client, RunEvent::CycleInterrupted).await;
                        return (RunOutcome::Cancelled, usage);
                    }
                    return (RunOutcome::Failed(failure), usage);
                }
            };
            if let Some(cycle_usage) = cycle.usage {
                accumulate_usage(&mut usage, cycle_usage);
            }

            if prepared.action == RunAction::Compact {
                if !cycle.calls.is_empty() {
                    return (
                        RunOutcome::Failed(RunFailure::Protocol(
                            "compaction model returned tool calls".into(),
                        )),
                        usage,
                    );
                }
                let summary = cycle.text.trim().to_string();
                if summary.is_empty() {
                    return (
                        RunOutcome::Failed(RunFailure::Protocol(
                            "compaction model returned an empty summary".into(),
                        )),
                        usage,
                    );
                }
                let event_id = format!("summary:{}", prepared.run_id);
                let summary_message = CanonicalMessage {
                    message_id: format!("runtime:{event_id}"),
                    role: Role::User,
                    origin: Origin::Runtime,
                    content: MessageContent::Parts {
                        parts: vec![crate::model::ContentPart::Text {
                            text: format!(
                                "<conversation_summary>\n{summary}\n</conversation_summary>"
                            ),
                        }],
                    },
                    runtime_event_id: Some(event_id),
                };
                checkpoint = match self
                    .store
                    .replace_checkpoint(
                        &prepared.conversation_id,
                        &prepared.run_id,
                        checkpoint,
                        &[summary_message],
                    )
                    .await
                {
                    Ok(checkpoint) => checkpoint,
                    Err(error) => return (RunOutcome::Failed(error.into()), usage),
                };
                if !pending_insertions.is_empty() {
                    checkpoint = match super::messages::append_batches(
                        &self.store,
                        prepared,
                        client,
                        cancellation,
                        checkpoint,
                        pending_insertions,
                    )
                    .await
                    {
                        Ok((next, _)) => next,
                        Err(outcome) => return (outcome, usage),
                    };
                }
                client.phase.begin_finalizing();
                let closing_insertions = match super::messages::drain_accepted(client) {
                    Ok(insertions) => insertions,
                    Err(outcome) => return (outcome, usage),
                };
                if !closing_insertions.is_empty() {
                    checkpoint = match super::messages::append_batches(
                        &self.store,
                        prepared,
                        client,
                        cancellation,
                        checkpoint,
                        closing_insertions,
                    )
                    .await
                    {
                        Ok((next, _)) => next,
                        Err(outcome) => return (outcome, usage),
                    };
                }
                let (barrier, ready) = CommitBarrier::before_continue();
                if emit(
                    client,
                    RunEvent::MessagesCommitted(MessagesCommitted {
                        checkpoint_id: checkpoint,
                        tool_round_version: 0,
                        cause: CommitCause::Compaction { summary },
                        barrier,
                    }),
                )
                .await
                .is_err()
                {
                    return (client_failure(), usage);
                }
                if let Err(outcome) = wait_for_state_ready(ready, cancellation).await {
                    return (outcome, usage);
                }
                return (RunOutcome::Completed, usage);
            }

            if cycle.calls.is_empty() {
                let assistant = CanonicalMessage {
                    message_id: format!("{}:assistant:{provider_call_index}", prepared.run_id),
                    role: Role::Assistant,
                    origin: Origin::Assistant,
                    content: MessageContent::Assistant {
                        text: cycle.text,
                        thinking: cycle.reasoning,
                        tool_round_id: None,
                        replay_state: cycle.replay_state,
                        tool_calls: Vec::new(),
                    },
                    runtime_event_id: None,
                };
                checkpoint = match self
                    .store
                    .append_checkpoint(
                        &prepared.conversation_id,
                        &prepared.run_id,
                        checkpoint,
                        &[assistant],
                    )
                    .await
                {
                    Ok(checkpoint) => checkpoint,
                    Err(error) => return (RunOutcome::Failed(error.into()), usage),
                };
                if !pending_insertions.is_empty() {
                    let inserted = match super::messages::append_batches(
                        &self.store,
                        prepared,
                        client,
                        cancellation,
                        checkpoint,
                        pending_insertions,
                    )
                    .await
                    {
                        Ok((next, inserted)) => {
                            checkpoint = next;
                            inserted
                        }
                        Err(outcome) => return (outcome, usage),
                    };
                    if inserted {
                        continue 'model;
                    }
                }
                client.phase.begin_finalizing();
                let closing_insertions = match super::messages::drain_accepted(client) {
                    Ok(insertions) => insertions,
                    Err(outcome) => return (outcome, usage),
                };
                if !closing_insertions.is_empty() {
                    checkpoint = match super::messages::append_batches(
                        &self.store,
                        prepared,
                        client,
                        cancellation,
                        checkpoint,
                        closing_insertions,
                    )
                    .await
                    {
                        Ok((next, _)) => next,
                        Err(outcome) => return (outcome, usage),
                    };
                    client.phase.resume_running();
                    continue 'model;
                }
                let (barrier, ready) = CommitBarrier::before_continue();
                if emit(
                    client,
                    RunEvent::MessagesCommitted(MessagesCommitted {
                        checkpoint_id: checkpoint,
                        tool_round_version: 0,
                        cause: CommitCause::FinalTurn,
                        barrier,
                    }),
                )
                .await
                .is_err()
                {
                    return (client_failure(), usage);
                }
                if let Err(outcome) = wait_for_state_ready(ready, cancellation).await {
                    return (outcome, usage);
                }
                return (RunOutcome::Completed, usage);
            }

            let round_id =
                ToolRoundId::new(format!("{}:round:{provider_call_index}", prepared.run_id));
            checkpoint = match super::tool_round::execute(
                &self.store,
                prepared,
                client,
                cancellation,
                checkpoint,
                super::tool_round::ToolRound {
                    id: round_id,
                    assistant: ToolRoundAssistant {
                        text: cycle.text,
                        thinking: cycle.reasoning,
                        model_call_id: cycle.model_call_id,
                        replay_state: cycle.replay_state,
                    },
                    calls: cycle.calls,
                    recovered_started_at_ms: None,
                },
                pending_insertions,
            )
            .await
            {
                Ok(checkpoint) => checkpoint,
                Err(outcome) => return (outcome, usage),
            };
        }
    }

    async fn auto_compact(
        &self,
        prepared: &PreparedRun,
        checkpoint: crate::model::CheckpointId,
        messages: &[CanonicalMessage],
        client: &mut RunPort,
        cancellation: &CancellationToken,
    ) -> std::result::Result<(crate::model::CheckpointId, Option<Usage>), RunOutcome> {
        let current_ids = prepared
            .initial_messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<HashSet<_>>();
        let (compactable, retained_request_context) =
            super::compaction::partition(messages, &current_ids);
        if compactable.is_empty() {
            return Ok((checkpoint, None));
        }

        emit(client, RunEvent::AutoCompactionStarted)
            .await
            .map_err(|_| client_failure())?;
        let provider_call_index = self
            .store
            .begin_provider_call(&prepared.run_id)
            .await
            .map_err(|error| RunOutcome::Failed(error.into()))?;
        let history = crate::model::project_compaction_messages(&compactable)
            .map_err(|error| RunOutcome::Failed(error.into()))?;
        let prompt = crate::model::PromptSpec {
            instructions: prepared.compaction_prompt.instructions.clone(),
            tools: Vec::new(),
        };
        let history_fingerprint = prompt_history_fingerprint(&prompt, &history);
        let mut model = prepared.model.clone();
        model.max_output_tokens = Some(super::compaction::OUTPUT_TOKENS);
        model.reasoning.enabled = false;
        model.reasoning.effort = None;
        let invocation = crate::model::ModelInvocation {
            call_id: format!("{}:{provider_call_index}", prepared.run_id),
            run_id: prepared.run_id.to_string(),
            conversation_id: prepared.conversation_id.to_string(),
            provider_call_index,
            canonical_message_count: compactable.len(),
            projected_message_count: history.len(),
            history_fingerprint,
            request: crate::model::ModelRequest {
                prompt,
                model,
                history,
            },
        };
        let cycle_cancellation = cancellation.child_token();
        let (silent_events, mut discarded_events) = tokio::sync::mpsc::channel(256);
        let drain = tokio::spawn(async move { while discarded_events.recv().await.is_some() {} });
        let mut pending_insertions = Vec::new();
        let mut break_messages = None;
        let cycle = {
            let cycle = consume_model_cycle(
                self.provider.stream(invocation, cycle_cancellation.clone()),
                &silent_events,
                &cycle_cancellation,
            );
            tokio::pin!(cycle);
            loop {
                tokio::select! {
                    biased;
                    command = client.commands.recv() => match command {
                        Some(RunCommand::InsertMessages(insertion)) => {
                            pending_insertions.push(insertion);
                        }
                        Some(RunCommand::BreakMessages(messages)) => {
                            cycle_cancellation.cancel();
                            break_messages = Some(messages);
                            break cycle.await;
                        }
                        Some(RunCommand::Cancel) => {
                            cycle_cancellation.cancel();
                            let _ = cycle.await;
                            return Err(RunOutcome::Cancelled);
                        }
                        Some(RunCommand::ToolResult(_)) => {
                            cycle_cancellation.cancel();
                            let _ = cycle.await;
                            return Err(RunOutcome::Failed(RunFailure::Protocol(
                                "received a tool result while automatic compaction was running".into(),
                            )));
                        }
                        None => {
                            cycle_cancellation.cancel();
                            let _ = cycle.await;
                            return Err(client_failure());
                        }
                    },
                    result = &mut cycle => break result,
                }
            }
        };
        drop(silent_events);
        let _ = drain.await;
        let (summary, compaction_usage) = match (break_messages.is_some(), cycle) {
            (true, Ok(cycle)) => (
                super::compaction::fallback_summary(&compactable),
                cycle.usage,
            ),
            (true, Err(failure)) => (
                super::compaction::fallback_summary(&compactable),
                failure.usage,
            ),
            (false, Ok(cycle)) if cycle.calls.is_empty() && !cycle.text.trim().is_empty() => {
                (cycle.text.trim().to_string(), cycle.usage)
            }
            (false, Ok(cycle)) => {
                tracing::warn!("automatic compaction returned no usable summary; using fallback");
                (
                    super::compaction::fallback_summary(&compactable),
                    cycle.usage,
                )
            }
            (false, Err(failure)) => {
                tracing::warn!(error = ?failure.failure, "automatic compaction model failed; using fallback");
                (
                    super::compaction::fallback_summary(&compactable),
                    failure.usage,
                )
            }
        };
        let event_id = format!("summary:auto:{}:{}", prepared.run_id, &Uuid::new_v4().simple().to_string()[..8]);
        let summary_message = CanonicalMessage {
            message_id: format!("runtime:{event_id}"),
            role: Role::User,
            origin: Origin::Runtime,
            content: MessageContent::Parts {
                parts: vec![crate::model::ContentPart::Text {
                    text: format!("<conversation_summary>\n{summary}\n</conversation_summary>"),
                }],
            },
            runtime_event_id: Some(event_id),
        };
        let mut replacement = retained_request_context.into_iter().collect::<Vec<_>>();
        replacement.push(summary_message);
        replacement.extend(prepared.initial_messages.iter().cloned());
        let mut checkpoint = self
            .store
            .replace_checkpoint(
                &prepared.conversation_id,
                &prepared.run_id,
                checkpoint,
                &replacement,
            )
            .await
            .map_err(|error| RunOutcome::Failed(error.into()))?;
        let (barrier, ready) = CommitBarrier::before_continue();
        emit(
            client,
            RunEvent::MessagesCommitted(MessagesCommitted {
                checkpoint_id: checkpoint,
                tool_round_version: 0,
                cause: CommitCause::Compaction { summary },
                barrier,
            }),
        )
        .await
        .map_err(|_| client_failure())?;
        wait_for_state_ready(ready, cancellation).await?;
        emit(client, RunEvent::AutoCompactionCompleted)
            .await
            .map_err(|_| client_failure())?;
        checkpoint = super::messages::append_batches(
            &self.store,
            prepared,
            client,
            cancellation,
            checkpoint,
            pending_insertions,
        )
        .await?
        .0;
        if let Some(messages) = break_messages {
            checkpoint = super::messages::append_batches(
                &self.store,
                prepared,
                client,
                cancellation,
                checkpoint,
                vec![messages],
            )
            .await?
            .0;
        }
        Ok((checkpoint, compaction_usage))
    }
}

fn prompt_history_fingerprint(
    prompt: &crate::model::PromptSpec,
    messages: &[crate::model::ProjectedMessage],
) -> String {
    let serialized = serde_json::to_vec(&(prompt.instructions.as_str(), messages, &prompt.tools))
        .unwrap_or_default();
    hex::encode(Sha256::digest(serialized))
}

async fn hydrate_tool_images(
    store: &Store,
    messages: &mut [crate::model::ProjectedMessage],
) -> crate::Result<()> {
    use crate::{
        model::{ContentPart, ProjectedContent},
        store::BlobId,
        Error,
    };

    for message in messages {
        let ProjectedContent::ToolResult(result) = &mut message.content else {
            continue;
        };
        let Some(image) = &result.image else {
            continue;
        };
        let id = BlobId::from_base64(&image.blob_id)?;
        let data = store.get_blob(&id).await?.ok_or_else(|| {
            Error::Protocol(format!("Read image Blob is missing: {}", image.blob_id))
        })?;
        result.provider_parts = vec![
            ContentPart::Text {
                text: result.content.clone(),
            },
            ContentPart::Image {
                mime_type: image.mime_type.clone(),
                data,
            },
        ];
    }
    Ok(())
}

fn accumulate_usage(total: &mut Option<Usage>, usage: Usage) {
    match total {
        Some(total) => *total += usage,
        None => *total = Some(usage),
    }
}

pub(super) async fn wait_for_state_ready(
    ready: tokio::sync::oneshot::Receiver<std::result::Result<(), String>>,
    cancellation: &CancellationToken,
) -> std::result::Result<(), RunOutcome> {
    let result = tokio::select! {
        biased;
        result = ready => result,
        _ = cancellation.cancelled() => return Err(RunOutcome::Cancelled),
    };
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(RunOutcome::Failed(RunFailure::Client(error))),
        Err(_) => Err(client_failure()),
    }
}

pub(super) async fn emit(client: &RunPort, event: RunEvent) -> Result<(), ()> {
    client.events.send(event).await.map_err(|_| ())
}

pub(super) fn client_failure() -> RunOutcome {
    RunOutcome::Failed(RunFailure::Client("client event channel closed".into()))
}

fn failure_message(failure: &RunFailure) -> String {
    match failure {
        RunFailure::Protocol(message)
        | RunFailure::Provider(message)
        | RunFailure::Store(message)
        | RunFailure::Client(message) => message.clone(),
    }
}
