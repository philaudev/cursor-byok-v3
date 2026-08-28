use tokio::sync::{mpsc, oneshot};

use crate::{
    cursor::{presentation::PresentationDelta, proto::agent::v1 as pb, CursorSessionHandle},
    model::{RevisionId, ToolCall, ToolRoundAssistant},
    store::Store,
    Error, Result,
};

use super::CheckpointBuilder;

pub(crate) struct CheckpointJob {
    pub kind: CheckpointKind,
    pub presentation: PresentationDelta,
    pub context_tokens: Option<u64>,
    pub ready: Option<oneshot::Sender<std::result::Result<(), String>>>,
}

pub(crate) enum CheckpointKind {
    Settled(RevisionId),
    ToolStarted {
        stable_revision_id: RevisionId,
        assistant: ToolRoundAssistant,
        calls: Vec<ToolCall>,
    },
    ToolSettled(RevisionId),
    Final {
        revision_id: RevisionId,
        result: oneshot::Sender<Result<FinalCheckpoints>>,
    },
    Compaction {
        revision_id: RevisionId,
        summary: String,
        result: oneshot::Sender<Result<pb::ConversationStateStructure>>,
    },
}

pub(crate) struct FinalCheckpoints {
    pub staged: pb::ConversationStateStructure,
    pub settled: pb::ConversationStateStructure,
}

pub(crate) struct CheckpointWorker {
    pub jobs: mpsc::Sender<CheckpointJob>,
    pub failures: mpsc::Receiver<Error>,
    task: tokio::task::JoinHandle<()>,
}

impl CheckpointWorker {
    pub fn spawn(
        store: Store,
        mut builder: CheckpointBuilder,
        handle: CursorSessionHandle,
        mode: i32,
    ) -> Self {
        let (jobs, mut receiver) = mpsc::channel::<CheckpointJob>(32);
        let (failures, failure_receiver) = mpsc::channel(1);
        let task = tokio::spawn(async move {
            while let Some(job) = receiver.recv().await {
                builder.record_context_tokens(job.context_tokens);
                let presentation = job.presentation;
                let ready = job.ready;
                let result = match job.kind {
                    CheckpointKind::Settled(revision_id)
                    | CheckpointKind::ToolSettled(revision_id) => {
                        publish_settled(
                            &store,
                            &mut builder,
                            &handle,
                            mode,
                            revision_id,
                            &presentation,
                        )
                        .await
                    }
                    CheckpointKind::ToolStarted {
                        stable_revision_id,
                        assistant,
                        calls,
                    } => {
                        publish_started(
                            &store,
                            &mut builder,
                            &handle,
                            mode,
                            stable_revision_id,
                            &assistant,
                            &calls,
                            &presentation,
                        )
                        .await
                    }
                    CheckpointKind::Final {
                        revision_id,
                        result,
                    } => {
                        let checkpoints =
                            build_final(&store, &mut builder, mode, revision_id, &presentation)
                                .await;
                        let _ = result.send(checkpoints);
                        Ok(())
                    }
                    CheckpointKind::Compaction {
                        revision_id,
                        summary,
                        result,
                    } => {
                        let messages = store.load_revision_messages(revision_id).await;
                        let checkpoint = match messages {
                            Ok(messages) => {
                                builder
                                    .compacted(&messages, mode, &summary, &presentation)
                                    .await
                            }
                            Err(error) => Err(error),
                        };
                        let _ = result.send(checkpoint);
                        Ok(())
                    }
                };

                if let Err(error) = result {
                    if let Some(ready) = ready {
                        let _ = ready.send(Err(error.to_string()));
                    }
                    tracing::error!(%error, "failed to build or publish Cursor checkpoint");
                    let _ = failures.send(error).await;
                    break;
                }
                if let Some(ready) = ready {
                    let _ = ready.send(Ok(()));
                }
            }
        });
        Self {
            jobs,
            failures: failure_receiver,
            task,
        }
    }

    pub fn abort(&self) {
        self.task.abort();
    }
}

async fn publish_settled(
    store: &Store,
    builder: &mut CheckpointBuilder,
    handle: &CursorSessionHandle,
    mode: i32,
    revision_id: RevisionId,
    presentation: &PresentationDelta,
) -> Result<()> {
    let messages = store.load_revision_messages(revision_id).await?;
    let checkpoint = builder.settled(&messages, mode, presentation).await?;
    builder.publish(handle, &checkpoint).await
}

async fn publish_started(
    store: &Store,
    builder: &mut CheckpointBuilder,
    handle: &CursorSessionHandle,
    mode: i32,
    stable_revision_id: RevisionId,
    assistant: &crate::model::ToolRoundAssistant,
    calls: &[crate::model::ToolCall],
    presentation: &PresentationDelta,
) -> Result<()> {
    let messages = store.load_revision_messages(stable_revision_id).await?;
    let created_at_ms = crate::cursor::tools::runtime::now_ms();
    let checkpoint = builder
        .staged_tool_round(
            &messages,
            mode,
            assistant,
            calls,
            created_at_ms,
            presentation,
        )
        .await?;
    builder.publish(handle, &checkpoint).await
}

async fn build_final(
    store: &Store,
    builder: &mut CheckpointBuilder,
    mode: i32,
    revision_id: RevisionId,
    presentation: &PresentationDelta,
) -> Result<FinalCheckpoints> {
    let messages = store.load_revision_messages(revision_id).await?;
    let (assistant, stable) = messages
        .split_last()
        .ok_or_else(|| Error::Store("final revision contains no assistant".into()))?;
    let started_at_ms = crate::cursor::tools::runtime::now_ms();
    let staged = builder
        .staged_final(stable, mode, assistant, started_at_ms, presentation)
        .await?;
    let settled = builder
        .settled(&messages, mode, &PresentationDelta::default())
        .await?;
    Ok(FinalCheckpoints { staged, settled })
}
