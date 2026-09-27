//! Local execution engine for the `InspectChanges` tool.
//! Inspects uncommitted git status and diffs in a token-safe manner.

use std::{collections::HashMap, path::Path, process::Command};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Result, model::ToolCall};

use super::ToolStart;
use crate::cursor::tools::{
    runtime::{ExecContext, now_ms},
    tool_call_result::{self as result, ToolResultSender},
};

const MAX_OUTPUT_CHARS: usize = 8_000;
const MAX_UNTRACKED_FILE_LINES: usize = 40;
const MAX_FILE_READ_BYTES: u64 = 5_000_000;
const MAX_CHANGED_FILES_SUMMARY: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum InspectMode {
    Summary,
    Diff,
    #[default]
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum InspectScope {
    #[default]
    All,
    Staged,
    Unstaged,
}

fn default_max_files() -> usize {
    MAX_CHANGED_FILES_SUMMARY
}

fn default_max_diff_chars() -> usize {
    MAX_OUTPUT_CHARS
}

#[derive(Debug, Deserialize)]
struct InspectChangesArgs {
    path: String,
    #[serde(default)]
    mode: InspectMode,
    #[serde(default)]
    scope: InspectScope,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default = "default_max_files")]
    max_files: usize,
    #[serde(default = "default_max_diff_chars")]
    max_diff_chars: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
