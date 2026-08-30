use std::collections::BTreeMap;

use crate::{cursor::proto::agent::v1 as pb, model::limit_tool_result_text};

const KIB: usize = 1024;
const READ_CONTENT_LIMIT: usize = 64 * KIB;
const READ_BINARY_LIMIT: usize = 32 * KIB;
const SHELL_STREAM_LIMIT: usize = 16 * KIB;
const SHELL_INTERLEAVED_LIMIT: usize = 32 * KIB;
const GREP_CONTENT_LIMIT: usize = 32 * KIB;
const GREP_MATCH_LIMIT: usize = 2 * KIB;
const GREP_MATCHES_PER_FILE: usize = 100;
const GREP_TOTAL_MATCHES: usize = 300;
const GREP_LIST_LIMIT: usize = 300;
const GLOB_FILE_LIMIT: usize = 200;
const EDIT_RESULT_LIMIT: usize = 32 * KIB;
const PATCH_EDIT_RESULT_LIMIT: usize = 4 * KIB;
const MCP_TEXT_LIMIT: usize = 32 * KIB;
const MCP_CONTENT_ITEM_LIMIT: usize = 20;
const MCP_STRUCTURED_LIMIT: usize = 32 * KIB;
const MCP_BINARY_LIMIT: usize = 32 * KIB;
const MCP_RESOURCE_LIMIT: usize = 200;
const MCP_RESOURCE_DESCRIPTION_LIMIT: usize = KIB;
const WEB_FETCH_LIMIT: usize = 32 * KIB;
const WEB_SEARCH_LIMIT: usize = 16 * KIB;
const WEB_SEARCH_TITLE_LIMIT: usize = 512;
const WEB_SEARCH_SNIPPET_LIMIT: usize = 2 * KIB;

pub(super) fn tool_completion(
    tool_name: &str,
    tool: &mut pb::tool_call::Tool,
    content: &mut String,
) {
    use pb::tool_call::Tool;

    match tool {
        Tool::ShellToolCall(tool) => gate_shell(tool),
        Tool::GrepToolCall(tool) => gate_grep(tool),
        Tool::GlobToolCall(tool) => gate_glob(tool),
        Tool::LsToolCall(tool) => gate_ls(tool),
        Tool::ReadToolCall(tool) => gate_read(tool),
        Tool::EditToolCall(tool) => gate_edit(tool_name, tool),
        Tool::McpToolCall(tool) => gate_mcp(tool),
        Tool::ListMcpResourcesToolCall(tool) => gate_mcp_resources(tool),
        Tool::ReadMcpResourceToolCall(tool) => gate_mcp_resource(tool),
        Tool::GetMcpToolsToolCall(tool) => gate_mcp_tools(tool),
        Tool::WebFetchToolCall(tool) => gate_web_fetch(tool),
        Tool::WebSearchToolCall(tool) => gate_web_search(tool),
        Tool::GenerateImageToolCall(tool) => gate_generate_image(tool),
        _ => {}
    }
    *content = limit_tool_result_text(tool_name, content);
}

pub(super) fn exec_message(message: &mut pb::exec_client_message::Message) {
    use pb::exec_client_message::Message;
    match message {
        Message::ShellResult(result) | Message::MiniSweAgentBashResult(result) => {
            gate_shell_result(result)
        }
        Message::LsResult(result) => gate_ls_result(result),
        _ => {}
    }
}

pub(super) fn gate_ls_result(result: &mut pb::LsResult) {
    let Some(pb::ls_result::Result::Success(success)) = result.result.as_mut() else {
        return;
    };
    let Some(root) = success.directory_tree_root.as_mut() else {
        return;
    };
    let total_dirs = root.children_dirs.len();
    let total_files = root.children_files.len();
    let max_each = GLOB_FILE_LIMIT / 2;
    let mut truncated = false;
    if total_dirs > max_each {
        root.children_dirs.truncate(max_each);
        truncated = true;
    }
    if total_files > max_each {
        root.children_files.truncate(max_each);
        truncated = true;
    }
    if truncated {
        root.children_were_processed = false;
    }
}

fn gate_shell(tool: &mut pb::ShellToolCall) {
    if let Some(result) = tool.result.as_mut() {
        gate_shell_result(result);
    }
}

