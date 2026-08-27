#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::{sync::Arc, time::Duration};

use cursor_server::{
    cursor::{
        connect,
        prompting::{PromptAssets, PromptCompiler},
        proto::agent::v1 as pb,
        CursorCommand, CursorSessionHandle, CursorSessionRegistry,
    },
    provider::{FinishReason, ModelEvent},
};
use prost::Message;

#[tokio::test]
async fn every_bidi_run_resolves_and_persists_its_own_subagent_model_and_background_state() {
    let (_directory, store) = fixtures::temp_store().await;
    let provider = fake_provider::FakeProvider::default();
    for suffix in ["a", "b"] {
        provider.push(task_response(suffix));
        provider.push(stop_response(suffix));
    }
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let registry = CursorSessionRegistry::new(
        store,
        Arc::new(provider),
        PromptCompiler::new(assets),
        Default::default(),
    );

    let first = registry.get_or_create("subagent-run-a").await.unwrap();
    let first_checkpoint = drive(
        &first,
        run_request("subagent-run-a", "user-a", "model-a", None),
        "model-a",
        "child-a",
    )
    .await;

    let second = registry.get_or_create("subagent-run-b").await.unwrap();
    drive(
        &second,
        run_request(
            "subagent-run-b",
            "user-b",
            "model-b",
            Some(first_checkpoint),
        ),
        "model-b",
        "child-b",
    )
    .await;
}