struct InspectCounts {
    changed_files: usize,
    staged_files: usize,
    unstaged_files: usize,
    untracked_files: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ChangedFile {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_path: Option<String>,
    status: String,
    index_status: String,
    worktree_status: String,
    staged: bool,
    unstaged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    staged_added: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    staged_deleted: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unstaged_added: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unstaged_deleted: Option<usize>,
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

    validate_files_parameter(&args.files)?;

    let target_path = Path::new(target_path_str);

    // Determine target directory vs single file
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

    let git_root_output = git_command()
        .args(["-C", &target_dir, "rev-parse", "--show-toplevel"])
        .output();

    let git_root = match git_root_output {
        Ok(out) if out.status.success() => {
            let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !root.is_empty() { Some(root) } else { None }
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

    let branch = get_branch_name(&repo_root);

    let max_files = args.max_files.clamp(1, MAX_CHANGED_FILES_SUMMARY);
    let max_diff_chars = args.max_diff_chars.min(MAX_OUTPUT_CHARS);

    let mut files_to_inspect = args.files.clone();
    if let Some(file) = specific_file {
        if files_to_inspect.is_empty() {
            let rel = to_repo_relative_path(&repo_root, &file);
            files_to_inspect.push(rel);
        }
    }

    let status_output = git_command()
        .args(["-C", &repo_root, "status", "--porcelain=v1", "-z"])
        .output()
        .map_err(|e| format!("git status failed: {e}"))?;

    if !status_output.status.success() {
        return Err(format!(
            "git status error: {}",
            String::from_utf8_lossy(&status_output.stderr)
        ));
    }

    let staged_numstat = match args.scope {
        InspectScope::All | InspectScope::Staged => get_staged_numstat(&repo_root),
        InspectScope::Unstaged => HashMap::new(),
    };
    let unstaged_numstat = match args.scope {
        InspectScope::All | InspectScope::Unstaged => get_unstaged_numstat(&repo_root),
        InspectScope::Staged => HashMap::new(),
    };

    let (all_files, _untracked_files) =
        parse_porcelain_z(&status_output.stdout, &staged_numstat, &unstaged_numstat);

    // Filter by scope before calculating counts so the response is internally consistent.
    let scoped_files: Vec<ChangedFile> = all_files
        .into_iter()
        .filter(|f| match args.scope {
            InspectScope::All => true,
            InspectScope::Staged => f.staged,
            InspectScope::Unstaged => f.unstaged,
        })
        .collect();

    let counts = InspectCounts {
        changed_files: scoped_files.len(),
        staged_files: scoped_files.iter().filter(|f| f.staged).count(),
        unstaged_files: scoped_files
            .iter()
            .filter(|f| f.unstaged && f.status != "Untracked")
            .count(),
        untracked_files: scoped_files
            .iter()
            .filter(|f| f.status == "Untracked")
            .count(),
    };

    if scoped_files.is_empty() {
        return Ok(json!({
            "is_git_repo": true,
            "repository_root": repo_root,
            "branch": branch,
            "path": target_path_str,
            "has_changes": false,
            "changed_files_count": 0,
            "mode": args.mode,
            "scope": args.scope,
            "counts": counts,
            "files": [],
            "diff": "",
            "truncated": false,
            "files_truncated": false,
            "diff_truncated": false,
            "selected_files": [],
            "omitted_files_count": 0,
            "next_cursor": null,
            "message": "No changes matched the requested scope."
        }));
    }

    // Filter by files_to_inspect
    let filtered_files: Vec<ChangedFile> = if files_to_inspect.is_empty() {
        scoped_files
    } else {
        let requested_set: std::collections::HashSet<String> = files_to_inspect
            .iter()
            .map(|f| f.trim().replace('\\', "/"))
            .collect();
        scoped_files
            .into_iter()
            .filter(|f| {
                requested_set.contains(&f.path)
                    || f.previous_path
                        .as_ref()
                        .is_some_and(|p| requested_set.contains(p))
            })
            .collect()
    };

    let total_matching = filtered_files.len();
    let files_truncated = total_matching > max_files;
    let displayed_files: Vec<ChangedFile> = if files_truncated {
        filtered_files.into_iter().take(max_files).collect()
    } else {
        filtered_files
    };
    let omitted_files_count = total_matching.saturating_sub(displayed_files.len());

    let selected_files: Vec<String> = displayed_files.iter().map(|f| f.path.clone()).collect();

    let mut combined_diff = String::new();
    let mut total_chars = 0;
    let mut is_truncated = false;

    if args.mode != InspectMode::Summary && !displayed_files.is_empty() {
        let diff_file_selectors: Vec<&str> = if selected_files.is_empty() {
            Vec::new()
        } else {
            displayed_files
                .iter()
                .filter(|f| f.status != "Untracked")
                .map(|f| f.path.as_str())
                .collect()
        };

        let diff_text = if !selected_files.is_empty() && diff_file_selectors.is_empty() {
            String::new()
        } else {
            run_git_diff(&repo_root, args.scope, &diff_file_selectors)?
        };

        for section in diff_text.split("diff --git ") {
            if section.trim().is_empty() {
                continue;
            }
            let first_line = section.lines().next().unwrap_or("");
            if is_noise_file(first_line) {
                continue;
            }

            let formatted_section = format!("diff --git {section}");
            let section_char_count = formatted_section.chars().count();
            if total_chars + section_char_count > max_diff_chars {
                is_truncated = true;
                break;
            }
            combined_diff.push_str(&formatted_section);
            total_chars += section_char_count;
        }

        // Untracked files previews if scope != Staged
        if args.scope != InspectScope::Staged && !is_truncated {
            let workspace_path = Path::new(&repo_root);
            for file in &displayed_files {
                if file.status != "Untracked" || is_noise_file(&file.path) {
                    continue;
                }
                let full_path = workspace_path.join(&file.path);
                if let Ok(metadata) = std::fs::symlink_metadata(&full_path) {
                    if metadata.file_type().is_file() && metadata.len() <= MAX_FILE_READ_BYTES {
                        if let Ok(content) = std::fs::read_to_string(&full_path) {
                            let lines: Vec<&str> =
                                content.lines().take(MAX_UNTRACKED_FILE_LINES).collect();
                            let snippet = format!(
                                "\n--- /dev/null\n+++ b/{}\n@@ -0,0 +1,{} @@\n{}\n",
                                file.path,
                                lines.len(),
                                lines.join("\n")
                            );
                            let snippet_chars = snippet.chars().count();
                            if total_chars + snippet_chars > max_diff_chars {
                                is_truncated = true;
                                break;
                            }
                            combined_diff.push_str(&snippet);
                            total_chars += snippet_chars;
                        }
                    }
                }
            }
        }
    }

    let has_changes = !displayed_files.is_empty();
    let diff_truncated = is_truncated;
    let truncated = files_truncated || diff_truncated;

    let mut response = json!({
        "is_git_repo": true,
        "repository_root": repo_root,
        "branch": branch,
        "path": target_path_str,
        "has_changes": has_changes,
        "changed_files_count": counts.changed_files,
        "mode": args.mode,
        "scope": args.scope,
        "counts": counts,
        "files": displayed_files,
        "diff": combined_diff,
        "truncated": truncated,
        "files_truncated": files_truncated,
        "diff_truncated": diff_truncated,
        "selected_files": selected_files,
        "omitted_files_count": omitted_files_count,
        "next_cursor": null,
    });

    if truncated {
        response["hint"] = json!(
            "Diff or file list was truncated due to limits. Use `files` to inspect specific files or adjust `max_files` / `max_diff_chars`."
        );
    }

    Ok(response)
}

fn validate_files_parameter(files: &[String]) -> std::result::Result<(), String> {
    for f in files {
        let trimmed = f.trim();
        if trimmed.is_empty() {
            return Err("The `files` entries must not be empty.".into());
        }
        if Path::new(trimmed).is_absolute()
            || trimmed.starts_with('/')
            || trimmed.starts_with('\\')
            || trimmed.contains(':')
        {
            return Err(format!(
                "The file path `{trimmed}` in `files` must be a repository-relative path."
            ));
        }
        if Path::new(trimmed)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(format!(
                "The file path `{trimmed}` in `files` must not contain parent path traversal ('..')."
            ));
        }
    }
    Ok(())
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

fn run_git_diff(
    workspace: &str,
    scope: InspectScope,
    file_selectors: &[&str],
) -> std::result::Result<String, String> {
    match scope {
        InspectScope::Staged => {
            let mut args = vec![
                "-C",
                workspace,
                "diff",
                "--cached",
                "--ignore-space-at-eol",
                "--ignore-cr-at-eol",
                "-U3",
            ];
            if !file_selectors.is_empty() {
                args.push("--");
                args.extend(file_selectors);
            }
            let out = git_command()
                .args(&args)
                .output()
                .map_err(|e| format!("failed to spawn git diff --cached: {e}"))?;
            if out.status.success() {
                Ok(String::from_utf8_lossy(&out.stdout).to_string())
            } else {
                Err(format!(
                    "git diff --cached error: {}",
                    String::from_utf8_lossy(&out.stderr)
                ))
            }
        }
        InspectScope::Unstaged => {
            let mut args = vec![
                "-C",
                workspace,
                "diff",
                "--ignore-space-at-eol",
                "--ignore-cr-at-eol",
                "-U3",
            ];
            if !file_selectors.is_empty() {
                args.push("--");
                args.extend(file_selectors);
            }
            let out = git_command()
                .args(&args)
                .output()
                .map_err(|e| format!("failed to spawn git diff: {e}"))?;
            if out.status.success() {
                Ok(String::from_utf8_lossy(&out.stdout).to_string())
            } else {
                Err(format!(
                    "git diff error: {}",
                    String::from_utf8_lossy(&out.stderr)
                ))
            }
        }
        InspectScope::All => {
            let mut args = vec![
                "-C",
                workspace,
                "diff",
                "HEAD",
                "--ignore-space-at-eol",
                "--ignore-cr-at-eol",
                "-U3",
            ];
            if !file_selectors.is_empty() {
                args.push("--");
                args.extend(file_selectors);
            }
            let out = git_command()
                .args(&args)
                .output()
                .map_err(|e| format!("failed to spawn git diff HEAD: {e}"))?;

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
                if !file_selectors.is_empty() {
                    unstaged_args.push("--");
                    unstaged_args.extend(file_selectors);
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
                if !file_selectors.is_empty() {
                    cached_args.push("--");
                    cached_args.extend(file_selectors);
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
                if !file_selectors.is_empty() {
                    unstaged_args.push("--");
                    unstaged_args.extend(file_selectors);
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
    }
}

fn get_staged_numstat(workspace: &str) -> HashMap<String, (usize, usize)> {
    let mut map = HashMap::new();
    if let Ok(out) = git_command()
        .args(["-C", workspace, "diff", "--cached", "--numstat", "-z"])
        .output()
    {
        if out.status.success() {
            parse_numstat_z(&out.stdout, &mut map);
        }
    }
    map
}

fn get_unstaged_numstat(workspace: &str) -> HashMap<String, (usize, usize)> {
    let mut map = HashMap::new();
    if let Ok(out) = git_command()
        .args(["-C", workspace, "diff", "--numstat", "-z"])
        .output()
    {
        if out.status.success() {
            parse_numstat_z(&out.stdout, &mut map);
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

            if file_path.is_empty() {
                let old_path = parts.next();
                let new_path = parts.next();
                if let Some(dest) = new_path.or(old_path) {
                    let entry = map.entry(dest.to_string()).or_insert((0, 0));
                    entry.0 += added;
                    entry.1 += deleted;
                }
            } else {
                let entry = map.entry(file_path).or_insert((0, 0));
                entry.0 += added;
                entry.1 += deleted;
            }
        }
    }
}

fn parse_porcelain_z(
    raw_bytes: &[u8],
    staged_numstat: &HashMap<String, (usize, usize)>,
    unstaged_numstat: &HashMap<String, (usize, usize)>,
) -> (Vec<ChangedFile>, Vec<String>) {
    let mut files = Vec::new();
    let mut untracked_files = Vec::new();
    let mut chunks = raw_bytes.split(|&b| b == 0);

    while let Some(chunk) = chunks.next() {
        if chunk.is_empty() {
            continue;
        }
        if chunk.len() < 3 {
            continue;
        }

        let index_status_char = chunk[0] as char;
        let worktree_status_char = chunk[1] as char;
        let file_path = String::from_utf8_lossy(&chunk[3..]).to_string();

        if index_status_char == '?' && worktree_status_char == '?' {
            untracked_files.push(file_path.clone());
            files.push(ChangedFile {
                path: file_path,
                previous_path: None,
                status: "Untracked".into(),
                index_status: "?".into(),
                worktree_status: "?".into(),
                staged: false,
                unstaged: true,
                staged_added: None,
                staged_deleted: None,
                unstaged_added: None,
                unstaged_deleted: None,
                added: None,
                deleted: None,
            });
            continue;
        }

        let mut previous_path = None;
        if index_status_char == 'R'
            || index_status_char == 'C'
            || worktree_status_char == 'R'
            || worktree_status_char == 'C'
        {
            if let Some(orig_chunk) = chunks.next() {
                let orig_str = String::from_utf8_lossy(orig_chunk).to_string();
                if !orig_str.is_empty() {
                    previous_path = Some(orig_str);
                }
            }
        }

        let staged = index_status_char != ' ' && index_status_char != '?';
        let unstaged = worktree_status_char != ' ' && worktree_status_char != '?';

        let status_desc = match (index_status_char, worktree_status_char) {
            ('A', _) | (_, 'A') => "Added",
            ('D', _) | (_, 'D') => "Deleted",
            ('R', _) | (_, 'R') => "Renamed",
            ('C', _) | (_, 'C') => "Copied",
            ('M', _) | (_, 'M') => "Modified",
            _ => "Changed",
        };

        let (staged_added, staged_deleted) = staged_numstat
            .get(&file_path)
            .map(|&(a, d)| (Some(a), Some(d)))
            .unwrap_or((None, None));

        let (unstaged_added, unstaged_deleted) = unstaged_numstat
            .get(&file_path)
            .map(|&(a, d)| (Some(a), Some(d)))
            .unwrap_or((None, None));

        let added = match (staged_added, unstaged_added) {
            (Some(s), Some(u)) => Some(s + u),
            (Some(s), None) => Some(s),
            (None, Some(u)) => Some(u),
            (None, None) => None,
        };

        let deleted = match (staged_deleted, unstaged_deleted) {
            (Some(s), Some(u)) => Some(s + u),
            (Some(s), None) => Some(s),
            (None, Some(u)) => Some(u),
            (None, None) => None,
        };

        files.push(ChangedFile {
            path: file_path,
            previous_path,
            status: status_desc.into(),
            index_status: index_status_char.to_string(),
            worktree_status: worktree_status_char.to_string(),
            staged,
            unstaged,
            staged_added,
            staged_deleted,
            unstaged_added,
            unstaged_deleted,
            added,
            deleted,
        });
    }

    (files, untracked_files)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_porcelain_z_records() {
        let mut staged = HashMap::new();
        staged.insert("file_staged.rs".to_string(), (10, 2));
        staged.insert("file_both.rs".to_string(), (5, 1));
        staged.insert("new_renamed.rs".to_string(), (8, 0));

        let mut unstaged = HashMap::new();
        unstaged.insert("file_unstaged.rs".to_string(), (3, 4));
        unstaged.insert("file_both.rs".to_string(), (2, 2));

        let raw = b" M file_unstaged.rs\0M  file_staged.rs\0MM file_both.rs\0?? untracked.rs\0R  new_renamed.rs\0old_name.rs\0C  copied.rs\0source.rs\0AD added_deleted.rs\0AM added_modified.rs\0";

        let (files, untracked) = parse_porcelain_z(raw, &staged, &unstaged);
        assert_eq!(untracked, vec!["untracked.rs"]);
        assert_eq!(files.len(), 8);

        // 1. " M file_unstaged.rs"
        let f0 = &files[0];
        assert_eq!(f0.path, "file_unstaged.rs");
        assert_eq!(f0.index_status, " ");
        assert_eq!(f0.worktree_status, "M");
        assert!(!f0.staged);
        assert!(f0.unstaged);
        assert_eq!(f0.unstaged_added, Some(3));
        assert_eq!(f0.unstaged_deleted, Some(4));
        assert_eq!(f0.staged_added, None);

        // 2. "M  file_staged.rs"
        let f1 = &files[1];
        assert_eq!(f1.path, "file_staged.rs");
        assert_eq!(f1.index_status, "M");
        assert_eq!(f1.worktree_status, " ");
        assert!(f1.staged);
        assert!(!f1.unstaged);
        assert_eq!(f1.staged_added, Some(10));
        assert_eq!(f1.staged_deleted, Some(2));
        assert_eq!(f1.unstaged_added, None);

        // 3. "MM file_both.rs"
        let f2 = &files[2];
        assert_eq!(f2.path, "file_both.rs");
        assert_eq!(f2.index_status, "M");
        assert_eq!(f2.worktree_status, "M");
        assert!(f2.staged);
        assert!(f2.unstaged);
        assert_eq!(f2.staged_added, Some(5));
        assert_eq!(f2.unstaged_added, Some(2));
        assert_eq!(f2.added, Some(7));
        assert_eq!(f2.deleted, Some(3));

        // 4. "?? untracked.rs"
        let f3 = &files[3];
        assert_eq!(f3.path, "untracked.rs");
        assert_eq!(f3.index_status, "?");
        assert_eq!(f3.worktree_status, "?");
        assert!(!f3.staged);
        assert!(f3.unstaged);
        assert_eq!(f3.status, "Untracked");

        // 5. "R  new_renamed.rs\0old_name.rs"
        let f4 = &files[4];
        assert_eq!(f4.path, "new_renamed.rs");
        assert_eq!(f4.previous_path.as_deref(), Some("old_name.rs"));
        assert_eq!(f4.index_status, "R");
        assert_eq!(f4.worktree_status, " ");
        assert!(f4.staged);
        assert!(!f4.unstaged);
        assert_eq!(f4.status, "Renamed");

        // 6. "C  copied.rs\0source.rs"
        let f5 = &files[5];
        assert_eq!(f5.path, "copied.rs");
        assert_eq!(f5.previous_path.as_deref(), Some("source.rs"));
        assert_eq!(f5.index_status, "C");
        assert_eq!(f5.worktree_status, " ");
        assert!(f5.staged);
        assert_eq!(f5.status, "Copied");

        // 7. "AD added_deleted.rs"
        let f6 = &files[6];
        assert_eq!(f6.path, "added_deleted.rs");
        assert_eq!(f6.index_status, "A");
        assert_eq!(f6.worktree_status, "D");
        assert!(f6.staged);
        assert!(f6.unstaged);
        assert_eq!(f6.status, "Added");

        // 8. "AM added_modified.rs"
        let f7 = &files[7];
        assert_eq!(f7.path, "added_modified.rs");
        assert_eq!(f7.index_status, "A");
        assert_eq!(f7.worktree_status, "M");
        assert!(f7.staged);
        assert!(f7.unstaged);
        assert_eq!(f7.status, "Added");
    }

    #[test]
    fn default_request_args() {
        let json_val = json!({ "path": "C:/repo" });
        let args: InspectChangesArgs = serde_json::from_value(json_val).unwrap();
        assert_eq!(args.path, "C:/repo");
        assert_eq!(args.mode, InspectMode::Review);
        assert_eq!(args.scope, InspectScope::All);
        assert!(args.files.is_empty());
        assert_eq!(args.max_files, 100);
        assert_eq!(args.max_diff_chars, 8000);
    }

    #[test]
    fn invalid_mode_or_scope_fails_deserialization() {
        let invalid_mode = json!({ "path": "C:/repo", "mode": "history" });
        assert!(serde_json::from_value::<InspectChangesArgs>(invalid_mode).is_err());

        let invalid_scope = json!({ "path": "C:/repo", "scope": "invalid" });
        assert!(serde_json::from_value::<InspectChangesArgs>(invalid_scope).is_err());
    }

    #[test]
    fn validate_files_parameter_rejects_empty_or_unsafe() {
        assert!(validate_files_parameter(&["".to_string()]).is_err());
        assert!(validate_files_parameter(&["   ".to_string()]).is_err());
        assert!(validate_files_parameter(&["/absolute/path.rs".to_string()]).is_err());
        assert!(validate_files_parameter(&["C:/absolute/path.rs".to_string()]).is_err());
        assert!(validate_files_parameter(&["../outside.rs".to_string()]).is_err());
        assert!(validate_files_parameter(&["dir/../../outside.rs".to_string()]).is_err());
        assert!(
            validate_files_parameter(&["src/main.rs".to_string(), "Cargo.toml".to_string()])
                .is_ok()
        );
    }

    #[test]
    fn custom_request_args_parsing() {
        let json_val = json!({
            "path": "C:/repo",
            "mode": "summary",
            "scope": "staged",
            "files": ["src/main.rs", "Cargo.toml"],
            "max_files": 25,
            "max_diff_chars": 4000
        });
        let args: InspectChangesArgs = serde_json::from_value(json_val).unwrap();
        assert_eq!(args.path, "C:/repo");
        assert_eq!(args.mode, InspectMode::Summary);
        assert_eq!(args.scope, InspectScope::Staged);
        assert_eq!(args.files, vec!["src/main.rs", "Cargo.toml"]);
        assert_eq!(args.max_files, 25);
        assert_eq!(args.max_diff_chars, 4000);
    }

    #[test]
    fn empty_path_returns_error() {
        let args = InspectChangesArgs {
            path: "   ".into(),
            mode: InspectMode::Review,
            scope: InspectScope::All,
            files: vec![],
            max_files: 100,
            max_diff_chars: 8000,
        };
        assert!(execute_sync(args).is_err());
    }
}