fn gate_shell_result(result: &mut pb::ShellResult) {
    use pb::shell_result::Result;
    match result.result.as_mut() {
        Some(Result::Success(success)) => {
            success.stdout = truncate_edges("Shell stdout", &success.stdout, SHELL_STREAM_LIMIT);
            success.stderr = truncate_edges("Shell stderr", &success.stderr, SHELL_STREAM_LIMIT);
            if let Some(interleaved) = success.interleaved_output.as_mut() {
                *interleaved = truncate_edges(
                    "Shell interleaved output",
                    interleaved,
                    SHELL_INTERLEAVED_LIMIT,
                );
            }
        }
        Some(Result::Failure(failure)) => {
            failure.stdout = truncate_edges("Shell stdout", &failure.stdout, SHELL_STREAM_LIMIT);
            failure.stderr = truncate_edges("Shell stderr", &failure.stderr, SHELL_STREAM_LIMIT);
            if let Some(interleaved) = failure.interleaved_output.as_mut() {
                *interleaved = truncate_edges(
                    "Shell interleaved output",
                    interleaved,
                    SHELL_INTERLEAVED_LIMIT,
                );
            }
        }
        _ => {}
    }
}

fn gate_read(tool: &mut pb::ReadToolCall) {
    let Some(pb::read_tool_result::Result::Success(success)) = tool
        .result
        .as_mut()
        .and_then(|result| result.result.as_mut())
    else {
        return;
    };
    let Some(output) = success.output.as_mut() else {
        return;
    };
    match output {
        pb::read_tool_success::Output::Content(value) => {
            let next = truncate_text("Read", value, READ_CONTENT_LIMIT);
            if next != *value {
                *value = next;
                success.exceeded_limit = true;
            }
        }
        pb::read_tool_success::Output::Data(value) if value.len() > READ_BINARY_LIMIT => {
            let notice = truncation_notice("Read binary data", READ_BINARY_LIMIT, 0, value.len());
            success.output = Some(pb::read_tool_success::Output::Content(notice));
            success.exceeded_limit = true;
        }
        _ => {}
    }
}

fn gate_glob(tool: &mut pb::GlobToolCall) {
    let Some(pb::glob_tool_result::Result::Success(success)) = tool
        .result
        .as_mut()
        .and_then(|result| result.result.as_mut())
    else {
        return;
    };
    let original = success.files.len();
    if original <= GLOB_FILE_LIMIT {
        if success.total_files <= 0 {
            success.total_files = original as i32;
        }
        return;
    }
    success.files.truncate(GLOB_FILE_LIMIT);
    success.total_files = success.total_files.max(original as i32);
    success.client_truncated = true;
}

fn gate_ls(tool: &mut pb::LsToolCall) {
    if let Some(result) = tool.result.as_mut() {
        gate_ls_result(result);
    }
}

fn gate_grep(tool: &mut pb::GrepToolCall) {
    let Some(pb::grep_result::Result::Success(success)) = tool
        .result
        .as_mut()
        .and_then(|result| result.result.as_mut())
    else {
        return;
    };
    let mut budget = GrepBudget {
        content_bytes: GREP_CONTENT_LIMIT,
        matches: GREP_TOTAL_MATCHES,
    };
    let mut workspace_names = success
        .workspace_results
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    workspace_names.sort_unstable();
    for name in workspace_names {
        if let Some(result) = success.workspace_results.get_mut(&name) {
            gate_grep_union(result, &mut budget);
        }
    }
    if let Some(result) = success.active_editor_result.as_mut() {
        gate_grep_union(result, &mut budget);
    }
}

struct GrepBudget {
    content_bytes: usize,
    matches: usize,
}

fn gate_grep_union(result: &mut pb::GrepUnionResult, budget: &mut GrepBudget) {
    use pb::grep_union_result::Result;
    match result.result.as_mut() {
        Some(Result::Content(content)) => gate_grep_content(content, budget),
        Some(Result::Files(files)) => {
            let original = files.files.len();
            if original > GREP_LIST_LIMIT {
                files.files.truncate(GREP_LIST_LIMIT);
                files.client_truncated = true;
            }
            if files.total_files <= 0 {
                files.total_files = original as i32;
            }
        }
        Some(Result::Count(counts)) => {
            let original = counts.counts.len();
            if original > GREP_LIST_LIMIT {
                counts.counts.truncate(GREP_LIST_LIMIT);
                counts.client_truncated = true;
            }
            if counts.total_files <= 0 {
                counts.total_files = original as i32;
            }
        }
        None => {}
    }
}

