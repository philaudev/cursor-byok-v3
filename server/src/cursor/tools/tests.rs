use super::*;
use serde_json::json;

fn edit_call(index: usize, call_id: &str, path: &str, old: &str, new: &str) -> ToolCall {
    ToolCall {
        index,
        call_id: call_id.into(),
        model_call_id: "model:0".into(),
        name: "StrReplace".into(),
        arguments_text: String::new(),
        arguments: json!({
            "path": path,
            "old_string": old,
            "new_string": new,
        }),
    }
}

#[tokio::test]
async fn same_path_edits_start_one_at_a_time() {
    let runtime = CursorToolRuntime::default();
    let dispatcher = ToolDispatcher::new(runtime.clone());
    let calls = [
        edit_call(0, "first", "/tmp/a.txt", "left", "LEFT"),
        edit_call(1, "second", "/tmp/a.txt", "right", "RIGHT"),
        edit_call(2, "other", "/tmp/b.txt", "other", "OTHER"),
    ];

    let dispatched = dispatcher
        .start_batch(
            &calls,
            ToolBatchState {
                completed: &HashSet::new(),
                started: &HashSet::new(),
                response_text: "",
                response_thinking: "",
            },
            &[],
            &BTreeMap::new(),
            &ExecContext::default(),
        )
        .await
        .unwrap();

    assert_eq!(dispatched.len(), 2);
    assert_eq!(exec(&dispatched[0]).exec_id, "first");
    assert_eq!(exec(&dispatched[1]).exec_id, "other");

    let mut file = "left right\n".to_string();
    let first_write = advance_read(&runtime, exec(&dispatched[0]).id, &file).await;
    file = write_text(&first_write);
    assert_eq!(file, "LEFT right\n");
    complete_write(&runtime, &first_write).await;

    let second = dispatcher
        .continue_after("first")
        .await
        .unwrap()
        .expect("second same-path edit should start after the first completes");
    assert_eq!(exec(&second).exec_id, "second");
    let second_write = advance_read(&runtime, exec(&second).id, &file).await;
    file = write_text(&second_write);
    assert_eq!(file, "LEFT RIGHT\n");
    complete_write(&runtime, &second_write).await;
    assert!(dispatcher.continue_after("second").await.unwrap().is_none());
}

fn exec(dispatched: &DispatchedTool) -> &pb::ExecServerMessage {
    dispatched
        .messages
        .iter()
        .find_map(|message| match message.message.as_ref() {
            Some(pb::agent_server_message::Message::ExecServerMessage(exec)) => Some(exec),
            _ => None,
        })
        .expect("dispatched edit should contain an Exec request")
}

async fn advance_read(
    runtime: &CursorToolRuntime,
    id: u32,
    content: &str,
) -> pb::ExecServerMessage {
    let event = codec::client_event(
        &pb::ExecClientMessage {
            id,
            message: Some(pb::exec_client_message::Message::ReadResult(
                pb::ReadResult {
                    result: Some(pb::read_result::Result::Success(pb::ReadSuccess {
                        output: Some(pb::read_success::Output::Content(content.into())),
                        ..Default::default()
                    })),
                },
            )),
            ..Default::default()
        },
        runtime,
    )
    .await
    .unwrap();
    let codec::ClientExecEvent::Message(message) = event else {
        panic!("edit read should advance to a write")
    };
    let Some(pb::agent_server_message::Message::ExecServerMessage(exec)) = message.message else {
        panic!("edit read should emit an Exec write request")
    };
    exec
}

fn write_text(exec: &pb::ExecServerMessage) -> String {
    let Some(pb::exec_server_message::Message::WriteArgs(args)) = exec.message.as_ref() else {
        panic!("expected WriteArgs")
    };
    args.file_text.clone()
}

async fn complete_write(runtime: &CursorToolRuntime, exec: &pb::ExecServerMessage) {
    let Some(pb::exec_server_message::Message::WriteArgs(args)) = exec.message.as_ref() else {
        panic!("expected WriteArgs")
    };
    let event = codec::client_event(
        &pb::ExecClientMessage {
            id: exec.id,
            message: Some(pb::exec_client_message::Message::WriteResult(
                pb::WriteResult {
                    result: Some(pb::write_result::Result::Success(pb::WriteSuccess {
                        path: args.path.clone(),
                        ..Default::default()
                    })),
                },
            )),
            ..Default::default()
        },
        runtime,
    )
    .await
    .unwrap();
    assert!(matches!(event, codec::ClientExecEvent::Completed(_)));
}

