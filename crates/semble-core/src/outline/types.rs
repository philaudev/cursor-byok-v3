//! Symbol and outline data structures.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Class,
    Interface,
    Struct,
    Enum,
    Trait,
    Record,
    Function,
    Method,
    Constructor,
    Field,
    Property,
    Constant,
    TypeAlias,
    Module,
    Package,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Record => "record",
            Self::Function => "function",
            Self::Method => "method",
            Self::Constructor => "constructor",
            Self::Field => "field",
            Self::Property => "property",
            Self::Constant => "constant",
            Self::TypeAlias => "type",
            Self::Module => "module",
            Self::Package => "package",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolNode {
    pub name: String,
    pub kind: SymbolKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SymbolNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOutline {
    pub file_path: String,
    pub language: Option<String>,
    pub total_lines: usize,
    pub symbols: Vec<SymbolNode>,
}

impl FileOutline {
    pub fn render_ascii_tree(&self) -> String {
        let mut output = String::new();
        output.push_str(&format!(
            "Outline for {} ({} lines, language: {})\n",
            self.file_path,
            self.total_lines,
            self.language.as_deref().unwrap_or("unknown")
        ));

        if self.symbols.is_empty() {
            output.push_str("  (No high-level symbols found)\n");
            return output;
        }

        for (i, symbol) in self.symbols.iter().enumerate() {
            let is_last = i == self.symbols.len() - 1;
            render_symbol_node(&mut output, symbol, "", is_last);
        }

        output
    }
}

fn render_symbol_node(output: &mut String, node: &SymbolNode, prefix: &str, is_last: bool) {
    let branch = if is_last { "└── " } else { "├── " };
    let line_str = if node.start_line == node.end_line {
        format!("[L{}]", node.start_line)
    } else {
        format!("[L{}-L{}]", node.start_line, node.end_line)
    };

    let mut meta = Vec::new();
    meta.push(node.kind.as_str());
    if let Some(vis) = &node.visibility {
        meta.push(vis.as_str());
    }

    let detail_str = node.detail.as_deref().unwrap_or("");
    output.push_str(&format!(
        "{}{}{}{} ({}) {}\n",
        prefix,
        branch,
        node.name,
        detail_str,
        meta.join(", "),
        line_str
    ));

    let child_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
    for (i, child) in node.children.iter().enumerate() {
        let child_is_last = i == node.children.len() - 1;
        render_symbol_node(output, child, &child_prefix, child_is_last);
    }
}