fn gate_grep_content(content: &mut pb::GrepContentResult, budget: &mut GrepBudget) {
    if content
        .matches
        .iter()
        .flat_map(|file| &file.matches)
        .any(is_grep_notice)
    {
        return;
    }
    let original_bytes = grep_content_bytes(&content.matches);
    let original_files = content.matches.len();
    let mut truncated = false;
    let mut files = Vec::with_capacity(original_files);

    for file in &content.matches {
        if budget.matches == 0 || budget.content_bytes == 0 {
            truncated = true;
            break;
        }
        let mut next = pb::GrepFileMatch {
            file: file.file.clone(),
            matches: Vec::new(),
        };
        for matched in &file.matches {
            if is_grep_notice(matched) {
                next.matches.push(matched.clone());
                continue;
            }
            if next.matches.len() >= GREP_MATCHES_PER_FILE
                || budget.matches == 0
                || budget.content_bytes == 0
            {
                truncated = true;
                break;
            }
            let mut next_match = matched.clone();
            let original = next_match.content.clone();
            next_match.content = truncate_text("Grep match", &original, GREP_MATCH_LIMIT);
            if next_match.content != original {
                next_match.content_truncated = true;
                truncated = true;
            }
            if next_match.content.len() > budget.content_bytes {
                next_match.content =
                    truncate_text("Grep", &next_match.content, budget.content_bytes);
                next_match.content_truncated = true;
                truncated = true;
            }
            if next_match.content.trim().is_empty() {
                truncated = true;
                break;
            }
            budget.content_bytes -= next_match.content.len();
            budget.matches -= 1;
            next.matches.push(next_match);
        }
        if next.matches.len() < file.matches.len() {
            truncated = true;
        }
        if !next.matches.is_empty() {
            files.push(next);
        }
    }
    if files.len() < original_files {
        truncated = true;
    }
    if truncated {
        content.client_truncated = true;
        add_grep_notice(&mut files, original_bytes);
    }
    content.matches = files;
}

fn add_grep_notice(files: &mut Vec<pb::GrepFileMatch>, original_bytes: usize) {
    if files
        .iter()
        .flat_map(|file| &file.matches)
        .any(is_grep_notice)
    {
        return;
    }
    loop {
        let used = grep_content_bytes(files);
        let notice = truncation_notice("Grep", GREP_CONTENT_LIMIT, used, original_bytes);
        if used.saturating_add(notice.len()) <= GREP_CONTENT_LIMIT {
            let matched = pb::GrepContentMatch {
                line_number: 0,
                content: notice,
                content_truncated: true,
                is_context_line: true,
            };
            if let Some(file) = files.last_mut() {
                file.matches.push(matched);
            } else {
                files.push(pb::GrepFileMatch {
                    file: "[truncated]".into(),
                    matches: vec![matched],
                });
            }
            return;
        }
        let Some(file) = files.last_mut() else {
            return;
        };
        file.matches.pop();
        if file.matches.is_empty() {
            files.pop();
        }
    }
}

fn is_grep_notice(matched: &pb::GrepContentMatch) -> bool {
    matched.line_number == 0
        && matched.content_truncated
        && matched
            .content
            .starts_with("[truncated: Grep result exceeded")
}

fn grep_content_bytes(files: &[pb::GrepFileMatch]) -> usize {
    files
        .iter()
        .flat_map(|file| &file.matches)
        .map(|matched| matched.content.len())
        .sum()
}

fn gate_edit(tool_name: &str, tool: &mut pb::EditToolCall) {
    let Some(pb::edit_result::Result::Success(success)) = tool
        .result
        .as_mut()
        .and_then(|result| result.result.as_mut())
    else {
        return;
    };
    let limit = match tool_name.trim() {
        "PatchEdit" | "PatchEditLines" | "PatchEditSpan" | "StrReplace" => PATCH_EDIT_RESULT_LIMIT,
        _ => EDIT_RESULT_LIMIT,
    };
    if let Some(diff) = success.diff_string.as_mut() {
        *diff = truncate_text(tool_name, diff, limit);
        success.before_full_file_content = None;
        success.after_full_file_content.clear();
    } else {
        success.before_full_file_content = None;
        success.after_full_file_content =
            truncate_text(tool_name, &success.after_full_file_content, limit);
    }
}

