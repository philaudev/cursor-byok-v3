//! Tree-sitter AST symbol extraction for supported languages.

use tree_sitter::Node;

use crate::language::parser_for;

use super::types::{FileOutline, SymbolKind, SymbolNode};

pub fn extract_outline(source: &str, file_path: &str, language: Option<&str>) -> FileOutline {
    let total_lines = source.lines().count().max(1);

    let symbols = match language {
        Some(lang) => match parser_for(lang) {
            Some(mut parser) => match parser.parse(source, None) {
                Some(tree) => extract_from_root(tree.root_node(), source, lang),
                None => Vec::new(),
            },
            None => Vec::new(),
        },
        None => Vec::new(),
    };

    FileOutline {
        file_path: file_path.to_owned(),
        language: language.map(str::to_owned),
        total_lines,
        symbols,
    }
}

fn extract_from_root(root: Node<'_>, source: &str, language: &str) -> Vec<SymbolNode> {
    let mut symbols = Vec::new();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if let Some(node) = extract_symbol(child, source, language) {
            symbols.push(node);
        }
    }
    symbols
}

fn extract_symbol(node: Node<'_>, source: &str, language: &str) -> Option<SymbolNode> {
    match language {
        "java" => extract_java_symbol(node, source),
        "rust" => extract_rust_symbol(node, source),
        "typescript" | "javascript" => extract_ts_symbol(node, source),
        "python" => extract_python_symbol(node, source),
        "go" => extract_go_symbol(node, source),
        "c" | "cpp" => extract_cpp_symbol(node, source),
        _ => None,
    }
}

fn line_range(node: Node<'_>, source: &str) -> (usize, usize) {
    let start_byte = node.start_byte();
    let end_byte = node.end_byte();
    let start_line = source[..start_byte.min(source.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count()
        + 1;
    let length = source[start_byte.min(source.len())..end_byte.min(source.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count();
    let end_line = start_line + length;
    (start_line, end_line)
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    let start = node.start_byte().min(source.len());
    let end = node.end_byte().min(source.len());
    &source[start..end]
}

// -----------------------------------------------------------------------------
// Java symbol extraction
// -----------------------------------------------------------------------------

fn extract_java_symbol(node: Node<'_>, source: &str) -> Option<SymbolNode> {
    let (start_line, end_line) = line_range(node, source);
    match node.kind() {
        "class_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "AnonymousClass".into());
            let visibility = get_java_visibility(node, source);
            let interfaces = node
                .child_by_field_name("interfaces")
                .map(|n| {
                    let text = node_text(n, source).trim();
                    if text.starts_with("implements") {
                        format!(" {}", text)
                    } else {
                        format!(" implements {}", text)
                    }
                });
            let superclass = node
                .child_by_field_name("superclass")
                .map(|n| {
                    let text = node_text(n, source).trim();
                    if text.starts_with("extends") {
                        format!(" {}", text)
                    } else {
                        format!(" extends {}", text)
                    }
                });
            let detail = match (superclass, interfaces) {
                (Some(s), Some(i)) => Some(format!("{}{}", s, i)),
                (Some(s), None) => Some(s),
                (None, Some(i)) => Some(i),
                (None, None) => None,
            };
            let children = node
                .child_by_field_name("body")
                .map(|body| extract_java_class_body(body, source))
                .unwrap_or_default();
            Some(SymbolNode {
                name,
                kind: SymbolKind::Class,
                detail,
                visibility,
                start_line,
                end_line,
                children,
            })
        }
        "interface_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "AnonymousInterface".into());
            let visibility = get_java_visibility(node, source);
            let children = node
                .child_by_field_name("body")
                .map(|body| extract_java_class_body(body, source))
                .unwrap_or_default();
            Some(SymbolNode {
                name,
                kind: SymbolKind::Interface,
                detail: None,
                visibility,
                start_line,
                end_line,
                children,
            })
        }
        "enum_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "AnonymousEnum".into());
            let visibility = get_java_visibility(node, source);
            let children = node
                .child_by_field_name("body")
                .map(|body| extract_java_class_body(body, source))
                .unwrap_or_default();
            Some(SymbolNode {
                name,
                kind: SymbolKind::Enum,
                detail: None,
                visibility,
                start_line,
                end_line,
                children,
            })
        }
        "record_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "AnonymousRecord".into());
            let visibility = get_java_visibility(node, source);
            let params = node
                .child_by_field_name("parameters")
                .map(|n| node_text(n, source).to_string());
            let children = node
                .child_by_field_name("body")
                .map(|body| extract_java_class_body(body, source))
                .unwrap_or_default();
            Some(SymbolNode {
                name,
                kind: SymbolKind::Record,
                detail: params,
                visibility,
                start_line,
                end_line,
                children,
            })
        }
        _ => None,
    }
}

