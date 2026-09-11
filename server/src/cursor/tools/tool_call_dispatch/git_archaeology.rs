//! Read-only Git history investigation for the `GitArchaeology` tool.
//! Uses Git CLI evidence rather than inferring historical intent from source alone.

use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    cursor::tools::{
        runtime::{now_ms, ExecContext},
        tool_call_result::{self as result, ToolResultSender},
    },
    model::ToolCall,
    Result,
};

use super::ToolStart;

const MAX_RECORDS: usize = 30;
const MAX_OUTPUT_CHARS: usize = 16_000;

#[derive(Debug, Deserialize)]
struct GitArchaeologyArgs {
    repository: String,
    operation: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    start_line: Option<u32>,
    #[serde(default)]
    end_line: Option<u32>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    regex: bool,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    commit: Option<String>,
}

pub(super) fn start(
    results: &ToolResultSender,
    call: &ToolCall,
    _context: &ExecContext,
) -> Result<ToolStart> {
    let args: GitArchaeologyArgs = serde_json::from_value(call.arguments.clone())?;
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

async fn execute(args: GitArchaeologyArgs) -> std::result::Result<Value, String> {
    tokio::task::spawn_blocking(move || execute_sync(args))
        .await
        .map_err(|error| error.to_string())?
}

fn execute_sync(args: GitArchaeologyArgs) -> std::result::Result<Value, String> {
    let repository = args.repository.trim();
    if repository.is_empty() {
        return Err("`repository` must be a non-empty repository path.".into());
    }

    let repo_root = git_output(repository, ["rev-parse", "--show-toplevel"])?;
    let repo_root = repo_root.trim().to_string();
    if repo_root.is_empty() {
        return Err(format!("`{repository}` is not inside a Git repository."));
    }

    let limit = args.limit.unwrap_or(20).clamp(1, MAX_RECORDS);
    let operation = args.operation.trim().to_ascii_lowercase();
    let result = match operation.as_str() {
        "lineage" => lineage(&repo_root, &args)?,
        "pickaxe" => pickaxe(&repo_root, &args, limit)?,
        "file_biography" => file_biography(&repo_root, &args, limit)?,
        "commit_context" => commit_context(&repo_root, &args)?,
        _ => return Err("`operation` must be one of: lineage, pickaxe, file_biography, commit_context.".into()),
    };

    Ok(json!({
        "repository": repo_root,
        "operation": operation,
        "result": result,
        "evidence_policy": "Every historical claim must be verified against the listed Git commit evidence. Git history alone cannot establish undocumented intent.",
        "still_unknown": "Commit messages and diffs may not record the full product or engineering rationale. Do not present unsupported intent as fact."
    }))
}

fn lineage(repository: &str, args: &GitArchaeologyArgs) -> std::result::Result<Value, String> {
    let path = required_path(args)?;
    let start = args.start_line.unwrap_or(1);
    let end = args.end_line.unwrap_or(start);
    if end < start {
        return Err("`end_line` must be greater than or equal to `start_line`.".into());
    }

    let range = format!("{start},{end}");
    let output = git_output(
        repository,
        [
            "blame", "-w", "-M", "-C", "--date=short", "-L", &range, "--", &path,
        ],
    )?;
    let lines = output
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    Ok(json!({
        "path": path,
        "line_range": { "start": start, "end": end },
        "blame_lines": lines,
        "evidence": bounded(&output),
        "method": "git blame -w -M -C follows formatting-only changes and detects moved/copied lines where Git can establish them."
    }))
}

fn pickaxe(repository: &str, args: &GitArchaeologyArgs, limit: usize) -> std::result::Result<Value, String> {
    let query = args.query.as_deref().map(str::trim).filter(|value| !value.is_empty())
        .ok_or("`query` is required for the pickaxe operation.")?;
    let count = format!("-{limit}");
    let mut command = git_command();
    command.current_dir(repository).args([
        "log", "--all", &count, "--date=short", "--format=%H%x1f%ad%x1f%an%x1f%s",
    ]);
    if args.regex {
        command.args(["-G", query]);
    } else {
        command.args(["-S", query]);
    }
    if let Some(path) = args.path.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        command.args(["--", path]);
    }
    let output = command.output().map_err(|error| format!("failed to spawn git log: {error}"))?;
    if !output.status.success() {
        return Err(git_failure("git log pickaxe", &output));
    }
    let commits = parse_log_records(&String::from_utf8_lossy(&output.stdout));
    Ok(json!({
        "query": query,
        "query_mode": if args.regex { "regex_diff" } else { "occurrence_change" },
        "path": args.path,
        "commits": commits,
        "interpretation": "A pickaxe hit means the query occurrence count or matching diff changed. Inspect each commit diff to determine whether it introduced or removed the behavior."
    }))
}