fn gate_mcp(tool: &mut pb::McpToolCall) {
    let Some(pb::mcp_tool_result::Result::Success(success)) = tool
        .result
        .as_mut()
        .and_then(|result| result.result.as_mut())
    else {
        return;
    };
    if success.content.iter().any(is_mcp_notice) {
        return;
    }
    let mut notices = Vec::new();
    if structured_json_len(&success.structured_content) > MCP_STRUCTURED_LIMIT {
        let original = structured_json_len(&success.structured_content);
        success.structured_content = truncated_struct(original, MCP_STRUCTURED_LIMIT);
        notices.push(truncation_notice(
            "MCP structured_content",
            MCP_STRUCTURED_LIMIT,
            0,
            original,
        ));
    }
    let original_items = success.content.len();
    if original_items > MCP_CONTENT_ITEM_LIMIT {
        success.content.truncate(MCP_CONTENT_ITEM_LIMIT);
        notices.push(format!(
            "[truncated: MCP content items exceeded {MCP_CONTENT_ITEM_LIMIT} items; showing {MCP_CONTENT_ITEM_LIMIT} of {original_items} items]"
        ));
    }
    let mut remaining_text = MCP_TEXT_LIMIT;
    let mut content = Vec::with_capacity(success.content.len() + notices.len());
    for mut item in std::mem::take(&mut success.content) {
        match item.content.as_mut() {
            Some(pb::mcp_tool_result_content_item::Content::Text(text)) => {
                let original = text.text.clone();
                let next = truncate_text("MCP content item", &original, MCP_TEXT_LIMIT);
                if remaining_text == 0 {
                    notices.push(truncation_notice(
                        "MCP text",
                        MCP_TEXT_LIMIT,
                        MCP_TEXT_LIMIT,
                        MCP_TEXT_LIMIT.saturating_add(original.len()),
                    ));
                    continue;
                }
                text.text = truncate_text("MCP text", &next, remaining_text);
                remaining_text = remaining_text.saturating_sub(text.text.len());
            }
            // MCP images are sent to the client as inline binary data. Truncating
            // an encoded image at an arbitrary byte boundary corrupts the image
            // and makes the client's image/screenshot fallback fail. The model
            // receives only the textual MCP summary below, which is bounded by
            // MCP_TEXT_LIMIT, so the image does not need this text-result gate.
            _ => {}
        }
        content.push(item);
    }
    content.extend(notices.into_iter().map(mcp_notice));
    success.content = content;
}

fn mcp_notice(text: String) -> pb::McpToolResultContentItem {
    pb::McpToolResultContentItem {
        content: Some(pb::mcp_tool_result_content_item::Content::Text(
            pb::McpTextContent {
                text,
                output_location: None,
            },
        )),
    }
}

fn is_mcp_notice(item: &pb::McpToolResultContentItem) -> bool {
    matches!(
        item.content.as_ref(),
        Some(pb::mcp_tool_result_content_item::Content::Text(text))
            if text.text.starts_with("[truncated:")
    )
}

fn structured_json_len(value: &Option<prost_types::Struct>) -> usize {
    value
        .as_ref()
        .and_then(|value| {
            serde_json::to_vec(&serde_json::Value::Object(
                value
                    .fields
                    .iter()
                    .map(|(key, value)| (key.clone(), super::prost_json(value)))
                    .collect(),
            ))
            .ok()
        })
        .map_or(0, |value| value.len())
}

fn truncated_struct(original: usize, limit: usize) -> Option<prost_types::Struct> {
    Some(prost_types::Struct {
        fields: BTreeMap::from([
            ("_truncated".into(), prost_bool(true)),
            ("original_json_bytes".into(), prost_number(original as f64)),
            ("limit_bytes".into(), prost_number(limit as f64)),
        ]),
    })
}

fn prost_bool(value: bool) -> prost_types::Value {
    prost_types::Value {
        kind: Some(prost_types::value::Kind::BoolValue(value)),
    }
}

fn prost_number(value: f64) -> prost_types::Value {
    prost_types::Value {
        kind: Some(prost_types::value::Kind::NumberValue(value)),
    }
}

fn gate_mcp_resources(tool: &mut pb::ListMcpResourcesToolCall) {
    let Some(pb::list_mcp_resources_exec_result::Result::Success(success)) = tool
        .result
        .as_mut()
        .and_then(|result| result.result.as_mut())
    else {
        return;
    };
    if success
        .resources
        .iter()
        .any(|resource| resource.uri == "truncated:list-mcp-resources")
    {
        return;
    }
    let original = success.resources.len();
    success.resources.truncate(MCP_RESOURCE_LIMIT);
    for resource in &mut success.resources {
        if let Some(description) = resource.description.as_mut() {
            *description = truncate_text(
                "MCP resource description",
                description,
                MCP_RESOURCE_DESCRIPTION_LIMIT,
            );
        }
    }
    if success.resources.len() < original {
        success
            .resources
            .push(pb::list_mcp_resources_exec_result::McpResource {
                uri: "truncated:list-mcp-resources".into(),
                name: Some("truncated".into()),
                description: Some(truncation_notice(
                    "ListMcpResources",
                    MCP_TEXT_LIMIT,
                    success.resources.len(),
                    original,
                )),
                ..Default::default()
            });
    }
}

