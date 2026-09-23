//! High-performance trigram-indexed search adapter using Microsoft tgrep.
use serde_json::Value;
use std::path::{Path, PathBuf};

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

/// Returns true if `path` is a user home directory, root drive, or system root directory.
pub fn is_user_home_or_system_root(path: &Path) -> bool {
    if path.parent().is_none() {
        return true;
    }
    if let Some(home) = dirs::home_dir() {
        if path == home {
            return true;
        }
        if let (Ok(canonical_path), Ok(canonical_home)) = (path.canonicalize(), home.canonicalize()) {
            if canonical_path == canonical_home {
                return true;
            }
        }
    }
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        let profile_path = Path::new(&userprofile);
        if path == profile_path {
            return true;
        }
        if let (Ok(canonical_path), Ok(canonical_profile)) = (path.canonicalize(), profile_path.canonicalize()) {
            if canonical_path == canonical_profile {
                return true;
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let home_path = Path::new(&home);
        if path == home_path {
            return true;
        }
        if let (Ok(canonical_path), Ok(canonical_home)) = (path.canonicalize(), home_path.canonicalize()) {
            if canonical_path == canonical_home {
                return true;
            }
        }
    }

    let path_str = path.to_string_lossy();
    if path_str.eq_ignore_ascii_case("C:\\Windows")
        || path_str.eq_ignore_ascii_case("C:\\Program Files")
        || path_str.eq_ignore_ascii_case("C:\\Program Files (x86)")
        || path_str.eq_ignore_ascii_case("C:\\ProgramData")
        || path_str.eq_ignore_ascii_case("C:\\Users")
        || path_str.eq_ignore_ascii_case("/home")
        || path_str.eq_ignore_ascii_case("/Users")
        || path_str.eq_ignore_ascii_case("/etc")
        || path_str.eq_ignore_ascii_case("/var")
        || path_str.eq_ignore_ascii_case("/usr")
    {
        return true;
    }

    false
}

/// Returns true if `path` is a user home root or a direct system/config boundary folder under user home.
pub fn is_home_or_config_boundary(path: &Path) -> bool {
    if is_user_home_or_system_root(path) {
        return true;
    }

    if let Some(home) = dirs::home_dir() {
        for config_name in [
            "AppData",
            "Local Settings",
            ".cursor",
            ".cursor-byok-v3",
            ".agents",
            ".cache",
            ".npm",
            ".cargo",
            ".rustup",
            ".vscode",
            ".config",
        ] {
            let config_path = home.join(config_name);
            if path == config_path {
                return true;
            }
            if let (Ok(p), Ok(c)) = (path.canonicalize(), config_path.canonicalize()) {
                if p == c {
                    return true;
                }
            }
        }
    }

    false
}

/// Checks if a directory qualifies as a project repository root that is safe to index with `tgrep serve`.
pub fn is_indexable_repo_root(path: &Path) -> bool {
    if is_home_or_config_boundary(path) {
        return false;
    }
    path.join(".git").exists()
        || path.join("Cargo.toml").exists()
        || path.join("package.json").exists()
        || path.join("go.mod").exists()
        || path.join("pyproject.toml").exists()
}

/// Finds the root directory of the repository/project for a given path.
pub fn find_repo_root(path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let candidate_path = if path.is_dir() {
        path.clone()
    } else {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.clone())
    };

    let mut current = candidate_path.as_path();

    while let Some(parent) = current.parent() {
        if is_home_or_config_boundary(current) {
            break;
        }
        if current.join(".git").exists()
            || current.join("Cargo.toml").exists()
            || current.join("package.json").exists()
            || current.join("go.mod").exists()
            || current.join("pyproject.toml").exists()
        {
            return current.to_path_buf();
        }
        current = parent;
    }

    if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
    }
}

/// Checks if `tgrep` is available on the machine.
pub fn is_tgrep_available(configured_path: Option<&str>) -> bool {
    resolve_tgrep_binary(configured_path).is_some()
}

fn tgrep_command(binary: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(binary);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd
}