#[tokio::test]
async fn completed_parent_allows_background_subagent_run_and_subsequent_parent_wakeup() {
    let (_directory, store) = fixtures::temp_store().await;
    let provider = fake_provider::FakeProvider::default();
    // 1. Parent launches background subagent
    provider.push(task_response("parent"));
    provider.push(stop_response("parent"));
    // 2. Subagent executes its own turn
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "subagent-call".into(),
        },
        ModelEvent::TextStart,
        ModelEvent::TextDelta("Subagent finished inspection successfully".into()),
        ModelEvent::TextEnd,
        ModelEvent::Done(FinishReason::Stop),
    ]);
    // 3. Parent follow-up turn responds to completion
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "parent-followup-call".into(),
        },
        ModelEvent::TextStart,
        ModelEvent::TextDelta("Parent received subagent results and continued".into()),
        ModelEvent::TextEnd,
        ModelEvent::Done(FinishReason::Stop),
    ]);

    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let registry = CursorSessionRegistry::new(
        store.clone(),
        Arc::new(provider.clone()),
        PromptCompiler::new(assets),
        Default::default(),
    );

    // Step 1: Run Parent turn to completion
    let parent_handle = registry.get_or_create("parent-run-id").await.unwrap();
    let parent_checkpoint = drive(
        &parent_handle,
        run_request("parent-run-id", "parent-user-msg", "gpt-4o", None),
        "gpt-4o",
        "child-parent",
    )
    .await;

    // Verify parent is completed in store
    let _parent_run_id = store
        .run_for_cursor_request("parent-run-id")
        .await
        .unwrap()
        .expect("parent run exists in store");

    // Step 2: Start Subagent with completed parent
    let subagent_handle = registry.get_or_create("subagent-req-id").await.unwrap();
    subagent_handle
        .set_parent(cursor_server::cursor::CursorParent {
            request_id: "parent-run-id".into(),
            tool_call_id: "task-child-parent".into(),
        })
        .unwrap();

    let subagent_request = pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                subagent_type_name: Some("generalPurpose".into()),
                conversation_id: Some("subagent-convo-id".into()),
                requested_model: Some(pb::RequestedModel {
                    model_id: "gpt-4o".into(),
                    ..Default::default()
                }),
                action: Some(pb::ConversationAction {
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(pb::UserMessage {
                                message_id: "subagent-task-msg".into(),
                                text: "inspect the codebase".into(),
                                mode: pb::AgentMode::Agent as i32,
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
    };

    let mut subagent_output = subagent_handle.subscribe();
    subagent_handle
        .command(CursorCommand::Append {
            seqno: 0,
            message: Box::new(subagent_request),
        })
        .await
        .unwrap();

    // Drive subagent stream to completion
    let mut saw_subagent_end = false;
    let mut subagent_seqno = 1;
    while let Some(frame) = tokio::time::timeout(Duration::from_secs(5), subagent_output.recv())
        .await
        .unwrap()
    {
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        if flags & connect::END_STREAM_FLAG != 0 {
            saw_subagent_end = true;
            break;
        }
        let server = pb::AgentServerMessage::decode(payload).unwrap();
        if let Some(pb::agent_server_message::Message::KvServerMessage(kv)) = server.message {
            subagent_handle
                .command(CursorCommand::Append {
                    seqno: subagent_seqno,
                    message: Box::new(kv_ack(kv.id)),
                })
                .await
                .unwrap();
            subagent_seqno += 1;
        }
    }
    assert!(saw_subagent_end, "subagent should finish with END_STREAM");

    // Step 3: Cursor IDE sends BackgroundTaskCompletionAction to wake up Parent
    let parent_wakeup_handle = registry.get_or_create("parent-wakeup-req-id").await.unwrap();
    let wakeup_request = pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                conversation_id: Some("parent-run-id".into()),
                requested_model: Some(pb::RequestedModel {
                    model_id: "gpt-4o".into(),
                    ..Default::default()
                }),
                conversation_state: Some(parent_checkpoint),
                action: Some(pb::ConversationAction {
                    action: Some(
                        pb::conversation_action::Action::BackgroundTaskCompletionAction(
                            pb::BackgroundTaskCompletionAction {
                                completions: vec![pb::BackgroundTaskCompletion {
                                    task_id: "child-parent".into(),
                                    kind: pb::BackgroundTaskKind::Subagent as i32,
                                    status: pb::BackgroundTaskStatus::Success as i32,
                                    title: "Inspect codebase".into(),
                                    detail: Some("Subagent finished inspection successfully".into()),
                                    reason: pb::BackgroundTaskCompletionReason::TaskFinished as i32,
                                    subagent_id: Some("child-parent".into()),
                                    tool_call_id: Some("task-child-parent".into()),
                                    ..Default::default()
                                }],
                            },
                        ),
                    ),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
    };

    let mut parent_wakeup_output = parent_wakeup_handle.subscribe();
    parent_wakeup_handle
        .command(CursorCommand::Append {
            seqno: 0,
            message: Box::new(wakeup_request),
        })
        .await
        .unwrap();

    let mut saw_parent_wakeup_end = false;
    let mut wakeup_seqno = 1;
    let mut final_parent_checkpoint = None;
    while let Some(frame) =
        tokio::time::timeout(Duration::from_secs(5), parent_wakeup_output.recv())
            .await
            .unwrap()
    {
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        if flags & connect::END_STREAM_FLAG != 0 {
            saw_parent_wakeup_end = true;
            break;
        }
        let server = pb::AgentServerMessage::decode(payload).unwrap();
        match server.message {
            Some(pb::agent_server_message::Message::ExecServerMessage(exec)) => {
                if matches!(
                    exec.message,
                    Some(pb::exec_server_message::Message::RequestContextArgs(_))
                ) {
                    parent_wakeup_handle
                        .command(CursorCommand::Append {
                            seqno: wakeup_seqno,
                            message: Box::new(pb::AgentClientMessage {
                                message: Some(
                                    pb::agent_client_message::Message::ExecClientControlMessage(
                                        pb::ExecClientControlMessage {
                                            message: Some(
                                                pb::exec_client_control_message::Message::StreamClose(
                                                    pb::ExecClientStreamClose { id: exec.id },
                                                ),
                                            ),
                                        },
                                    ),
                                ),
                            }),
                        })
                        .await
                        .unwrap();
                    wakeup_seqno += 1;
                    parent_wakeup_handle
                        .command(CursorCommand::Append {
                            seqno: wakeup_seqno,
                            message: Box::new(pb::AgentClientMessage {
                                message: Some(pb::agent_client_message::Message::ExecClientMessage(
                                    pb::ExecClientMessage {
                                        id: exec.id,
                                        message: Some(
                                            pb::exec_client_message::Message::RequestContextResult(
                                                pb::RequestContextResult {
                                                    result: Some(
                                                        pb::request_context_result::Result::Success(
                                                            pb::RequestContextSuccess {
                                                                request_context: Some(
                                                                    pb::RequestContext::default(),
                                                                ),
                                                                ..Default::default()
                                                            },
                                                        ),
                                                    ),
                                                },
                                            ),
                                        ),
                                        ..Default::default()
                                    },
                                )),
                            }),
                        })
                        .await
                        .unwrap();
                    wakeup_seqno += 1;
                }
            }
            Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                parent_wakeup_handle
                    .command(CursorCommand::Append {
                        seqno: wakeup_seqno,
                        message: Box::new(kv_ack(kv.id)),
                    })
                    .await
                    .unwrap();
                wakeup_seqno += 1;
            }
            Some(pb::agent_server_message::Message::ConversationCheckpointUpdate(state)) => {
                if state.pending_tool_calls.is_empty() {
                    final_parent_checkpoint = Some(state);
                }
            }
            _ => {}
        }
    }
    assert!(saw_parent_wakeup_end, "parent wakeup should finish with END_STREAM");

    // Step 4: Verify the final Parent checkpoint marks subagent as Success
    let final_checkpoint = final_parent_checkpoint.expect("final parent checkpoint");
    let subagent_run = final_checkpoint
        .subagent_runs_by_parent_tool_call_id
        .get("task-child-parent")
        .expect("subagent run in parent checkpoint");
    assert_eq!(subagent_run.status, pb::SubagentRunStatus::Success as i32);
    assert_eq!(
        subagent_run.detail.as_deref(),
        Some("Subagent finished inspection successfully")
    );
}

async fn drive(
    handle: &CursorSessionHandle,
    request: pb::AgentClientMessage,
    expected_model: &str,
    child_id: &str,
) -> pb::ConversationStateStructure {
    let mut output = handle.subscribe();
    handle
        .command(CursorCommand::Append {
            seqno: 0,
            message: Box::new(request),
        })
        .await
        .unwrap();

    let mut seqno = 1;
    let mut saw_started = false;
    let mut saw_exec = false;
    let mut saw_completed = false;
    let mut checkpoint = None;
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), output.recv())
            .await
            .unwrap()
            .expect("RunSSE closed before EndStream");
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        if flags & connect::END_STREAM_FLAG != 0 {
            break;
        }
        let server = pb::AgentServerMessage::decode(payload).unwrap();
        match server.message {
            Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                handle
                    .command(CursorCommand::Append {
                        seqno,
                        message: Box::new(kv_ack(kv.id)),
                    })
                    .await
                    .unwrap();
                seqno += 1;
            }
            Some(pb::agent_server_message::Message::ExecServerMessage(exec)) => {
                let Some(pb::exec_server_message::Message::SubagentArgs(args)) = exec.message
                else {
                    continue;
                };
                assert_eq!(args.model_id, expected_model);
                assert_eq!(args.run_in_background, Some(true));
                saw_exec = true;
                handle
                    .command(CursorCommand::Append {
                        seqno,
                        message: Box::new(subagent_result(exec.id, child_id)),
                    })
                    .await
                    .unwrap();
                seqno += 1;
            }
            Some(pb::agent_server_message::Message::InteractionUpdate(update)) => {
                match update.message {
                    Some(pb::interaction_update::Message::ToolCallStarted(started)) => {
                        let task = task(started.tool_call.as_ref().unwrap());
                        assert_eq!(
                            task.args.as_ref().unwrap().model.as_deref(),
                            Some(expected_model)
                        );
                        saw_started = true;
                    }
                    Some(pb::interaction_update::Message::ToolCallCompleted(completed)) => {
                        let task = task(completed.tool_call.as_ref().unwrap());
                        let Some(pb::task_result::Result::Success(success)) = task
                            .result
                            .as_ref()
                            .and_then(|result| result.result.as_ref())
                        else {
                            panic!("expected Task success")
                        };
                        assert_eq!(
                            task.args.as_ref().unwrap().model.as_deref(),
                            Some(expected_model)
                        );
                        assert!(success.is_background);
                        assert_eq!(success.agent_id.as_deref(), Some(child_id));
                        saw_completed = true;
                    }
                    _ => {}
                }
            }
            Some(pb::agent_server_message::Message::ConversationCheckpointUpdate(state))
                if state.pending_tool_calls.is_empty() =>
            {
                checkpoint = Some(state);
                if saw_completed {
                    break;
                }
            }
            _ => {}
        }
    }

    assert!(saw_started && saw_exec && saw_completed);
    let checkpoint = checkpoint.expect("settled checkpoint");
    let state = checkpoint
        .subagent_states
        .get(child_id)
        .expect("background subagent persisted state");
    assert_eq!(state.model_id.as_deref(), Some(expected_model));
    let run = checkpoint
        .subagent_runs_by_parent_tool_call_id
        .get(&format!("task-{child_id}"))
        .expect("background subagent run state");
    assert_eq!(run.status, pb::SubagentRunStatus::Running as i32);
    checkpoint
}