fn gate_mcp_resource(tool: &mut pb::ReadMcpResourceToolCall) {
    let Some(pb::read_mcp_resource_exec_result::Result::Success(success)) = tool
        .result
        .as_mut()
        .and_then(|result| result.result.as_mut())
    else {
        return;
    };
    match success.content.as_mut() {
        Some(pb::read_mcp_resource_success::Content::Text(text)) => {
            *text = truncate_text("FetchMcpResource", text, MCP_TEXT_LIMIT);
        }
        Some(pb::read_mcp_resource_success::Content::Blob(blob))
            if blob.len() > MCP_BINARY_LIMIT =>
        {
            let notice =
                truncation_notice("FetchMcpResource blob", MCP_BINARY_LIMIT, 0, blob.len());
            success.content = Some(pb::read_mcp_resource_success::Content::Text(notice));
        }
        _ => {}
    }
}

fn gate_mcp_tools(tool: &mut pb::GetMcpToolsToolCall) {
    let Some(pb::get_mcp_tools_agent_result::Result::Success(success)) = tool
        .result
        .as_mut()
        .and_then(|result| result.result.as_mut())
    else {
        return;
    };
    success.content = truncate_text("GetMcpTools", &success.content, MCP_TEXT_LIMIT);
}

fn gate_web_fetch(tool: &mut pb::WebFetchToolCall) {
    let Some(pb::web_fetch_result::Result::Success(success)) = tool
        .result
        .as_mut()
        .and_then(|result| result.result.as_mut())
    else {
        return;
    };
    success.markdown = truncate_text("WebFetch", &success.markdown, WEB_FETCH_LIMIT);
}

fn gate_web_search(tool: &mut pb::WebSearchToolCall) {
    let Some(pb::web_search_result::Result::Success(success)) = tool
        .result
        .as_mut()
        .and_then(|result| result.result.as_mut())
    else {
        return;
    };
    for reference in &mut success.references {
        reference.title =
            truncate_text("WebSearch title", &reference.title, WEB_SEARCH_TITLE_LIMIT);
        reference.chunk = truncate_text(
            "WebSearch snippet",
            &reference.chunk,
            WEB_SEARCH_SNIPPET_LIMIT,
        );
    }
    let original = web_search_bytes(&success.references);
    while success.references.len() > 1 && web_search_bytes(&success.references) > WEB_SEARCH_LIMIT {
        success.references.pop();
    }
    if original > WEB_SEARCH_LIMIT {
        let total = web_search_bytes(&success.references);
        if let Some(reference) = success.references.last_mut() {
            let other = total.saturating_sub(reference.chunk.len());
            let notice = truncation_notice(
                "WebSearch",
                WEB_SEARCH_LIMIT,
                WEB_SEARCH_LIMIT.saturating_sub(other),
                original,
            );
            let available = WEB_SEARCH_LIMIT.saturating_sub(other + notice.len() + 2);
            reference.chunk = format!(
                "{}\n\n{notice}",
                utf8_prefix(&reference.chunk, available).trim_end_matches('\n')
            );
        }
    }
}

fn web_search_bytes(references: &[pb::WebSearchReference]) -> usize {
    references
        .iter()
        .map(|reference| reference.title.len() + reference.url.len() + reference.chunk.len())
        .sum()
}

fn gate_generate_image(tool: &mut pb::GenerateImageToolCall) {
    let Some(pb::generate_image_result::Result::Success(success)) = tool
        .result
        .as_mut()
        .and_then(|result| result.result.as_mut())
    else {
        return;
    };
    if !success.image_data.trim().is_empty()
        && !success
            .image_data
            .starts_with("[base64 image data omitted from replay; bytes=")
    {
        let original = success.image_data.trim().len();
        success.image_data = format!("[base64 image data omitted from replay; bytes={original}]");
    }
}

