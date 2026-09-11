use std::fs;
use std::time::Duration;
use tempfile::tempdir;

#[tokio::test]
async fn tgrep_freshness_tracks_create_modify_delete_and_rename() {
    let temp_dir = tempdir().unwrap();
    let repo_root = temp_dir.path();

    // Create a mock Cargo.toml to identify as repo root
    fs::write(repo_root.join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();

    let registry = cursor_server::search::TgrepRegistry::new();
    let configured_path: Option<&str> = None;

    // Check if tgrep is available on host
    if !cursor_server::search::tgrep::is_tgrep_available(configured_path) {
        eprintln!("tgrep not found on system, skipping freshness test");
        return;
    }

    // 1. Create file
    let file_path = repo_root.join("test_fresh.txt");
    fs::write(&file_path, "hello_unique_alpha_12345\n").unwrap();

    let args = serde_json::json!({
        "pattern": "unique_alpha_12345",
        "path": repo_root.to_string_lossy().to_string()
    });

    let outcome = cursor_server::search::tgrep::execute_tgrep(&args, configured_path).await;
    assert!(outcome.is_ok(), "tgrep should execute successfully");
    let result = outcome.unwrap();
    assert!(
        result.contains("unique_alpha_12345"),
        "must find newly created file content"
    );

    // 2. Modify file
    fs::write(&file_path, "hello_unique_beta_67890\n").unwrap();

    let args_beta = serde_json::json!({
        "pattern": "unique_beta_67890",
        "path": repo_root.to_string_lossy().to_string()
    });
    let outcome_beta = cursor_server::search::tgrep::execute_tgrep(&args_beta, configured_path).await;
    assert!(outcome_beta.is_ok());
    assert!(
        outcome_beta.unwrap().contains("unique_beta_67890"),
        "must find modified content"
    );

    let outcome_alpha_old = cursor_server::search::tgrep::execute_tgrep(&args, configured_path).await;
    assert!(outcome_alpha_old.is_ok());
    assert!(
        outcome_alpha_old.unwrap().contains("No matches found"),
        "old content must return NoMatch"
    );

    // 3. Rename file
    let renamed_path = repo_root.join("test_renamed.txt");
    fs::rename(&file_path, &renamed_path).unwrap();

    let args_renamed = serde_json::json!({
        "pattern": "unique_beta_67890",
        "path": repo_root.to_string_lossy().to_string(),
        "output_mode": "files_with_matches"
    });
    let outcome_renamed = cursor_server::search::tgrep::execute_tgrep(&args_renamed, configured_path).await;
    assert!(outcome_renamed.is_ok());
    let renamed_str = outcome_renamed.unwrap();
    assert!(
        renamed_str.contains("test_renamed"),
        "must reflect renamed file, got: {renamed_str}"
    );

    // 4. Delete file
    fs::remove_file(&renamed_path).unwrap();
    let outcome_deleted = cursor_server::search::tgrep::execute_tgrep(&args_beta, configured_path).await;
    assert!(outcome_deleted.is_ok());
    assert!(
        outcome_deleted.unwrap().contains("No matches found"),
        "deleted file must return NoMatch"
    );

    // 5. Ensure server and test idle reaper
    let _readiness = registry.ensure_server(repo_root, configured_path).await;
    assert_eq!(registry.active_count().await, 1);

    // Reap with 0 duration to simulate elapsed idle timeout
    let reaped = registry.reap_idle(Duration::from_millis(0)).await;
    assert_eq!(reaped, 1);
    assert_eq!(registry.active_count().await, 0);

    registry.shutdown().await;
}

#[tokio::test]
async fn tgrep_registry_rejects_stale_serve_json_with_mismatched_pid() {
    let temp_dir = tempdir().unwrap();
    let repo_root = temp_dir.path();
    fs::write(repo_root.join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();

    // Create a stale serve.json with fake PID
    let tgrep_dir = repo_root.join(".tgrep");
    fs::create_dir_all(&tgrep_dir).unwrap();
    fs::write(
        tgrep_dir.join("serve.json"),
        r#"{"pid": 9999999, "port": 54321}"#,
    )
    .unwrap();

    let registry = cursor_server::search::TgrepRegistry::new();
    let readiness = registry.ensure_server(repo_root, None).await;

    // Should NOT adopt the fake port 54321, readiness will be Starting / Indexing / Ready from spawned child or Unhealthy if no binary
    assert_ne!(
        readiness,
        cursor_server::search::ServerReadiness::Unhealthy,
        "spawned process should not crash"
    );

    registry.shutdown().await;
}
