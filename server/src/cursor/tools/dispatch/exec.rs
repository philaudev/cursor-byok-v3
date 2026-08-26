//! Direct Exec and dynamic MCP dispatch.

use crate::{cursor::proto::agent::v1 as pb, model::ToolCall, Result};

use super::{normalized, ToolStart};
use crate::cursor::tools::{
    codec, result,
    runtime::{CursorToolRuntime, ExecContext},
};

pub(super) async fn start(
    runtime: &CursorToolRuntime,
    call: &ToolCall,
    context: &ExecContext,
) -> Result<ToolStart> {
    let message = match normalized(&call.name).as_str() {
        "getmcptools" => {
            let id = runtime.reserve_exec(call, context).await?;
            codec::mcp_state_request(id, call)
        }
        "callmcptool" => {
            let requested_server = call
                .arguments
                .get("server")
                .or_else(|| call.arguments.get("providerIdentifier"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let mut requested_tool = call
                .arguments
                .get("toolName")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();

            // Resilient name extraction: if toolName is empty, check "name"
            if requested_tool.is_empty() {
                if let Some(raw_name) = call.arguments.get("name").and_then(serde_json::Value::as_str) {
                    if !requested_server.is_empty() && raw_name.starts_with(&format!("{requested_server}-")) {
                        requested_tool = raw_name.trim_start_matches(&format!("{requested_server}-"));
                    } else if raw_name.starts_with("user-") {
                        if let Some((_, rest)) = raw_name.split_once('-') {
                            requested_tool = rest;
                        }
                    } else {
                        requested_tool = raw_name;
                    }
                }
            }

            // Resilient lookup: Match exact, strip/add 'user-' prefix, or match tool name
            let resolved = context
                .mcp_routes
                .get(&(requested_server.to_string(), requested_tool.to_string()))
                .or_else(|| {
                    let alt_server = requested_server
                        .strip_prefix("user-")
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| format!("user-{requested_server}"));
                    context
                        .mcp_routes
                        .get(&(alt_server, requested_tool.to_string()))
                })
                .or_else(|| {
                    // Fallback: match by tool_name alone if server is omitted/ambiguous
                    context
                        .mcp_routes
                        .iter()
                        .find(|((_, t), _)| t == requested_tool)
                        .map(|(_, route)| route)
                });

            let Some(route) = resolved else {
                return Ok(ToolStart {
                    messages: Vec::new(),
                    completion: Some(result::mcp_failure(
                        call,
                        format!("MCP descriptor not found for {requested_server}/{requested_tool}"),
                    )?),
                });
            };
            let id = runtime.reserve_exec(call, context).await?;
            codec::mcp_meta_request(id, call, &route.provider_identifier, route)?
        }
        _ => {
            let id = runtime.reserve_exec(call, context).await?;
            codec::request(id, call, context)?
        }
    };
    Ok(ToolStart {
        messages: vec![message],
        completion: None,
    })
}

pub(super) async fn start_dynamic(
    runtime: &CursorToolRuntime,
    call: &ToolCall,
    definition: &pb::McpToolDefinition,
    context: &ExecContext,
) -> Result<ToolStart> {
    let id = runtime
        .reserve_dynamic_mcp(call, context, definition)
        .await?;
    Ok(ToolStart {
        messages: vec![codec::mcp_request(id, call, definition)?],
        completion: None,
    })
}