fn truncate_text(tool_name: &str, content: &str, limit: usize) -> String {
    if content.len() <= limit {
        return content.to_string();
    }
    let original = content.len();
    let mut shown = limit;
    loop {
        let notice = format!(
            "\n\n[truncated: {tool_name} result exceeded {limit} bytes; showing {shown} of {original} bytes]"
        );
        let available = limit.saturating_sub(notice.len());
        let kept = utf8_prefix(content, available);
        if kept.len() == shown {
            return format!("{}{notice}", kept.trim_end_matches('\n'));
        }
        shown = kept.len();
    }
}

fn truncate_edges(tool_name: &str, content: &str, limit: usize) -> String {
    if content.len() <= limit {
        return content.to_string();
    }
    let original = content.len();
    let mut shown = limit;
    loop {
        let notice = format!(
            "\n\n[truncated: {tool_name} result exceeded {limit} bytes; omitted middle; showing {shown} of {original} bytes]\n\n"
        );
        let available = limit.saturating_sub(notice.len());
        let head = utf8_prefix(content, available / 2);
        let tail = utf8_suffix(content, available.saturating_sub(head.len()));
        let next_shown = head.len().saturating_add(tail.len());
        if next_shown == shown {
            return format!("{head}{notice}{tail}");
        }
        shown = next_shown;
    }
}

fn truncation_notice(tool_name: &str, limit: usize, shown: usize, original: usize) -> String {
    format!(
        "[truncated: {tool_name} result exceeded {limit} bytes; showing {shown} of {original} bytes]"
    )
}

