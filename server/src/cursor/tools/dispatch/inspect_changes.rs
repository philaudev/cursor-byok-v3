//! Local execution engine for the `InspectChanges` tool.
//! Inspects uncommitted git status and diffs in a token-safe manner.

use std::{
    path::Path,
    process::Command,
};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    model::ToolCall,
    Result,
};

use super::ToolStart;
use crate::cursor::tools::{
    result::{self, ToolResultSender},
    runtime::{now_ms, ExecContext},
};

const MAX_OUTPUT_CHARS: usize = 8_000;
const MAX_UNTRACKED_FILE_LINES: usize = 40;

#[derive(Debug, Deserialize)]
struct InspectChangesArgs {
    path: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ChangedFile {
    path: String,
    status: String,
    staged: bool,
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
    let git_root_output = Command::new("git")
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
    Command::new("git")
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
                Command::new("git")
                    .args(["-C", workspace, "rev-parse", "--short", "HEAD"])
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| format!("HEAD ({})", String::from_utf8_lossy(&o.stdout).trim()))
            }
        })
}

fn inspect_single_file(workspace: &str, target_path: &str) -> std::result::Result<Value, String> {
    let branch = get_branch_name(workspace);
    let diff_output = Command::new("git")
        .args([
            "-C",
            workspace,
            "diff",
            "HEAD",
            "--ignore-space-at-eol",
            "--ignore-cr-at-eol",
            "-U3",
            "--",
            target_path,
        ])
        .output();

    let diff_text = match diff_output {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).to_string();
            if !s.is_empty() {
                s
            } else {
                // If HEAD comparison is empty, try unstaged diff
                Command::new("git")
                    .args([
                        "-C",
                        workspace,
                        "diff",
                        "--ignore-space-at-eol",
                        "--ignore-cr-at-eol",
                        "-U3",
                        "--",
                        target_path,
                    ])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default()
            }
        }
        _ => String::new(),
    };

    if !diff_text.is_empty() {
        let is_truncated = diff_text.len() > MAX_OUTPUT_CHARS;
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

    // Check if it's an untracked file
    let full_path = if Path::new(target_path).is_absolute() {
        target_path.to_string()
    } else {
        format!("{workspace}/{target_path}")
    };

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

    Ok(json!({
        "is_git_repo": true,
        "branch": branch,
        "path": workspace,
        "target_file": target_path,
        "has_changes": false,
        "message": "No changes found for this file."
    }))
}

fn inspect_all_changes(workspace: &str) -> std::result::Result<Value, String> {
    let branch = get_branch_name(workspace);
    let status_output = Command::new("git")
        .args(["-C", workspace, "status", "--porcelain"])
        .output()
        .map_err(|e| format!("git status failed: {e}"))?;

    if !status_output.status.success() {
        return Err(format!(
            "git status error: {}",
            String::from_utf8_lossy(&status_output.stderr)
        ));
    }

    let status_str = String::from_utf8_lossy(&status_output.stdout);
    let mut files = Vec::new();
    let mut untracked_files = Vec::new();

    for line in status_str.lines() {
        if line.len() < 4 {
            continue;
        }
        let index_status = &line[0..1];
        let worktree_status = &line[1..2];
        let file_path = line[3..].trim().to_string();

        if index_status == "?" && worktree_status == "?" {
            untracked_files.push(file_path.clone());
            files.push(ChangedFile {
                path: file_path,
                status: "Untracked".into(),
                staged: false,
            });
            continue;
        }

        let is_staged = index_status != " " && index_status != "?";
        let status_desc = match (index_status, worktree_status) {
            ("M", _) | (_, "M") => "Modified",
            ("A", _) => "Added",
            ("D", _) | (_, "D") => "Deleted",
            ("R", _) => "Renamed",
            _ => "Changed",
        };

        files.push(ChangedFile {
            path: file_path,
            status: status_desc.into(),
            staged: is_staged,
        });
    }

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

    // Get git diff
    let diff_output = Command::new("git")
        .args([
            "-C",
            workspace,
            "diff",
            "HEAD",
            "--ignore-space-at-eol",
            "--ignore-cr-at-eol",
            "-U2",
        ])
        .output();

    let diff_text = match diff_output {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).to_string();
            if !s.is_empty() {
                s
            } else {
                Command::new("git")
                    .args([
                        "-C",
                        workspace,
                        "diff",
                        "--ignore-space-at-eol",
                        "--ignore-cr-at-eol",
                        "-U2",
                    ])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default()
            }
        }
        _ => String::new(),
    };

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
        if total_chars + formatted_hunk.len() > MAX_OUTPUT_CHARS {
            is_truncated = true;
            break;
        }
        combined_diff.push_str(&formatted_hunk);
        total_chars += formatted_hunk.len();
    }

    // Include previews for untracked files if budget allows
    if !is_truncated && !untracked_files.is_empty() {
        for untracked in &untracked_files {
            if is_noise_file(untracked) {
                continue;
            }
            let full_path = format!("{workspace}/{untracked}");
            if let Ok(content) = std::fs::read_to_string(&full_path) {
                let lines: Vec<&str> = content.lines().take(20).collect();
                let snippet = format!(
                    "\n--- /dev/null\n+++ b/{untracked}\n@@ -0,0 +1,{} @@\n{}\n",
                    lines.len(),
                    lines.join("\n")
                );
                if total_chars + snippet.len() > MAX_OUTPUT_CHARS {
                    is_truncated = true;
                    break;
                }
                combined_diff.push_str(&snippet);
                total_chars += snippet.len();
            }
        }
    }

    let changed_count = files.len();
    let mut response = json!({
        "is_git_repo": true,
        "branch": branch,
        "path": workspace,
        "has_changes": true,
        "changed_files_count": changed_count,
        "files": files,
        "diff": combined_diff,
        "truncated": is_truncated
    });

    if is_truncated {
        response["hint"] = json!("Diff was truncated due to large size. Call `InspectChanges(path=\"<path>\")` to inspect the detailed diff of a specific file.");
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