#[tokio::test]
async fn test_inspect_changes_non_git_workspace() {
    let temp_dir = tempfile::tempdir().unwrap();
    let non_git_dir = temp_dir.path().join("nongit");
    std::fs::create_dir_all(&non_git_dir).unwrap();

    let runtime = CursorToolRuntime::default();
    let (results, mut rx) = result::tool_result_channel();
    let call = ToolCall {
        index: 0,
        call_id: "inspect_non_git".into(),
        model_call_id: "model:0".into(),
        name: "InspectChanges".into(),
        arguments_text: String::new(),
        arguments: json!({
            "path": non_git_dir.to_str().unwrap()
        }),
    };

    let context = ExecContext::default();

    dispatch::start(
        &runtime,
        &results,
        &call,
        0,
        &BTreeMap::new(),
        &context,
        None,
    )
    .await
    .unwrap();

    let completion = rx.recv().await.unwrap().expect("should receive completion");
    let content = completion.result().content.clone();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(val["is_git_repo"], false);
    assert_eq!(val["path"], non_git_dir.to_str().unwrap());
    assert!(val["message"]
        .as_str()
        .unwrap()
        .contains("is not a git repository"));
}

#[tokio::test]
async fn test_inspect_changes_git_workflow() {
    let temp_dir = tempfile::tempdir().unwrap();
    let repo_dir = temp_dir.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();

    // Initialize git repo
    let init = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "init"])
        .output()
        .unwrap();
    if !init.status.success() {
        return; // Git not installed or accessible in test environment
    }
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.name", "Test"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.email", "test@test.com"])
        .output();

    // 1. Clean repo test
    let runtime = CursorToolRuntime::default();
    let (results, mut rx) = result::tool_result_channel();
    let call = ToolCall {
        index: 0,
        call_id: "inspect_clean".into(),
        model_call_id: "model:0".into(),
        name: "InspectChanges".into(),
        arguments_text: String::new(),
        arguments: json!({
            "path": repo_dir.to_str().unwrap()
        }),
    };

    let context = ExecContext::default();

    dispatch::start(
        &runtime,
        &results,
        &call,
        0,
        &BTreeMap::new(),
        &context,
        None,
    )
    .await
    .unwrap();

    let completion = rx.recv().await.unwrap().unwrap();
    let val: serde_json::Value = serde_json::from_str(&completion.result().content).unwrap();
    assert_eq!(val["is_git_repo"], true);
    assert_eq!(val["has_changes"], false);
    assert!(val.get("branch").is_some());

    // 2. Add modified & untracked files
    let file_a = repo_dir.join("a.txt");
    let file_b = repo_dir.join("b.txt");
    std::fs::write(&file_a, "initial content\n").unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "add", "a.txt"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "commit", "-m", "init"])
        .output();

    // Modify a.txt and create untracked b.txt
    std::fs::write(&file_a, "modified content\n").unwrap();
    std::fs::write(&file_b, "new file content\n").unwrap();

    let (results2, mut rx2) = result::tool_result_channel();
    let call2 = ToolCall {
        index: 0,
        call_id: "inspect_all".into(),
        model_call_id: "model:0".into(),
        name: "InspectChanges".into(),
        arguments_text: String::new(),
        arguments: json!({
            "path": repo_dir.to_str().unwrap()
        }),
    };

    dispatch::start(
        &runtime,
        &results2,
        &call2,
        0,
        &BTreeMap::new(),
        &context,
        None,
    )
    .await
    .unwrap();

    let completion2 = rx2.recv().await.unwrap().unwrap();
    let val2: serde_json::Value = serde_json::from_str(&completion2.result().content).unwrap();
    assert_eq!(val2["is_git_repo"], true);
    assert_eq!(val2["has_changes"], true);
    assert_eq!(val2["changed_files_count"], 2);

    // 3. Test single file filter
    let (results3, mut rx3) = result::tool_result_channel();
    let file_a_path = repo_dir.join("a.txt");
    let call3 = ToolCall {
        index: 0,
        call_id: "inspect_single".into(),
        model_call_id: "model:0".into(),
        name: "InspectChanges".into(),
        arguments_text: String::new(),
        arguments: json!({"path": file_a_path.to_str().unwrap()}),
    };

    dispatch::start(
        &runtime,
        &results3,
        &call3,
        0,
        &BTreeMap::new(),
        &context,
        None,
    )
    .await
    .unwrap();

    let completion3 = rx3.recv().await.unwrap().unwrap();
    let val3: serde_json::Value = serde_json::from_str(&completion3.result().content).unwrap();
    assert_eq!(val3["is_git_repo"], true);
    assert_eq!(val3["target_file"], file_a_path.to_str().unwrap());
    assert_eq!(val3["has_changes"], true);
    assert!(val3["diff"].as_str().unwrap().contains("-initial content"));
    assert!(val3["diff"].as_str().unwrap().contains("+modified content"));
}

