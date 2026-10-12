//! Executes structural symbol searches using bundled Tree-sitter parsers.

use std::{fs, path::Path};

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct SymbolSearchArguments {
    path: String,
    query: String,
    #[serde(default)]
    kinds: Vec<String>,
    #[serde(default)]
    exact_match: bool,
    #[serde(default = "default_max_results")]
    max_results: usize,
}

fn default_max_results() -> usize {
    50
}

pub async fn execute(arguments: Value) -> std::result::Result<Value, String> {
    let args: SymbolSearchArguments = serde_json::from_value(arguments)
        .map_err(|error| format!("Invalid arguments for SymbolSearch: {error}"))?;
    if args.query.trim().is_empty() {
        return Err("SymbolSearch query must not be empty".into());
    }
    if args.max_results == 0 {
        return Err("SymbolSearch max_results must be greater than zero".into());
    }

    let path = Path::new(&args.path);
    if !path.exists() {
        return Err(format!("Path not found: {}", args.path));
    }

    let kind_filters = args
        .kinds
        .iter()
        .map(|kind| kind.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut matches = Vec::new();
    search_path(path, &args, &kind_filters, &mut matches)?;

    serde_json::to_value(serde_json::json!({
        "query": args.query,
        "path": args.path,
        "total_matches": matches.len(),
        "matches": matches,
    }))
    .map_err(|error| format!("Failed to serialize SymbolSearch results: {error}"))
}

fn search_path(
    path: &Path,
    args: &SymbolSearchArguments,
    kind_filters: &[String],
    matches: &mut Vec<Value>,
) -> std::result::Result<(), String> {
    if matches.len() >= args.max_results {
        return Ok(());
    }
    if path.is_file() {
        return search_file(path, args, kind_filters, matches);
    }
    if !path.is_dir() {
        return Ok(());
    }

    let mut entries = match fs::read_dir(path) {
        Ok(entries) => entries.filter_map(Result::ok).collect::<Vec<_>>(),
        Err(_) => return Ok(()),
    };
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if matches.len() >= args.max_results {
            break;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let entry_path = entry.path();
        if file_type.is_dir() {
            if !is_ignored_directory(&entry_path) {
                search_path(&entry_path, args, kind_filters, matches)?;
            }
        } else if file_type.is_file() {
            search_file(&entry_path, args, kind_filters, matches)?;
        }
    }
    Ok(())
}

fn is_ignored_directory(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".git" | "node_modules" | "target" | "dist" | "build")
    )
}

fn search_file(
    path: &Path,
    args: &SymbolSearchArguments,
    kind_filters: &[String],
    matches: &mut Vec<Value>,
) -> std::result::Result<(), String> {
    let Some(language) = semble_core::language::detect_language(path) else {
        return Ok(());
    };
    if semble_core::language::parser_for(language).is_none() {
        return Ok(());
    }

    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(_) => return Ok(()),
    };
    let file_path = path.to_string_lossy();
    let outline = semble_core::extract_outline(&source, &file_path, Some(language));
    for symbol in outline.symbols {
        collect_matches(&symbol, &file_path, args, kind_filters, matches);
        if matches.len() >= args.max_results {
            break;
        }
    }
    Ok(())
}

fn collect_matches(
    symbol: &semble_core::SymbolNode,
    file_path: &str,
    args: &SymbolSearchArguments,
    kind_filters: &[String],
    matches: &mut Vec<Value>,
) {
    if matches.len() >= args.max_results {
        return;
    }

    let matches_name = if args.exact_match {
        symbol.name == args.query
    } else {
        symbol
            .name
            .to_lowercase()
            .contains(&args.query.to_lowercase())
    };
    let kind = symbol.kind.as_str();
    let matches_kind = kind_filters.is_empty() || kind_filters.iter().any(|filter| filter == kind);
    if matches_name && matches_kind {
        matches.push(serde_json::json!({
            "name": symbol.name,
            "kind": kind,
            "file_path": file_path,
            "start_line": symbol.start_line,
            "end_line": symbol.end_line,
            "detail": symbol.detail,
            "visibility": symbol.visibility,
        }));
    }

    for child in &symbol.children {
        collect_matches(child, file_path, args, kind_filters, matches);
        if matches.len() >= args.max_results {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_generated_directories() {
        assert!(is_ignored_directory(Path::new("node_modules")));
        assert!(!is_ignored_directory(Path::new("src")));
    }
}