fn get_java_visibility(node: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let text = node_text(child, source);
            if text.contains("public") {
                return Some("public".into());
            } else if text.contains("private") {
                return Some("private".into());
            } else if text.contains("protected") {
                return Some("protected".into());
            }
        }
    }
    None
}

fn extract_java_class_body(body: Node<'_>, source: &str) -> Vec<SymbolNode> {
    let mut symbols = Vec::new();
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        let (start_line, end_line) = line_range(child, source);
        match child.kind() {
            "field_declaration" => {
                let visibility = get_java_visibility(child, source);
                let field_type = child
                    .child_by_field_name("type")
                    .map(|n| node_text(n, source).trim().to_string())
                    .unwrap_or_else(|| "Object".into());
                let declarator = child.child_by_field_name("declarator");
                let name = declarator
                    .and_then(|d| d.child_by_field_name("name"))
                    .map(|n| node_text(n, source).trim().to_string())
                    .unwrap_or_else(|| "field".into());
                symbols.push(SymbolNode {
                    name,
                    kind: SymbolKind::Field,
                    detail: Some(format!(": {}", field_type)),
                    visibility,
                    start_line,
                    end_line,
                    children: Vec::new(),
                });
            }
            "method_declaration" => {
                let visibility = get_java_visibility(child, source);
                let return_type = child
                    .child_by_field_name("type")
                    .map(|n| node_text(n, source).trim().to_string())
                    .unwrap_or_else(|| "void".into());
                let name = child
                    .child_by_field_name("name")
                    .map(|n| node_text(n, source).trim().to_string())
                    .unwrap_or_else(|| "method".into());
                let params = child
                    .child_by_field_name("parameters")
                    .map(|n| node_text(n, source).trim().to_string())
                    .unwrap_or_else(|| "()".into());
                symbols.push(SymbolNode {
                    name,
                    kind: SymbolKind::Method,
                    detail: Some(format!("{}: {}", params, return_type)),
                    visibility,
                    start_line,
                    end_line,
                    children: Vec::new(),
                });
            }
            "constructor_declaration" => {
                let visibility = get_java_visibility(child, source);
                let name = child
                    .child_by_field_name("name")
                    .map(|n| node_text(n, source).trim().to_string())
                    .unwrap_or_else(|| "Constructor".into());
                let params = child
                    .child_by_field_name("parameters")
                    .map(|n| node_text(n, source).trim().to_string())
                    .unwrap_or_else(|| "()".into());
                symbols.push(SymbolNode {
                    name,
                    kind: SymbolKind::Constructor,
                    detail: Some(params),
                    visibility,
                    start_line,
                    end_line,
                    children: Vec::new(),
                });
            }
            _ => {
                if let Some(nested) = extract_java_symbol(child, source) {
                    symbols.push(nested);
                }
            }
        }
    }
    symbols
}

// -----------------------------------------------------------------------------
// Rust symbol extraction
// -----------------------------------------------------------------------------