#[tokio::test]
async fn test_inspect_changes_lockfile_noise_and_truncation() {
    let temp_dir = tempfile::tempdir().unwrap();
    let repo_dir = temp_dir.path().join("repo_truncation");
    std::fs::create_dir_all(&repo_dir).unwrap();

    let init = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "init"])
        .output()
        .unwrap();
    if !init.status.success() {
        return;
    }
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.name", "Test"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.email", "test@test.com"])
        .output();

    // 1. Commit initial files
    let lock_file = repo_dir.join("Cargo.lock");
    let code_file = repo_dir.join("large.rs");
    std::fs::write(&lock_file, "lock initial\n").unwrap();
    std::fs::write(&code_file, "fn main() {}\n").unwrap();

    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "add", "."])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "commit", "-m", "init"])
        .output();

    // 2. Modify Cargo.lock (should be filtered out from diff) and generate a massive diff in large.rs (>8000 chars)
    std::fs::write(&lock_file, "lock modified version 2.0 with many locks\n").unwrap();
    let large_content = (0..300)
        .map(|i| format!("    println!(\"This is a very long log line number {i} to test truncation behavior\");\n"))
        .collect::<String>();
    std::fs::write(&code_file, format!("fn main() {{\n{large_content}}}\n")).unwrap();

    let runtime = CursorToolRuntime::default();
    let (results, mut rx) = result::tool_result_channel();
    let call = ToolCall {
        index: 0,
        call_id: "inspect_truncation".into(),
        model_call_id: "model:0".into(),
        name: "InspectChanges".into(),
        arguments_text: String::new(),
        arguments: json!({
            "path": repo_dir.to_str().unwrap()
        }),
    };

    let context = ExecContext::default();

    dispatch::start(
        &runtime,
        &results,
        &call,
        0,
        &BTreeMap::new(),
        &context,
        None,
    )
    .await
    .unwrap();

    let completion = rx.recv().await.unwrap().unwrap();
    let val: serde_json::Value = serde_json::from_str(&completion.result().content).unwrap();
    assert_eq!(val["is_git_repo"], true);
    assert_eq!(val["has_changes"], true);
    assert_eq!(val["changed_files_count"], 2);
    assert_eq!(val["truncated"], true);
    assert!(val.get("hint").is_some());

    // Verify Cargo.lock diff is filtered out from combined diff
    let diff_str = val["diff"].as_str().unwrap();
    assert!(!diff_str.contains("Cargo.lock"));
    assert!(diff_str.len() <= 8000);
}

