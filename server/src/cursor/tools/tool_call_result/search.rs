//! Converts search completions into Tool results.
//! Cursor MCP-card rendering for direct Semble Agent tools.

use serde_json::Value;

use crate::{
    cursor::protocol::proto::agent::v1 as pb,
    model::{ToolCall, ToolResult},
    Result,
};

use super::ToolCompletion;

const PROVIDER_IDENTIFIER: &str = "builtin-semble";

pub(crate) fn complete(
    call: &ToolCall,
    started_at_ms: u64,
    output: std::result::Result<Value, String>,
) -> Result<ToolCompletion> {
    use pb::{mcp_tool_result::Result as McpResult, tool_call::Tool};

    let (tool_name, fallback_description) = match normalized(&call.name).as_str() {
        "semblesearch" => ("search", "Search the codebase"),
        "semblefindrelated" => ("find_related", "Find related code"),
        "inspectchanges" => ("inspect_changes", "Inspect uncommitted git changes"),
        _ => (call.name.as_str(), "Search the codebase"),
    };
    let description = call
        .arguments
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_description)
        .to_owned();
    let arguments = call
        .arguments
        .as_object()
        .map(|arguments| {
            let mut arguments = arguments.clone();
            arguments.remove("description");
            crate::cursor::tools::codec::json_object_to_prost(&arguments)
        })
        .unwrap_or_default();
    let (content, is_error, result) = match output {
        Ok(value) => {
            let content = serde_json::to_string_pretty(&value)?;
            let structured_content = value.as_object().map(|value| prost_types::Struct {
                fields: crate::cursor::tools::codec::json_object_to_prost(value)
                    .into_iter()
                    .collect(),
            });
            (
                content.clone(),
                false,
                McpResult::Success(pb::McpSuccess {
                    content: vec![pb::McpToolResultContentItem {
                        content: Some(pb::mcp_tool_result_content_item::Content::Text(
                            pb::McpTextContent {
                                text: content,
                                output_location: None,
                            },
                        )),
                    }],
                    is_error: false,
                    structured_content,
                }),
            )
        }
        Err(error) => (
            error.clone(),
            true,
            McpResult::Error(pb::McpToolError {
                error,
                read_tool_def_reminder: String::new(),
            }),
        ),
    };
    Ok(ToolCompletion::new(
        call,
        started_at_ms,
        ToolResult {
            call_id: call.call_id.clone(),
            content,
            is_error,
            image: None,
        },
        Tool::McpToolCall(pb::McpToolCall {
            args: Some(pb::McpArgs {
                name: tool_name.into(),
                args: arguments,
                tool_call_id: call.call_id.clone(),
                provider_identifier: PROVIDER_IDENTIFIER.into(),
                tool_name: tool_name.into(),
                server_identifier: PROVIDER_IDENTIFIER.into(),
                ..Default::default()
            }),
            result: Some(pb::McpToolResult {
                result: Some(result),
            }),
            description: Some(description),
        }),
    ))
}

