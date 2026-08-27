use std::collections::{BTreeMap, HashSet};

use base64::{engine::general_purpose::STANDARD, Engine};
use uuid::Uuid;

use crate::{
    cursor::prompting::{Mode, PromptCompiler},
    cursor::{
        blob_sync::BlobSynchronizer,
        checkpoint::CheckpointBuilder,
        context_sync::RequestContextSynchronizer,
        projection,
        proto::agent::v1 as pb,
        tools::runtime::{ExecContext, SubagentModel},
    },
    model::{
        CanonicalMessage, ContentPart, ConversationId, MessageContent, Origin, PreparedRun,
        PromptSpec, Role, RunAction, RunId, RunKind, ToolCallContent, ToolResultContent,
    },
    store::{BlobId, Store},
    Error, Result,
};

use super::{background, context, model, runtime};

struct ActionProjection {
    mode: i32,
    turn_user: Option<pb::UserMessage>,
    action_context: String,
    event_id: Option<String>,
    input_id: Option<String>,
    starts_turn: bool,
    compacting: bool,
    background_completion: bool,
}

pub struct CursorRunContext {
    pub request_id: String,
    pub mode: i32,
    pub turn_user: Option<pb::UserMessage>,
    pub exec: ExecContext,
    pub dynamic_tools: BTreeMap<String, pb::McpToolDefinition>,
    pub checkpoint_prompt: PromptSpec,
    pub compacting: bool,
    pub background_completion: bool,
}

pub(crate) struct PrepareDependencies<'a> {
    pub compiler: &'a PromptCompiler,
    pub store: &'a Store,
    pub checkpoint: &'a CheckpointBuilder,
    pub blob_sync: &'a BlobSynchronizer,
    pub context_sync: &'a RequestContextSynchronizer,
}

