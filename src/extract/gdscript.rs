//! GDScript tree-sitter extractor.
//!
//! Generic path: functions, classes, complexity, decorators.
//! Custom passes: signals, export/onready vars, class_name, extends,
//!   doc comments (##), resource references (preload/load).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

use super::configs::GDSCRIPT_CONFIG;
use super::generic::GenericExtractor;
use super::{ExtractionResult, Extractor};

pub struct GDScriptExtractor;

impl Default for GDScriptExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl GDScriptExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for GDScriptExtractor {
    fn extensions(&self) -> &[&str] {
        &["gd"]
    }

    fn name(&self) -> &str {
        "gdscript-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut result = GenericExtractor::new(&GDSCRIPT_CONFIG).run(path, content)?;

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_gdscript::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            let source = content.as_bytes();
            let mut extends_type: Option<String> = None;
            let mut class_name_idx: Option<usize> = None;

            collect_gdscript_nodes(
                tree.root_node(),
                path,
                source,
                &mut result.nodes,
                &mut result.edges,
                &mut extends_type,
                &mut class_name_idx,
            );

            // Attach extends metadata to the class_name node if both exist.
            if let Some(idx) = class_name_idx
                && let Some(ref ext) = extends_type
            {
                result.nodes[idx]
                    .metadata
                    .insert("extends".to_string(), ext.clone());
            }

            // Godot 4 uses `variable_statement` + `annotations` child (not
            // `export_variable_statement`).  Enrich generic-extracted Field nodes
            // with decorator metadata when annotations are present.
            enrich_annotations(tree.root_node(), source, &mut result.nodes, path);
        }

        Ok(result)
    }
}

fn collect_gdscript_nodes(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    extends_type: &mut Option<String>,
    class_name_idx: &mut Option<usize>,
) {
    for i in 0..node.child_count() {
        let Some(child) = node.child(i as u32) else {
            continue;
        };

        match child.kind() {
            "signal_statement" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = name_node.utf8_text(source).unwrap_or("").to_string();
                    let signature = first_line_trimmed(&child, source);
                    nodes.push(Node {
                        id: NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name,
                            kind: NodeKind::Other("signal".to_string()),
                        },
                        language: "gdscript".to_string(),
                        line_start: child.start_position().row + 1,
                        line_end: child.end_position().row + 1,
                        signature,
                        body: String::new(),
                        metadata: BTreeMap::new(),
                        source: ExtractionSource::TreeSitter,
                    });
                }
            }
            "export_variable_statement" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = name_node.utf8_text(source).unwrap_or("").to_string();
                    let signature = first_line_trimmed(&child, source);
                    let mut metadata = BTreeMap::new();
                    metadata.insert("decorator".to_string(), "@export".to_string());
                    if let Some(type_node) = child.child_by_field_name("type") {
                        let t = type_node.utf8_text(source).unwrap_or("").trim().to_string();
                        if !t.is_empty() {
                            metadata.insert("type".to_string(), t);
                        }
                    }
                    if let Some(val_node) = child.child_by_field_name("value") {
                        let v = val_node.utf8_text(source).unwrap_or("").trim().to_string();
                        if !v.is_empty() {
                            metadata.insert("value".to_string(), v);
                        }
                    }
                    nodes.push(Node {
                        id: NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name,
                            kind: NodeKind::Field,
                        },
                        language: "gdscript".to_string(),
                        line_start: child.start_position().row + 1,
                        line_end: child.end_position().row + 1,
                        signature,
                        body: String::new(),
                        metadata,
                        source: ExtractionSource::TreeSitter,
                    });
                    collect_resource_refs(&child, path, source, edges);
                }
            }
            "onready_variable_statement" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = name_node.utf8_text(source).unwrap_or("").to_string();
                    let signature = first_line_trimmed(&child, source);
                    let mut metadata = BTreeMap::new();
                    metadata.insert("decorator".to_string(), "@onready".to_string());
                    nodes.push(Node {
                        id: NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name,
                            kind: NodeKind::Field,
                        },
                        language: "gdscript".to_string(),
                        line_start: child.start_position().row + 1,
                        line_end: child.end_position().row + 1,
                        signature,
                        body: String::new(),
                        metadata,
                        source: ExtractionSource::TreeSitter,
                    });
                    collect_resource_refs(&child, path, source, edges);
                }
            }
            "class_name_statement" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = name_node.utf8_text(source).unwrap_or("").to_string();
                    let signature = first_line_trimmed(&child, source);
                    let idx = nodes.len();
                    nodes.push(Node {
                        id: NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name,
                            kind: NodeKind::Struct,
                        },
                        language: "gdscript".to_string(),
                        line_start: child.start_position().row + 1,
                        line_end: child.end_position().row + 1,
                        signature,
                        body: String::new(),
                        metadata: BTreeMap::new(),
                        source: ExtractionSource::TreeSitter,
                    });
                    *class_name_idx = Some(idx);
                }
            }
            "extends_statement" => {
                let ext_text = child
                    .utf8_text(source)
                    .unwrap_or("")
                    .trim()
                    .strip_prefix("extends")
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !ext_text.is_empty() {
                    *extends_type = Some(ext_text);
                }
            }
            // Recurse into class_definition bodies to find nested signals/export/onready.
            "class_definition" => {
                if let Some(body) = child.child_by_field_name("body") {
                    collect_gdscript_nodes(
                        body,
                        path,
                        source,
                        nodes,
                        edges,
                        extends_type,
                        class_name_idx,
                    );
                }
            }
            _ => {
                // Scan for preload()/load() resource references in any statement.
                collect_resource_refs(&child, path, source, edges);
            }
        }
    }
}

