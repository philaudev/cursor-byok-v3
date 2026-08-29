use crate::{
    cursor::{
        blob_sync::BlobSynchronizer,
        prompting::{Mode, PromptCompiler},
        proto::agent::v1 as pb,
    },
    model::{CanonicalMessage, ContentPart, MessageContent, Origin, Role},
    store::BlobId,
    Error, Result,
};

pub(crate) async fn compile_user_message_action(
    action: &pb::UserMessageAction,
    current_mode: i32,
    compiler: &PromptCompiler,
    blobs: &BlobSynchronizer,
) -> Result<CanonicalMessage> {
    let user = action.user_message.as_ref().ok_or_else(|| {
        Error::Protocol("Cursor user message action has no UserMessage".into())
    })?;
    compile(
        format!("user-message:{}", user.message_id),
        mode_from_proto_or(current_mode),
        user,
        action.request_context.as_ref().unwrap_or(&pb::RequestContext::default()),
        &action.prepend_user_messages.iter().map(|m| m.text.as_str()).collect::<Vec<_>>().join("\n\n"),
        compiler,
        blobs,
    ).await
}

pub(crate) async fn compile_injection(
    injection: &pb::InjectContextAction,
    mode: i32,
    compiler: &PromptCompiler,
    blobs: &BlobSynchronizer,
) -> Result<CanonicalMessage> {
    if injection.injection_id.is_empty() { return Err(Error::Protocol("InjectContextAction has no injection_id".into())); }
    let event_id = format!("inject-context:{}", injection.injection_id);
    match injection.payload.as_ref() {
        Some(pb::inject_context_action::Payload::UserContext(context)) => {
            let user = context.user_message.as_ref().ok_or_else(|| Error::Protocol("InjectContextAction UserContext has no UserMessage".into()))?;
            compile(event_id, mode_from_proto_or(mode), user, context.request_context.as_ref().unwrap_or(&pb::RequestContext::default()), "", compiler, blobs).await
        }
        Some(pb::inject_context_action::Payload::SystemContext(context)) => message(event_id, format!("<system_context_injection>\n<producer>{}</producer>\n{}\n</system_context_injection>", context.producer, context.content)),
        None => Err(Error::Protocol("InjectContextAction has no payload".into())),
    }
}

pub(crate) async fn compile(
    event_id: String, mode: Mode, user: &pb::UserMessage, _request_context: &pb::RequestContext,
    action_context: &str, compiler: &PromptCompiler, _blobs: &BlobSynchronizer,
) -> Result<CanonicalMessage> {
    let values = std::collections::BTreeMap::from([
        ("OPEN_FILES", String::new()), ("SELECTED_CONTEXT", String::new()),
        ("ACTION_CONTEXT", action_context.to_string()), ("TIMESTAMP", String::new()),
        ("USER_QUERY", user.text.clone()), ("DEBUG_SERVER_ENDPOINT", String::new()),
        ("DEBUG_LOG_PATH", String::new()), ("DEBUG_SESSION_ID", String::new()),
    ]);
    message(event_id, compiler.runtime_message(mode, &values)?)
}

pub(super) async fn user_event_id(
    input_id: &str, mode: Mode, user: &pb::UserMessage, request_context: &pb::RequestContext,
    action_context: &str, projected_request_context: Option<&MessageContent>, compiler: &PromptCompiler,
    blobs: &BlobSynchronizer,
) -> Result<String> {
    let runtime = compile("identity".into(), mode, user, request_context, action_context, compiler, blobs).await?;
    let semantic = serde_json::to_vec(&(projected_request_context, runtime.content))?;
    Ok(format!("{input_id}:{}", BlobId::digest(&semantic).to_base64()))
}

pub(super) fn compile_request_context(
    event_id: &str, request_context: &pb::RequestContext, history: &[CanonicalMessage],
) -> Result<Option<CanonicalMessage>> {
    let text = super::context::compile_context(request_context, "");
    if text.is_empty() || history.iter().rev().find(|m| m.message_id.starts_with("request-context:")).is_some_and(|m| m.content == MessageContent::Parts { parts: vec![ContentPart::Text { text: text.clone() }] }) { return Ok(None); }
    Ok(Some(CanonicalMessage::text(format!("request-context:{event_id}"), Role::User, Origin::Prompt, text)))
}

pub async fn compile_background(
    event_id: String, user: &pb::UserMessage, _request_context: &pb::RequestContext,
    action_context: &str, _blobs: &BlobSynchronizer,
) -> Result<(CanonicalMessage, String)> {
    let text = format!("{}\n<user_query>{}</user_query>", action_context.trim(), user.text);
    Ok((message(event_id, text.clone())?, text))
}

fn mode_from_proto_or(value: i32) -> Mode { super::prepare::mode_from_proto(value).unwrap_or(Mode::Agent) }
fn message(event_id: String, text: String) -> Result<CanonicalMessage> { Ok(CanonicalMessage { message_id: format!("runtime:{event_id}"), role: Role::User, origin: Origin::Runtime, content: MessageContent::Parts { parts: vec![ContentPart::Text { text }] }, runtime_event_id: Some(event_id) }) }