pub(crate) async fn prepare(
    request_id: &str,
    request: &pb::AgentRunRequest,
    parent: Option<(RunId, String)>,
    dependencies: PrepareDependencies<'_>,
) -> Result<(PreparedRun, CursorRunContext)> {
    let PrepareDependencies {
        compiler,
        store,
        checkpoint,
        blob_sync,
        context_sync,
    } = dependencies;
    checkpoint
        .import_prefetched(&request.pre_fetched_blobs)
        .await?;
    let conversation_id = ConversationId::new(
        request
            .conversation_id
            .clone()
            .unwrap_or_else(|| request_id.into()),
    );
    // RunSSE/Bidi request_id identifies this concrete execution attempt. Cursor may
    // reuse AgentRunRequest.run_id when a queued or subagent-driven attempt resumes.
    let run_id = execution_run_id(request_id);
    let history = conversation_history(request);
    let mut base_messages = if request.conversation_state.is_some() {
        Some(
            checkpoint
                .hydrate_messages(request.conversation_state.as_ref())
                .await?,
        )
    } else {
        None
    };
    if let Some(history) = history.filter(|history| !history.messages.is_empty()) {
        let decoded = decode_conversation_history(history)?;
        match base_messages.as_mut() {
            Some(messages)
                if needs_history_restore(request.conversation_state.as_ref(), messages.len()) =>
            {
                messages.extend(decoded)
            }
            Some(_) => {}
            None => base_messages = Some(decoded),
        }
    }
    if let Some(trace) = blob_sync.trace() {
        let hydrated_messages = base_messages.as_deref().unwrap_or_default();
        let hydrated_images = hydrated_messages
            .iter()
            .map(|message| match &message.content {
                MessageContent::Parts { parts } => parts
                    .iter()
                    .filter(|part| matches!(part, ContentPart::Image { .. }))
                    .count(),
                _ => 0,
            })
            .sum::<usize>();
        let history = conversation_history(request);
        let history_source = if history.is_some_and(|history| !history.messages.is_empty()) {
            "conversation_history"
        } else if request.conversation_state.is_some()
            && !base_messages.as_deref().unwrap_or_default().is_empty()
        {
            "root_prompt_messages_json"
        } else {
            "none"
        };
        let summary = serde_json::json!({
            "checkpoint_root_count": request.conversation_state.as_ref().map_or(0, |state| state.root_prompt_messages_json.len()),
            "checkpoint_turn_count": request.conversation_state.as_ref().map_or(0, |state| state.turns.len()),
            "conversation_history_message_count": history.map_or(0, |history| history.messages.len()),
            "hydrated_message_count": hydrated_messages.len(),
            "hydrated_image_count": hydrated_images,
            "selected_source": history_source,
        });
        let encoded = serde_json::to_vec(&summary)?;
        trace
            .artifact("history_projection", "byok_server", &encoded, summary)
            .await;
    }
    let request_context = context::hydrate(request, context_sync).await?;
    let ActionProjection {
        mode: mode_number,
        mut turn_user,
        action_context,
        mut event_id,
        input_id,
        starts_turn,
        compacting,
        background_completion,
    } = action(request)?;
    let checkpoint_mode = if request.subagent_type_name.is_some() {
        Mode::Subagent
    } else {
        mode_from_proto(mode_number)?
    };
    let mut model = model::requested_model(request)?;
    if let Some(configured_model) = store.model(&model.model_id).await? {
        configured_model.configure(&mut model);
    }
    let dynamic = context::dynamic_mcp(request, &request_context)?;
    let custom_subagent = selected_custom_subagent(request, &request_context);
    let subagent_model_overrides = model::overrides(request)?;
    let subagents_disabled = model::override_for(&subagent_model_overrides, "generalPurpose")
        .is_some_and(|selection| matches!(selection, crate::model::SubagentModelOverride::Disabled));
    let mut checkpoint_prompt = compiler.prompt_spec_with_custom_instructions(
        checkpoint_mode,
        &model,
        &dynamic
            .values()
            .map(|(_, definition)| definition.clone())
            .collect::<Vec<_>>(),
        request.suppress_subagent_progress_update_tool == Some(true),
        custom_subagent.map(|agent| agent.prompt.as_str()),
    )?;
    if let Some(agent) = custom_subagent.filter(|agent| !agent.tools.is_empty()) {
        restrict_tools(&mut checkpoint_prompt, &agent.tools)?;
    }
    if subagents_disabled {
        checkpoint_prompt.tools.retain(|tool| tool.name != "Task");
    }
    let compaction_prompt = compiler.prompt_spec(Mode::Compaction, &model, &[], false)?;
    let prompt = if compacting {
        compaction_prompt.clone()
    } else {
        checkpoint_prompt.clone()
    };
    let base_revision_id = if compacting {
        match base_messages.as_deref() {
            Some(messages) if !messages.is_empty() => {
                validate_prompt_root(messages)?;
                store.import_revision(&conversation_id, messages).await?
            }
            Some(_) | None => store.ensure_conversation(&conversation_id).await?,
        }
    } else {
        let proposed_base_revision_id = match base_messages.as_mut() {
            Some(messages) if !messages.is_empty() => {
                validate_prompt_root(messages)?;
                messages.retain(|message| {
                    !(message.role == Role::System && message.origin == Origin::Prompt)
                });
                store.import_revision(&conversation_id, messages).await?
            }
            Some(_) | None => store.ensure_conversation(&conversation_id).await?,
        };
        match input_id.as_deref() {
            Some(input_id) => {
                store
                    .anchor_input(&conversation_id, input_id, proposed_base_revision_id)
                    .await?
            }
            None => proposed_base_revision_id,
        }
    };
    let mut projected_user_context = if input_id.is_some() && !compacting && !background_completion
    {
        runtime::compile_request_context(
            "identity",
            &request_context,
            base_messages.as_deref().unwrap_or_default(),
        )?
    } else {
        None
    };
    if event_id.is_none() {
        if let (Some(input_id), Some(user)) = (input_id.as_deref(), turn_user.as_ref()) {
            event_id = Some(
                runtime::user_event_id(
                    input_id,
                    checkpoint_mode,
                    user,
                    &request_context,
                    &action_context,
                    projected_user_context
                        .as_ref()
                        .map(|message| &message.content),
                    compiler,
                    blob_sync,
                )
                .await?,
            );
        }
    }
    let existing_runtime = match event_id.as_deref() {
        Some(event_id) => {
            store
                .message(&conversation_id, &format!("runtime:{event_id}"))
                .await?
        }
        _ => None,
    };
    let request_context_message = match event_id.as_deref() {
        Some(event_id) if !compacting && !background_completion => {
            let message_id = format!("request-context:{event_id}");
            match store.message(&conversation_id, &message_id).await? {
                Some(message) => Some(message),
                None if input_id.is_some() => projected_user_context.take().map(|mut message| {
                    message.message_id = message_id;
                    message
                }),
                None => runtime::compile_request_context(
                    event_id,
                    &request_context,
                    base_messages.as_deref().unwrap_or_default(),
                )?,
            }
        }
        _ => None,
    };
    let mut initial_messages = if compacting {
        Vec::new()
    } else {
        match (turn_user.clone(), event_id) {
            (Some(mut user), Some(event_id)) if background_completion => {
                let (message, text) = match existing_runtime {
                    Some(message) => {
                        let text = runtime_message_text(&message)?;
                        (message, text)
                    }
                    None => {
                        runtime::compile_background(
                            event_id,
                            &user,
                            &request_context,
                            &action_context,
                            blob_sync,
                        )
                        .await?
                    }
                };
                user.text = text;
                turn_user = Some(user);
                vec![message]
            }
            (Some(user), Some(event_id)) => {
                let runtime = match existing_runtime {
                    Some(message) => message,
                    None => {
                        runtime::compile(
                            event_id,
                            checkpoint_mode,
                            &user,
                            &request_context,
                            &action_context,
                            compiler,
                            blob_sync,
                        )
                        .await?
                    }
                };
                request_context_message
                    .into_iter()
                    .chain(std::iter::once(runtime))
                    .collect()
            }
            (None, None) => Vec::new(),
            _ => {
                return Err(Error::Protocol(
                    "Cursor action has an incomplete runtime event".into(),
                ))
            }
        }
    };
    let (base_revision_id, reused) = store
        .match_revision_prefix(&conversation_id, base_revision_id, &initial_messages)
        .await?;
    initial_messages.drain(..reused);
    let action = if compacting {
        RunAction::Compact
    } else if starts_turn {
        RunAction::Start
    } else {
        let pending_tool_round = match request
            .conversation_state
            .as_ref()
            .map(|state| state.pending_tool_calls.as_slice())
            .unwrap_or_default()
        {
            [] => None,
            [pending] => Some(projection::decode_pending(pending)?),
            pending => {
                return Err(Error::Protocol(format!(
                    "Cursor resume contains {} pending assistant messages",
                    pending.len()
                )))
            }
        };
        RunAction::Resume { pending_tool_round }
    };
    let kind = run_kind(request.subagent_type_name.as_deref(), parent)?;
    let exec = exec_context(
        request,
        &request_context,
        &conversation_id,
        &model.model_id,
        subagents_disabled,
        &subagent_model_overrides,
    );
    Ok((
        PreparedRun {
            run_id,
            cursor_request_id: Some(request_id.into()),
            conversation_id,
            kind,
            model,
            prompt,
            compaction_prompt,
            initial_messages,
            action,
            base_revision_id,
        },
        CursorRunContext {
            request_id: request_id.into(),
            mode: mode_number,
            turn_user,
            exec,
            dynamic_tools: dynamic
                .into_iter()
                .map(|(name, (wire, _))| (name, wire))
                .collect(),
            checkpoint_prompt,
            compacting,
            background_completion,
        },
    ))
}

