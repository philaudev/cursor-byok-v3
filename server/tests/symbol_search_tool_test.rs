use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn searches_nested_declarations_by_name_and_kind() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("symbols.rs");
    tokio::fs::write(
        &path,
        "pub struct ToolRegistry;\n\nimpl ToolRegistry {\n    pub fn select_json(&self) {}\n}\n\nfn parse_tool() {}\n",
    )
    .await
    .unwrap();

    let result = cursor_server::search::execute_symbol_search(json!({
        "path": directory.path(),
        "query": "select_json",
        "kinds": ["function"],
        "exact_match": true
    }))
    .await
    .unwrap();

    assert_eq!(result["total_matches"], 1);
    let matched = &result["matches"][0];
    assert_eq!(matched["name"], "select_json");
    assert_eq!(matched["kind"], "function");
    assert_eq!(matched["file_path"], path.to_string_lossy().as_ref());
}

#[tokio::test]
async fn skips_ignored_directories_and_limits_results() {
    let directory = tempdir().unwrap();
    let source = "fn parse_tool() {}\n";
    tokio::fs::write(directory.path().join("one.rs"), source)
        .await
        .unwrap();
    tokio::fs::create_dir(directory.path().join("node_modules"))
        .await
        .unwrap();
    tokio::fs::write(directory.path().join("node_modules").join("two.rs"), source)
        .await
        .unwrap();

    let result = cursor_server::search::execute_symbol_search(json!({
        "path": directory.path(),
        "query": "parse_tool",
        "max_results": 1
    }))
    .await
    .unwrap();

    assert_eq!(result["total_matches"], 1);
    assert!(result["matches"][0]["file_path"]
        .as_str()
        .unwrap()
        .ends_with("one.rs"));
}
