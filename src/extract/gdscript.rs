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

/// Scan a subtree for `preload("res://...")` and `load("res://...")` calls.
/// Creates DependsOn edges from the containing file to the referenced resource.
fn collect_resource_refs(
    node: &tree_sitter::Node,
    path: &Path,
    source: &[u8],
    edges: &mut Vec<Edge>,
) {
    if node.kind() == "call"
        && let Some(func_node) = node.child_by_field_name("function")
    {
        let func_name = func_node.utf8_text(source).unwrap_or("");
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