fn needs_history_restore(
    state: Option<&pb::ConversationStateStructure>,
    root_message_count: usize,
) -> bool {
    let has_summary = state.and_then(|state| state.summary.as_ref()).is_some();
    if has_summary {
        return false;
    }
    root_message_count <= 1
}

fn conversation_history(request: &pb::AgentRunRequest) -> Option<&pb::ConversationHistory> {
    request
        .action
        .as_ref()
        .and_then(|action| action.action.as_ref())
        .and_then(|action| match action {
            pb::conversation_action::Action::UserMessageAction(action) => {
                action.conversation_history.as_ref()
            }
            _ => None,
        })
}

fn decode_conversation_history(history: &pb::ConversationHistory) -> Result<Vec<CanonicalMessage>> {
    use pb::{
        conversation_history_assistant_content::Content as AssistantContent,
        conversation_history_message::Message,
        conversation_history_tool_result_content::Content as ToolContent,
        conversation_history_user_content::Content as UserContent,
    };

    let mut messages = Vec::with_capacity(history.messages.len());
    for (index, entry) in history.messages.iter().enumerate() {
        let Some(entry) = entry.message.as_ref() else {
            continue;
        };
        let message = match entry {
            Message::User(user) => {
                let mut parts = Vec::new();
                for content in &user.content {
                    match content.content.as_ref() {
                        Some(UserContent::Text(text)) => parts.push(ContentPart::Text {
                            text: text.text.clone(),
                        }),
                        Some(UserContent::Image(image)) => parts.push(ContentPart::Image {
                            mime_type: image
                                .mime_type
                                .clone()
                                .unwrap_or_else(|| "application/octet-stream".into()),
                            data: STANDARD.decode(&image.data).map_err(|error| {
                                Error::Protocol(format!(
                                    "invalid conversation history image base64: {error}"
                                ))
                            })?,
                        }),
                        None => {}
                    }
                }
                CanonicalMessage {
                    message_id: format!("cursor-history:{index}"),
                    role: Role::User,
                    origin: Origin::User,
                    content: MessageContent::Parts { parts },
                    runtime_event_id: None,
                }
            }
            Message::Assistant(assistant) => {
                let mut text = String::new();
                let mut thinking = String::new();
                let mut tool_calls = Vec::new();
                for content in &assistant.content {
                    match content.content.as_ref() {
                        Some(AssistantContent::Text(value)) => text.push_str(&value.text),
                        Some(AssistantContent::Reasoning(value)) => thinking.push_str(&value.text),
                        Some(AssistantContent::RedactedReasoning(_)) | None => {}
                        Some(AssistantContent::ToolCall(call)) => {
                            let arguments = serde_json::from_str(&call.args_json)
                                .unwrap_or(serde_json::Value::Null);
                            tool_calls.push(ToolCallContent {
                                index: tool_calls.len(),
                                call_id: call.tool_call_id.clone(),
                                name: call.tool_name.clone(),
                                arguments,
                            });
                        }
                    }
                }
                CanonicalMessage {
                    message_id: format!("cursor-history:{index}"),
                    role: Role::Assistant,
                    origin: Origin::Assistant,
                    content: MessageContent::Assistant {
                        text,
                        thinking,
                        tool_round_id: (!tool_calls.is_empty()).then(|| {
                            crate::model::ToolRoundId::new(format!(
                                "cursor-history:{index}:tool-round"
                            ))
                        }),
                        replay_state: None,
                        tool_calls,
                    },
                    runtime_event_id: None,
                }
            }
            Message::Tool(tool) => {
                let mut content = String::new();
                let mut provider_parts = Vec::new();
                for part in &tool.content {
                    match part.content.as_ref() {
                        Some(ToolContent::Text(value)) => {
                            content.push_str(&value.text);
                            provider_parts.push(ContentPart::Text {
                                text: value.text.clone(),
                            });
                        }
                        Some(ToolContent::Image(image)) => {
                            let data = STANDARD.decode(&image.data).map_err(|error| {
                                Error::Protocol(format!(
                                    "invalid conversation history image base64: {error}"
                                ))
                            });
                            match data {
                                Ok(data) => provider_parts.push(ContentPart::Image {
                                    mime_type: image
                                        .mime_type
                                        .clone()
                                        .unwrap_or_else(|| "application/octet-stream".into()),
                                    data,
                                }),
                                Err(error) => return Err(error),
                            }
                        }
                        None => {}
                    }
                }
                CanonicalMessage {
                    message_id: format!("cursor-history:{index}"),
                    role: Role::Tool,
                    origin: Origin::Tool,
                    content: MessageContent::ToolResult(ToolResultContent {
                        call_id: tool.tool_call_id.clone(),
                        name: tool.tool_name.clone(),
                        content,
                        is_error: tool.is_error.unwrap_or(false),
                        image: None,
                        provider_parts,
                    }),
                    runtime_event_id: None,
                }
            }
        };
        messages.push(message);
    }
    Ok(messages)
}