pub(crate) fn grep(
    call: &ToolCall,
    started_at_ms: u64,
    output: std::result::Result<String, String>,
) -> Result<ToolCompletion> {
    use pb::{
        grep_result::Result as GrepResultEnum,
        tool_call::Tool,
        GrepError, GrepResult, GrepToolCall,
    };

    let string = |name: &str| {
        call.arguments
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let optional = |name: &str| {
        call.arguments
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    };

    let pattern = string("pattern");
    let path = call
        .arguments
        .get("path")
        .or_else(|| call.arguments.get("target_directory"))
        .and_then(Value::as_str)
        .unwrap_or(".")
        .to_string();
    let output_mode = call.arguments.get("output_mode").and_then(Value::as_str);

    let (content, is_error, result) = match output {
        Ok(text) => {
            let success = parse_grep_output_to_success(pattern.clone(), path.clone(), output_mode, &text);
            (text, false, GrepResultEnum::Success(success))
        }
        Err(error) => (
            error.clone(),
            true,
            GrepResultEnum::Error(GrepError {
                error,
            }),
        ),
    };

    Ok(ToolCompletion::new(
        call,
        started_at_ms,
        ToolResult {
            call_id: call.call_id.clone(),
            content,
            is_error,
            image: None,
        },
        Tool::GrepToolCall(GrepToolCall {
            args: Some(pb::GrepArgs {
                pattern,
                path: optional("path"),
                glob: optional("glob"),
                output_mode: optional("output_mode"),
                tool_call_id: call.call_id.clone(),
                ..Default::default()
            }),
            result: Some(GrepResult {
                result: Some(result),
            }),
        }),
    ))
}

fn parse_grep_output_to_success(
    pattern: String,
    path: String,
    output_mode: Option<&str>,
    text: &str,
) -> pb::GrepSuccess {
    let mode = output_mode.unwrap_or("content");
    let mut workspace_results = std::collections::HashMap::new();

    if !text.is_empty() && !text.starts_with("No matches found") {
        match mode {
            "files_with_matches" => {
                let files: Vec<String> = text
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(ToString::to_string)
                    .collect();
                let total_files = files.len() as i32;
                workspace_results.insert(
                    "workspace".to_string(),
                    pb::GrepUnionResult {
                        result: Some(pb::grep_union_result::Result::Files(pb::GrepFilesResult {
                            files,
                            total_files,
                            client_truncated: false,
                            ripgrep_truncated: false,
                            head_limit_applied: None,
                            offset_applied: None,
                        })),
                    },
                );
            }
            "count" => {
                let mut counts = Vec::new();
                let mut total_matches = 0;
                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Some(idx) = trimmed.rfind(':') {
                        let file = &trimmed[..idx];
                        let count_str = &trimmed[idx + 1..];
                        if let Ok(count) = count_str.parse::<i32>() {
                            total_matches += count;
                            counts.push(pb::GrepFileCount {
                                file: file.to_string(),
                                count,
                            });
                        }
                    }
                }
                let total_files = counts.len() as i32;
                workspace_results.insert(
                    "workspace".to_string(),
                    pb::GrepUnionResult {
                        result: Some(pb::grep_union_result::Result::Count(pb::GrepCountResult {
                            counts,
                            total_files,
                            total_matches,
                            client_truncated: false,
                            ripgrep_truncated: false,
                            head_limit_applied: None,
                            offset_applied: None,
                        })),
                    },
                );
            }
            _ => {
                let mut file_matches: Vec<pb::GrepFileMatch> = Vec::new();
                let mut total_matched_lines = 0;
                let mut total_lines = 0;

                for line in text.lines() {
                    if line == "--" || line.trim().is_empty() {
                        continue;
                    }
                    if let Some((file, line_number, content, is_context_line)) = parse_grep_content_line(line) {
                        if !is_context_line {
                            total_matched_lines += 1;
                        }
                        total_lines += 1;

                        if let Some(last) = file_matches.last_mut() {
                            if last.file == file {
                                last.matches.push(pb::GrepContentMatch {
                                    line_number,
                                    content,
                                    content_truncated: false,
                                    is_context_line,
                                });
                                continue;
                            }
                        }

                        file_matches.push(pb::GrepFileMatch {
                            file,
                            matches: vec![pb::GrepContentMatch {
                                line_number,
                                content,
                                content_truncated: false,
                                is_context_line,
                            }],
                        });
                    }
                }

                workspace_results.insert(
                    "workspace".to_string(),
                    pb::GrepUnionResult {
                        result: Some(pb::grep_union_result::Result::Content(
                            pb::GrepContentResult {
                                matches: file_matches,
                                total_lines,
                                total_matched_lines,
                                client_truncated: false,
                                ripgrep_truncated: false,
                                head_limit_applied: None,
                                offset_applied: None,
                            },
                        )),
                    },
                );
            }
        }
    }

    pb::GrepSuccess {
        pattern,
        path,
        output_mode: mode.to_string(),
        workspace_results,
        active_editor_result: None,
    }
}

fn parse_grep_content_line(line: &str) -> Option<(String, i32, String, bool)> {
    let start_idx = if line.len() >= 2
        && line.as_bytes()[1] == b':'
        && line.as_bytes()[0].is_ascii_alphabetic()
    {
        2
    } else {
        0
    };

    let slice = &line[start_idx..];
    for (i, b) in slice.bytes().enumerate() {
        if b == b':' || b == b'-' {
            let sep1_idx = start_idx + i;
            let sep_char = b as char;
            let after_sep1 = &line[sep1_idx + 1..];

            if let Some(pos2) = after_sep1.find(sep_char) {
                let num_str = &after_sep1[..pos2];
                if !num_str.is_empty() && num_str.chars().all(|c| c.is_ascii_digit()) {
                    if let Ok(line_num) = num_str.parse::<i32>() {
                        let file = line[..sep1_idx].to_string();
                        let content = after_sep1[pos2 + 1..].to_string();
                        let is_context_line = sep_char == '-';
                        return Some((file, line_num, content, is_context_line));
                    }
                }
            }
        }
    }
    None
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_grep_content_line_windows_and_posix() {
        let (file, line_num, content, is_ctx) =
            parse_grep_content_line(r"c:\path\to\file.rs:17:pub fn resolve() {").unwrap();
        assert_eq!(file, r"c:\path\to\file.rs");
        assert_eq!(line_num, 17);
        assert_eq!(content, "pub fn resolve() {");
        assert!(!is_ctx);

        let (file, line_num, content, is_ctx) =
            parse_grep_content_line(r"c:\path-with-dashes\file.rs-16-use foo;").unwrap();
        assert_eq!(file, r"c:\path-with-dashes\file.rs");
        assert_eq!(line_num, 16);
        assert_eq!(content, "use foo;");
        assert!(is_ctx);

        let (file, line_num, content, is_ctx) =
            parse_grep_content_line("src/main.rs:42:println!(\"hello\");").unwrap();
        assert_eq!(file, "src/main.rs");
        assert_eq!(line_num, 42);
        assert_eq!(content, "println!(\"hello\");");
        assert!(!is_ctx);
    }

    #[test]
    fn parses_grep_output_to_success_populates_workspace_results() {
        let text = "src/lib.rs:10:fn foo() {}\nsrc/lib.rs:11:fn bar() {}\nsrc/main.rs:5:fn main() {}";
        let success = parse_grep_output_to_success("fn".into(), ".".into(), Some("content"), text);
        assert_eq!(success.pattern, "fn");
        assert_eq!(success.workspace_results.len(), 1);
        let union_res = success.workspace_results.get("workspace").unwrap();
        if let Some(pb::grep_union_result::Result::Content(c)) = &union_res.result {
            assert_eq!(c.matches.len(), 2);
            assert_eq!(c.total_matched_lines, 3);
        } else {
            panic!("expected content result");
        }
    }
}
