//! Verifies local markdown rules are merged into the request-context message.
#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::sync::Arc;

use prost::Message;

use cursor_server::{
    cursor::{
        prompting::{PromptAssets, PromptCompiler},
        protocol::connect,
        protocol::proto::agent::v1 as pb,
        TransportCommand, TransportRegistry,
    },
    model::{ContentPart, ProjectedContent},
    provider::{FinishReason, ModelEvent},
};

#[tokio::test]
async fn local_markdown_rules_land_in_the_request_context_message() {
    let (_store_dir, store) = fixtures::temp_store().await;
    let provider = fake_provider::FakeProvider::default();
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "call-1".into(),
        },
        ModelEvent::TextStart,
        ModelEvent::TextDelta("ok".into()),
        ModelEvent::TextEnd,
        ModelEvent::Done(FinishReason::Stop),
    ]);
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();

    let rules_dir = tempfile::tempdir().unwrap();
    let rules_root = rules_dir.path().join("rules");
    std::fs::create_dir_all(&rules_root).unwrap();
    std::fs::write(rules_root.join("17353272.md"), "Always answer in haiku.").unwrap();

    let registry = TransportRegistry::with_local_rules(
        store,
        Arc::new(provider.clone()),
        PromptCompiler::new(assets),
        rules_root,
    );
    let handle = registry.get_or_create("rules-request").await.unwrap();
    let mut output = handle.subscribe();
    handle
        .command(TransportCommand::Append {
            seqno: 0,
            message: Box::new(user_run()),
        })
        .await
        .unwrap();

    let mut append_seqno = 1;
    loop {
        let frame = match tokio::time::timeout(std::time::Duration::from_secs(5), output.recv()).await {
            Ok(Some(frame)) => frame,
            Ok(None) => panic!("output closed prematurely"),
            Err(_) => panic!("timed out waiting for frame"),
        };
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        if flags & connect::END_STREAM_FLAG != 0 {
            break;
        }
        let server = pb::AgentServerMessage::decode(payload).unwrap();
        match server.message {
            Some(pb::agent_server_message::Message::ExecServerMessage(exec)) => {
                println!("Got ExecServerMessage: {:?}", exec);
                handle
                    .command(TransportCommand::Append {
                        seqno: append_seqno,
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
                append_seqno += 1;
                handle
                    .command(TransportCommand::Append {
                        seqno: append_seqno,
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
                append_seqno += 1;
            }
            Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                println!("Got KvServerMessage: {:?}", kv);
                handle
                    .command(TransportCommand::Append {
                        seqno: append_seqno,
                        message: Box::new(pb::AgentClientMessage {
                            message: Some(pb::agent_client_message::Message::KvClientMessage(
                                pb::KvClientMessage {
                                    id: kv.id,
                                    message: Some(pb::kv_client_message::Message::SetBlobResult(
                                        pb::SetBlobResult { error: None },
                                    )),
                                },
                            )),
                        }),
                    })
                    .await
                    .unwrap();
                append_seqno += 1;
            }
            other => {
                println!("Got other message: {:?}", other);
            }
        }
    }

    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    let context_texts = requests[0]
        .history
        .iter()
        .filter(|message| message.message_id.starts_with("request-context:"))
        .map(|message| {
            let ProjectedContent::Parts(parts) = &message.content else {
                panic!("request context message must be parts")
            };
            let [ContentPart::Text { text }] = parts.as_slice() else {
                panic!("request context message must be one text part")
            };
            text.clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        context_texts.len(),
        1,
        "exactly one request-context message is projected"
    );
    assert!(
        context_texts[0].contains("<user_rule>\nAlways answer in haiku.\n</user_rule>"),
        "local markdown rule must appear as a user rule: {}",
        context_texts[0]
    );

    registry.shutdown().await;
}

fn user_run() -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                action: Some(pb::ConversationAction {
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(pb::UserMessage {
                                text: "hello".into(),
                                message_id: "rules-user".into(),
                                mode: pb::AgentMode::Agent as i32,
                                ..Default::default()
                            }),
                            request_context: Some(pb::RequestContext::default()),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                conversation_id: Some("rules-conversation".into()),
                run_id: Some("rules-request".into()),
                requested_model: Some(pb::RequestedModel {
                    model_id: "test-model".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
    }
}