fn runtime_message_text(message: &CanonicalMessage) -> Result<String> {
    let MessageContent::Parts { parts } = &message.content else {
        return Err(Error::Protocol(
            "stored runtime message does not contain parts".into(),
        ));
    };
    let Some(ContentPart::Text { text }) = parts.first() else {
        return Err(Error::Protocol(
            "stored runtime message does not start with text".into(),
        ));
    };
    Ok(text.clone())
}

fn run_kind(subagent_type_name: Option<&str>, parent: Option<(RunId, String)>) -> Result<RunKind> {
    match (subagent_type_name, parent) {
        (None | Some("side-chat"), _) => Ok(RunKind::Root),
        (Some(name), Some((parent_run_id, parent_tool_call_id))) => Ok(RunKind::Subagent {
            parent_run_id,
            parent_tool_call_id,
            kind: model::subagent_kind(name),
            background: false,
        }),
        (Some(_), None) => Err(Error::Protocol(
            "subagent Run is missing its parent Run and tool call".into(),
        )),
    }
}

fn validate_prompt_root(messages: &[CanonicalMessage]) -> Result<()> {
    let prompts = messages
        .iter()
        .filter(|message| message.role == Role::System && message.origin == Origin::Prompt)
        .collect::<Vec<_>>();
    let [prompt] = prompts.as_slice() else {
        return Err(Error::Protocol(format!(
            "Cursor history contains {} system prompt roots",
            prompts.len()
        )));
    };
    let MessageContent::Parts { parts } = &prompt.content else {
        return Err(Error::Protocol(
            "Cursor system prompt root is not textual content".into(),
        ));
    };
    let [ContentPart::Text { .. }] = parts.as_slice() else {
        return Err(Error::Protocol(
            "Cursor system prompt root is not one text part".into(),
        ));
    };
    Ok(())
}

