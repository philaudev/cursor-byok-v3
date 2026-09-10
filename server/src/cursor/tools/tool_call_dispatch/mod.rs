//! Dispatches Tool calls to their execution adapters.
mod await_shell;
mod edit;
mod exec;
mod inspect_changes;
mod interaction;
mod local;
mod search;

use std::collections::BTreeMap;

use crate::{
    cursor::protocol::proto::agent::v1 as pb,
    model::ToolCall,
    search::{WebFetch, WebSearch},
    store::Store,
    Error, Result,
};

use super::{
    compat,
    runtime::{CursorToolRuntime, ExecContext, PendingInteraction},
    tool_call_result::{ToolCompletion, ToolResultSender},
};

pub(super) struct ToolStart {
    pub messages: Vec<pb::AgentServerMessage>,
    pub completion: Option<ToolCompletion>,
}

pub(super) enum InteractionContinuation {
    Completed(Box<ToolCompletion>),
    Pending,
}

pub(super) async fn start(
    runtime: &CursorToolRuntime,
    results: &ToolResultSender,
    call: &ToolCall,
    message_index: usize,
    dynamic_mcp: &BTreeMap<String, pb::McpToolDefinition>,
    context: &ExecContext,
    store: Option<&Store>,
    tgrep_registry: &crate::search::TgrepRegistry,
) -> Result<ToolStart> {
    if let Some(definition) = dynamic_mcp.get(&call.name) {
        return exec::start_dynamic(runtime, call, definition, context).await;
    }

    if is_mcp_auth(call) {
        return interaction::start(runtime, call).await;
    }

    if context.task_disabled(call) {
        return local::subagents_disabled(call);
    }

    let normalized_call = normalize_block_until_ms(call)?;
    let call = normalized_call.as_ref().unwrap_or(call);
    let tool_name = normalized(&call.name);

    if tool_name == "grep" {
        if let Some(store) = store {
            if let Ok(settings) = store.search_settings().await {
                match settings.grep_engine {
                    crate::store::GrepEngine::Ripgrep => {}
                    crate::store::GrepEngine::Tgrep => {
                        let outcome = search::tgrep_outcome(
                            call,
                            settings.tgrep_path.as_deref(),
                            tgrep_registry,
                        )
                        .await;
                        return search::local_tgrep_completion(call, crate::cursor::tools::runtime::now_ms(), outcome);
                    }
                    crate::store::GrepEngine::Auto => {
                        let outcome = search::tgrep_outcome(
                            call,
                            settings.tgrep_path.as_deref(),
                            tgrep_registry,
                        )
                        .await;
                        if !outcome.should_auto_fallback() {
                            return search::local_tgrep_completion(
                                call,
                                crate::cursor::tools::runtime::now_ms(),
                                outcome,
                            );
                        }
                    }
                }
            }
        }
    }

    match tool_name.as_str() {
        "shell" | "bash" | "read" | "delete" | "grep" | "glob" | "ls" | "readlints" | "task"
        | "callmcptool" | "fetchmcpresource" | "getmcptools" => {
            exec::start(runtime, call, context).await
        }
        "write" | "strreplace" | "editnotebook" => edit::start(runtime, call, context).await,
        "askquestion" | "websearch" | "webfetch" | "switchmode" | "createplan"
        | "generateimage" => interaction::start(runtime, call).await,
        "todowrite" | "updatecurrentstep" => local::start(call, message_index),
        "awaitshell" => await_shell::start(runtime, results, call, context).await,
        "semblesearch" | "semblefindrelated" => search::start(results, call, store.cloned()),
        "inspectchanges" => inspect_changes::start(results, call, context),
        _ => Ok(unavailable_tool(call)),
    }
}

fn unavailable_tool(call: &ToolCall) -> ToolStart {
    ToolStart {
        messages: Vec::new(),
        completion: Some(compat::failure(call)),
    }
}