fn extract_rust_symbol(node: Node<'_>, source: &str) -> Option<SymbolNode> {
    let (start_line, end_line) = line_range(node, source);
    let visibility = get_rust_visibility(node, source);

    match node.kind() {
        "struct_item" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "AnonymousStruct".into());
            let mut children = Vec::new();
            if let Some(field_list) = node.child_by_field_name("body") {
                let mut cursor = field_list.walk();
                for field in field_list.children(&mut cursor) {
                    if field.kind() == "field_declaration" {
                        let (f_start, f_end) = line_range(field, source);
                        let f_name = field
                            .child_by_field_name("name")
                            .map(|n| node_text(n, source).to_string())
                            .unwrap_or_else(|| "field".into());
                        let f_type = field
                            .child_by_field_name("type")
                            .map(|n| format!(": {}", node_text(n, source).trim()));
                        let f_vis = get_rust_visibility(field, source);
                        children.push(SymbolNode {
                            name: f_name,
                            kind: SymbolKind::Field,
                            detail: f_type,
                            visibility: f_vis,
                            start_line: f_start,
                            end_line: f_end,
                            children: Vec::new(),
                        });
                    }
                }
            }
            Some(SymbolNode {
                name,
                kind: SymbolKind::Struct,
                detail: None,
                visibility,
                start_line,
                end_line,
                children,
            })
        }
        "enum_item" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "AnonymousEnum".into());
            Some(SymbolNode {
                name,
                kind: SymbolKind::Enum,
                detail: None,
                visibility,
                start_line,
                end_line,
                children: Vec::new(),
            })
        }
        "trait_item" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "AnonymousTrait".into());
            let mut children = Vec::new();
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for item in body.children(&mut cursor) {
                    if item.kind() == "function_item" || item.kind() == "function_signature_item" {
                        if let Some(fn_node) = extract_rust_fn(item, source) {
                            children.push(fn_node);
                        }
                    }
                }
            }
            Some(SymbolNode {
                name,
                kind: SymbolKind::Trait,
                detail: None,
                visibility,
                start_line,
                end_line,
                children,
            })
        }
        "impl_item" => {
            let type_name = node
                .child_by_field_name("type")
                .map(|n| node_text(n, source).trim().to_string())
                .unwrap_or_else(|| "Self".into());
            let trait_name = node
                .child_by_field_name("trait")
                .map(|n| format!("for {}", node_text(n, source).trim()));
            let name = match trait_name {
                Some(tr) => format!("impl {} {}", tr, type_name),
                None => format!("impl {}", type_name),
            };
            let mut children = Vec::new();
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for item in body.children(&mut cursor) {
                    if item.kind() == "function_item" {
                        if let Some(fn_node) = extract_rust_fn(item, source) {
                            children.push(fn_node);
                        }
                    }
                }
            }
            Some(SymbolNode {
                name,
                kind: SymbolKind::Class,
                detail: None,
                visibility: None,
                start_line,
                end_line,
                children,
            })
        }
        "function_item" => extract_rust_fn(node, source),
        "mod_item" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "mod".into());
            Some(SymbolNode {
                name,
                kind: SymbolKind::Module,
                detail: None,
                visibility,
                start_line,
                end_line,
                children: Vec::new(),
            })
        }
        _ => None,
    }
}

fn get_rust_visibility(node: Node<'_>, source: &str) -> Option<String> {
    node.child_by_field_name("visibility")
        .map(|n| node_text(n, source).trim().to_string())
}

fn extract_rust_fn(node: Node<'_>, source: &str) -> Option<SymbolNode> {
    let (start_line, end_line) = line_range(node, source);
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(n, source).trim().to_string())
        .unwrap_or_else(|| "fn".into());
    let params = node
        .child_by_field_name("parameters")
        .map(|n| node_text(n, source).trim().to_string())
        .unwrap_or_else(|| "()".into());
    let return_type = node
        .child_by_field_name("return_type")
        .map(|n| format!(" -> {}", node_text(n, source).trim()));
    let detail = Some(format!("{}{}", params, return_type.unwrap_or_default()));
    let visibility = get_rust_visibility(node, source);
    Some(SymbolNode {
        name,
        kind: SymbolKind::Function,
        detail,
        visibility,
        start_line,
        end_line,
        children: Vec::new(),
    })
}

// -----------------------------------------------------------------------------
// TypeScript / JavaScript symbol extraction
// -----------------------------------------------------------------------------