#[tokio::test]
async fn test_inspect_changes_staged_and_deleted_files() {
    let temp_dir = tempfile::tempdir().unwrap();
    let repo_dir = temp_dir.path().join("repo_staged");
    std::fs::create_dir_all(&repo_dir).unwrap();

    let init = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "init"])
        .output()
        .unwrap();
    if !init.status.success() {
        return;
    }
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.name", "Test"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.email", "test@test.com"])
        .output();

    let file_del = repo_dir.join("to_delete.txt");
    let file_staged = repo_dir.join("staged.txt");
    std::fs::write(&file_del, "delete me\n").unwrap();
    std::fs::write(&file_staged, "initial\n").unwrap();

    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "add", "."])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "commit", "-m", "init"])
        .output();

    // Delete to_delete.txt and stage modified staged.txt
    std::fs::remove_file(&file_del).unwrap();
    std::fs::write(&file_staged, "staged content\n").unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "add", "staged.txt"])
        .output();

    let runtime = CursorToolRuntime::default();
    let (results, mut rx) = result::tool_result_channel();
    let call = ToolCall {
        index: 0,
        call_id: "inspect_staged_del".into(),
        model_call_id: "model:0".into(),
        name: "InspectChanges".into(),
        arguments_text: String::new(),
        arguments: json!({
            "path": repo_dir.to_str().unwrap()
        }),
    };

    let context = ExecContext::default();

    dispatch::start(
        &runtime,
        &results,
        &call,
        0,
        &BTreeMap::new(),
        &context,
        None,
    )
    .await
    .unwrap();

    let completion = rx.recv().await.unwrap().unwrap();
    let val: serde_json::Value = serde_json::from_str(&completion.result().content).unwrap();
    assert_eq!(val["is_git_repo"], true);
    assert_eq!(val["has_changes"], true);

    let files = val["files"].as_array().unwrap();
    let staged_item = files.iter().find(|f| f["path"] == "staged.txt").unwrap();
    let del_item = files.iter().find(|f| f["path"] == "to_delete.txt").unwrap();

    assert_eq!(staged_item["staged"], true);
    assert_eq!(staged_item["status"], "Modified");
    assert_eq!(del_item["status"], "Deleted");
}

#[tokio::test]
async fn test_inspect_changes_single_clean_and_ignored_file() {
    let temp_dir = tempfile::tempdir().unwrap();
    let repo_dir = temp_dir.path().join("repo_clean_ignored");
    std::fs::create_dir_all(&repo_dir).unwrap();

    let init = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "init"])
        .output()
        .unwrap();
    if !init.status.success() {
        return;
    }
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.name", "Test"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.email", "test@test.com"])
        .output();

    let clean_file = repo_dir.join("clean.txt");
    let gitignore_file = repo_dir.join(".gitignore");
    let ignored_file = repo_dir.join("ignored.log");

    std::fs::write(&clean_file, "clean content\n").unwrap();
    std::fs::write(&gitignore_file, "*.log\n").unwrap();
    std::fs::write(&ignored_file, "some log output\n").unwrap();

    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "add", "clean.txt", ".gitignore"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "commit", "-m", "init"])
        .output();

    let runtime = CursorToolRuntime::default();
    let context = ExecContext::default();

    // 1. Inspect clean file -> has_changes must be false
    let (results1, mut rx1) = result::tool_result_channel();
    let call1 = ToolCall {
        index: 0,
        call_id: "inspect_clean_single".into(),
        model_call_id: "model:0".into(),
        name: "InspectChanges".into(),
        arguments_text: String::new(),
        arguments: json!({
            "path": clean_file.to_str().unwrap()
        }),
    };

    dispatch::start(
        &runtime,
        &results1,
        &call1,
        0,
        &BTreeMap::new(),
        &context,
        None,
    )
    .await
    .unwrap();

    let completion1 = rx1.recv().await.unwrap().unwrap();
    let val1: serde_json::Value = serde_json::from_str(&completion1.result().content).unwrap();
    assert_eq!(val1["is_git_repo"], true);
    assert_eq!(val1["has_changes"], false);
    assert!(val1["message"].as_str().unwrap().contains("No changes found"));

    // 2. Inspect ignored file -> has_changes must be false (not false-positive untracked)
    let (results2, mut rx2) = result::tool_result_channel();
    let call2 = ToolCall {
        index: 0,
        call_id: "inspect_ignored_single".into(),
        model_call_id: "model:0".into(),
        name: "InspectChanges".into(),
        arguments_text: String::new(),
        arguments: json!({
            "path": ignored_file.to_str().unwrap()
        }),
    };

    dispatch::start(
        &runtime,
        &results2,
        &call2,
        0,
        &BTreeMap::new(),
        &context,
        None,
    )
    .await
    .unwrap();

    let completion2 = rx2.recv().await.unwrap().unwrap();
    let val2: serde_json::Value = serde_json::from_str(&completion2.result().content).unwrap();
    assert_eq!(val2["is_git_repo"], true);
    assert_eq!(val2["has_changes"], false);
    assert!(val2["message"].as_str().unwrap().contains("No changes found"));
}