fn file_biography(repository: &str, args: &GitArchaeologyArgs, limit: usize) -> std::result::Result<Value, String> {
    let path = required_path(args)?;
    let count = format!("-{limit}");

    // 1. Creation commit (the earliest addition commit)
    let creation_raw = git_output(repository, [
        "log", "--follow", "--diff-filter=A", "--date=short", "--format=%H%x1f%ad%x1f%an%x1f%s", "--", &path,
    ]).unwrap_or_default();
    let creation = parse_log_records(&creation_raw).into_iter().last();

    // 2. Rename history
    let renames_raw = git_output(repository, [
        "log", "--follow", "--name-status", "--diff-filter=R", "--date=short", "--format=COMMIT%x1f%H%x1f%ad%x1f%an%x1f%s", "--", &path,
    ]).unwrap_or_default();
    let mut renames = Vec::new();
    let mut current_commit: Option<Value> = None;
    for line in renames_raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("COMMIT\u{1f}") {
            let mut parts = rest.split('\u{1f}');
            current_commit = Some(json!({
                "commit": parts.next().unwrap_or(""),
                "date": parts.next().unwrap_or(""),
                "author": parts.next().unwrap_or(""),
                "subject": parts.next().unwrap_or("")
            }));
        } else if (trimmed.starts_with('R') || trimmed.starts_with('r')) && trimmed.contains('\t') {
            let parts = trimmed.split('\t').collect::<Vec<_>>();
            if parts.len() >= 3 {
                renames.push(json!({
                    "from": parts[1],
                    "to": parts[2],
                    "commit": current_commit.clone()
                }));
            }
        }
    }

    // 3. Timeline of commits with shortstat
    let timeline_raw = git_output(repository, [
        "log", "--follow", "--date=short", "--shortstat", &count, "--format=%H%x1f%ad%x1f%an%x1f%s", "--", &path,
    ])?;

    let mut timeline = Vec::new();
    let mut pending_commit: Option<Value> = None;
    for line in timeline_raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.contains('\u{1f}') {
            if let Some(mut prev) = pending_commit.take() {
                if prev.get("stat").is_none() {
                    prev["stat"] = json!("");
                }
                timeline.push(prev);
            }
            let mut parts = trimmed.split('\u{1f}');
            pending_commit = Some(json!({
                "commit": parts.next().unwrap_or(""),
                "date": parts.next().unwrap_or(""),
                "author": parts.next().unwrap_or(""),
                "subject": parts.next().unwrap_or("")
            }));
        } else if let Some(mut commit) = pending_commit.take() {
            commit["stat"] = json!(trimmed);
            timeline.push(commit);
        }
    }
    if let Some(mut prev) = pending_commit.take() {
        if prev.get("stat").is_none() {
            prev["stat"] = json!("");
        }
        timeline.push(prev);
    }

    Ok(json!({
        "path": path,
        "creation": creation,
        "renames": renames,
        "timeline": timeline,
        "total_commits": timeline.len(),
        "method": "git log --follow --find-renames traces the file's recorded rename history."
    }))
}

