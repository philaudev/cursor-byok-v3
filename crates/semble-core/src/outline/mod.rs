//! File structure and outline symbol extraction.

pub mod tree_sitter;
pub mod types;

pub use tree_sitter::extract_outline;
pub use types::{FileOutline, SymbolKind, SymbolNode};
