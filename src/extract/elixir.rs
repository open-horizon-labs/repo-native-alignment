//! Elixir tree-sitter extractor.
//!
//! Elixir's grammar represents all macro calls (def, defp, defmodule, defmacro)
//! as `call` nodes with a `do_block`. There are no dedicated function/module node
//! types like other languages. This extractor uses a custom AST walker to identify
//! these patterns from call node children.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::{ExtractionResult, Extractor};

pub struct ElixirExtractor;

impl Default for ElixirExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl ElixirExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for ElixirExtractor {
    fn extensions(&self) -> &[&str] {
        &["ex", "exs"]
    }

    fn name(&self) -> &str {
        "elixir-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_elixir::LANGUAGE.into())?;

        let tree = match parser.parse(content, None) {
            Some(t) => t,
            None => return Ok(ExtractionResult::default()),
        };

        let mut nodes = Vec::new();
        collect_elixir_nodes(tree.root_node(), path, content.as_bytes(), None, &mut nodes);

        Ok(ExtractionResult {
            nodes,
            edges: vec![],
        })
    }
}

/// Walk the Elixir AST. Elixir uses `call` nodes for all macro invocations.
/// We identify `defmodule`, `def`, `defp`, `defmacro`, `defmacrop` by inspecting
/// the first child of `call` nodes.
fn collect_elixir_nodes(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    scope: Option<&str>,
    nodes: &mut Vec<Node>,
) {
    if node.kind() == "call" {
        // The first child is typically the macro name (identifier or dot access)
        if let Some(name_node) = node.child(0) {
            let macro_name = name_node.utf8_text(source).unwrap_or("").trim().to_string();

            match macro_name.as_str() {
                "defmodule" => {
                    // defmodule ModuleName do ... end
                    // Second child is the module name (argument)
                    if let Some(args) = node.child(1) {
                        let mod_name = extract_elixir_module_name(args, source)
                            .unwrap_or_else(|| "Unknown".to_string());

                        nodes.push(Node {
                            id: NodeId {
                                root: String::new(),
                                file: path.to_path_buf(),
                                name: mod_name.clone(),
                                kind: NodeKind::Module,
                            },
                            language: "elixir".to_string(),
                            line_start: node.start_position().row + 1,
                            line_end: node.end_position().row + 1,
                            signature: format!("defmodule {}", mod_name),
                            body: node.utf8_text(source).unwrap_or("").to_string(),
                            metadata: BTreeMap::new(),
                            source: ExtractionSource::TreeSitter,
                        });

                        // Recurse into module body with the module name as scope
                        for i in 0..node.child_count() {
                            if let Some(child) = node.child(i as u32) {
                                collect_elixir_nodes(child, path, source, Some(&mod_name), nodes);
                            }
                        }
                        return;
                    }
                }
                "def" | "defp" => {
                    // def function_name(args) do ... end
                    if let Some(args) = node.child(1)
                        && let Some(fn_name) = extract_elixir_function_name(args, source)
                    {
                        let qualified = match scope {
                            Some(s) => format!("{}.{}", s, fn_name),
                            None => fn_name.clone(),
                        };
                        let body = node.utf8_text(source).unwrap_or("").to_string();
                        let sig = body.lines().next().unwrap_or("").trim().to_string();

                        nodes.push(Node {
                            id: NodeId {
                                root: String::new(),
                                file: path.to_path_buf(),
                                name: qualified,
                                kind: NodeKind::Function,
                            },
                            language: "elixir".to_string(),
                            line_start: node.start_position().row + 1,
                            line_end: node.end_position().row + 1,
                            signature: sig,
                            body,
                            metadata: BTreeMap::new(),
                            source: ExtractionSource::TreeSitter,
                        });
                    }
                }
                "defmacro" | "defmacrop" => {
                    // defmacro macro_name(args) do ... end
                    if let Some(args) = node.child(1)
                        && let Some(macro_fn_name) = extract_elixir_function_name(args, source)
                    {
                        let qualified = match scope {
                            Some(s) => format!("{}.{}", s, macro_fn_name),
                            None => macro_fn_name.clone(),
                        };
                        let body = node.utf8_text(source).unwrap_or("").to_string();
                        let sig = body.lines().next().unwrap_or("").trim().to_string();

                        nodes.push(Node {
                            id: NodeId {
                                root: String::new(),
                                file: path.to_path_buf(),
                                name: qualified,
                                kind: NodeKind::Macro,
                            },
                            language: "elixir".to_string(),
                            line_start: node.start_position().row + 1,
                            line_end: node.end_position().row + 1,
                            signature: sig,
                            body,
                            metadata: BTreeMap::new(),
                            source: ExtractionSource::TreeSitter,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_elixir_nodes(child, path, source, scope, nodes);
        }
    }
}

/// Extract module name from the arguments part of a `defmodule` call.
/// The module name is typically an alias node (e.g., MyApp.UserRepository).
fn extract_elixir_module_name(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "alias" | "identifier" => Some(node.utf8_text(source).unwrap_or("Unknown").to_string()),
        "arguments" => {
            // First child of arguments is the module name
            node.child(0)
                .and_then(|c| extract_elixir_module_name(c, source))
        }
        _ => {
            // Try reading the full text as fallback
            let text = node.utf8_text(source).unwrap_or("").trim().to_string();
            if !text.is_empty() && !text.starts_with('(') {
                Some(text)
            } else {
                None
            }
        }
    }
}

/// Extract the function name from the arguments part of a `def`/`defp` call.
/// The function call is typically a `call` node with the function name as first child.
fn extract_elixir_function_name(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => Some(node.utf8_text(source).unwrap_or("").to_string()),
        "call" => {
            // def call(args) — the function name is the first child
            node.child(0).and_then(|n| {
                let text = n.utf8_text(source).unwrap_or("").to_string();
                if !text.is_empty() { Some(text) } else { None }
            })
        }
        "arguments" => {
            // arguments wrapping the call
            node.child(0)
                .and_then(|c| extract_elixir_function_name(c, source))
        }
        _ => {
            let text = node.utf8_text(source).unwrap_or("").trim().to_string();
            // Take just the first token (before any whitespace/paren)
            let first_token = text
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next();
            first_token.filter(|s| !s.is_empty()).map(|s| s.to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_elixir_extract_module() {
        let extractor = ElixirExtractor::new();
        let code = r#"
defmodule MyApp.UserRepository do
  def find(id) do
    {:ok, %User{id: id}}
  end

  defp validate(user) do
    user.name != nil
  end
end
"#;
        let result = extractor
            .extract(Path::new("lib/user_repository.ex"), code)
            .unwrap();
        let modules: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Module)
            .collect();
        assert!(
            !modules.is_empty(),
            "Should extract Elixir modules, got: {:?}",
            result
                .nodes
                .iter()
                .map(|n| (&n.id.name, &n.id.kind))
                .collect::<Vec<_>>()
        );

        let funcs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Function)
            .collect();
        assert!(
            !funcs.is_empty(),
            "Should extract Elixir functions (def/defp), got: {:?}",
            result
                .nodes
                .iter()
                .map(|n| (&n.id.name, &n.id.kind))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_elixir_extract_macro() {
        let extractor = ElixirExtractor::new();
        let code = r#"
defmodule MyMacros do
  defmacro debug(expr) do
    quote do
      IO.inspect(unquote(expr), label: unquote(to_string(expr)))
    end
  end
end
"#;
        let result = extractor
            .extract(Path::new("lib/my_macros.ex"), code)
            .unwrap();
        let macros: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Macro)
            .collect();
        assert!(
            !macros.is_empty(),
            "Should extract Elixir defmacro, got: {:?}",
            result
                .nodes
                .iter()
                .map(|n| (&n.id.name, &n.id.kind))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_elixir_extractor_extensions() {
        let extractor = ElixirExtractor::new();
        assert!(extractor.extensions().contains(&"ex"));
        assert!(extractor.extensions().contains(&"exs"));
        assert_eq!(extractor.name(), "elixir-tree-sitter");
    }

    #[test]
    fn test_elixir_multiple_functions() {
        let extractor = ElixirExtractor::new();
        let code = r#"
defmodule Calculator do
  def add(a, b), do: a + b
  def subtract(a, b), do: a - b
  def multiply(a, b), do: a * b
end
"#;
        let result = extractor
            .extract(Path::new("lib/calculator.ex"), code)
            .unwrap();
        let funcs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Function)
            .collect();
        assert!(
            !funcs.is_empty(),
            "Should extract multiple Elixir functions"
        );
    }
}
