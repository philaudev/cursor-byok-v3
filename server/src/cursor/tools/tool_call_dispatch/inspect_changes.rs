//! Local execution engine for the `InspectChanges` tool.
//! Inspects uncommitted git status and diffs in a token-safe manner.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    model::ToolCall,
    Result,
};

use super::ToolStart;
use crate::cursor::tools::{
    runtime::{now_ms, ExecContext},
    tool_call_result::{self as result, ToolResultSender},
};

const MAX_OUTPUT_CHARS: usize = 8_000;
const MAX_UNTRACKED_FILE_LINES: usize = 40;
const MAX_FILE_READ_BYTES: u64 = 5_000_000;
const MAX_CHANGED_FILES_SUMMARY: usize = 100;

#[derive(Debug, Deserialize)]
struct InspectChangesArgs {
    path: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ChangedFile {
    path: String,
    status: String,
    staged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    added: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted: Option<usize>,
}

pub(super) fn start(
    results: &ToolResultSender,
    call: &ToolCall,
    _context: &ExecContext,
) -> Result<ToolStart> {
    let args: InspectChangesArgs = serde_json::from_value(call.arguments.clone())?;
    let call = call.clone();
    let results = results.clone();
    let started_at_ms = now_ms();

    tokio::spawn(async move {
        let output = execute(args).await;
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

fn git_command() -> Command {
    let mut cmd = Command::new("git");
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd
}

async fn execute(args: InspectChangesArgs) -> std::result::Result<Value, String> {
    tokio::task::spawn_blocking(move || execute_sync(args))
        .await
        .map_err(|e| e.to_string())?
}

fn execute_sync(args: InspectChangesArgs) -> std::result::Result<Value, String> {
    let target_path_str = args.path.trim();
    if target_path_str.is_empty() {
        return Err("The `path` parameter must not be empty.".into());
    }

    let target_path = Path::new(target_path_str);

    // Determine target directory vs file
    let (target_dir, specific_file) = if target_path.is_dir() {
        (target_path_str.to_string(), None)
    } else if target_path.is_file() || target_path.parent().is_some() {
        let parent = target_path
            .parent()
            .and_then(|p| p.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(".");
        (parent.to_string(), Some(target_path_str.to_string()))
    } else {
        (target_path_str.to_string(), None)
    };

    // Strict git check on the given path - NO FALLBACK
    let git_root_output = git_command()
        .args(["-C", &target_dir, "rev-parse", "--show-toplevel"])
        .output();

    let git_root = match git_root_output {
        Ok(out) if out.status.success() => {
            let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !root.is_empty() {
                Some(root)
            } else {
                None
            }
        }
        _ => None,
    };

    let Some(repo_root) = git_root else {
        return Ok(json!({
            "is_git_repo": false,
            "path": target_path_str,
            "message": format!("The path `{target_path_str}` is not a git repository or not inside a git repository.")
        }));
    };

    // If specific file requested, inspect that single file
    if let Some(file) = specific_file {
        return inspect_single_file(&repo_root, &file);
    }

    inspect_all_changes(&repo_root)
}

fn get_branch_name(workspace: &str) -> Option<String> {
    git_command()
        .args(["-C", workspace, "branch", "--show-current"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !s.is_empty() {
                Some(s)
            } else {
                // If in detached HEAD state, get short commit hash
                git_command()
                    .args(["-C", workspace, "rev-parse", "--short", "HEAD"])
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| format!("HEAD ({})", String::from_utf8_lossy(&o.stdout).trim()))
            }
        })
}

fn to_repo_relative_path(workspace: &str, target_path: &str) -> String {
    let ws_path = Path::new(workspace);
    let target = Path::new(target_path);

    if target.is_relative() {
        return target_path.replace('\\', "/");
    }

    if let Ok(rel) = target.strip_prefix(ws_path) {
        return rel.to_string_lossy().replace('\\', "/");
    }

    // Try canonicalized forms if possible
    if let (Ok(can_ws), Ok(can_target)) = (ws_path.canonicalize(), target.canonicalize()) {
        if let Ok(rel) = can_target.strip_prefix(&can_ws) {
            let rel_str = rel.to_string_lossy();
            return rel_str.trim_start_matches(r"\\?\").replace('\\', "/");
        }
    }

    target_path.replace('\\', "/")
}

fn run_git_diff(workspace: &str, file_rel_path: Option<&str>) -> std::result::Result<String, String> {
    let mut args = vec![
        "-C",
        workspace,
        "diff",
        "HEAD",
        "--ignore-space-at-eol",
        "--ignore-cr-at-eol",
        "-U3",
    ];

    if let Some(file) = file_rel_path {
        args.push("--");
        args.push(file);
    }

    let out = git_command()
        .args(&args)
        .output()
        .map_err(|e| format!("failed to spawn git diff: {e}"))?;

    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).to_string();
        if !s.is_empty() {
            return Ok(s);
        }
        // Check unstaged diff if HEAD comparison was empty
        let mut unstaged_args = vec![
            "-C",
            workspace,
            "diff",
            "--ignore-space-at-eol",
            "--ignore-cr-at-eol",
            "-U3",
        ];
        if let Some(file) = file_rel_path {
            unstaged_args.push("--");
            unstaged_args.push(file);
        }
        let unstaged_out = git_command()
            .args(&unstaged_args)
            .output()
            .map_err(|e| format!("failed to spawn git diff: {e}"))?;
        if unstaged_out.status.success() {
            return Ok(String::from_utf8_lossy(&unstaged_out.stdout).to_string());
        }
    }

    let stderr = String::from_utf8_lossy(&out.stderr);
    // If repository has no commits yet (bad revision 'HEAD'), fallback to diff --cached and unstaged diff
    if stderr.contains("bad revision 'HEAD'") || stderr.contains("unknown revision") {
        let mut cached_args = vec![
            "-C",
            workspace,
            "diff",
            "--cached",
            "--ignore-space-at-eol",
            "--ignore-cr-at-eol",
            "-U3",
        ];
        if let Some(file) = file_rel_path {
            cached_args.push("--");
            cached_args.push(file);
        }
        let cached_out = git_command()
            .args(&cached_args)
            .output()
            .map_err(|e| format!("failed to spawn git diff --cached: {e}"))?;
        let cached_str = if cached_out.status.success() {
            String::from_utf8_lossy(&cached_out.stdout).to_string()
        } else {
            String::new()
        };

        let mut unstaged_args = vec![
            "-C",
            workspace,
            "diff",
            "--ignore-space-at-eol",
            "--ignore-cr-at-eol",
            "-U3",
        ];
        if let Some(file) = file_rel_path {
            unstaged_args.push("--");
            unstaged_args.push(file);
        }
        let unstaged_out = git_command()
            .args(&unstaged_args)
            .output()
            .map_err(|e| format!("failed to spawn git diff: {e}"))?;
        let unstaged_str = if unstaged_out.status.success() {
            String::from_utf8_lossy(&unstaged_out.stdout).to_string()
        } else {
            String::new()
        };

        let combined = format!("{cached_str}{unstaged_str}");
        return Ok(combined);
    }

    Err(format!("git diff error: {stderr}"))
}

fn inspect_single_file(workspace: &str, target_path: &str) -> std::result::Result<Value, String> {
    let branch = get_branch_name(workspace);
    let workspace_path = Path::new(workspace);
    let rel_path = to_repo_relative_path(workspace, target_path);
    let full_path = if Path::new(target_path).is_absolute() {
        PathBuf::from(target_path)
    } else {
        workspace_path.join(&rel_path)
    };

    let diff_text = run_git_diff(workspace, Some(&rel_path))?;

    if !diff_text.is_empty() {
        if is_noise_file(target_path) {
            return Ok(json!({
                "is_git_repo": true,
                "branch": branch,
                "path": workspace,
                "target_file": target_path,
                "has_changes": true,
                "status": "Modified (Noise/Binary file)",
                "diff": format!("+++ {target_path} (Noise or binary file changed, full diff suppressed)"),
                "truncated": false
            }));
        }

        let is_truncated = diff_text.chars().count() > MAX_OUTPUT_CHARS;
        let final_diff = if is_truncated {
            diff_text.chars().take(MAX_OUTPUT_CHARS).collect::<String>()
        } else {
            diff_text
        };
        return Ok(json!({
            "is_git_repo": true,
            "branch": branch,
            "path": workspace,
            "target_file": target_path,
            "has_changes": true,
            "diff": final_diff,
            "truncated": is_truncated
        }));
    }

    // Check git status specifically for this file to avoid false positives on clean or ignored files
    let status_output = git_command()
        .args(["-C", workspace, "status", "--porcelain=v1", "-z", "--", &rel_path])
        .output()
        .map_err(|e| format!("git status failed: {e}"))?;

    if !status_output.status.success() {
        return Err(format!(
            "git status error: {}",
            String::from_utf8_lossy(&status_output.stderr)
        ));
    }

    let stdout_bytes = status_output.stdout;
    if stdout_bytes.is_empty() {
        return Ok(json!({
            "is_git_repo": true,
            "branch": branch,
            "path": workspace,
            "target_file": target_path,
            "has_changes": false,
            "message": "No changes found for this file."
        }));
    }

    let status_code = if stdout_bytes.len() >= 2 {
        String::from_utf8_lossy(&stdout_bytes[0..2]).to_string()
    } else {
        String::new()
    };

    if status_code.starts_with("??") {
        if is_noise_file(target_path) {
            return Ok(json!({
                "is_git_repo": true,
                "branch": branch,
                "path": workspace,
                "target_file": target_path,
                "has_changes": true,
                "status": "Untracked (Noise/Binary file)",
                "diff": format!("+++ {target_path} (Untracked binary or lockfile)"),
                "truncated": false
            }));
        }

        if let Ok(metadata) = std::fs::symlink_metadata(&full_path) {
            if metadata.file_type().is_file() && metadata.len() <= MAX_FILE_READ_BYTES {
                if let Ok(content) = std::fs::read_to_string(&full_path) {
                    let lines: Vec<&str> = content.lines().take(MAX_UNTRACKED_FILE_LINES).collect();
                    let untracked_preview = lines.join("\n");
                    let is_truncated = content.lines().count() > MAX_UNTRACKED_FILE_LINES;
                    return Ok(json!({
                        "is_git_repo": true,
                        "branch": branch,
                        "path": workspace,
                        "target_file": target_path,
                        "has_changes": true,
                        "status": "Untracked (New file)",
                        "diff": format!("+++ {target_path}\n@@ -0,0 +1,{} @@\n{}", lines.len(), untracked_preview),
                        "truncated": is_truncated
                    }));
                }
            }
        }

        return Ok(json!({
            "is_git_repo": true,
            "branch": branch,
            "path": workspace,
            "target_file": target_path,
            "has_changes": true,
            "status": "Untracked",
            "diff": format!("+++ {target_path} (Untracked file)"),
            "truncated": false
        }));
    }

    let status_desc = match status_code.as_str() {
        "A " | " A" | "AM" | "AD" => "Added",
        "D " | " D" => "Deleted",
        "R " | " R" | "RM" | "RD" => "Renamed",
        "M " | " M" | "MM" => "Modified",
        _ => "Changed",
    };

    Ok(json!({
        "is_git_repo": true,
        "branch": branch,
        "path": workspace,
        "target_file": target_path,
        "has_changes": true,
        "status": status_desc,
        "diff": format!("=== {target_path} ({status_desc}) ==="),
        "truncated": false
    }))
}

fn get_diff_numstat(workspace: &str) -> HashMap<String, (usize, usize)> {
    let mut map = HashMap::new();

    // 1. Try HEAD numstat
    let out = git_command()
        .args(["-C", workspace, "diff", "HEAD", "--numstat", "-z"])
        .output();

    let (stdout, success) = match out {
        Ok(o) if o.status.success() => (o.stdout, true),
        _ => (Vec::new(), false),
    };

    if success {
        parse_numstat_z(&stdout, &mut map);
        // Also get unstaged numstat in case HEAD diff didn't capture something
        if let Ok(unstaged_out) = git_command()
            .args(["-C", workspace, "diff", "--numstat", "-z"])
            .output()
        {
            if unstaged_out.status.success() {
                parse_numstat_z(&unstaged_out.stdout, &mut map);
            }
        }
        return map;
    }

    // If HEAD failed (e.g. empty repo), try cached and unstaged
    if let Ok(cached_out) = git_command()
        .args(["-C", workspace, "diff", "--cached", "--numstat", "-z"])
        .output()
    {
        if cached_out.status.success() {
            parse_numstat_z(&cached_out.stdout, &mut map);
        }
    }
    if let Ok(unstaged_out) = git_command()
        .args(["-C", workspace, "diff", "--numstat", "-z"])
        .output()
    {
        if unstaged_out.status.success() {
            parse_numstat_z(&unstaged_out.stdout, &mut map);
        }
    }

    map
}

fn parse_numstat_z(raw_bytes: &[u8], map: &mut HashMap<String, (usize, usize)>) {
    let text = String::from_utf8_lossy(raw_bytes);
    let mut parts = text.split('\0');

    while let Some(line) = parts.next() {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() >= 3 {
            let added = fields[0].trim().parse::<usize>().unwrap_or(0);
            let deleted = fields[1].trim().parse::<usize>().unwrap_or(0);
            let file_path = fields[2].trim().to_string();

            let entry = map.entry(file_path).or_insert((0, 0));
            entry.0 = entry.0.max(added);
            entry.1 = entry.1.max(deleted);
        }
    }
}

fn parse_porcelain_z(raw_bytes: &[u8], numstat_map: &HashMap<String, (usize, usize)>) -> (Vec<ChangedFile>, Vec<String>) {
    let mut files = Vec::new();
    let mut untracked_files = Vec::new();
    let mut chunks = raw_bytes.split(|&b| b == 0);

    while let Some(chunk) = chunks.next() {
        if chunk.len() < 3 {
            continue;
        }

        let index_status = chunk[0] as char;
        let worktree_status = chunk[1] as char;
        let file_path = String::from_utf8_lossy(&chunk[3..]).to_string();

        if index_status == '?' && worktree_status == '?' {
            untracked_files.push(file_path.clone());
            files.push(ChangedFile {
                path: file_path,
                status: "Untracked".into(),
                staged: false,
                added: None,
                deleted: None,
            });
            continue;
        }

        // If it's a rename (R) or copy (C), next NUL chunk is the original path
        if index_status == 'R' || index_status == 'C' || worktree_status == 'R' || worktree_status == 'C' {
            let _orig_path = chunks.next();
        }

        let is_staged = index_status != ' ' && index_status != '?';
        let status_desc = match (index_status, worktree_status) {
            ('M', _) | (_, 'M') => "Modified",
            ('A', _) | (_, 'A') => "Added",
            ('D', _) | (_, 'D') => "Deleted",
            ('R', _) | (_, 'R') => "Renamed",
            _ => "Changed",
        };

        let (added, deleted) = numstat_map
            .get(&file_path)
            .map(|&(a, d)| (Some(a), Some(d)))
            .unwrap_or((None, None));

        files.push(ChangedFile {
            path: file_path,
            status: status_desc.into(),
            staged: is_staged,
            added,
            deleted,
        });
    }

    (files, untracked_files)
}

fn inspect_all_changes(workspace: &str) -> std::result::Result<Value, String> {
    let branch = get_branch_name(workspace);
    let status_output = git_command()
        .args(["-C", workspace, "status", "--porcelain=v1", "-z"])
        .output()
        .map_err(|e| format!("git status failed: {e}"))?;

    if !status_output.status.success() {
        return Err(format!(
            "git status error: {}",
            String::from_utf8_lossy(&status_output.stderr)
        ));
    }

    let numstat_map = get_diff_numstat(workspace);
    let (files, untracked_files) = parse_porcelain_z(&status_output.stdout, &numstat_map);

    if files.is_empty() {
        return Ok(json!({
            "is_git_repo": true,
            "branch": branch,
            "path": workspace,
            "has_changes": false,
            "changed_files_count": 0,
            "message": "Working tree is clean, no uncommitted changes."
        }));
    }

    let diff_text = run_git_diff(workspace, None).unwrap_or_default();

    let mut combined_diff = String::new();
    let mut total_chars = 0;
    let mut is_truncated = false;

    // Filter lockfiles and large diff hunks
    for hunk in diff_text.split("diff --git ") {
        if hunk.trim().is_empty() {
            continue;
        }
        let first_line = hunk.lines().next().unwrap_or("");
        if is_noise_file(first_line) {
            continue;
        }

        let formatted_hunk = format!("diff --git {hunk}");
        let hunk_char_count = formatted_hunk.chars().count();
        if total_chars + hunk_char_count > MAX_OUTPUT_CHARS {
            is_truncated = true;
            let remaining_budget = MAX_OUTPUT_CHARS.saturating_sub(total_chars);
            if remaining_budget > 0 {
                let partial: String = formatted_hunk.chars().take(remaining_budget).collect();
                combined_diff.push_str(&partial);
            }
            break;
        }
        combined_diff.push_str(&formatted_hunk);
        total_chars += hunk_char_count;
    }

    // Include previews for untracked files if budget allows
    if !is_truncated && !untracked_files.is_empty() {
        let workspace_path = Path::new(workspace);
        for untracked in &untracked_files {
            if is_noise_file(untracked) {
                continue;
            }
            let full_path = workspace_path.join(untracked);
            if let Ok(metadata) = std::fs::symlink_metadata(&full_path) {
                if metadata.file_type().is_file() && metadata.len() <= MAX_FILE_READ_BYTES {
                    if let Ok(content) = std::fs::read_to_string(&full_path) {
                        let lines: Vec<&str> = content.lines().take(20).collect();
                        let snippet = format!(
                            "\n--- /dev/null\n+++ b/{untracked}\n@@ -0,0 +1,{} @@\n{}\n",
                            lines.len(),
                            lines.join("\n")
                        );
                        let snippet_chars = snippet.chars().count();
                        if total_chars + snippet_chars > MAX_OUTPUT_CHARS {
                            is_truncated = true;
                            let remaining_budget = MAX_OUTPUT_CHARS.saturating_sub(total_chars);
                            if remaining_budget > 0 {
                                let partial: String = snippet.chars().take(remaining_budget).collect();
                                combined_diff.push_str(&partial);
                            }
                            break;
                        }
                        combined_diff.push_str(&snippet);
                        total_chars += snippet_chars;
                    }
                }
            }
        }
    }

    let changed_count = files.len();
    let truncated_files = changed_count > MAX_CHANGED_FILES_SUMMARY;
    let displayed_files: Vec<ChangedFile> = if truncated_files {
        files.into_iter().take(MAX_CHANGED_FILES_SUMMARY).collect()
    } else {
        files
    };

    let mut response = json!({
        "is_git_repo": true,
        "branch": branch,
        "path": workspace,
        "has_changes": true,
        "changed_files_count": changed_count,
        "files": displayed_files,
        "diff": combined_diff,
        "truncated": is_truncated || truncated_files
    });

    if truncated_files {
        response["files_truncated"] = json!(true);
    }

    if is_truncated || truncated_files {
        response["hint"] = json!("Diff or file list was truncated due to large size. Call `InspectChanges(path=\"<path>\")` to inspect the detailed diff of a specific file.");
    }

    Ok(response)
}

fn is_noise_file(path_str: &str) -> bool {
    let lower = path_str.to_ascii_lowercase();
    lower.ends_with(".lock")
        || lower.ends_with("package-lock.json")
        || lower.ends_with("pnpm-lock.yaml")
        || lower.ends_with("yarn.lock")
        || lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".ico")
        || lower.ends_with(".pdf")
        || lower.ends_with(".wasm")
        || lower.ends_with(".exe")
        || lower.ends_with(".dll")
        || lower.ends_with(".so")
}