/// Executes a search using `tgrep` and returns a typed outcome for engine routing.
pub(crate) async fn execute_tgrep_outcome(
    arguments: &Value,
    configured_path: Option<&str>,
    force_no_index: bool,
) -> crate::search::TgrepOutcome {
    if let Err(failure) = validate_tgrep_arguments(arguments) {
        return crate::search::TgrepOutcome::Failure(failure);
    }

    let binary = match resolve_tgrep_binary(configured_path) {
        Some(binary) => binary,
        None => {
            return crate::search::TgrepOutcome::Failure(
                crate::search::TgrepFailure::Unavailable,
            )
        }
    };

    let pattern = arguments
        .get("pattern")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let target_path = match tgrep_workspace_path(arguments) {
        Ok(path) => path,
        Err(failure) => return crate::search::TgrepOutcome::Failure(failure),
    };
    let target_path_str = target_path.to_string_lossy().to_string();
    let repo_root = find_repo_root(&target_path);
    let mut args: Vec<String> = vec!["--color".into(), "never".into(), "--no-heading".into()];
    if force_no_index || !is_indexable_repo_root(&repo_root) {
        args.push("--no-index".into());
    } else {
        let index_dir = repo_root.join(".tgrep");
        if index_dir.exists() {
            args.extend(["--index-path".into(), index_dir.to_string_lossy().into()]);
        }
    }

    let output_mode = arguments
        .get("output_mode")
        .and_then(Value::as_str)
        .unwrap_or("content");
    match output_mode {
        "files_with_matches" => args.push("-l".into()),
        "count" => args.push("-c".into()),
        _ => args.extend(["-n".into(), "-H".into()]),
    }
    if arguments.get("-i").and_then(Value::as_bool).unwrap_or(false) {
        args.push("-i".into());
    }
    for (key, flag) in [("-B", "-B"), ("-A", "-A"), ("-C", "-C")] {
        if let Some(value) = arguments.get(key).and_then(Value::as_u64) {
            args.extend([flag.into(), value.to_string()]);
        }
    }
    if let Some(glob) = arguments
        .get("glob")
        .or_else(|| arguments.get("glob_pattern"))
        .and_then(Value::as_str)
    {
        args.extend(["-g".into(), glob.into()]);
    }
    if arguments.get("multiline").and_then(Value::as_bool).unwrap_or(false) {
        args.push("-U".into());
    }
    if let Some(file_type) = arguments
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        args.extend(["-t".into(), file_type.into()]);
    }
    if let Some(limit) = arguments.get("head_limit").and_then(Value::as_u64) {
        args.extend(["-m".into(), limit.to_string()]);
    }
    if !pattern.is_empty() {
        args.push(pattern.into());
    } else if output_mode == "files_with_matches" {
        args.push(".*".into());
    }
    args.push(target_path_str);

    let output = match tgrep_command(&binary).args(&args).output().await {
        Ok(output) => output,
        Err(error) => {
            return crate::search::TgrepOutcome::Failure(
                crate::search::TgrepFailure::Infrastructure {
                    reason: format!("failed to spawn tgrep: {error}"),
                },
            )
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    match output.status.code() {
        Some(0) if stdout.is_empty() => crate::search::TgrepOutcome::NoMatch,
        Some(0) => crate::search::TgrepOutcome::Match(stdout),
        Some(1) => crate::search::TgrepOutcome::NoMatch,
        code => crate::search::TgrepOutcome::Failure(crate::search::TgrepFailure::Infrastructure {
            reason: if stderr.is_empty() {
                format!("tgrep exited with code {code:?}")
            } else {
                format!("tgrep error: {stderr}")
            },
        }),
    }
}

/// Returns the workspace path to search, resolving relative paths to absolute paths.
pub(crate) fn tgrep_workspace_path(
    arguments: &Value,
) -> std::result::Result<PathBuf, crate::search::TgrepFailure> {
    let path_str = arguments
        .get("path")
        .or_else(|| arguments.get("target_directory"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .unwrap_or(".");

    let path = Path::new(path_str);
    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|dir| dir.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };

    Ok(absolute_path)
}

fn validate_tgrep_arguments(
    arguments: &Value,
) -> std::result::Result<(), crate::search::TgrepFailure> {
    let invalid = |reason: &str| crate::search::TgrepFailure::InvalidRequest {
        reason: reason.into(),
    };
    if arguments
        .get("pattern")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(invalid("pattern must be non-empty"));
    }
    if let Some(value) = arguments.get("type") {
        if !value.is_null() {
            if let Some(file_type) = value.as_str() {
                let trimmed = file_type.trim();
                if !trimmed.is_empty()
                    && !trimmed
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '+')
                {
                    return Err(invalid(
                        "type must be a valid file type identifier (e.g. js, py, rust)",
                    ));
                }
            } else {
                return Err(invalid("type must be a string if provided"));
            }
        }
    }
    if arguments
        .get("offset")
        .and_then(Value::as_u64)
        .is_some_and(|offset| offset > 0)
    {
        return Err(crate::search::TgrepFailure::UnsupportedRequest {
            reason: "offset is not supported by tgrep".into(),
        });
    }
    if arguments.get("sort").is_some() || arguments.get("sort_ascending").is_some() {
        return Err(crate::search::TgrepFailure::UnsupportedRequest {
            reason: "sort options are not supported by tgrep".into(),
        });
    }
    for key in ["-A", "-B", "-C", "head_limit", "offset"] {
        if let Some(value) = arguments.get(key) {
            if value.as_u64().is_none() {
                return Err(invalid(&format!("{key} must be a non-negative integer")));
            }
        }
    }
    Ok(())
}