fn commit_context(repository: &str, args: &GitArchaeologyArgs) -> std::result::Result<Value, String> {
    let commit = args.commit.as_deref().map(str::trim).filter(|value| !value.is_empty())
        .ok_or("`commit` is required for the commit_context operation.")?;
    let output = git_output(repository, [
        "show", "--no-ext-diff", "--date=short", "--format=fuller", "--stat", "--summary", commit,
    ])?;
    Ok(json!({
        "commit": commit,
        "evidence": bounded(&output),
        "note": "This is read-only commit metadata and change statistics. Use a focused Git show only after selecting a relevant commit."
    }))
}

fn required_path(args: &GitArchaeologyArgs) -> std::result::Result<String, String> {
    args.path.as_deref().map(str::trim).filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or("`path` is required for this operation.".into())
}

fn git_output<const N: usize>(repository: &str, args: [&str; N]) -> std::result::Result<String, String> {
    let output = git_command().current_dir(repository).args(args).output()
        .map_err(|error| format!("failed to spawn git: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(git_failure("git", &output))
    }
}

fn git_failure(command: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        format!("{command} exited with {}", output.status)
    } else {
        format!("{command} failed: {stderr}")
    }
}

fn bounded(value: &str) -> String {
    if value.len() <= MAX_OUTPUT_CHARS {
        value.trim().to_string()
    } else {
        let mut end = MAX_OUTPUT_CHARS;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}\n\n[output truncated to {MAX_OUTPUT_CHARS} characters]", value[..end].trim())
    }
}

fn parse_log_records(value: &str) -> Vec<Value> {
    value.lines().filter_map(|line| {
        let mut fields = line.split('\u{1f}');
        Some(json!({
            "commit": fields.next()?,
            "date": fields.next()?,
            "author": fields.next()?,
            "subject": fields.next()?
        }))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_log_records() {
        let records = parse_log_records("abc\u{1f}2026-09-11\u{1f}Ada\u{1f}feat: add search\n");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["commit"], "abc");
        assert_eq!(records[0]["subject"], "feat: add search");
    }

    #[test]
    fn runs_all_read_only_operations_against_repository() {
        let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_string_lossy()
            .to_string();

        let lineage = execute_sync(GitArchaeologyArgs {
            repository: repository.clone(),
            operation: "lineage".into(),
            path: Some("server/src/search/tgrep.rs".into()),
            start_line: Some(1),
            end_line: Some(1),
            query: None,
            regex: false,
            limit: None,
            commit: None,
        })
        .unwrap();
        assert_eq!(lineage["operation"], "lineage");
        assert!(lineage["result"]["evidence"].as_str().unwrap().contains("tgrep"));

        let pickaxe = execute_sync(GitArchaeologyArgs {
            repository: repository.clone(),
            operation: "pickaxe".into(),
            path: Some("server/src/search/tgrep.rs".into()),
            start_line: None,
            end_line: None,
            query: Some("tgrep".into()),
            regex: false,
            limit: Some(5),
            commit: None,
        })
        .unwrap();
        assert_eq!(pickaxe["operation"], "pickaxe");
        assert!(!pickaxe["result"]["commits"].as_array().unwrap().is_empty());

        let biography = execute_sync(GitArchaeologyArgs {
            repository: repository.clone(),
            operation: "file_biography".into(),
            path: Some("server/src/search/tgrep.rs".into()),
            start_line: None,
            end_line: None,
            query: None,
            regex: false,
            limit: Some(5),
            commit: None,
        })
        .unwrap();
        assert_eq!(biography["operation"], "file_biography");
        assert!(!biography["result"]["timeline"].as_array().unwrap().is_empty());
        assert!(biography["result"]["creation"]["subject"].as_str().unwrap().contains("tgrep"));

        let context = execute_sync(GitArchaeologyArgs {
            repository,
            operation: "commit_context".into(),
            path: None,
            start_line: None,
            end_line: None,
            query: None,
            regex: false,
            limit: None,
            commit: Some("HEAD".into()),
        })
        .unwrap();
        assert_eq!(context["operation"], "commit_context");
        assert!(context["result"]["evidence"].as_str().unwrap().contains("commit"));
    }
}