fn execution_run_id(request_id: &str) -> RunId {
    let execution_id = Uuid::new_v4().simple().to_string();
    RunId::new(format!("{request_id}:{}", &execution_id[..8]))
}

fn action(request: &pb::AgentRunRequest) -> Result<ActionProjection> {
    let conversation_mode = request
        .conversation_state
        .as_ref()
        .and_then(|state| state.mode);
    let mode = conversation_mode.unwrap_or(pb::AgentMode::Agent as i32);
    let Some(action) = request
        .action
        .as_ref()
        .and_then(|action| action.action.as_ref())
    else {
        return Ok(ActionProjection {
            mode,
            turn_user: None,
            action_context: String::new(),
            event_id: None,
            input_id: None,
            starts_turn: false,
            compacting: false,
            background_completion: false,
        });
    };
    match action {
        pb::conversation_action::Action::UserMessageAction(action) => {
            let user = action.user_message.as_ref().ok_or_else(|| {
                Error::Protocol("Cursor user message action has no UserMessage".into())
            })?;
            let mode = if user.mode == pb::AgentMode::Unspecified as i32 {
                conversation_mode.unwrap_or(user.mode)
            } else {
                user.mode
            };
            if user.message_id.is_empty() {
                return Err(Error::Protocol(
                    "Cursor user message action has no message_id".into(),
                ));
            }
            if user.text.trim() == "/summarize" {
                return Ok(ActionProjection {
                    mode,
                    turn_user: Some(user.clone()),
                    action_context: String::new(),
                    event_id: None,
                    input_id: None,
                    starts_turn: false,
                    compacting: true,
                    background_completion: false,
                });
            }
            let mut context = action
                .prepend_user_messages
                .iter()
                .map(|message| message.text.trim())
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            context.extend(
                user.subagent_system_reminder
                    .iter()
                    .filter(|text| !text.is_empty())
                    .cloned(),
            );
            let input_id = format!("cursor:user:{}", user.message_id);
            Ok(ActionProjection {
                mode,
                turn_user: Some(user.clone()),
                action_context: context.join("\n\n"),
                event_id: None,
                input_id: Some(input_id),
                starts_turn: true,
                compacting: false,
                background_completion: false,
            })
        }
        pb::conversation_action::Action::BackgroundTaskCompletionAction(action) => {
            let projection = background::project_background_completion(action, mode)?;
            let event_id = projection.turn_user.message_id.clone();
            Ok(ActionProjection {
                mode,
                action_context: projection.context,
                event_id: Some(event_id),
                input_id: None,
                turn_user: Some(projection.turn_user),
                starts_turn: true,
                compacting: false,
                background_completion: true,
            })
        }
        pb::conversation_action::Action::ExecutePlanAction(action) => execute_plan(action),
        pb::conversation_action::Action::SummarizeAction(_) => Ok(ActionProjection {
            mode,
            turn_user: None,
            action_context: String::new(),
            event_id: None,
            input_id: None,
            starts_turn: false,
            compacting: true,
            background_completion: false,
        }),
        _ => Ok(ActionProjection {
            mode,
            turn_user: None,
            action_context: String::new(),
            event_id: None,
            input_id: None,
            starts_turn: false,
            compacting: false,
            background_completion: false,
        }),
    }
}