fn task(call: &pb::ToolCall) -> &pb::TaskToolCall {
    let Some(pb::tool_call::Tool::TaskToolCall(task)) = call.tool.as_ref() else {
        panic!("expected TaskToolCall")
    };
    task
}

fn run_request(
    request_id: &str,
    user_id: &str,
    subagent_model: &str,
    conversation_state: Option<pb::ConversationStateStructure>,
) -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                action: Some(pb::ConversationAction {
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(pb::UserMessage {
                                text: format!("start {request_id}"),
                                message_id: user_id.into(),
                                mode: pb::AgentMode::Multitask as i32,
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                conversation_id: Some("subagent-e2e-conversation".into()),
                run_id: Some(request_id.into()),
                requested_model: Some(pb::RequestedModel {
                    model_id: "parent-model".into(),
                    ..Default::default()
                }),
                conversation_state,
                subagent_model_overrides: vec![pb::SubagentModelOverride {
                    subagent_type: "generalPurpose".into(),
                    selection: Some(pb::subagent_model_override::Selection::Model(
                        pb::RequestedModel {
                            model_id: subagent_model.into(),
                            ..Default::default()
                        },
                    )),
                }],
                ..Default::default()
            },
        )),
    }
}

fn task_response(suffix: &str) -> Vec<ModelEvent> {
    let child_id = format!("child-{suffix}");
    let arguments = serde_json::json!({
        "description": format!("background {suffix}"),
        "prompt": "inspect",
        "subagent_type": "generalPurpose",
        "run_in_background": true
    })
    .to_string();
    vec![
        ModelEvent::Start {
            model_call_id: format!("model-call-{suffix}"),
        },
        ModelEvent::ToolCallStart {
            index: 0,
            call_id: format!("task-{child_id}"),
            name: "Task".into(),
        },
        ModelEvent::ToolCallArgumentsDelta {
            index: 0,
            delta: arguments,
        },
        ModelEvent::ToolCallEnd { index: 0 },
        ModelEvent::Done(FinishReason::ToolUse),
    ]
}