#[tokio::test]
async fn test_inspect_changes_empty_repo_with_staged_file() {
    let temp_dir = tempfile::tempdir().unwrap();
    let repo_dir = temp_dir.path().join("repo_empty_staged");
    std::fs::create_dir_all(&repo_dir).unwrap();

    let init = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "init"])
        .output()
        .unwrap();
    if !init.status.success() {
        return;
    }
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.name", "Test"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.email", "test@test.com"])
        .output();

    let staged_file = repo_dir.join("init.txt");
    std::fs::write(&staged_file, "initial staged content\n").unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "add", "init.txt"])
        .output();

    let runtime = CursorToolRuntime::default();
    let context = ExecContext::default();

    // Single file in empty repo
    let (results1, mut rx1) = result::tool_result_channel();
    let call1 = ToolCall {
        index: 0,
        call_id: "inspect_empty_single".into(),
        model_call_id: "model:0".into(),
        name: "InspectChanges".into(),
        arguments_text: String::new(),
        arguments: json!({
            "path": staged_file.to_str().unwrap()
        }),
    };

    dispatch::start(
        &runtime,
        &results1,
        &call1,
        0,
        &BTreeMap::new(),
        &context,
        None,
    )
    .await
    .unwrap();

    let completion1 = rx1.recv().await.unwrap().unwrap();
    let val1: serde_json::Value = serde_json::from_str(&completion1.result().content).unwrap();
    assert_eq!(val1["is_git_repo"], true);
    assert_eq!(val1["has_changes"], true);
    assert!(val1["diff"].as_str().unwrap().contains("+initial staged content"));

    // Directory in empty repo
    let (results2, mut rx2) = result::tool_result_channel();
    let call2 = ToolCall {
        index: 0,
        call_id: "inspect_empty_repo".into(),
        model_call_id: "model:0".into(),
        name: "InspectChanges".into(),
        arguments_text: String::new(),
        arguments: json!({
            "path": repo_dir.to_str().unwrap()
        }),
    };

    dispatch::start(
        &runtime,
        &results2,
        &call2,
        0,
        &BTreeMap::new(),
        &context,
        None,
    )
    .await
    .unwrap();

    let completion2 = rx2.recv().await.unwrap().unwrap();
    let val2: serde_json::Value = serde_json::from_str(&completion2.result().content).unwrap();
    assert_eq!(val2["is_git_repo"], true);
    assert_eq!(val2["has_changes"], true);
    assert_eq!(val2["changed_files_count"], 1);
    assert_eq!(val2["files"][0]["status"], "Added");
    assert_eq!(val2["files"][0]["staged"], true);
}

#[tokio::test]
async fn test_inspect_changes_spaces_and_special_paths() {
    let temp_dir = tempfile::tempdir().unwrap();
    let repo_dir = temp_dir.path().join("repo special path with spaces");
    std::fs::create_dir_all(&repo_dir).unwrap();

    let init = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "init"])
        .output()
        .unwrap();
    if !init.status.success() {
        return;
    }
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.name", "Test"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.email", "test@test.com"])
        .output();

    let special_file = repo_dir.join("file with space and [brackets].rs");
    std::fs::write(&special_file, "fn space() {}\n").unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "add", "."])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "commit", "-m", "init"])
        .output();

    std::fs::write(&special_file, "fn space_modified() {}\n").unwrap();

    let runtime = CursorToolRuntime::default();
    let context = ExecContext::default();

    let (results, mut rx) = result::tool_result_channel();
    let call = ToolCall {
        index: 0,
        call_id: "inspect_special".into(),
        model_call_id: "model:0".into(),
        name: "InspectChanges".into(),
        arguments_text: String::new(),
        arguments: json!({
            "path": special_file.to_str().unwrap()
        }),
    };

    dispatch::start(
        &runtime,
        &results,
        &call,
        0,
        &BTreeMap::new(),
        &context,
        None,
    )
    .await
    .unwrap();

    let completion = rx.recv().await.unwrap().unwrap();
    let val: serde_json::Value = serde_json::from_str(&completion.result().content).unwrap();
    assert_eq!(val["is_git_repo"], true);
    assert_eq!(val["has_changes"], true);
    assert!(val["diff"].as_str().unwrap().contains("+fn space_modified()"));
}