fn execute_plan(action: &pb::ExecutePlanAction) -> Result<ActionProjection> {
    let plan = action
        .plan_file_content
        .as_deref()
        .or_else(|| action.plan.as_ref().map(|plan| plan.plan.as_str()))
        .filter(|plan| !plan.trim().is_empty())
        .ok_or_else(|| Error::Protocol("ExecutePlan is missing plan content".into()))?;
    let source = action
        .plan_file_uri
        .as_deref()
        .or(action.plan_file_path.as_deref())
        .filter(|source| !source.is_empty());
    let action_context = match source {
        Some(source) => {
            format!("<approved_plan>\n<plan_file>{source}</plan_file>\n{plan}\n</approved_plan>")
        }
        None => format!("<approved_plan>\n{plan}\n</approved_plan>"),
    };
    let identity = BlobId::digest(
        format!(
            "{}\0{}\0{}\0{}\0{}",
            action.execution_mode,
            action.plan_id.as_deref().unwrap_or_default(),
            action.kickoff_message_id.as_deref().unwrap_or_default(),
            source.unwrap_or_default(),
            plan,
        )
        .as_bytes(),
    )
    .to_base64();
    let event_id = format!("execute-plan:{identity}");
    Ok(ActionProjection {
        mode: action.execution_mode,
        turn_user: Some(pb::UserMessage {
            text: "Execute the approved plan.".into(),
            message_id: event_id.clone(),
            mode: action.execution_mode,
            ..Default::default()
        }),
        action_context,
        event_id: Some(event_id),
        input_id: None,
        starts_turn: true,
        compacting: false,
        background_completion: false,
    })
}

pub(super) fn mode_from_proto(mode: i32) -> Result<Mode> {
    let mode = pb::AgentMode::try_from(mode)
        .map_err(|_| Error::Protocol(format!("unknown Cursor agent mode: {mode}")))?;
    match mode {
        pb::AgentMode::Agent => Ok(Mode::Agent),
        pb::AgentMode::Ask => Ok(Mode::Ask),
        pb::AgentMode::Plan => Ok(Mode::Plan),
        pb::AgentMode::Debug => Ok(Mode::Debug),
        pb::AgentMode::Multitask => Ok(Mode::Multitask),
        mode => Err(Error::Protocol(format!(
            "unsupported Cursor agent mode: {}",
            mode.as_str_name()
        ))),
    }
}

fn selected_custom_subagent<'a>(
    request: &pb::AgentRunRequest,
    request_context: &'a pb::RequestContext,
) -> Option<&'a pb::CustomSubagent> {
    request
        .subagent_type_name
        .as_deref()
        .and_then(|name| request_context.custom_subagents.iter().find(|agent| agent.name == name))
}

fn restrict_tools(prompt: &mut PromptSpec, allowed: &[String]) -> Result<()> {
    let allowed = allowed.iter().map(String::as_str).collect::<HashSet<_>>();
    let unknown = allowed
        .iter()
        .filter(|name| !prompt.tools.iter().any(|tool| tool.name == **name))
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(Error::Protocol(format!(
            "custom subagent declares unavailable tools: {}",
            unknown.into_iter().copied().collect::<Vec<_>>().join(", ")
        )));
    }
    prompt.tools.retain(|tool| allowed.contains(tool.name.as_str()));
    Ok(())
}

