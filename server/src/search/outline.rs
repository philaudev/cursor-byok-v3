//! Executes the Outline tool to extract high-level code structure using Tree-sitter.

use std::path::Path;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct OutlineArguments {
    path: String,
    language: Option<String>,
}

pub async fn execute(arguments: Value) -> std::result::Result<Value, String> {
    let args: OutlineArguments =
        serde_json::from_value(arguments).map_err(|error| format!("Invalid arguments for Outline: {error}"))?;

    let file_path = Path::new(&args.path);
    if !file_path.exists() {
        return Err(format!("File not found: {}", args.path));
    }

    let source = tokio::fs::read_to_string(file_path)
        .await
        .map_err(|error| format!("Failed to read file {}: {error}", args.path))?;

    let detected_lang = args
        .language
        .as_deref()
        .or_else(|| semble_core::language::detect_language(file_path));

    let outline = semble_core::extract_outline(&source, &args.path, detected_lang);
    let rendered = outline.render_ascii_tree();

    serde_json::to_value(serde_json::json!({
        "file_path": outline.file_path,
        "language": outline.language,
        "total_lines": outline.total_lines,
        "rendered": rendered,
        "symbols": outline.symbols,
    }))
    .map_err(|error| format!("Failed to serialize outline: {error}"))
}