#[tokio::test]
async fn test_inspect_changes_numstat_counts() {
    let temp_dir = tempfile::tempdir().unwrap();
    let repo_dir = temp_dir.path().join("repo_numstat");
    std::fs::create_dir_all(&repo_dir).unwrap();

    let init = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "init"])
        .output()
        .unwrap();
    if !init.status.success() {
        return;
    }
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.name", "Test"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.email", "test@test.com"])
        .output();

    let test_file = repo_dir.join("stat.rs");
    std::fs::write(&test_file, "line1\nline2\nline3\n").unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "add", "."])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "commit", "-m", "init"])
        .output();

    // Modify: delete 1 line, add 3 lines
    std::fs::write(&test_file, "line1\nline_new_a\nline_new_b\nline_new_c\nline3\n").unwrap();

    let runtime = CursorToolRuntime::default();
    let context = ExecContext::default();

    let (results, mut rx) = result::tool_result_channel();
    let call = ToolCall {
        index: 0,
        call_id: "inspect_stat".into(),
        model_call_id: "model:0".into(),
        name: "InspectChanges".into(),
        arguments_text: String::new(),
        arguments: json!({
            "path": repo_dir.to_str().unwrap()
        }),
    };

    dispatch::start(
        &runtime,
        &results,
        &call,
        0,
        &BTreeMap::new(),
        &context,
        None,
    )
    .await
    .unwrap();

    let completion = rx.recv().await.unwrap().unwrap();
    let val: serde_json::Value = serde_json::from_str(&completion.result().content).unwrap();
    assert_eq!(val["is_git_repo"], true);
    assert_eq!(val["has_changes"], true);
    let files = val["files"].as_array().unwrap();
    let file = files.iter().find(|f| f["path"] == "stat.rs").unwrap();
    assert_eq!(file["status"], "Modified");
    assert_eq!(file["added"], 3);
    assert_eq!(file["deleted"], 1);
}

#[tokio::test]
async fn test_inspect_changes_files_cap_truncation() {
    let temp_dir = tempfile::tempdir().unwrap();
    let repo_dir = temp_dir.path().join("repo_many_files");
    std::fs::create_dir_all(&repo_dir).unwrap();

    let init = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "init"])
        .output()
        .unwrap();
    if !init.status.success() {
        return;
    }
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.name", "Test"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C", repo_dir.to_str().unwrap(), "config", "user.email", "test@test.com"])
        .output();

    // Create 120 files (> MAX_CHANGED_FILES_SUMMARY = 100)
    for i in 0..120 {
        let f = repo_dir.join(format!("file_{i:03}.txt"));
        std::fs::write(&f, format!("content {i}\n")).unwrap();
    }

    let runtime = CursorToolRuntime::default();
    let context = ExecContext::default();

    let (results, mut rx) = result::tool_result_channel();
    let call = ToolCall {
        index: 0,
        call_id: "inspect_many".into(),
        model_call_id: "model:0".into(),
        name: "InspectChanges".into(),
        arguments_text: String::new(),
        arguments: json!({
            "path": repo_dir.to_str().unwrap()
        }),
    };

    dispatch::start(
        &runtime,
        &results,
        &call,
        0,
        &BTreeMap::new(),
        &context,
        None,
    )
    .await
    .unwrap();

    let completion = rx.recv().await.unwrap().unwrap();
    let val: serde_json::Value = serde_json::from_str(&completion.result().content).unwrap();
    assert_eq!(val["is_git_repo"], true);
    assert_eq!(val["has_changes"], true);
    assert_eq!(val["changed_files_count"], 120);

    let files = val["files"].as_array().unwrap();
    assert_eq!(files.len(), 100);
    assert_eq!(val["files_truncated"], true);
    assert_eq!(val["truncated"], true);
    assert!(val["hint"].as_str().unwrap().contains("truncated"));
}