fn extract_ts_symbol(node: Node<'_>, source: &str) -> Option<SymbolNode> {
    let (start_line, end_line) = line_range(node, source);
    match node.kind() {
        "class_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "AnonymousClass".into());
            let mut children = Vec::new();
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for member in body.children(&mut cursor) {
                    let (m_start, m_end) = line_range(member, source);
                    match member.kind() {
                        "method_definition" => {
                            let m_name = member
                                .child_by_field_name("name")
                                .map(|n| node_text(n, source).trim().to_string())
                                .unwrap_or_else(|| "method".into());
                            let params = member
                                .child_by_field_name("parameters")
                                .map(|n| node_text(n, source).trim().to_string())
                                .unwrap_or_else(|| "()".into());
                            let ret = member
                                .child_by_field_name("return_type")
                                .map(|n| {
                                    let text = node_text(n, source).trim();
                                    if text.starts_with(':') {
                                        format!(" {}", text)
                                    } else {
                                        format!(": {}", text)
                                    }
                                });
                            children.push(SymbolNode {
                                name: m_name,
                                kind: SymbolKind::Method,
                                detail: Some(format!("{}{}", params, ret.unwrap_or_default())),
                                visibility: None,
                                start_line: m_start,
                                end_line: m_end,
                                children: Vec::new(),
                            });
                        }
                        "public_field_definition" | "field_definition" | "property_definition" => {
                            let f_name = member
                                .child_by_field_name("name")
                                .map(|n| node_text(n, source).trim().to_string())
                                .unwrap_or_else(|| "field".into());
                            let f_type = member
                                .child_by_field_name("type")
                                .map(|n| {
                                    let text = node_text(n, source).trim();
                                    if text.starts_with(':') {
                                        format!(" {}", text)
                                    } else {
                                        format!(": {}", text)
                                    }
                                });
                            children.push(SymbolNode {
                                name: f_name,
                                kind: SymbolKind::Property,
                                detail: f_type,
                                visibility: None,
                                start_line: m_start,
                                end_line: m_end,
                                children: Vec::new(),
                            });
                        }
                        _ => {}
                    }
                }
            }
            Some(SymbolNode {
                name,
                kind: SymbolKind::Class,
                detail: None,
                visibility: None,
                start_line,
                end_line,
                children,
            })
        }
        "interface_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "AnonymousInterface".into());
            Some(SymbolNode {
                name,
                kind: SymbolKind::Interface,
                detail: None,
                visibility: None,
                start_line,
                end_line,
                children: Vec::new(),
            })
        }
        "type_alias_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "AnonymousType".into());
            Some(SymbolNode {
                name,
                kind: SymbolKind::TypeAlias,
                detail: None,
                visibility: None,
                start_line,
                end_line,
                children: Vec::new(),
            })
        }
        "function_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "anonymous".into());
            let params = node
                .child_by_field_name("parameters")
                .map(|n| node_text(n, source).trim().to_string())
                .unwrap_or_else(|| "()".into());
            let ret = node
                .child_by_field_name("return_type")
                .map(|n| {
                    let text = node_text(n, source).trim();
                    if text.starts_with(':') {
                        format!(" {}", text)
                    } else {
                        format!(": {}", text)
                    }
                });
            Some(SymbolNode {
                name,
                kind: SymbolKind::Function,
                detail: Some(format!("{}{}", params, ret.unwrap_or_default())),
                visibility: None,
                start_line,
                end_line,
                children: Vec::new(),
            })
        }
        "export_statement" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(mut sym) = extract_ts_symbol(child, source) {
                    sym.visibility = Some("export".into());
                    return Some(sym);
                }
            }
            None
        }
        _ => None,
    }
}

// -----------------------------------------------------------------------------
// Python symbol extraction
// -----------------------------------------------------------------------------

fn extract_python_symbol(node: Node<'_>, source: &str) -> Option<SymbolNode> {
    let (start_line, end_line) = line_range(node, source);
    match node.kind() {
        "class_definition" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "AnonymousClass".into());
            let superclasses = node
                .child_by_field_name("superclasses")
                .map(|n| node_text(n, source).trim().to_string());
            let mut children = Vec::new();
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    if child.kind() == "function_definition" {
                        if let Some(fn_node) = extract_python_symbol(child, source) {
                            children.push(fn_node);
                        }
                    }
                }
            }
            Some(SymbolNode {
                name,
                kind: SymbolKind::Class,
                detail: superclasses,
                visibility: None,
                start_line,
                end_line,
                children,
            })
        }
        "function_definition" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "fn".into());
            let params = node
                .child_by_field_name("parameters")
                .map(|n| node_text(n, source).trim().to_string())
                .unwrap_or_else(|| "()".into());
            let ret = node
                .child_by_field_name("return_type")
                .map(|n| format!(" -> {}", node_text(n, source).trim()));
            Some(SymbolNode {
                name,
                kind: SymbolKind::Function,
                detail: Some(format!("{}{}", params, ret.unwrap_or_default())),
                visibility: None,
                start_line,
                end_line,
                children: Vec::new(),
            })
        }
        _ => None,
    }
}