/// Compatibility wrapper for callers that still consume string output.
pub async fn execute_tgrep(
    arguments: &Value,
    configured_path: Option<&str>,
) -> std::result::Result<String, String> {
    match execute_tgrep_outcome(arguments, configured_path, false).await {
        crate::search::TgrepOutcome::Match(output) => Ok(output),
        crate::search::TgrepOutcome::NoMatch => {
            let pattern = arguments.get("pattern").and_then(Value::as_str).unwrap_or_default();
            let path = arguments.get("path").and_then(Value::as_str).unwrap_or(".");
            Ok(format!("No matches found for pattern `{pattern}` in {path}"))
        }
        crate::search::TgrepOutcome::Failure(failure) => Err(failure.to_string()),
    }
}

impl std::fmt::Display for crate::search::TgrepFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(formatter, "tgrep binary not found on system PATH or configured path"),
            Self::UnsupportedRequest { reason }
            | Self::Infrastructure { reason }
            | Self::InvalidRequest { reason } => formatter.write_str(reason),
        }
    }
}

/* legacy implementation removed: execute_tgrep_outcome is the single command adapter. */
/*

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

    let mut command = tgrep_command(&binary);
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

*/

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
            root.join("Cargo.toml").exists(),
            "Resolved root should contain Cargo.toml"
        );
    }

    #[test]
    fn user_home_is_never_an_indexable_repo_root() {
        if let Some(home) = dirs::home_dir() {
            assert!(is_user_home_or_system_root(&home));
            assert!(!is_indexable_repo_root(&home));
            let root = find_repo_root(&home);
            assert!(!is_indexable_repo_root(&root), "find_repo_root must not return an indexable root for home directory");

            let subpath = home.join(".cursor").join("projects");
            let sub_root = find_repo_root(&subpath);
            assert!(!is_indexable_repo_root(&sub_root), "subdirectories in home must not be treated as indexable repo roots");
        }
    }

    #[test]
    fn blacklisted_locations_are_never_indexable() {
        if let Some(home) = dirs::home_dir() {
            for config_name in ["AppData", ".cursor", ".agents", ".vscode", ".cache", ".npm", ".cargo"] {
                let path = home.join(config_name);
                assert!(
                    is_home_or_config_boundary(&path),
                    "boundary {config_name} must be recognized"
                );
                assert!(
                    !is_indexable_repo_root(&path),
                    "boundary {config_name} must not be indexable"
                );
            }
        }
    }

    #[test]
    fn root_drive_is_never_an_indexable_repo_root() {
        let drive = Path::new(if cfg!(windows) { "C:\\" } else { "/" });
        assert!(is_user_home_or_system_root(drive));
        assert!(!is_indexable_repo_root(drive));
    }

    #[test]
    fn tgrep_workspace_path_resolves_missing_or_relative_paths() {
        for arguments in [json!({"pattern": "test"}), json!({"path": "."})] {
            let path = tgrep_workspace_path(&arguments).unwrap();
            assert!(path.is_absolute());
        }
    }

    #[test]
    fn tgrep_workspace_path_accepts_an_absolute_path() {
        let directory = tempfile::tempdir().unwrap();
        let arguments = json!({"path": directory.path()});

        assert_eq!(
            tgrep_workspace_path(&arguments).unwrap(),
            directory.path()
        );
    }

    #[tokio::test]
    async fn tgrep_allows_empty_type() {
        let args = json!({
            "pattern": "test",
            "type": ""
        });
        let result = execute_tgrep_outcome(&args, Some("missing-tgrep.exe"), false).await;
        assert!(matches!(
            result,
            crate::search::TgrepOutcome::Failure(crate::search::TgrepFailure::Unavailable)
        ));
    }

    #[tokio::test]
    async fn tgrep_rejects_invalid_type_syntax() {
        let args = json!({
            "pattern": "test",
            "type": "invalid type; name!"
        });
        let result = execute_tgrep_outcome(&args, Some("missing-tgrep.exe"), false).await;
        assert!(matches!(
            result,
            crate::search::TgrepOutcome::Failure(
                crate::search::TgrepFailure::InvalidRequest { .. }
            )
        ));
    }

    #[tokio::test]
    async fn tgrep_rejects_offset_before_binary_lookup() {
        let args = json!({
            "pattern": "test",
            "offset": 1
        });
        let result = execute_tgrep_outcome(&args, Some("missing-tgrep.exe"), false).await;
        assert!(matches!(
            result,
            crate::search::TgrepOutcome::Failure(
                crate::search::TgrepFailure::UnsupportedRequest { .. }
            )
        ));
    }

    #[tokio::test]
    async fn tgrep_reports_unavailable_as_typed_failure() {
        let args = json!({"pattern": "test"});
        let result = execute_tgrep_outcome(&args, Some("missing-tgrep.exe"), false).await;
        assert!(matches!(
            result,
            crate::search::TgrepOutcome::Failure(crate::search::TgrepFailure::Unavailable)
        ));
    }

    #[tokio::test]
    async fn tgrep_rejects_empty_pattern() {
        let args = json!({"pattern": ""});
        let result = execute_tgrep_outcome(&args, Some("missing-tgrep.exe"), false).await;
        assert!(matches!(
            result,
            crate::search::TgrepOutcome::Failure(
                crate::search::TgrepFailure::InvalidRequest { .. }
            )
        ));
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
