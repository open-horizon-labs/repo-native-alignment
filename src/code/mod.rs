use std::path::Path;

use anyhow::Result;

use crate::types::{CodeSymbol, SymbolKind};

/// Recursively find all `.rs` files under `repo_root`, excluding `.git/` and `target/`,
/// parse each with tree-sitter, and return the collected symbols.
pub fn extract_symbols(repo_root: &Path) -> Result<Vec<CodeSymbol>> {
    let mut symbols = Vec::new();
    walk_dir(repo_root, &mut symbols)?;
    Ok(symbols)
}

fn walk_dir(dir: &Path, symbols: &mut Vec<CodeSymbol>) -> Result<()> {
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        // Skip .git/ and target/ directories
        if path.is_dir() {
            if name == ".git" || name == "target" {
                continue;
            }
            walk_dir(&path, symbols)?;
        } else if path.extension().map_or(false, |ext| ext == "rs") {
            match parse_rust_file(&path) {
                Ok(file_symbols) => symbols.extend(file_symbols),
                Err(e) => {
                    tracing::warn!("Failed to parse {}: {}", path.display(), e);
                }
            }
        }
    }
    Ok(())
}

/// Parse a single Rust file with tree-sitter and extract code symbols.
pub fn parse_rust_file(path: &Path) -> Result<Vec<CodeSymbol>> {
    let source = std::fs::read_to_string(path)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
    let tree = parser
        .parse(&source, None)
        .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

    let mut symbols = Vec::new();
    let root = tree.root_node();
    collect_symbols(root, path, source.as_bytes(), &None, &mut symbols);
    Ok(symbols)
}

fn collect_symbols(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    parent_scope: &Option<String>,
    symbols: &mut Vec<CodeSymbol>,
) {
    let kind_str = node.kind();

    let symbol_kind = match kind_str {
        "function_item" => Some(SymbolKind::Function),
        "struct_item" => Some(SymbolKind::Struct),
        "trait_item" => Some(SymbolKind::Trait),
        "impl_item" => Some(SymbolKind::Impl),
        "enum_item" => Some(SymbolKind::Enum),
        "const_item" => Some(SymbolKind::Const),
        "mod_item" => Some(SymbolKind::Module),
        "use_declaration" => Some(SymbolKind::Import),
        _ => None,
    };

    if let Some(kind) = symbol_kind {
        let name = extract_name(&node, &kind, source);
        let body = node.utf8_text(source).unwrap_or("").to_string();
        let signature = extract_signature(&body);
        let line_start = node.start_position().row + 1;
        let line_end = node.end_position().row + 1;

        let sym = CodeSymbol {
            file_path: path.to_path_buf(),
            name: name.clone(),
            kind: kind.clone(),
            line_start,
            line_end,
            signature,
            parent_scope: parent_scope.clone(),
            body,
        };
        symbols.push(sym);

        // For impl blocks, recurse into children with the impl target as parent scope
        if kind == SymbolKind::Impl {
            let scope = Some(name);
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    collect_symbols(child, path, source, &scope, symbols);
                }
            }
            return; // already recursed children
        }
    }

    // Recurse into children for non-impl nodes (or nodes that aren't symbols)
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            collect_symbols(child, path, source, parent_scope, symbols);
        }
    }
}

/// Extract the symbol name from a tree-sitter node.
fn extract_name(node: &tree_sitter::Node, kind: &SymbolKind, source: &[u8]) -> String {
    match kind {
        SymbolKind::Impl => {
            // For impl blocks, the type being implemented is in the "type" field
            if let Some(type_node) = node.child_by_field_name("type") {
                type_node
                    .utf8_text(source)
                    .unwrap_or("unknown")
                    .to_string()
            } else {
                // Fallback: try to find the type from the "trait" field (for trait impls)
                if let Some(trait_node) = node.child_by_field_name("trait") {
                    let trait_name = trait_node
                        .utf8_text(source)
                        .unwrap_or("")
                        .to_string();
                    if let Some(type_node) = node.child_by_field_name("type") {
                        format!(
                            "{} for {}",
                            trait_name,
                            type_node.utf8_text(source).unwrap_or("unknown")
                        )
                    } else {
                        trait_name
                    }
                } else {
                    "unknown".to_string()
                }
            }
        }
        SymbolKind::Import => {
            // use declarations don't have a "name" field — use the full text
            node.utf8_text(source)
                .unwrap_or("unknown")
                .to_string()
                .trim()
                .to_string()
        }
        _ => {
            // Most items (function, struct, trait, enum, const, mod) have a "name" field
            if let Some(name_node) = node.child_by_field_name("name") {
                name_node
                    .utf8_text(source)
                    .unwrap_or("unknown")
                    .to_string()
            } else {
                "unknown".to_string()
            }
        }
    }
}

/// Extract the signature: text from the node start up to the first `{`, or the first line.
fn extract_signature(body: &str) -> String {
    // Try to find the first `{` and take everything before it
    if let Some(brace_pos) = body.find('{') {
        let sig = body[..brace_pos].trim();
        if !sig.is_empty() {
            return sig.to_string();
        }
    }
    // Fallback: first line
    body.lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Case-insensitive substring search across symbol name, signature, and body.
pub fn search_symbols<'a>(symbols: &'a [CodeSymbol], query: &str) -> Vec<&'a CodeSymbol> {
    let query_lower = query.to_lowercase();
    symbols
        .iter()
        .filter(|sym| {
            sym.name.to_lowercase().contains(&query_lower)
                || sym.signature.to_lowercase().contains(&query_lower)
                || sym.body.to_lowercase().contains(&query_lower)
        })
        .collect()
}