// -----------------------------------------------------------------------------
// Go symbol extraction
// -----------------------------------------------------------------------------

fn extract_go_symbol(node: Node<'_>, source: &str) -> Option<SymbolNode> {
    let (start_line, end_line) = line_range(node, source);
    match node.kind() {
        "type_declaration" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "type_spec" {
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| node_text(n, source).to_string())
                        .unwrap_or_else(|| "Type".into());
                    let type_kind = child.child_by_field_name("type");
                    let (kind, children) = match type_kind.map(|n| n.kind()) {
                        Some("struct_type") => (SymbolKind::Struct, Vec::new()),
                        Some("interface_type") => (SymbolKind::Interface, Vec::new()),
                        _ => (SymbolKind::TypeAlias, Vec::new()),
                    };
                    return Some(SymbolNode {
                        name,
                        kind,
                        detail: None,
                        visibility: None,
                        start_line,
                        end_line,
                        children,
                    });
                }
            }
            None
        }
        "function_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "fn".into());
            let params = node
                .child_by_field_name("parameters")
                .map(|n| node_text(n, source).trim().to_string())
                .unwrap_or_else(|| "()".into());
            let result = node
                .child_by_field_name("result")
                .map(|n| format!(" {}", node_text(n, source).trim()));
            Some(SymbolNode {
                name,
                kind: SymbolKind::Function,
                detail: Some(format!("{}{}", params, result.unwrap_or_default())),
                visibility: None,
                start_line,
                end_line,
                children: Vec::new(),
            })
        }
        "method_declaration" => {
            let receiver = node
                .child_by_field_name("receiver")
                .map(|n| node_text(n, source).trim().to_string())
                .unwrap_or_default();
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "method".into());
            let params = node
                .child_by_field_name("parameters")
                .map(|n| node_text(n, source).trim().to_string())
                .unwrap_or_else(|| "()".into());
            let result = node
                .child_by_field_name("result")
                .map(|n| format!(" {}", node_text(n, source).trim()));
            Some(SymbolNode {
                name,
                kind: SymbolKind::Method,
                detail: Some(format!("{} {}{}", receiver, params, result.unwrap_or_default())),
                visibility: None,
                start_line,
                end_line,
                children: Vec::new(),
            })
        }
        _ => None,
    }
}

// -----------------------------------------------------------------------------
// C/C++ symbol extraction
// -----------------------------------------------------------------------------

fn extract_cpp_symbol(node: Node<'_>, source: &str) -> Option<SymbolNode> {
    let (start_line, end_line) = line_range(node, source);
    match node.kind() {
        "class_specifier" | "struct_specifier" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "AnonymousClass".into());
            let kind = if node.kind() == "struct_specifier" {
                SymbolKind::Struct
            } else {
                SymbolKind::Class
            };
            Some(SymbolNode {
                name,
                kind,
                detail: None,
                visibility: None,
                start_line,
                end_line,
                children: Vec::new(),
            })
        }
        "function_definition" => {
            let declarator = node.child_by_field_name("declarator");
            let name = declarator
                .map(|n| node_text(n, source).trim().to_string())
                .unwrap_or_else(|| "fn".into());
            let ret_type = node
                .child_by_field_name("type")
                .map(|n| node_text(n, source).trim().to_string());
            Some(SymbolNode {
                name,
                kind: SymbolKind::Function,
                detail: ret_type.map(|t| format!(": {}", t)),
                visibility: None,
                start_line,
                end_line,
                children: Vec::new(),
            })
        }
        _ => None,
    }
}