/// Godot 4 uses `variable_statement` with an `annotations` child instead of
/// `export_variable_statement`.  Walk the tree, find variable_statements with
/// annotation children, and add decorator metadata to the matching Node.
fn enrich_annotations(
    root: tree_sitter::Node,
    source: &[u8],
    nodes: &mut [Node],
    path: &Path,
) {
    fn walk(n: tree_sitter::Node, source: &[u8], nodes: &mut [Node], path: &Path) {
        if n.kind() == "variable_statement" {
            // Check for annotations child
            if let Some(annots) = (0..n.child_count())
                .filter_map(|i| n.child(i as u32))
                .find(|c| c.kind() == "annotations")
            {
                let line = n.start_position().row + 1;
                let name_text = n.child_by_field_name("name")
                    .and_then(|nm| nm.utf8_text(source).ok())
                    .unwrap_or("");
                // Collect annotation text (e.g. "@export", "@onready")
                let dec_text: Vec<String> = (0..annots.child_count())
                    .filter_map(|i| annots.child(i as u32))
                    .filter(|c| c.kind() == "annotation")
                    .filter_map(|c| c.utf8_text(source).ok())
                    .map(|t| t.trim().to_string())
                    .collect();
                if !dec_text.is_empty() {
                    // Find the matching node
                    if let Some(node) = nodes.iter_mut().find(|nd| {
                        nd.id.file == path
                            && nd.id.name == name_text
                            && nd.line_start == line
                    }) {
                        node.metadata.insert("decorator".to_string(), dec_text.join(", "));
                    }
                }
            }
            return; // Don't recurse into variable children
        }
        for i in 0..n.child_count() {
            if let Some(c) = n.child(i as u32) {
                walk(c, source, nodes, path);
            }
        }
    }
    walk(root, source, nodes, path);
}