fn normalize_block_until_ms(call: &ToolCall) -> Result<Option<ToolCall>> {
    if !is_shell_tool(&call.name) {
        return Ok(None);
    }
    let Some(value) = call.arguments.get("block_until_ms") else {
        return Ok(None);
    };

    let integer = if let Some(value) = value.as_i64() {
        value
    } else {
        let value = value.as_f64().ok_or_else(|| {
            Error::Protocol(format!("{} block_until_ms must be an integer", call.name))
        })?;
        if !value.is_finite() || value.fract() != 0.0 {
            return Err(Error::Protocol(format!(
                "{} block_until_ms must be an integer",
                call.name
            )));
        }
        if value < i64::MIN as f64 || value > i64::MAX as f64 {
            return Err(Error::Protocol(format!(
                "{} block_until_ms is out of range",
                call.name
            )));
        }
        value as i64
    };

    if integer < 0 {
        return Err(Error::Protocol(format!(
            "{} block_until_ms is out of range",
            call.name
        )));
    }

    if value.as_i64().is_some() {
        return Ok(None);
    }

    let mut normalized_call = call.clone();
    normalized_call
        .arguments
        .as_object_mut()
        .ok_or_else(|| Error::Protocol(format!("{} arguments must be a JSON object", call.name)))?
        .insert("block_until_ms".into(), serde_json::Value::from(integer));
    Ok(Some(normalized_call))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cursor::tools::{
            runtime::CursorToolRuntime,
            tool_call_result::{tool_result_channel, ToolResultSender},
        },
        model::ToolCall,
        store::{GrepEngine, SearchSettings, Store},
    };
    use serde_json::json;
    use std::{
        collections::{BTreeMap, HashSet},
        sync::Arc,
    };

    fn grep_call(call_id: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            index: 0,
            call_id: call_id.into(),
            model_call_id: "model:0".into(),
            name: "Grep".into(),
            arguments_text: arguments.to_string(),
            arguments,
            argument_error: None,
        }
    }

    async fn dispatcher_with_engine(engine: GrepEngine) -> (Store, ToolResultSender) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!("sqlite://{}", directory.path().join("test.db").display()))
            .await
            .unwrap();
        store
            .set_search_settings(SearchSettings {
                grep_engine: engine,
                tgrep_path: Some("missing-tgrep.exe".into()),
            })
            .await
            .unwrap();
        let (results, _receiver) = tool_result_channel();
        (store, results)
    }

    #[tokio::test]
    async fn auto_infrastructure_failure_falls_back_once_without_local_completion() {
        let fake_binary = tempfile::NamedTempFile::new().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!("sqlite://{}", directory.path().join("test.db").display()))
            .await
            .unwrap();
        store
            .set_search_settings(SearchSettings {
                grep_engine: GrepEngine::Auto,
                tgrep_path: Some(fake_binary.path().to_string_lossy().into_owned()),
            })
            .await
            .unwrap();
        let (results, _receiver) = tool_result_channel();
        let call = grep_call("auto-infrastructure", json!({"pattern": "needle"}));
        let started = start(
            &CursorToolRuntime::default(),
            &results,
            &call,
            0,
            &BTreeMap::new(),
            &ExecContext::default(),
            Some(&store),
            &crate::search::TgrepRegistry::default(),
        )
        .await
        .unwrap();

        assert_eq!(started.messages.len(), 1);
        assert!(started.completion.is_none());
        let Some(pb::agent_server_message::Message::ExecServerMessage(exec)) =
            started.messages[0].message.as_ref()
        else {
            panic!("expected one protocol fallback request")
        };
        assert_eq!(exec.exec_id, "auto-infrastructure");
        assert!(matches!(exec.message, Some(pb::exec_server_message::Message::GrepArgs(_))));
    }

    #[tokio::test]
    async fn explicit_tgrep_infrastructure_failure_does_not_fallback() {
        let fake_binary = tempfile::NamedTempFile::new().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!("sqlite://{}", directory.path().join("test.db").display()))
            .await
            .unwrap();
        store
            .set_search_settings(SearchSettings {
                grep_engine: GrepEngine::Tgrep,
                tgrep_path: Some(fake_binary.path().to_string_lossy().into_owned()),
            })
            .await
            .unwrap();
        let (results, _receiver) = tool_result_channel();
        let call = grep_call("tgrep-infrastructure", json!({"pattern": "needle"}));
        let started = start(
            &CursorToolRuntime::default(),
            &results,
            &call,
            0,
            &BTreeMap::new(),
            &ExecContext::default(),
            Some(&store),
            &crate::search::TgrepRegistry::default(),
        )
        .await
        .unwrap();

        assert!(started.messages.is_empty());
        let completion = started.completion.expect("tgrep-only must complete locally");
        assert_eq!(completion.result().call_id, "tgrep-infrastructure");
        assert!(completion.result().is_error);
    }

    #[tokio::test]
    async fn auto_fallback_reserve_failure_returns_one_terminal_error_without_retry() {
        let (store, results) = dispatcher_with_engine(GrepEngine::Auto).await;
        let runtime = CursorToolRuntime::with_shared_ids(Arc::new(std::sync::atomic::AtomicU32::new(
            u32::MAX,
        )));
        let call = grep_call("auto-reserve-failure", json!({"pattern": "needle"}));
        let completed = HashSet::new();
        let started = HashSet::new();
        let dispatched = crate::cursor::tools::ToolDispatcher::with_results(
            runtime,
            results,
            store,
            crate::search::WebCache::default(),
            crate::search::TgrepRegistry::default(),
        )
        .start_batch(
            &[call],
            crate::cursor::tools::ToolBatchState {
                completed: &completed,
                started: &started,
                response_text: "",
                response_thinking: "",
            },
            &[],
            &BTreeMap::new(),
            &ExecContext::default(),
        )
        .await
        .unwrap();

        assert_eq!(dispatched.len(), 1);
        assert!(dispatched[0].messages.is_empty());
        assert!(dispatched[0].completion.as_ref().is_some_and(|completion| {
            completion.result().call_id == "auto-reserve-failure" && completion.result().is_error
        }));
    }

    #[tokio::test]
    async fn auto_unavailable_returns_one_protocol_request() {
        let (store, results) = dispatcher_with_engine(GrepEngine::Auto).await;
        let call = grep_call("auto-unavailable", json!({"pattern": "needle"}));
        let started = start(
            &CursorToolRuntime::default(),
            &results,
            &call,
            0,
            &BTreeMap::new(),
            &ExecContext::default(),
            Some(&store),
            &crate::search::TgrepRegistry::default(),
        )
        .await
        .unwrap();

        assert_eq!(started.messages.len(), 1);
        assert!(started.completion.is_none());
        let Some(pb::agent_server_message::Message::ExecServerMessage(exec)) =
            started.messages[0].message.as_ref()
        else {
            panic!("expected protocol Grep request")
        };
        assert_eq!(exec.exec_id, "auto-unavailable");
        let Some(pb::exec_server_message::Message::GrepArgs(args)) = exec.message.as_ref() else {
            panic!("expected GrepArgs")
        };
        assert_eq!(args.tool_call_id, "auto-unavailable");
    }

    #[tokio::test]
    async fn explicit_tgrep_unavailable_returns_one_local_error() {
        let (store, results) = dispatcher_with_engine(GrepEngine::Tgrep).await;
        let call = grep_call("tgrep-unavailable", json!({"pattern": "needle"}));
        let started = start(
            &CursorToolRuntime::default(),
            &results,
            &call,
            0,
            &BTreeMap::new(),
            &ExecContext::default(),
            Some(&store),
            &crate::search::TgrepRegistry::default(),
        )
        .await
        .unwrap();

        assert!(started.messages.is_empty());
        let completion = started.completion.expect("explicit tgrep must complete locally");
        assert_eq!(completion.result().call_id, "tgrep-unavailable");
        assert!(completion.result().is_error);
        assert!(completion.result().content.contains("tgrep binary not found"));
    }

    #[tokio::test]
    async fn auto_no_match_returns_one_local_success() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!("sqlite://{}", directory.path().join("test.db").display()))
            .await
            .unwrap();
        store
            .set_search_settings(SearchSettings {
                grep_engine: GrepEngine::Auto,
                tgrep_path: None,
            })
            .await
            .unwrap();
        let (results, _receiver) = tool_result_channel();
        let call = grep_call(
            "auto-no-match",
            json!({"pattern": "__definitely_not_present__", "path": "src/search/tgrep.rs"}),
        );
        let started = start(
            &CursorToolRuntime::default(),
            &results,
            &call,
            0,
            &BTreeMap::new(),
            &ExecContext::default(),
            Some(&store),
            &crate::search::TgrepRegistry::default(),
        )
        .await
        .unwrap();

        assert!(started.messages.is_empty());
        let completion = started.completion.expect("no-match must complete locally");
        assert_eq!(completion.result().call_id, "auto-no-match");
        assert!(!completion.result().is_error);
        assert!(completion.result().content.contains("No matches found"));
    }
}

fn is_mcp_auth(call: &ToolCall) -> bool {
    normalized(&call.name) == "callmcptool"
        && call
            .arguments
            .get("toolName")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|tool| normalized(tool) == "mcpauth")
}

pub(super) async fn resume_interaction(
    results: &ToolResultSender,
    search: &WebSearch,
    fetch: &WebFetch,
    pending: PendingInteraction,
    response: &pb::InteractionResponse,
) -> Result<InteractionContinuation> {
    interaction::resume(results, search, fetch, pending, response).await
}

fn is_shell_tool(name: &str) -> bool {
    matches!(normalized(name).as_str(), "shell" | "bash")
}

pub(super) fn normalized(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
