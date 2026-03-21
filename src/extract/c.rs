//! C tree-sitter extractor.
//!
//! Generic path: functions, structs, unions, enums, macros, fields.
//! Special cases:
//!   - `function_definition` with complex `declarator` name extraction
//!   - `preproc_def` / `preproc_function_def` as Macro nodes
//!
//! C uses the same tree-sitter-c grammar (distinct from tree-sitter-cpp).
//! We reuse most of the CPP_CONFIG patterns but with the C language function.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::configs::C_CONFIG;
use super::generic::GenericExtractor;
use super::{ExtractionResult, Extractor};

pub struct CExtractor;

impl CExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for CExtractor {
    fn extensions(&self) -> &[&str] {
        // NOTE: .c and .h files only — .cpp/.cc/.cxx/.hpp go to CppExtractor.
        // The CppExtractor already handles .c and .h via the cpp grammar which
        // is a superset, but we add a dedicated C extractor for pure C files
        // that registers with the C grammar for better accuracy.
        &["c", "h"]
    }

    fn name(&self) -> &str {
        "c-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut result = GenericExtractor::new(&C_CONFIG).run(path, content)?;

        // C-specific: functions with complex declarators.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_c::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            collect_c_functions(tree.root_node(), path, content.as_bytes(), &mut result.nodes);
        }

        Ok(result)
    }
}

/// Extract the simple identifier from a C declarator node.
fn extract_c_name(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => Some(node.utf8_text(source).unwrap_or("unknown").to_string()),
        "function_declarator" => node
            .child_by_field_name("declarator")
            .and_then(|n| extract_c_name(n, source)),
        "pointer_declarator" => {
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    if let Some(name) = extract_c_name(child, source) {
                        return Some(name);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Walk AST for C function definitions (complex declarator extraction).
fn collect_c_functions(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
) {
    if node.kind() == "function_definition" {
        if let Some(declarator) = node.child_by_field_name("declarator") {
            if let Some(name) = extract_c_name(declarator, source) {
                let body = node.utf8_text(source).unwrap_or("").to_string();
                let sig = body.lines().next().unwrap_or("").trim().to_string();

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name,
                        kind: NodeKind::Function,
                    },
                    language: "c".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: sig,
                    body,
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });
            }
        }
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_c_functions(child, path, source, nodes);
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
    fn test_c_extract_functions() {
        let extractor = CExtractor::new();
        let code = r#"
#include <stdio.h>

int add(int a, int b) {
    return a + b;
}

void print_hello(void) {
    printf("Hello, world!\n");
}
"#;
        let result = extractor.extract(Path::new("src/math.c"), code).unwrap();
        let funcs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Function)
            .collect();
        assert!(!funcs.is_empty(), "Should extract C functions");
        let names: Vec<&str> = funcs.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"add"), "Should find 'add'");
        assert!(names.contains(&"print_hello"), "Should find 'print_hello'");
    }

    #[test]
    fn test_c_extract_structs() {
        let extractor = CExtractor::new();
        let code = r#"
struct Point {
    int x;
    int y;
};

typedef struct {
    float r;
    float g;
    float b;
} Color;
"#;
        let result = extractor.extract(Path::new("src/types.h"), code).unwrap();
        let structs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Struct)
            .collect();
        assert!(!structs.is_empty(), "Should extract C structs");
    }

    #[test]
    fn test_c_extract_macros() {
        let extractor = CExtractor::new();
        let code = r#"
#define MAX_SIZE 1024
#define MIN(a, b) ((a) < (b) ? (a) : (b))
"#;
        let result = extractor.extract(Path::new("src/defs.h"), code).unwrap();
        let macros: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Macro)
            .collect();
        assert_eq!(macros.len(), 2, "Should extract 2 macros");
        let names: Vec<&str> = macros.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"MAX_SIZE"));
        assert!(names.contains(&"MIN"));
    }

    #[test]
    fn test_c_extractor_extensions() {
        let extractor = CExtractor::new();
        assert!(extractor.extensions().contains(&"c"));
        assert!(extractor.extensions().contains(&"h"));
        assert_eq!(extractor.name(), "c-tree-sitter");
    }

    #[test]
    fn test_c_extract_enum() {
        let extractor = CExtractor::new();
        let code = r#"
enum Color {
    RED,
    GREEN,
    BLUE
};
"#;
        let result = extractor.extract(Path::new("src/color.h"), code).unwrap();
        let enums: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Enum)
            .collect();
        assert!(!enums.is_empty(), "Should extract C enums");
    }
}