/// Scan a subtree for `preload("res://...")` and `load("res://...")` calls.
/// Creates DependsOn edges from the containing file to the referenced resource.
fn collect_resource_refs(
    node: &tree_sitter::Node,
    path: &Path,
    source: &[u8],
    edges: &mut Vec<Edge>,
) {
    if node.kind() == "call" {
        // tree-sitter-gdscript v6.1: function name is the first named child,
        // not a "function" field.  Try field first, fall back to first child.
        let func_name = node
            .child_by_field_name("function")
            .or_else(|| node.child(0))
            .and_then(|n| n.utf8_text(source).ok())
            .unwrap_or("");
        if (func_name == "preload" || func_name == "load")
            && let Some(args) = node.child_by_field_name("arguments")
        {
            for j in 0..args.child_count() {
                if let Some(arg) = args.child(j as u32)
                    && arg.kind() == "string"
                {
                    let raw = arg.utf8_text(source).unwrap_or("").trim().to_string();
                    let value = raw
                        .trim_start_matches('"')
                        .trim_end_matches('"')
                        .trim_start_matches('\'')
                        .trim_end_matches('\'')
                        .to_string();
                    if value.starts_with("res://") || value.starts_with("user://") {
                        // Strip res:// prefix, normalize to relative path
                        let rel_path = value
                            .strip_prefix("res://")
                            .or_else(|| value.strip_prefix("user://"))
                            .unwrap_or(&value);
                        edges.push(Edge {
                            from: NodeId {
                                root: String::new(),
                                file: path.to_path_buf(),
                                name: path
                                    .file_stem()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .to_string(),
                                kind: NodeKind::Module,
                            },
                            to: NodeId {
                                root: String::new(),
                                file: std::path::PathBuf::from(rel_path),
                                name: rel_path
                                    .split('/')
                                    .next_back()
                                    .unwrap_or(rel_path)
                                    .to_string(),
                                kind: NodeKind::Module,
                            },
                            kind: EdgeKind::DependsOn,
                            source: ExtractionSource::TreeSitter,
                            confidence: Confidence::Detected,
                evidence: Vec::new(),
                        });
                    }
                }
            }
        }
    }

    // Recurse into children
    for j in 0..node.child_count() {
        if let Some(child) = node.child(j as u32) {
            collect_resource_refs(&child, path, source, edges);
        }
    }
}

fn first_line_trimmed(node: &tree_sitter::Node, source: &[u8]) -> String {
    node.utf8_text(source)
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}


#[cfg(test)]
mod tests {
    use super::*;

    fn extract(code: &str) -> ExtractionResult {
        let ext = GDScriptExtractor::new();
        ext.extract(Path::new("test.gd"), code).unwrap()
    }

    #[test]
    fn test_signal_extracted() {
        let result = extract("signal health_changed(old_val, new_val)\n");
        let signals: Vec<_> = result.nodes.iter()
            .filter(|n| matches!(&n.id.kind, NodeKind::Other(s) if s == "signal"))
            .collect();
        assert_eq!(signals.len(), 1, "expected 1 signal, got {}", signals.len());
        assert_eq!(signals[0].id.name, "health_changed");
    }

    #[test]
    fn test_export_var_extracted() {
        let result = extract("@export var speed: float = 10.0\n");
        let exports: Vec<_> = result.nodes.iter()
            .filter(|n| n.metadata.get("decorator").map_or(false, |d| d.contains("@export")))
            .collect();
        assert_eq!(exports.len(), 1, "expected 1 export, got {:?}",
            result.nodes.iter().map(|n| (&n.id.name, &n.metadata)).collect::<Vec<_>>());
        assert_eq!(exports[0].id.name, "speed");
        assert_eq!(exports[0].id.kind, NodeKind::Field);
    }

    #[test]
    fn test_onready_var_extracted() {
        let result = extract("@onready var label = $Label\n");
        let onready: Vec<_> = result.nodes.iter()
            .filter(|n| n.metadata.get("decorator").map_or(false, |d| d.contains("@onready")))
            .collect();
        assert_eq!(onready.len(), 1, "expected 1 onready");
        assert_eq!(onready[0].id.name, "label");
    }

