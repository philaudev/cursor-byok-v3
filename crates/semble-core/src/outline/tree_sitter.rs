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
        "dart" => extract_dart_symbol(node, source),
        _ => None,
    }
}

// -----------------------------------------------------------------------------
// Dart symbol extraction
// -----------------------------------------------------------------------------

fn get_dart_visibility(name: &str) -> Option<String> {
    if name.starts_with('_') {
        Some("private".into())
    } else {
        Some("public".into())
    }
}

fn find_child_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            return Some(child);
        }
    }
    None
}

fn extract_dart_symbol(node: Node<'_>, source: &str) -> Option<SymbolNode> {
    let (start_line, end_line) = line_range(node, source);
    match node.kind() {
        "class_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "AnonymousClass".into());
            let visibility = get_dart_visibility(&name);

            let mut clause_parts = Vec::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "superclass" => {
                        let text = node_text(child, source).trim();
                        if !text.is_empty() {
                            clause_parts.push(text.to_string());
                        }
                    }
                    "mixins" => {
                        let text = node_text(child, source).trim();
                        if !text.is_empty() {
                            clause_parts.push(text.to_string());
                        }
                    }
                    "interfaces" => {
                        let text = node_text(child, source).trim();
                        if !text.is_empty() {
                            clause_parts.push(text.to_string());
                        }
                    }
                    _ => {}
                }
            }

            let detail = if clause_parts.is_empty() {
                None
            } else {
                Some(format!(" {}", clause_parts.join(" ")))
            };

            let children = node
                .child_by_field_name("body")
                .map(|body| extract_dart_class_body(body, source))
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
        "mixin_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "AnonymousMixin".into());
            let visibility = get_dart_visibility(&name);
            let children = node
                .child_by_field_name("body")
                .map(|body| extract_dart_class_body(body, source))
                .unwrap_or_default();
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
        "extension_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_else(|| "Extension".into());
            let visibility = get_dart_visibility(&name);
            let on_type = node
                .child_by_field_name("class")
                .map(|n| format!(" on {}", node_text(n, source).trim()));
            let children = node
                .child_by_field_name("body")
                .map(|body| extract_dart_class_body(body, source))
                .unwrap_or_default();
            Some(SymbolNode {
                name,
                kind: SymbolKind::Class,
                detail: on_type,
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
            let visibility = get_dart_visibility(&name);
            let mut children = Vec::new();
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    if child.kind() == "enum_constant" {
                        let (c_start, c_end) = line_range(child, source);
                        let c_name = child
                            .child_by_field_name("name")
                            .map(|n| node_text(n, source).to_string())
                            .unwrap_or_else(|| "constant".into());
                        children.push(SymbolNode {
                            name: c_name,
                            kind: SymbolKind::Constant,
                            detail: None,
                            visibility: None,
                            start_line: c_start,
                            end_line: c_end,
                            children: Vec::new(),
                        });
                    } else if child.kind() == "class_member" {
                        if let Some(member_sym) = extract_dart_class_member(child, source) {
                            children.push(member_sym);
                        }
                    }
                }
            }
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
        "function_declaration" => {
            let signature = node.child_by_field_name("signature")?;
            extract_dart_function_signature(signature, source, start_line, end_line)
        }
        "top_level_variable_declaration" => {
            let var_type = node
                .child_by_field_name("type")
                .or_else(|| find_child_by_kind(node, "type"))
                .map(|n| node_text(n, source).trim().to_string());
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "static_final_declaration_list"
                    || child.kind() == "initialized_identifier_list"
                {
                    let mut list_cursor = child.walk();
                    for item in child.children(&mut list_cursor) {
                        if item.kind() == "static_final_declaration"
                            || item.kind() == "initialized_identifier"
                        {
                            let name = item
                                .child_by_field_name("name")
                                .map(|n| node_text(n, source).to_string())
                                .unwrap_or_else(|| "var".into());
                            let visibility = get_dart_visibility(&name);
                            let detail = var_type.as_ref().map(|t| format!(": {}", t));
                            return Some(SymbolNode {
                                name,
                                kind: SymbolKind::Field,
                                detail,
                                visibility,
                                start_line,
                                end_line,
                                children: Vec::new(),
                            });
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_dart_class_body(body: Node<'_>, source: &str) -> Vec<SymbolNode> {
    let mut symbols = Vec::new();
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == "class_member" {
            if let Some(member) = extract_dart_class_member(child, source) {
                symbols.push(member);
            }
        }
    }
    symbols
}

fn extract_dart_class_member(member: Node<'_>, source: &str) -> Option<SymbolNode> {
    let (start_line, end_line) = line_range(member, source);
    let mut cursor = member.walk();
    for child in member.children(&mut cursor) {
        match child.kind() {
            "method_declaration" => {
                if let Some(sig) = child.child_by_field_name("signature") {
                    return extract_dart_method_signature(sig, source, start_line, end_line);
                }
            }
            "declaration" => {
                // Check if this declaration is a constructor signature (e.g. `ClassName(this.param);`)
                let mut decl_cursor = child.walk();
                for decl_child in child.children(&mut decl_cursor) {
                    if decl_child.kind() == "constructor_signature"
                        || decl_child.kind() == "factory_constructor_signature"
                    {
                        return extract_dart_constructor_signature(decl_child, source, start_line, end_line);
                    }
                }

                // Check field declaration
                let field_type = child
                    .child_by_field_name("type")
                    .or_else(|| find_child_by_kind(child, "type"))
                    .map(|n| node_text(n, source).trim().to_string());
                for decl_child in child.children(&mut decl_cursor) {
                    if decl_child.kind() == "initialized_identifier_list"
                        || decl_child.kind() == "static_final_declaration_list"
                    {
                        let mut list_cursor = decl_child.walk();
                        for item in decl_child.children(&mut list_cursor) {
                            if item.kind() == "initialized_identifier"
                                || item.kind() == "static_final_declaration"
                            {
                                let name = item
                                    .child_by_field_name("name")
                                    .map(|n| node_text(n, source).to_string())
                                    .unwrap_or_else(|| "field".into());
                                let visibility = get_dart_visibility(&name);
                                let detail = field_type.as_ref().map(|t| format!(": {}", t));
                                return Some(SymbolNode {
                                    name,
                                    kind: SymbolKind::Field,
                                    detail,
                                    visibility,
                                    start_line,
                                    end_line,
                                    children: Vec::new(),
                                });
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_dart_method_signature(
    sig: Node<'_>,
    source: &str,
    start_line: usize,
    end_line: usize,
) -> Option<SymbolNode> {
    let mut cursor = sig.walk();
    for child in sig.children(&mut cursor) {
        match child.kind() {
            "function_signature" => {
                return extract_dart_function_signature(child, source, start_line, end_line);
            }
            "getter_signature" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| node_text(n, source).to_string())
                    .unwrap_or_else(|| "getter".into());
                let ret_type = child
                    .child_by_field_name("return_type")
                    .map(|n| format!(": {}", node_text(n, source).trim()));
                let visibility = get_dart_visibility(&name);
                return Some(SymbolNode {
                    name,
                    kind: SymbolKind::Property,
                    detail: ret_type.map(|t| format!("{} (getter)", t)),
                    visibility,
                    start_line,
                    end_line,
                    children: Vec::new(),
                });
            }
            "setter_signature" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| node_text(n, source).to_string())
                    .unwrap_or_else(|| "setter".into());
                let params = child
                    .child_by_field_name("parameters")
                    .map(|n| node_text(n, source).trim().to_string())
                    .unwrap_or_else(|| "()".into());
                let visibility = get_dart_visibility(&name);
                return Some(SymbolNode {
                    name,
                    kind: SymbolKind::Property,
                    detail: Some(format!("{} (setter)", params)),
                    visibility,
                    start_line,
                    end_line,
                    children: Vec::new(),
                });
            }
            "constructor_signature" | "factory_constructor_signature" => {
                return extract_dart_constructor_signature(child, source, start_line, end_line);
            }
            _ => {}
        }
    }
    None
}

fn extract_dart_function_signature(
    node: Node<'_>,
    source: &str,
    start_line: usize,
    end_line: usize,
) -> Option<SymbolNode> {
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(n, source).to_string())
        .unwrap_or_else(|| "fn".into());
    let params = node
        .child_by_field_name("parameters")
        .map(|n| node_text(n, source).trim().to_string())
        .unwrap_or_else(|| "()".into());
    let ret_type = node
        .child_by_field_name("return_type")
        .map(|n| format!(": {}", node_text(n, source).trim()));
    let detail = Some(format!("{}{}", params, ret_type.unwrap_or_default()));
    let visibility = get_dart_visibility(&name);
    Some(SymbolNode {
        name,
        kind: SymbolKind::Method,
        detail,
        visibility,
        start_line,
        end_line,
        children: Vec::new(),
    })
}

fn extract_dart_constructor_signature(
    node: Node<'_>,
    source: &str,
    start_line: usize,
    end_line: usize,
) -> Option<SymbolNode> {
    let mut names = Vec::new();
    let mut params_str = "()".to_string();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" || child.kind() == "type_identifier" {
            names.push(node_text(child, source).trim().to_string());
        } else if child.kind() == "formal_parameter_list" {
            params_str = node_text(child, source).trim().to_string();
        }
    }
    let name = if names.is_empty() {
        "Constructor".into()
    } else {
        names.join(".")
    };
    let visibility = get_dart_visibility(&name);
    Some(SymbolNode {
        name,
        kind: SymbolKind::Constructor,
        detail: Some(params_str),
        visibility,
        start_line,
        end_line,
        children: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dart_outline_extraction() {
        let code = r#"
class OrderDeliveryUpdateNotifier extends Notifier<OrderDeliveryUpdateState> {
  OrderDeliveryUpdateNotifier(this.repo);
  factory OrderDeliveryUpdateNotifier.create() => OrderDeliveryUpdateNotifier(null);

  @override
  OrderDeliveryUpdateState build() => const OrderDeliveryUpdateState();

  final int _id = 1;

  int get myId => _id;
  set myId(int v) => _id = v;

  void reset() {
    state = const OrderDeliveryUpdateState();
  }

  Future<void> init() async {
  }
}

enum OrderStatus { pending, done }

mixin DeliveryHelper {
  void helperMethod() {}
}

extension StringExt on String {
  bool get isValid => isNotEmpty;
}
"#;
        let outline = extract_outline(code, "test.dart", Some("dart"));
        println!("Rendered Dart Outline:\n{}", outline.render_ascii_tree());
        assert_eq!(outline.symbols.len(), 4);
        assert_eq!(outline.symbols[0].name, "OrderDeliveryUpdateNotifier");
        assert_eq!(outline.symbols[0].children.len(), 8);
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
        "lexical_declaration" | "variable_declaration" => extract_ts_function_binding(node, source),
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


fn extract_ts_function_binding(node: Node<'_>, source: &str) -> Option<SymbolNode> {
    let mut cursor = node.walk();
    for declarator in node.children(&mut cursor) {
        if declarator.kind() != "variable_declarator" {
            continue;
        }

        let value = declarator.child_by_field_name("value")?;
        if !matches!(value.kind(), "arrow_function" | "function_expression") {
            continue;
        }

        let name = declarator
            .child_by_field_name("name")
            .map(|name| node_text(name, source).trim().to_string())?;
        let params = value
            .child_by_field_name("parameters")
            .or_else(|| value.child_by_field_name("parameter"))
            .map(|params| node_text(params, source).trim().to_string())
            .map(|params| if params.starts_with('(') { params } else { format!("({params})") })
            .unwrap_or_else(|| "()".into());
        let (start_line, end_line) = line_range(declarator, source);

        return Some(SymbolNode {
            name,
            kind: SymbolKind::Function,
            detail: Some(params),
            visibility: None,
            start_line,
            end_line,
            children: extract_ts_function_children(value, source),
        });
    }
    None
}

fn extract_ts_function_children(function: Node<'_>, source: &str) -> Vec<SymbolNode> {
    let mut children = Vec::new();
    extract_ts_parameters(function, source, &mut children);

    let Some(body) = function.child_by_field_name("body") else {
        return children;
    };
    if body.kind() != "statement_block" {
        return children;
    }

    let mut cursor = body.walk();
    for statement in body.children(&mut cursor) {
        match statement.kind() {
            "lexical_declaration" | "variable_declaration" => {
                extract_ts_local_bindings(statement, source, &mut children);
            }
            "expression_statement" => {
                if let Some(effect) = extract_ts_effect_callback(statement, source) {
                    children.push(effect);
                }
            }
            _ => {}
        }
    }
    children
}

fn extract_ts_parameters(function: Node<'_>, source: &str, children: &mut Vec<SymbolNode>) {
    let Some(params) = function
        .child_by_field_name("parameters")
        .or_else(|| function.child_by_field_name("parameter"))
    else {
        return;
    };

    let parameter_node = if params.kind() == "formal_parameters" {
        let mut cursor = params.walk();
        let parameter = params.children(&mut cursor).find(|child| child.is_named());
        parameter
    } else {
        Some(params)
    };
    let Some(parameter_node) = parameter_node else {
        return;
    };

    if parameter_node.kind() == "object_pattern" {
        children.extend(extract_ts_object_pattern_properties(parameter_node, source));
    }
}

fn extract_ts_local_bindings(node: Node<'_>, source: &str, children: &mut Vec<SymbolNode>) {
    let mut cursor = node.walk();
    for declarator in node.children(&mut cursor) {
        if declarator.kind() != "variable_declarator" {
            continue;
        }

        let Some(name) = declarator.child_by_field_name("name") else {
            continue;
        };
        let value = declarator.child_by_field_name("value");
        let (start_line, end_line) = line_range(declarator, source);

        if name.kind() == "array_pattern" {
            let state_detail = value
                .filter(|value| is_ts_call_to(*value, source, "useState"))
                .map(|value| format!(" state from {}", node_text(value, source).trim()));
            let mut pattern_cursor = name.walk();
            for (index, element) in name
                .children(&mut pattern_cursor)
                .filter(|element| element.is_named())
                .enumerate()
            {
                children.push(SymbolNode {
                    name: node_text(element, source).trim().to_string(),
                    kind: SymbolKind::Property,
                    detail: if index == 1 && state_detail.is_some() {
                        Some(" state setter".into())
                    } else {
                        state_detail.clone()
                    },
                    visibility: None,
                    start_line,
                    end_line,
                    children: Vec::new(),
                });
            }
            continue;
        }

        if name.kind() == "object_pattern" {
            let detail = value.map(|value| format!(" from {}", shorten_ts_detail(node_text(value, source))));
            let mut properties = extract_ts_object_pattern_properties(name, source);
            for property in &mut properties {
                property.detail = detail.clone();
            }
            children.extend(properties);
            continue;
        }

        let name = node_text(name, source).trim().to_string();
        let is_function = value.is_some_and(|value| matches!(value.kind(), "arrow_function" | "function_expression"));
        let nested_children = if is_function {
            value.map(|value| extract_ts_function_children(value, source)).unwrap_or_default()
        } else {
            value
                .filter(|value| value.kind() == "object")
                .map(|value| extract_ts_object_properties(value, source))
                .unwrap_or_default()
        };
        let detail = if is_function {
            None
        } else {
            value.map(|value| format!(" from {}", shorten_ts_detail(node_text(value, source))))
        };
        children.push(SymbolNode {
            name,
            kind: if is_function { SymbolKind::Function } else { SymbolKind::Property },
            detail,
            visibility: None,
            start_line,
            end_line,
            children: nested_children,
        });
    }
}

fn extract_ts_object_pattern_properties(pattern: Node<'_>, source: &str) -> Vec<SymbolNode> {
    let mut properties = Vec::new();
    let mut cursor = pattern.walk();
    for entry in pattern.children(&mut cursor) {
        if !entry.is_named() {
            continue;
        }
        let (start_line, end_line) = line_range(entry, source);
        let key = entry.child_by_field_name("key");
        let value = entry.child_by_field_name("value");
        let (name, detail) = match (key, value) {
            (Some(key), Some(value)) => {
                let key = node_text(key, source).trim();
                let value = node_text(value, source).trim();
                if key == value {
                    (value.to_string(), None)
                } else {
                    (value.to_string(), Some(format!("alias of {key}")))
                }
            }
            (_, Some(value)) => (node_text(value, source).trim().to_string(), None),
            _ => (node_text(entry, source).trim().to_string(), None),
        };
        properties.push(SymbolNode {
            name,
            kind: SymbolKind::Property,
            detail,
            visibility: None,
            start_line,
            end_line,
            children: Vec::new(),
        });
    }
    properties
}

fn is_ts_call_to(node: Node<'_>, source: &str, function_name: &str) -> bool {
    node.kind() == "call_expression"
        && node
            .child_by_field_name("function")
            .is_some_and(|function| node_text(function, source).trim() == function_name)
}

fn shorten_ts_detail(text: &str) -> String {
    const MAX_DETAIL_LENGTH: usize = 80;
    let text = text.trim().replace(['\n', '\r'], " ");
    if text.chars().count() <= MAX_DETAIL_LENGTH {
        return text;
    }
    format!("{}...", text.chars().take(MAX_DETAIL_LENGTH).collect::<String>())
}

fn extract_ts_effect_callback(statement: Node<'_>, source: &str) -> Option<SymbolNode> {
    let expression = statement
        .child_by_field_name("expression")
        .or_else(|| {
            let mut cursor = statement.walk();
            let expression = statement.children(&mut cursor).find(|child| child.is_named());
            expression
        })?;
    if expression.kind() != "call_expression" {
        return None;
    }
    let function = expression.child_by_field_name("function")?;
    if node_text(function, source).trim() != "useEffect" {
        return None;
    }
    let arguments = expression.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let callback = arguments
        .children(&mut cursor)
        .find(|argument| matches!(argument.kind(), "arrow_function" | "function_expression"))?;
    let (start_line, end_line) = line_range(expression, source);
    Some(SymbolNode {
        name: "useEffect".into(),
        kind: SymbolKind::Function,
        detail: Some("callback".into()),
        visibility: None,
        start_line,
        end_line,
        children: extract_ts_function_children(callback, source),
    })
}

fn extract_ts_object_properties(object: Node<'_>, source: &str) -> Vec<SymbolNode> {
    let mut properties = Vec::new();
    let mut cursor = object.walk();
    for pair in object.children(&mut cursor) {
        if pair.kind() != "pair" {
            continue;
        }
        let Some(key) = pair.child_by_field_name("key") else {
            continue;
        };
        let (start_line, end_line) = line_range(pair, source);
        properties.push(SymbolNode {
            name: node_text(key, source).trim().to_string(),
            kind: SymbolKind::Property,
            detail: None,
            visibility: None,
            start_line,
            end_line,
            children: Vec::new(),
        });
    }
    properties
}


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
