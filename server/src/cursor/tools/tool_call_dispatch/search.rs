//! Dispatches search Tool calls.
//! Cursor tool orchestration for application-owned Semble search.

use crate::{
    cursor::tools::{
        runtime::now_ms,
        tool_call_result::{self as result, ToolResultSender},
    },
    model::ToolCall,
    search,
    store::Store,
    Result,
};

use super::ToolStart;

pub(super) fn start(
    results: &ToolResultSender,
    call: &ToolCall,
    store: Option<Store>,
) -> Result<ToolStart> {
    let tool_name = super::normalized(&call.name);
    let arguments = call.arguments.clone();
    let call = call.clone();
    let results = results.clone();
    let started_at_ms = now_ms();
    tokio::spawn(async move {
        let output = search::execute_semble(&tool_name, arguments, store).await;
        match result::semble(&call, started_at_ms, output) {
            Ok(completion) => results.send(completion),
            Err(error) => results.send_error(error),
        }
    });
    Ok(ToolStart {
        messages: Vec::new(),
        completion: None,
    })
}

pub(super) async fn tgrep_outcome(
    call: &ToolCall,
    configured_path: Option<&str>,
    registry: &search::TgrepRegistry,
) -> search::TgrepOutcome {
    let target_path_str = call
        .arguments
        .get("path")
        .or_else(|| call.arguments.get("target_directory"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(".");
    let target_path = std::path::Path::new(target_path_str);
    let repo_root = search::tgrep::find_repo_root(target_path);

    let readiness = registry.ensure_server(&repo_root, configured_path).await;

    if readiness == search::ServerReadiness::Unhealthy {
        if !search::tgrep::is_tgrep_available(configured_path) {
            return search::TgrepOutcome::Failure(search::TgrepFailure::Unavailable);
        }
        return search::TgrepOutcome::Failure(search::TgrepFailure::Infrastructure {
            reason: "tgrep server process is unhealthy or unreachable".into(),
        });
    }

    let force_no_index = matches!(
        readiness,
        search::ServerReadiness::Starting | search::ServerReadiness::Indexing
    );

    search::tgrep::execute_tgrep_outcome(&call.arguments, configured_path, force_no_index).await
}

pub(super) fn local_tgrep_completion(
    call: &ToolCall,
    started_at_ms: u64,
    outcome: search::TgrepOutcome,
) -> Result<ToolStart> {
    let output = match outcome {
        search::TgrepOutcome::Match(output) => Ok(output),
        search::TgrepOutcome::NoMatch => {
            let pattern = call
                .arguments
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".");
            Ok(format!(
                "No matches found for pattern `{pattern}` in {path}"
            ))
        }
        search::TgrepOutcome::Failure(failure) => Err(failure.to_string()),
    };
    Ok(ToolStart {
        messages: Vec::new(),
        completion: Some(result::grep_completion(call, started_at_ms, output)?),
    })
}