    #[test]
    fn test_export_with_preload_emits_depends_on() {
        // Regression: export/onready branches must call collect_resource_refs
        let code = r#"@export var texture = preload("res://assets/icon.png")
"#;
        let ext = GDScriptExtractor::new();
        let result = ext.extract(Path::new("player.gd"), code).unwrap();
        let deps: Vec<_> = result.edges.iter()
            .filter(|e| matches!(e.kind, EdgeKind::DependsOn))
            .collect();
        assert!(!deps.is_empty(), "@export var with preload() should emit DependsOn edge");
    }

    #[test]
    fn test_onready_with_preload_emits_depends_on() {
        let code = r#"@onready var sprite = preload("res://scenes/sprite.tscn")
"#;
        let ext = GDScriptExtractor::new();
        let result = ext.extract(Path::new("enemy.gd"), code).unwrap();
        let deps: Vec<_> = result.edges.iter()
            .filter(|e| matches!(e.kind, EdgeKind::DependsOn))
            .collect();
        assert!(!deps.is_empty(), "@onready var with preload() should emit DependsOn edge");
    }

    #[test]
    fn test_resource_ref_uses_file_stem_not_file_name() {
        // Regression: DependsOn edge from-node should use file_stem ("player"), not file_name ("player.gd")
        let code = r#"var scene = preload("res://levels/main.tscn")
"#;
        let ext = GDScriptExtractor::new();
        let result = ext.extract(Path::new("player.gd"), code).unwrap();
        let deps: Vec<_> = result.edges.iter()
            .filter(|e| matches!(e.kind, EdgeKind::DependsOn))
            .collect();
        assert!(!deps.is_empty(), "expected DependsOn edge");
        assert_eq!(deps[0].from.name, "player", "edge from-node should use file stem, not file name");
    }

    #[test]
    fn test_class_name_and_extends() {
        let code = "class_name Player\nextends CharacterBody2D\n\nfunc _ready():\n\tpass\n";
        let result = extract(code);
        let class_nodes: Vec<_> = result.nodes.iter()
            .filter(|n| n.id.kind == NodeKind::Struct && n.id.name == "Player")
            .collect();
        assert_eq!(class_nodes.len(), 1, "expected class_name node");
        assert_eq!(class_nodes[0].metadata.get("extends").map(|s| s.as_str()), Some("CharacterBody2D"));
    }

    #[test]
    fn test_function_extracted_by_generic() {
        let code = "func _ready():\n\tprint(\"hello\")\n";
        let result = extract(code);
        let fns: Vec<_> = result.nodes.iter()
            .filter(|n| n.id.kind == NodeKind::Function)
            .collect();
        assert!(!fns.is_empty(), "expected at least one function");
        assert!(fns.iter().any(|f| f.id.name == "_ready"), "expected _ready function");
    }

    #[test]
    fn test_gdscript_doc_comment_filtering() {
        // ## doc comments directly above a function should be collected
        let code = "## This is a doc comment\nfunc documented():\n\tpass\n";
        let result = extract(code);
        let func = result.nodes.iter()
            .find(|n| n.id.kind == NodeKind::Function && n.id.name == "documented")
            .expect("expected documented function");
        let doc = func.metadata.get("doc_comment").map(|s| s.as_str()).unwrap_or("");
        assert!(doc.contains("This is a doc comment"), "doc should contain ## comment, got: {}", doc);
    }

    #[test]
    fn test_gdscript_regular_comment_breaks_doc_chain() {
        // A regular # comment between ## doc and func should break the chain
        let code = "## This doc is too far\n# regular comment\nfunc nodoc():\n\tpass\n";
        let result = extract(code);
        let func = result.nodes.iter()
            .find(|n| n.id.kind == NodeKind::Function && n.id.name == "nodoc")
            .expect("expected nodoc function");
        let doc = func.metadata.get("doc_comment").map(|s| s.as_str()).unwrap_or("");
        assert!(doc.is_empty(), "doc should be empty when # comment breaks chain, got: {}", doc);
    }
}