fn stop_response(suffix: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::Start {
            model_call_id: format!("final-{suffix}"),
        },
        ModelEvent::TextStart,
        ModelEvent::TextDelta("background task started".into()),
        ModelEvent::TextEnd,
        ModelEvent::Done(FinishReason::Stop),
    ]
}

fn subagent_result(id: u32, child_id: &str) -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::ExecClientMessage(
            pb::ExecClientMessage {
                id,
                message: Some(pb::exec_client_message::Message::SubagentResult(
                    pb::SubagentResult {
                        result: Some(pb::subagent_result::Result::Success(pb::SubagentSuccess {
                            agent_id: child_id.into(),
                            final_message: Some("running in background".into()),
                            background_reason: pb::SubagentBackgroundReason::AgentRequest as i32,
                            ..Default::default()
                        })),
                    },
                )),
                ..Default::default()
            },
        )),
    }
}

fn kv_ack(id: u32) -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::KvClientMessage(
            pb::KvClientMessage {
                id,
                message: Some(pb::kv_client_message::Message::SetBlobResult(
                    pb::SetBlobResult { error: None },
                )),
            },
        )),
    }
}

#[tokio::test]
async fn active_parent_stream_held_open_auto_resumes_and_final_checkpoint_is_success() {
    let (_directory, store) = fixtures::temp_store().await;
    let provider = fake_provider::FakeProvider::default();
    // 1. Parent launches background subagent
    provider.push(task_response("parent"));
    provider.push(stop_response("parent"));
    // 2. Subagent executes its own turn
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "subagent-call".into(),
        },
        ModelEvent::TextStart,
        ModelEvent::TextDelta("Subagent work finished".into()),
        ModelEvent::TextEnd,
        ModelEvent::Done(FinishReason::Stop),
    ]);
    // 3. Parent follow-up turn responds to auto-injected completion
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "parent-followup-call".into(),
        },
        ModelEvent::TextStart,
        ModelEvent::TextDelta("Parent received subagent completion and finalizes".into()),
        ModelEvent::TextEnd,
        ModelEvent::Done(FinishReason::Stop),
    ]);

    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let registry = CursorSessionRegistry::new(
        store.clone(),
        Arc::new(provider.clone()),
        PromptCompiler::new(assets),
        Default::default(),
    );

    let parent_handle = registry.get_or_create("parent-run-stream").await.unwrap();
    let mut parent_output = parent_handle.subscribe();
    parent_handle
        .command(CursorCommand::Append {
            seqno: 0,
            message: Box::new(run_request(
                "parent-run-stream",
                "parent-user-msg",
                "gpt-4o",
                None,
            )),
        })
        .await
        .unwrap();

    let mut parent_seqno = 1;
    let mut saw_parent_task_exec = false;
    let mut intermediate_checkpoint = None;
    while let Some(frame) = tokio::time::timeout(Duration::from_secs(2), parent_output.recv())
        .await
        .ok()
        .flatten()
    {
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        if flags & connect::END_STREAM_FLAG != 0 {
            break;
        }
        let server = pb::AgentServerMessage::decode(payload).unwrap();
        match server.message {
            Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                parent_handle
                    .command(CursorCommand::Append {
                        seqno: parent_seqno,
                        message: Box::new(kv_ack(kv.id)),
                    })
                    .await
                    .unwrap();
                parent_seqno += 1;
            }
            Some(pb::agent_server_message::Message::ExecServerMessage(exec)) => {
                if let Some(pb::exec_server_message::Message::SubagentArgs(_)) = exec.message {
                    saw_parent_task_exec = true;
                    parent_handle
                        .command(CursorCommand::Append {
                            seqno: parent_seqno,
                            message: Box::new(subagent_result(exec.id, "child-parent")),
                        })
                        .await
                        .unwrap();
                    parent_seqno += 1;
                }
            }
            Some(pb::agent_server_message::Message::ConversationCheckpointUpdate(state)) => {
                if state.pending_tool_calls.is_empty() {
                    intermediate_checkpoint = Some(state);
                    if saw_parent_task_exec {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    assert!(saw_parent_task_exec);
    let int_chk = intermediate_checkpoint.expect("intermediate checkpoint");
    let int_subagent = int_chk
        .subagent_runs_by_parent_tool_call_id
        .get("task-child-parent")
        .expect("subagent run in intermediate checkpoint");
    assert_eq!(int_subagent.status, pb::SubagentRunStatus::Running as i32);

    // Now start the Subagent, which upon completion notifies parent via internal insert_messages
    let subagent_handle = registry.get_or_create("subagent-child-req").await.unwrap();
    subagent_handle
        .set_parent(cursor_server::cursor::CursorParent {
            request_id: "parent-run-stream".into(),
            tool_call_id: "task-child-parent".into(),
        })
        .unwrap();

    let subagent_request = pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                subagent_type_name: Some("generalPurpose".into()),
                conversation_id: Some("subagent-child-convo".into()),
                requested_model: Some(pb::RequestedModel {
                    model_id: "gpt-4o".into(),
                    ..Default::default()
                }),
                action: Some(pb::ConversationAction {
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(pb::UserMessage {
                                message_id: "subagent-task-msg".into(),
                                text: "inspect the codebase".into(),
                                mode: pb::AgentMode::Agent as i32,
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
    };

    let mut subagent_output = subagent_handle.subscribe();
    subagent_handle
        .command(CursorCommand::Append {
            seqno: 0,
            message: Box::new(subagent_request),
        })
        .await
        .unwrap();

    let mut subagent_seqno = 1;
    while let Some(frame) = tokio::time::timeout(Duration::from_secs(5), subagent_output.recv())
        .await
        .unwrap()
    {
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        if flags & connect::END_STREAM_FLAG != 0 {
            break;
        }
        let server = pb::AgentServerMessage::decode(payload).unwrap();
        if let Some(pb::agent_server_message::Message::KvServerMessage(kv)) = server.message {
            subagent_handle
                .command(CursorCommand::Append {
                    seqno: subagent_seqno,
                    message: Box::new(kv_ack(kv.id)),
                })
                .await
                .unwrap();
            subagent_seqno += 1;
        }
    }

    // Now parent stream receives the completion, wakes up, runs the follow-up, and finishes!
    let mut final_parent_checkpoint = None;
    let mut saw_parent_stream_end = false;
    while let Some(frame) = tokio::time::timeout(Duration::from_secs(5), parent_output.recv())
        .await
        .unwrap()
    {
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        if flags & connect::END_STREAM_FLAG != 0 {
            saw_parent_stream_end = true;
            break;
        }
        let server = pb::AgentServerMessage::decode(payload).unwrap();
        match server.message {
            Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                parent_handle
                    .command(CursorCommand::Append {
                        seqno: parent_seqno,
                        message: Box::new(kv_ack(kv.id)),
                    })
                    .await
                    .unwrap();
                parent_seqno += 1;
            }
            Some(pb::agent_server_message::Message::ConversationCheckpointUpdate(state)) => {
                if state.pending_tool_calls.is_empty() {
                    final_parent_checkpoint = Some(state);
                }
            }
            _ => {}
        }
    }
    assert!(saw_parent_stream_end, "parent stream must complete with END_STREAM");
    let final_chk = final_parent_checkpoint.expect("final parent checkpoint");
    let final_subagent = final_chk
        .subagent_runs_by_parent_tool_call_id
        .get("task-child-parent")
        .expect("subagent run in final checkpoint");
    assert_eq!(
        final_subagent.status,
        pb::SubagentRunStatus::Success as i32,
        "final checkpoint status must be Success"
    );
}