fn utf8_prefix(value: &str, limit: usize) -> &str {
    let mut end = limit.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn utf8_suffix(value: &str, limit: usize) -> &str {
    let mut start = value.len().saturating_sub(limit);
    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_output_keeps_both_ends_within_its_budget() {
        let mut content = format!("HEAD{}TAIL", " ".repeat(1024 * KIB));
        let mut tool = pb::tool_call::Tool::ShellToolCall(pb::ShellToolCall::default());

        tool_completion("Shell", &mut tool, &mut content);

        assert!(content.len() <= 128 * KIB);
        assert!(content.starts_with("HEAD"));
        assert!(content.contains("[truncated: Shell result exceeded"));
    }

    #[test]
    fn grep_limits_matches_per_file_total_bytes_and_adds_notice() {
        let matches = (0..150)
            .map(|line_number| pb::GrepContentMatch {
                line_number,
                content: "x".repeat(3 * KIB),
                ..Default::default()
            })
            .collect();
        let mut tool = pb::tool_call::Tool::GrepToolCall(pb::GrepToolCall {
            result: Some(pb::GrepResult {
                result: Some(pb::grep_result::Result::Success(pb::GrepSuccess {
                    workspace_results: std::collections::HashMap::from([(
                        "workspace".into(),
                        pb::GrepUnionResult {
                            result: Some(pb::grep_union_result::Result::Content(
                                pb::GrepContentResult {
                                    matches: vec![pb::GrepFileMatch {
                                        file: "large.txt".into(),
                                        matches,
                                    }],
                                    ..Default::default()
                                },
                            )),
                        },
                    )]),
                    ..Default::default()
                })),
            }),
            ..Default::default()
        });
        let mut model_content = "x".repeat(128 * KIB);

        tool_completion("Grep", &mut tool, &mut model_content);

        assert!(model_content.len() <= GREP_CONTENT_LIMIT);
        assert!(model_content.contains("[truncated: Grep result exceeded"));
        let pb::tool_call::Tool::GrepToolCall(tool) = tool else {
            unreachable!()
        };
        let Some(pb::grep_result::Result::Success(success)) =
            tool.result.clone().and_then(|result| result.result)
        else {
            panic!("expected grep success")
        };
        let result = success.workspace_results.get("workspace").unwrap();
        let Some(pb::grep_union_result::Result::Content(content)) = result.result.as_ref() else {
            panic!("expected grep content")
        };
        assert!(content.client_truncated);
        assert!(grep_content_bytes(&content.matches) <= GREP_CONTENT_LIMIT);
        assert!(content.matches[0].matches.len() <= GREP_MATCHES_PER_FILE + 1);
        assert!(content.matches[0]
            .matches
            .last()
            .unwrap()
            .content
            .contains("[truncated: Grep result exceeded"));

        let once = tool.clone();
        let mut tool_enum = pb::tool_call::Tool::GrepToolCall(tool);
        let mut second_content = model_content.clone();
        tool_completion("Grep", &mut tool_enum, &mut second_content);
        let pb::tool_call::Tool::GrepToolCall(second) = tool_enum else {
            panic!("expected grep tool")
        };
        assert_eq!(second, once);
        assert_eq!(second_content, model_content);
    }

    #[test]
    fn read_content_is_limited_and_marked() {
        let mut tool = pb::tool_call::Tool::ReadToolCall(pb::ReadToolCall {
            result: Some(pb::ReadToolResult {
                result: Some(pb::read_tool_result::Result::Success(pb::ReadToolSuccess {
                    output: Some(pb::read_tool_success::Output::Content(
                        "前".repeat(READ_CONTENT_LIMIT),
                    )),
                    ..Default::default()
                })),
            }),
            ..Default::default()
        });
        let mut content = "前".repeat(READ_CONTENT_LIMIT);

        tool_completion("Read", &mut tool, &mut content);

        assert!(content.len() <= READ_CONTENT_LIMIT);
        let pb::tool_call::Tool::ReadToolCall(tool) = tool else {
            unreachable!()
        };
        let Some(pb::read_tool_result::Result::Success(success)) =
            tool.result.and_then(|result| result.result)
        else {
            panic!("expected read success")
        };
        assert!(success.exceeded_limit);
        let Some(pb::read_tool_success::Output::Content(output)) = success.output else {
            panic!("expected text output")
        };
        assert!(output.len() <= READ_CONTENT_LIMIT);
        assert!(output.contains("[truncated: Read result exceeded"));
    }

    #[test]
    fn mcp_limits_items_text_and_structured_content() {
        let mut tool = pb::tool_call::Tool::McpToolCall(pb::McpToolCall {
            result: Some(pb::McpToolResult {
                result: Some(pb::mcp_tool_result::Result::Success(pb::McpSuccess {
                    content: (0..25)
                        .map(|_| pb::McpToolResultContentItem {
                            content: Some(pb::mcp_tool_result_content_item::Content::Text(
                                pb::McpTextContent {
                                    text: "x".repeat(4 * KIB),
                                    ..Default::default()
                                },
                            )),
                        })
                        .collect(),
                    structured_content: Some(prost_types::Struct {
                        fields: BTreeMap::from([(
                            "large".into(),
                            prost_types::Value {
                                kind: Some(prost_types::value::Kind::StringValue(
                                    "x".repeat(64 * KIB),
                                )),
                            },
                        )]),
                    }),
                    ..Default::default()
                })),
            }),
            ..Default::default()
        });
        let mut content = "x".repeat(64 * KIB);

        tool_completion("CallMcpTool", &mut tool, &mut content);

        assert!(content.len() <= MCP_TEXT_LIMIT);
        let pb::tool_call::Tool::McpToolCall(tool) = tool else {
            unreachable!()
        };
        let Some(pb::mcp_tool_result::Result::Success(success)) =
            tool.result.and_then(|result| result.result)
        else {
            panic!("expected mcp success")
        };
        assert!(success.content.len() > MCP_CONTENT_ITEM_LIMIT);
        assert_eq!(
            success
                .structured_content
                .unwrap()
                .fields
                .get("_truncated")
                .unwrap()
                .kind,
            Some(prost_types::value::Kind::BoolValue(true))
        );
        assert!(success.content.iter().any(|item| matches!(
            item.content.as_ref(),
            Some(pb::mcp_tool_result_content_item::Content::Text(text))
                if text.text.contains("MCP content items exceeded")
        )));
    }

    #[test]
    fn mcp_images_are_not_truncated_at_an_invalid_binary_boundary() {
        let image_data = (0..(MCP_BINARY_LIMIT + 1))
            .map(|value| (value % 251) as u8)
            .collect::<Vec<_>>();
        let original_image_data = image_data.clone();
        let mut tool = pb::tool_call::Tool::McpToolCall(pb::McpToolCall {
            result: Some(pb::McpToolResult {
                result: Some(pb::mcp_tool_result::Result::Success(pb::McpSuccess {
                    content: vec![pb::McpToolResultContentItem {
                        content: Some(pb::mcp_tool_result_content_item::Content::Image(
                            pb::McpImageContent {
                                data: image_data,
                                mime_type: "image/png".into(),
                            },
                        )),
                    }],
                    ..Default::default()
                })),
            }),
            ..Default::default()
        });
        let mut content = "MCP image".into();

        tool_completion("CallMcpTool", &mut tool, &mut content);

        let pb::tool_call::Tool::McpToolCall(tool) = tool else {
            unreachable!()
        };
        let Some(pb::mcp_tool_result::Result::Success(success)) =
            tool.result.and_then(|result| result.result)
        else {
            panic!("expected mcp success")
        };
        let Some(pb::mcp_tool_result_content_item::Content::Image(image)) =
            success.content[0].content.as_ref()
        else {
            panic!("expected mcp image")
        };
        assert_eq!(image.data, original_image_data);
        assert!(!success.content.iter().any(is_mcp_notice));
    }

    #[test]
    fn edit_keeps_only_a_bounded_diff() {
        let mut tool = pb::tool_call::Tool::EditToolCall(pb::EditToolCall {
            result: Some(pb::EditResult {
                result: Some(pb::edit_result::Result::Success(pb::EditSuccess {
                    diff_string: Some("d".repeat(16 * KIB)),
                    before_full_file_content: Some("b".repeat(64 * KIB)),
                    after_full_file_content: "a".repeat(64 * KIB),
                    ..Default::default()
                })),
            }),
            ..Default::default()
        });
        let mut content = "x".repeat(64 * KIB);

        tool_completion("StrReplace", &mut tool, &mut content);

        assert!(content.len() <= PATCH_EDIT_RESULT_LIMIT);
        let pb::tool_call::Tool::EditToolCall(tool) = tool else {
            unreachable!()
        };
        let Some(pb::edit_result::Result::Success(success)) =
            tool.result.and_then(|result| result.result)
        else {
            panic!("expected edit success")
        };
        assert!(success.diff_string.unwrap().len() <= PATCH_EDIT_RESULT_LIMIT);
        assert!(success.before_full_file_content.is_none());
        assert!(success.after_full_file_content.is_empty());
    }

    #[test]
    fn shell_streams_are_limited_before_rendering() {
        let mut message = pb::exec_client_message::Message::ShellResult(pb::ShellResult {
            result: Some(pb::shell_result::Result::Success(pb::ShellSuccess {
                stdout: format!("HEAD{}TAIL", "x".repeat(64 * KIB)),
                stderr: format!("ERROR_HEAD{}ERROR_TAIL", "y".repeat(64 * KIB)),
                interleaved_output: Some(format!("START{}END", "z".repeat(64 * KIB))),
                ..Default::default()
            })),
            ..Default::default()
        });

        exec_message(&mut message);

        let pb::exec_client_message::Message::ShellResult(result) = message else {
            panic!("expected Shell result");
        };
        let Some(pb::shell_result::Result::Success(success)) = result.result else {
            panic!("expected Shell success");
        };
        assert!(success.stdout.len() <= SHELL_STREAM_LIMIT);
        assert!(success.stdout.starts_with("HEAD"));
        assert!(success.stdout.ends_with("TAIL"));
        assert!(success.stderr.len() <= SHELL_STREAM_LIMIT);
        assert!(success.stderr.starts_with("ERROR_HEAD"));
        assert!(success.stderr.ends_with("ERROR_TAIL"));
        assert!(success.interleaved_output.unwrap().len() <= SHELL_INTERLEAVED_LIMIT);
    }

    #[test]
    fn ls_results_are_gated_and_marked_truncated() {
        let mut children_dirs = Vec::new();
        for i in 0..150 {
            children_dirs.push(pb::LsDirectoryTreeNode {
                abs_path: format!("dir_{i}"),
                ..Default::default()
            });
        }
        let mut children_files = Vec::new();
        for i in 0..150 {
            children_files.push(pb::ls_directory_tree_node::File {
                name: format!("file_{i}.txt"),
                terminal_metadata: None,
            });
        }

        let mut tool = pb::tool_call::Tool::LsToolCall(pb::LsToolCall {
            result: Some(pb::LsResult {
                result: Some(pb::ls_result::Result::Success(pb::LsSuccess {
                    directory_tree_root: Some(pb::LsDirectoryTreeNode {
                        abs_path: "/workspace/large".into(),
                        children_dirs,
                        children_files,
                        children_were_processed: true,
                        ..Default::default()
                    }),
                })),
            }),
            ..Default::default()
        });
        let mut content = "ls content".into();

        tool_completion("Ls", &mut tool, &mut content);

        let pb::tool_call::Tool::LsToolCall(tool) = tool else {
            panic!("expected LsToolCall");
        };
        let Some(pb::ls_result::Result::Success(success)) = tool.result.and_then(|r| r.result) else {
            panic!("expected LsSuccess");
        };
        let root = success.directory_tree_root.unwrap();
        assert_eq!(root.children_dirs.len(), 100);
        assert_eq!(root.children_files.len(), 100);
        assert!(!root.children_were_processed);
    }
}