fn exec_context(
    request: &pb::AgentRunRequest,
    request_context: &pb::RequestContext,
    conversation_id: &ConversationId,
    model_id: &str,
    subagents_disabled: bool,
    overrides: &[(
        crate::model::SubagentKind,
        crate::model::SubagentModelOverride,
    )],
) -> ExecContext {
    let subagent_models = overrides
        .iter()
        .map(|(kind, selection)| {
            let name = match kind {
                crate::model::SubagentKind::GeneralPurpose => "generalPurpose".to_string(),
                crate::model::SubagentKind::Named(name) => name.clone(),
            };
            let model = match selection {
                crate::model::SubagentModelOverride::Explicit(model) => {
                    SubagentModel::Model(model.model_id.clone())
                }
                crate::model::SubagentModelOverride::Inherit => SubagentModel::Model(model_id.into()),
                crate::model::SubagentModelOverride::Disabled => SubagentModel::Disabled,
            };
            (name, model)
        })
        .collect();
    let subagent_model = model::override_for(
        overrides,
        request.subagent_type_name.as_deref().unwrap_or("generalPurpose"),
    )
    .map(|value| match value {
        crate::model::SubagentModelOverride::Explicit(model) => {
            SubagentModel::Model(model.model_id.clone())
        }
        crate::model::SubagentModelOverride::Inherit => SubagentModel::Model(model_id.into()),
        crate::model::SubagentModelOverride::Disabled => SubagentModel::Disabled,
    });
    ExecContext {
        conversation_id: conversation_id.to_string(),
        root_conversation_id: request
            .conversation_group_id
            .clone()
            .unwrap_or_else(|| conversation_id.to_string()),
        default_subagent_model: model_id.into(),
        subagent_model,
        subagent_models,
        custom_subagents: request_context.custom_subagents.clone(),
        allow_subagents: request.subagent_type_name.is_none() && !subagents_disabled,
        subagents_disabled,
        terminals_folder: request_context
            .env
            .as_ref()
            .map(|env| env.terminals_folder.clone())
            .unwrap_or_default(),
        admin_command_denylist: request_context.admin_command_denylist.clone(),
        mcp_routes: context::meta_mcp_routes(request_context),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compacted_checkpoint_keeps_summary_without_restoring_full_history() {
        let compacted = pb::ConversationStateStructure {
            root_prompt_messages_json: vec![vec![1], vec![2]],
            summary: Some(vec![3]),
            ..Default::default()
        };

        assert!(!needs_history_restore(Some(&compacted), 2));
        assert!(!needs_history_restore(Some(&compacted), 1));
        assert!(needs_history_restore(None, 1));
    }

    #[test]
    fn restored_system_root_is_structural_not_bound_to_the_next_model() {
        let prompt = CanonicalMessage::text(
            "root",
            Role::System,
            Origin::Prompt,
            "prompt from the previous model",
        );
        validate_prompt_root(std::slice::from_ref(&prompt)).unwrap();
        assert!(validate_prompt_root(&[prompt.clone(), prompt]).is_err());
    }

    #[test]
    fn unsupported_cursor_mode_is_not_silently_treated_as_agent() {
        assert_eq!(
            mode_from_proto(pb::AgentMode::Agent as i32).unwrap(),
            Mode::Agent
        );
        assert!(mode_from_proto(pb::AgentMode::Project as i32).is_err());
        assert!(mode_from_proto(99).is_err());
    }

    #[test]
    fn side_chat_without_task_parent_is_an_independent_root_run() {
        assert!(matches!(
            run_kind(Some("side-chat"), None).unwrap(),
            RunKind::Root
        ));
    }

    #[test]
    fn task_subagent_without_parent_is_still_rejected() {
        assert!(matches!(
            run_kind(Some("explore"), None),
            Err(Error::Protocol(message))
                if message == "subagent Run is missing its parent Run and tool call"
        ));
    }

    #[test]
    fn execution_run_id_keeps_the_request_id_and_adds_eight_uuid_hex_digits() {
        let run_id = execution_run_id("01bba7c5-9c00-4922-b1df-1f58146b5d90");
        let suffix = run_id
            .as_str()
            .strip_prefix("01bba7c5-9c00-4922-b1df-1f58146b5d90:")
            .unwrap();

        assert_eq!(suffix.len(), 8);
        assert!(suffix.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn current_user_message_consumes_the_mode_instead_of_history_mode() {
        let request = pb::AgentRunRequest {
            conversation_state: Some(pb::ConversationStateStructure {
                mode: Some(pb::AgentMode::Agent as i32),
                ..Default::default()
            }),
            action: Some(pb::ConversationAction {
                action: Some(pb::conversation_action::Action::UserMessageAction(
                    pb::UserMessageAction {
                        user_message: Some(pb::UserMessage {
                            text: "explain".into(),
                            message_id: "user-message".into(),
                            mode: pb::AgentMode::Ask as i32,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            }),
            ..Default::default()
        };
        let projection = action(&request).unwrap();
        assert_eq!(projection.mode, pb::AgentMode::Ask as i32);
        assert_eq!(
            projection.input_id.as_deref(),
            Some("cursor:user:user-message")
        );
        assert_eq!(mode_from_proto(projection.mode).unwrap(), Mode::Ask);
    }

    #[test]
    fn queued_user_message_without_mode_inherits_conversation_mode() {
        let request = pb::AgentRunRequest {
            conversation_state: Some(pb::ConversationStateStructure {
                mode: Some(pb::AgentMode::Agent as i32),
                ..Default::default()
            }),
            action: Some(pb::ConversationAction {
                action: Some(pb::conversation_action::Action::UserMessageAction(
                    pb::UserMessageAction {
                        user_message: Some(pb::UserMessage {
                            text: "queued follow-up".into(),
                            message_id: "queued-user-message".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            }),
            ..Default::default()
        };

        let projection = action(&request).unwrap();

        assert_eq!(projection.mode, pb::AgentMode::Agent as i32);
        assert_eq!(mode_from_proto(projection.mode).unwrap(), Mode::Agent);
    }

    #[test]
    fn queued_messages_keep_distinct_input_anchors_until_runtime_identity_is_compiled() {
        let request = |message_id: &str| pb::AgentRunRequest {
            action: Some(pb::ConversationAction {
                action: Some(pb::conversation_action::Action::UserMessageAction(
                    pb::UserMessageAction {
                        user_message: Some(pb::UserMessage {
                            text: "queued follow-up".into(),
                            message_id: message_id.into(),
                            mode: pb::AgentMode::Agent as i32,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            }),
            ..Default::default()
        };

        let first = action(&request("message-one")).unwrap();
        let second = action(&request("message-two")).unwrap();

        assert_eq!(first.event_id, None);
        assert_eq!(second.event_id, None);
        assert_eq!(first.input_id.as_deref(), Some("cursor:user:message-one"));
        assert_eq!(second.input_id.as_deref(), Some("cursor:user:message-two"));
        assert_ne!(first.input_id, second.input_id);
    }

    #[test]
    fn execute_plan_appends_the_approved_plan_as_a_stable_runtime_event() {
        let execute = pb::ExecutePlanAction {
            plan_file_uri: Some("file:///workspace/example.plan.md".into()),
            plan_file_content: Some("# Build\n\n- implement it".into()),
            execution_mode: pb::AgentMode::Agent as i32,
            ..Default::default()
        };
        let request = pb::AgentRunRequest {
            action: Some(pb::ConversationAction {
                action: Some(pb::conversation_action::Action::ExecutePlanAction(
                    execute.clone(),
                )),
                ..Default::default()
            }),
            ..Default::default()
        };

        let first = action(&request).unwrap();
        let second = action(&request).unwrap();
        assert_eq!(first.mode, pb::AgentMode::Agent as i32);
        assert!(first.starts_turn);
        assert_eq!(first.event_id, second.event_id);
        assert_eq!(first.input_id, None);
        assert_eq!(
            first.turn_user.as_ref().map(|user| user.text.as_str()),
            Some("Execute the approved plan.")
        );
        assert!(first
            .action_context
            .contains("file:///workspace/example.plan.md"));
        assert!(first.action_context.contains("# Build\n\n- implement it"));
    }

    #[test]
    fn execute_plan_requires_content() {
        let result = execute_plan(&pb::ExecutePlanAction {
            execution_mode: pb::AgentMode::Agent as i32,
            ..Default::default()
        });
        assert!(matches!(
            result,
            Err(Error::Protocol(message)) if message.contains("missing plan content")
        ));
    }
}
