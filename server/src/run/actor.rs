use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::{
    client::{ClientCommand, ClientPort},
    cursor::proto::agent::v1 as pb,
    model::{PreparedRun, RunKind, SubagentKind},
    provider::Provider,
    store::Store,
};

use super::{RunEngine, RunOutcome, RunRegistry};

#[derive(Clone)]
pub struct RunActor {
    store: Store,
    provider: Arc<dyn Provider>,
    registry: RunRegistry,
}

impl RunActor {
    pub fn new(store: Store, provider: Arc<dyn Provider>, registry: RunRegistry) -> Self {
        Self {
            store,
            provider,
            registry,
        }
    }

    pub async fn spawn(
        &self,
        prepared: PreparedRun,
        client: ClientPort,
        commands: tokio::sync::mpsc::Sender<ClientCommand>,
        cancellation: CancellationToken,
    ) -> tokio::task::JoinHandle<RunOutcome> {
        let run_id = prepared.run_id.clone();
        let conversation_id = prepared.conversation_id.clone();
        self.registry
            .activate(
                conversation_id.clone(),
                run_id.clone(),
                cancellation.clone(),
                commands,
            )
            .await;
        let actor = self.clone();
        tokio::spawn(async move {
            let outcome = RunEngine::new(actor.store.clone(), actor.provider.clone())
                .run(prepared.clone(), client, cancellation)
                .await;
            actor.registry.release(&conversation_id, &run_id).await;
            actor.notify_subagent_completion(&prepared, &outcome).await;
            outcome
        })
    }

    async fn notify_subagent_completion(&self, prepared: &PreparedRun, outcome: &RunOutcome) {
        let RunKind::Subagent {
            parent_run_id,
            parent_tool_call_id,
            kind,
            background,
        } = &prepared.kind
        else {
            return;
        };
        let parent_conversation_id = if let Ok(Some(parent_info)) =
            self.store.subagent_parent_info(&prepared.run_id).await
        {
            parent_info.parent_conversation_id
        } else if let Ok(Some(conv_id)) = self.store.run_conversation_id(parent_run_id).await {
            conv_id
        } else {
            return;
        };

        // Foreground subagents deliver their result synchronously as the Task tool result.
        // Only background subagents need asynchronous system_notification to wake up parent.
        if !background
            && !self
                .is_background_subagent(&parent_conversation_id, parent_tool_call_id)
                .await
        {
            return;
        }

        let subagent_output = self
            .store
            .load_current_messages(&prepared.conversation_id)
            .await
            .ok()
            .and_then(|messages| {
                messages
                    .into_iter()
                    .rev()
                    .find(|m| m.role == crate::model::Role::Assistant)
                    .and_then(|m| m.extract_text())
            });
        let status = match outcome {
            RunOutcome::Completed => pb::BackgroundTaskStatus::Success,
            RunOutcome::Cancelled => pb::BackgroundTaskStatus::Aborted,
            _ => pb::BackgroundTaskStatus::Error,
        };
        let subagent_id = prepared
            .cursor_request_id
            .clone()
            .unwrap_or_else(|| prepared.conversation_id.to_string());
        let subagent_name = match kind {
            SubagentKind::GeneralPurpose => "generalPurpose".to_string(),
            SubagentKind::Named(name) => name.clone(),
        };
        let completion = pb::BackgroundTaskCompletion {
            task_id: subagent_id.clone(),
            kind: pb::BackgroundTaskKind::Subagent as i32,
            status: status as i32,
            title: format!("Subagent {subagent_name}"),
            detail: subagent_output,
            output_path: None,
            thread_id: None,
            reason: pb::BackgroundTaskCompletionReason::TaskFinished as i32,
            subagent_id: Some(subagent_id),
            tool_call_id: Some(parent_tool_call_id.clone()),
            notification_context: pb::BackgroundTaskNotificationContext::Unspecified as i32,
        };
        let action = pb::BackgroundTaskCompletionAction {
            completions: vec![completion],
        };
        let projection = match crate::cursor::request::project_background_completion(
            &action,
            pb::AgentMode::Multitask as i32,
        ) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(%err, "failed to project subagent background completion");
                return;
            }
        };
        let event = crate::model::RuntimeEvent {
            event_id: projection.turn_user.message_id,
            text: format!("{}\n\n{}", projection.context, projection.turn_user.text),
        };
        let message = event.into_message();
        let inserted = self
            .registry
            .insert_messages(&parent_conversation_id, vec![message.clone()])
            .await;
        if !inserted {
            let _ = self
                .store
                .append_idle_messages(&parent_conversation_id, &[message])
                .await;
        }
    }

    async fn is_background_subagent(
        &self,
        parent_conversation_id: &crate::model::ConversationId,
        parent_tool_call_id: &str,
    ) -> bool {
        if let Ok(messages) = self.store.load_current_messages(parent_conversation_id).await {
            for message in messages {
                match &message.content {
                    crate::model::MessageContent::Assistant { tool_calls, .. } => {
                        for call in tool_calls {
                            if call.call_id == parent_tool_call_id {
                                return call
                                    .arguments
                                    .get("run_in_background")
                                    .and_then(|v| v.as_bool())
                                    == Some(true);
                            }
                        }
                    }
                    crate::model::MessageContent::ToolResult(result) => {
                        if result.call_id == parent_tool_call_id {
                            if let Ok(val) =
                                serde_json::from_str::<serde_json::Value>(&result.content)
                            {
                                if val.get("is_background").and_then(|v| v.as_bool()) == Some(true)
                                {
                                    return true;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        false
    }
}
