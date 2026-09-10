//! High-performance trigram-indexed search adapter using Microsoft tgrep.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use serde_json::Value;

// Track repos currently being indexed to avoid duplicate concurrent indexing builds
static INDEXING_REPOS: std::sync::OnceLock<Arc<Mutex<HashSet<PathBuf>>>> =
    std::sync::OnceLock::new();

fn indexing_set() -> &'static Arc<Mutex<HashSet<PathBuf>>> {
    INDEXING_REPOS.get_or_init(|| Arc::new(Mutex::new(HashSet::new())))
}

/// Resolves the absolute path to the `tgrep` binary on the host machine.
pub fn resolve_tgrep_binary(configured_path: Option<&str>) -> Option<PathBuf> {
    if let Some(custom) = configured_path.map(str::trim).filter(|s| !s.is_empty()) {
        let path = PathBuf::from(custom);
        if path.is_file() {
            return Some(path);
        }
        return None;
    }

    // Common search locations
    let binary_name = if cfg!(windows) { "tgrep.exe" } else { "tgrep" };

    // 1. Check next to current running executable (for packaged exe / desktop app)
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let direct = exe_dir.join(binary_name);
            if direct.is_file() {
                return Some(direct);
            }
            let in_bin = exe_dir.join("bin").join(binary_name);
            if in_bin.is_file() {
                return Some(in_bin);
            }
            let in_resources = exe_dir.join("resources").join(binary_name);
            if in_resources.is_file() {
                return Some(in_resources);
            }
        }
    }

    // 2. Check application managed data directory (~/.cursor-byok-v3/bin/tgrep.exe)
    if let Ok(data_dir) = crate::config::managed_data_dir() {
        let candidate = data_dir.join("bin").join(binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // 3. Check workspace parent directories (e.g. C:\PROJECTS\cursor-byok\tgrep.exe)
    if let Ok(current_dir) = std::env::current_dir() {
        let mut check_dir = Some(current_dir.as_path());
        while let Some(dir) = check_dir {
            let candidate = dir.join(binary_name);
            if candidate.is_file() {
                return Some(candidate);
            }
            let sub_bin = dir.join("bin").join(binary_name);
            if sub_bin.is_file() {
                return Some(sub_bin);
            }
            let support_bin = dir.join("support").join("bin").join(binary_name);
            if support_bin.is_file() {
                return Some(support_bin);
            }
            check_dir = dir.parent();
        }
    }

    // 4. Check system PATH
    if let Some(path_var) = std::env::var_os("PATH") {
        for split in std::env::split_paths(&path_var) {
            let candidate = split.join(binary_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Finds the root directory of the repository/project for a given path.
pub fn find_repo_root(path: &Path) -> PathBuf {
    let candidate_path = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().map(Path::to_path_buf).unwrap_or_else(|| path.to_path_buf())
    };

    let mut current = candidate_path.as_path();
    let mut found_root = None;

    while let Some(parent) = current.parent() {
        if current.join(".git").exists()
            || current.join(".tgrep").exists()
            || current.join("Cargo.toml").exists()
            || current.join("package.json").exists()
        {
            found_root = Some(current.to_path_buf());
            // Continue up to find outermost repo if nested
        }
        current = parent;
    }

    found_root.unwrap_or_else(|| {
        if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
        }
    })
}

/// Checks if `tgrep` is available on the machine.
pub fn is_tgrep_available(configured_path: Option<&str>) -> bool {
    resolve_tgrep_binary(configured_path).is_some()
}

/// Automatically builds the index in the background for a repository if missing.
pub async fn auto_ensure_index(root: &Path, configured_path: Option<&str>) {
    let index_dir = root.join(".tgrep");
    if index_dir.exists() {
        return;
    }

    let Some(binary) = resolve_tgrep_binary(configured_path) else {
        return;
    };

    let root_buf = root.to_path_buf();
    let set = indexing_set();
    let mut lock = set.lock().await;
    if lock.contains(&root_buf) {
        return;
    }
    lock.insert(root_buf.clone());
    drop(lock);

    let set_clone = set.clone();
    tokio::spawn(async move {
        tracing::info!(repo = ?root_buf, "tgrep: building trigram index in background");
        let _ = tokio::process::Command::new(binary)
            .args(["index", root_buf.to_string_lossy().as_ref()])
            .output()
            .await;
        let mut lock = set_clone.lock().await;
        lock.remove(&root_buf);
        tracing::info!(repo = ?root_buf, "tgrep: trigram index build finished");
    });
}

/// Executes a search using `tgrep` with arguments matching Cursor's Grep tool schema.
pub async fn execute_tgrep(
    arguments: &Value,
    configured_path: Option<&str>,
) -> std::result::Result<String, String> {
    let binary = resolve_tgrep_binary(configured_path)
        .ok_or_else(|| "tgrep binary not found on system PATH or configured path".to_string())?;

    let pattern = arguments
        .get("pattern")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let target_path_str = arguments
        .get("path")
        .or_else(|| arguments.get("target_directory"))
        .and_then(Value::as_str)
        .unwrap_or(".");

    let target_path = Path::new(target_path_str);
    let repo_root = find_repo_root(target_path);

    // Auto-trigger background indexing if not indexed yet
    auto_ensure_index(&repo_root, configured_path).await;

    let mut args: Vec<String> = Vec::new();

    // Color and output flags
    args.push("--color".into());
    args.push("never".into());
    args.push("--no-heading".into());

    // If an index exists at repo root, supply --index-path so subdirectories use the parent index
    let index_dir = repo_root.join(".tgrep");
    if index_dir.exists() {
        args.push("--index-path".into());
        args.push(index_dir.to_string_lossy().to_string());
    }

    let output_mode = arguments
        .get("output_mode")
        .and_then(Value::as_str)
        .unwrap_or("content");

    match output_mode {
        "files_with_matches" => {
            args.push("-l".into());
        }
        "count" => {
            args.push("-c".into());
        }
        _ => {
            args.push("-n".into());
            args.push("-H".into());
        }
    }

    // Case insensitive
    if arguments.get("-i").and_then(Value::as_bool).unwrap_or(false) {
        args.push("-i".into());
    }

    // Context lines
    if let Some(before) = arguments.get("-B").and_then(Value::as_u64) {
        args.push("-B".into());
        args.push(before.to_string());
    }
    if let Some(after) = arguments.get("-A").and_then(Value::as_u64) {
        args.push("-A".into());
        args.push(after.to_string());
    }
    if let Some(context) = arguments.get("-C").and_then(Value::as_u64) {
        args.push("-C".into());
        args.push(context.to_string());
    }

    // Glob filter
    if let Some(glob) = arguments.get("glob").or_else(|| arguments.get("glob_pattern")).and_then(Value::as_str) {
        args.push("-g".into());
        args.push(glob.to_string());
    }

    // Multiline
    if arguments.get("multiline").and_then(Value::as_bool).unwrap_or(false) {
        args.push("-U".into());
    }

    // File type
    if let Some(file_type) = arguments.get("type").and_then(Value::as_str) {
        args.push("-t".into());
        args.push(file_type.to_string());
    }

    // Head limit
    if let Some(limit) = arguments.get("head_limit").and_then(Value::as_u64) {
        args.push("-m".into());
        args.push(limit.to_string());
    }

    // Pattern (if empty, treat as matching all or files listing)
    if !pattern.is_empty() {
        args.push(pattern.to_string());
    } else if output_mode == "files_with_matches" {
        args.push(".*".into());
    }

    // Search target path
    args.push(target_path_str.to_string());

    let mut command = tokio::process::Command::new(binary);
    command.args(&args);

    let output = command
        .output()
        .await
        .map_err(|error| format!("failed to spawn tgrep: {error}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    match output.status.code() {
        Some(0) => {
            if stdout.is_empty() {
                Ok(format!("No matches found for pattern `{pattern}` in {target_path_str}"))
            } else {
                Ok(stdout)
            }
        }
        Some(1) => {
            // Code 1 means no match found in grep/tgrep
            Ok(format!("No matches found for pattern `{pattern}` in {target_path_str}"))
        }
        _ => {
            if !stdout.is_empty() {
                Ok(stdout)
            } else if !stderr.is_empty() {
                Err(format!("tgrep error: {stderr}"))
            } else {
                Err(format!("tgrep exited with code {:?}", output.status.code()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tgrep_binary_resolves_if_present() {
        let binary = resolve_tgrep_binary(None);
        assert!(binary.is_some(), "tgrep binary should be resolved in the workspace parent");
    }

    #[test]
    fn tgrep_find_repo_root_detects_project_root() {
        let current_file = Path::new("src/search/tgrep.rs");
        let root = find_repo_root(current_file);
        assert!(root.exists(), "Resolved root must exist");
        assert!(
            root.join("Cargo.toml").exists() || root.join(".tgrep").exists(),
            "Resolved root should contain Cargo.toml or .tgrep"
        );
    }

    #[tokio::test]
    async fn tgrep_executes_search_successfully() {
        let args = json!({
            "pattern": "tgrep_binary_resolves_if_present",
            "path": "src/search/tgrep.rs",
            "-i": true
        });
        let result = execute_tgrep(&args, None).await;
        assert!(result.is_ok(), "tgrep execution should succeed: {:?}", result.err());
        let output = result.unwrap();
        assert!(output.contains("tgrep_binary_resolves_if_present"));
    }

    #[tokio::test]
    async fn tgrep_executes_files_with_matches_output_mode() {
        let args = json!({
            "pattern": "tgrep_binary_resolves_if_present",
            "path": "src/search/tgrep.rs",
            "output_mode": "files_with_matches"
        });
        let result = execute_tgrep(&args, None).await;
        assert!(result.is_ok(), "tgrep files_with_matches should succeed");
        let output = result.unwrap();
        assert!(output.contains("tgrep.rs"));
    }

    #[tokio::test]
    async fn tgrep_executes_count_output_mode() {
        let args = json!({
            "pattern": "pub fn resolve_tgrep_binary",
            "path": "src/search/tgrep.rs",
            "output_mode": "count"
        });
        let result = execute_tgrep(&args, None).await;
        assert!(result.is_ok(), "tgrep count should succeed");
        let output = result.unwrap();
        assert!(
            output.trim().parse::<usize>().is_ok() || output.contains("tgrep.rs"),
            "count output should be a number or file:count: {output}"
        );
    }

    #[tokio::test]
    async fn tgrep_executes_context_lines_search() {
        let args = json!({
            "pattern": "pub fn resolve_tgrep_binary",
            "path": "src/search/tgrep.rs",
            "-C": 2
        });
        let result = execute_tgrep(&args, None).await;
        assert!(result.is_ok(), "tgrep context lines search should succeed");
        let output = result.unwrap();
        assert!(output.contains("pub fn resolve_tgrep_binary"));
        assert!(output.contains("-")); // Context separator
    }

    #[tokio::test]
    async fn tgrep_executes_glob_filter() {
        let args = json!({
            "pattern": "resolve_tgrep_binary",
            "path": "src/search",
            "glob": "*.rs"
        });
        let result = execute_tgrep(&args, None).await;
        assert!(result.is_ok(), "tgrep glob filter should succeed");
        let output = result.unwrap();
        assert!(output.contains("tgrep.rs"));
    }

    #[tokio::test]
    async fn tgrep_returns_canonical_message_when_no_match() {
        let unique_pattern = format!("__NO_MATCH_{}_PATTERN__", std::process::id());
        let args = json!({
            "pattern": unique_pattern,
            "path": "src/search/tgrep.rs"
        });
        let result = execute_tgrep(&args, None).await;
        assert!(result.is_ok(), "tgrep no-match should succeed gracefully");
        let output = result.unwrap();
        assert!(
            output.contains("No matches found for pattern"),
            "Expected canonical no match message, got: {output}"
        );
    }

    #[tokio::test]
    async fn tgrep_handles_missing_binary_gracefully() {
        let args = json!({
            "pattern": "test",
            "path": "src/search/tgrep.rs"
        });
        let result = execute_tgrep(&args, Some("non_existent_path/fake_tgrep.exe")).await;
        assert!(result.is_err(), "Should return error when binary is missing");
        let err = result.unwrap_err();
        assert!(err.contains("not found"));
    }
}
