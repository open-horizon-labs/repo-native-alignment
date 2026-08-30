//! Markdown extractor: heading-aware sections as graph nodes with YAML frontmatter.
//!
//! Reuses the existing `pulldown-cmark` parsing from `src/markdown/mod.rs`
//! but produces graph `Node` types for the unified graph model.
//!
//! Emits five kinds of edges:
//! - **Hierarchy (Defines):** parent heading section -> child heading section
//! - **Frontmatter refs (DependsOn):** .oh/ artifact -> referenced outcome/signal/guardrail
//! - **Cross-file links (References):** section containing `[text](path)` -> target file
//! - **ADR validation links (References):** ADR section -> exact test function declared in frontmatter
//! - **Local knowledge relationships:** optional `rna` frontmatter nodes and custom relationship edges

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Result;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::Deserialize;

use crate::graph::{
    Confidence, Edge, EdgeEvidence, EdgeKind, EvidenceDiagnostic, ExtractionSource, Node, NodeId,
    NodeKind, ValidationStatus,
};

use super::{ExtractionResult, Extractor};

/// Extractor for Markdown files. Produces one node per heading section,
/// with heading hierarchy and YAML frontmatter as metadata.
/// Also emits hierarchy, frontmatter reference, and cross-file link edges.
pub struct MarkdownExtractor;

impl Default for MarkdownExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for MarkdownExtractor {
    fn extensions(&self) -> &[&str] {
        &["md", "mdx"]
    }

    fn name(&self) -> &str {
        "markdown"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        // Extract YAML frontmatter if present
        let frontmatter = extract_frontmatter(content);

        // Use existing pulldown-cmark parser for heading-aware chunking
        let chunks = parse_markdown_file_from_source(content, path);

        for (i, chunk) in chunks.iter().enumerate() {
            let section_name = if chunk.is_frontmatter {
                "frontmatter".to_string()
            } else if chunk.heading_hierarchy.is_empty() {
                "preamble".to_string()
            } else {
                chunk
                    .heading_hierarchy
                    .last()
                    .map(|h| h.trim_start_matches('#').trim().to_string())
                    .unwrap_or_else(|| format!("section_{}", i))
            };

            let mut metadata = BTreeMap::new();

            // Heading hierarchy as metadata
            if !chunk.heading_hierarchy.is_empty() {
                metadata.insert(
                    "heading_hierarchy".to_string(),
                    chunk.heading_hierarchy.join(" > "),
                );
            }
            metadata.insert("heading_level".to_string(), chunk.heading_level.to_string());

            // Heading text (without # prefix)
            if !chunk.heading_text.is_empty() {
                metadata.insert("heading_text".to_string(), chunk.heading_text.clone());
            }

            // Parent heading for hierarchy context
            if let Some(ref parent) = chunk.parent_heading {
                metadata.insert("parent_heading".to_string(), parent.clone());
            }

            // Section path breadcrumbs (e.g., "Aim > Mechanism > Hypothesis")
            let sp = chunk.section_path();
            if !sp.is_empty() {
                metadata.insert("section_path".to_string(), sp.clone());
            }

            // Frontmatter flag
            if chunk.is_frontmatter {
                metadata.insert("is_frontmatter".to_string(), "true".to_string());
            }

            // Detect .oh/ artifact kind from file path
            if let Some(oh_kind) = detect_oh_kind(path) {
                metadata.insert("oh_kind".to_string(), oh_kind);
            }

            // Code spans as metadata (potential cross-references)
            if !chunk.code_spans.is_empty() {
                metadata.insert("code_spans".to_string(), chunk.code_spans.join(", "));
            }

            // Attach frontmatter key-value pairs to the frontmatter chunk itself,
            // or to the first non-frontmatter chunk if there's no frontmatter chunk.
            let attach_frontmatter = if chunk.is_frontmatter {
                true
            } else if i == 0 || (i == 1 && chunks.first().is_some_and(|c| c.is_frontmatter)) {
                // First non-frontmatter chunk: attach frontmatter for backward compat
                !frontmatter.is_empty()
            } else {
                false
            };
            if attach_frontmatter {
                for (key, value) in &frontmatter {
                    metadata.insert(format!("frontmatter.{}", key), value.clone());
                }
            }

            // Compute line numbers from byte offsets
            let line_start = content[..chunk.byte_offset]
                .chars()
                .filter(|&c| c == '\n')
                .count()
                + 1;
            let line_end = content[..chunk.byte_offset + chunk.byte_len]
                .chars()
                .filter(|&c| c == '\n')
                .count()
                + 1;

            let node = Node {
                id: NodeId {
                    root: String::new(), // populated during multi-root integration
                    file: path.to_path_buf(),
                    name: section_name,
                    kind: NodeKind::MarkdownSection,
                },
                language: "markdown".to_string(),
                line_start,
                line_end,
                signature: if chunk.is_frontmatter {
                    "[frontmatter]".to_string()
                } else if !sp.is_empty() {
                    sp
                } else {
                    chunk.heading_hierarchy.join(" > ")
                },
                body: chunk.content.clone(),
                metadata,
                source: ExtractionSource::Markdown,
            };
            nodes.push(node);
        }

        // --- Edge emission ---

        // 1. Hierarchy edges: parent section -> child section (Defines)
        emit_hierarchy_edges(&nodes, &mut edges);

        // 2. Frontmatter reference edges: artifact -> referenced artifact (DependsOn)
        emit_frontmatter_ref_edges(&nodes, &frontmatter, path, &mut edges);

        // 3. Cross-file link edges: section -> target file (References)
        emit_link_edges(&nodes, &chunks, path, &mut edges);

        // 4. Local knowledge graph declarations: human-editable frontmatter -> custom nodes/edges.
        emit_local_knowledge_graph(path, content, &mut nodes, &mut edges);

        // 5. Source-addressable body AST. Keep the legacy section nodes above for
        // compatibility while exposing every Markdown body construct through the
        // canonical content-source selector contract.
        emit_body_ast(path, content, &mut nodes, &mut edges);

        Ok(ExtractionResult { nodes, edges })
    }
}

const MARKDOWN_EXTRACTOR_ID: &str = "rna.markdown.pulldown-cmark@1";

#[derive(Debug)]
struct OpenBodyNode {
    kind: &'static str,
    start: usize,
    ordinal: usize,
    parent_path: String,
    explicit_id: Option<String>,
    anchor: Option<String>,
    target: Option<String>,
}

fn emit_body_ast(path: &Path, content: &str, nodes: &mut Vec<Node>, _edges: &mut Vec<Edge>) {
    let mut stack: Vec<OpenBodyNode> = Vec::new();
    let mut sibling_counts: Vec<BTreeMap<&'static str, usize>> = vec![BTreeMap::new()];
    let mut explicit_ids: BTreeMap<String, usize> = BTreeMap::new();
    let options = Options::all();

    for (event, range) in Parser::new_ext(content, options).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                let Some(kind) = body_kind(&tag) else {
                    continue;
                };
                let depth = stack.len();
                while sibling_counts.len() <= depth {
                    sibling_counts.push(BTreeMap::new());
                }
                let ordinal = *sibling_counts[depth].entry(kind).or_insert(0);
                *sibling_counts[depth].entry(kind).or_insert(0) += 1;
                sibling_counts.truncate(depth + 1);
                sibling_counts.push(BTreeMap::new());
                let parent_path = stack.last().map(ast_path).unwrap_or_default();
                let (explicit_id, anchor, target) = tag_identity(&tag, content, range.clone());
                stack.push(OpenBodyNode {
                    kind,
                    start: range.start,
                    ordinal,
                    parent_path,
                    explicit_id,
                    anchor,
                    target,
                });
            }
            Event::End(end) => {
                let Some(kind) = end_kind(end) else { continue };
                let Some(index) = stack.iter().rposition(|open| open.kind == kind) else {
                    continue;
                };
                let open = stack.remove(index);
                let end = range.end.max(open.start).min(content.len());
                let node = make_body_node(path, content, &open, end, &mut explicit_ids);
                if open.kind == "image" {
                    let selected = &content[open.start..end];
                    if let Some(close) = selected.strip_prefix("![").and_then(|rest| rest.find(']'))
                        && close > 0
                    {
                        let caption_start = open.start + 2;
                        emit_leaf_body_node(
                            path,
                            content,
                            "caption",
                            caption_start..caption_start + close,
                            None,
                            ast_path(&open),
                            0,
                            nodes,
                        );
                    }
                }
                nodes.push(node);
            }
            Event::FootnoteReference(label) => {
                let depth = stack.len();
                while sibling_counts.len() <= depth {
                    sibling_counts.push(BTreeMap::new());
                }
                let ordinal = *sibling_counts[depth].entry("citation").or_insert(0);
                *sibling_counts[depth].entry("citation").or_insert(0) += 1;
                let open = OpenBodyNode {
                    kind: "citation",
                    start: range.start,
                    ordinal,
                    parent_path: stack.last().map(ast_path).unwrap_or_default(),
                    explicit_id: None,
                    anchor: None,
                    target: None,
                };
                let mut node =
                    make_body_node(path, content, &open, range.end, &mut BTreeMap::new());
                node.metadata
                    .insert("citation_label".into(), label.to_string());
                nodes.push(node);
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                let kind = if html.trim_start().starts_with("<!--") {
                    "html_comment"
                } else {
                    "html_directive"
                };
                let depth = stack.len();
                while sibling_counts.len() <= depth {
                    sibling_counts.push(BTreeMap::new());
                }
                let ordinal = *sibling_counts[depth].entry(kind).or_insert(0);
                *sibling_counts[depth].entry(kind).or_insert(0) += 1;
                emit_leaf_body_node(
                    path,
                    content,
                    kind,
                    range.clone(),
                    None,
                    stack.last().map(ast_path).unwrap_or_default(),
                    ordinal,
                    nodes,
                );
                let directive = html
                    .trim()
                    .trim_start_matches("<!--")
                    .trim_start_matches('<')
                    .trim()
                    .split(|c: char| c == ':' || c == '>' || c.is_whitespace())
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if matches!(directive.as_str(), "prompt" | "exercise") {
                    let semantic_kind = if directive == "prompt" {
                        "prompt"
                    } else {
                        "exercise"
                    };
                    let ordinal = *sibling_counts[depth].entry(semantic_kind).or_insert(0);
                    *sibling_counts[depth].entry(semantic_kind).or_insert(0) += 1;
                    emit_leaf_body_node(
                        path,
                        content,
                        semantic_kind,
                        range,
                        None,
                        stack.last().map(ast_path).unwrap_or_default(),
                        ordinal,
                        nodes,
                    );
                }
            }
            _ => {}
        }
    }

    for (explicit_id, _count) in explicit_ids.iter().filter(|(_, count)| **count > 1) {
        for node in nodes.iter_mut().filter(|node| {
            node.id.file == path && node.metadata.get("explicit_id") == Some(explicit_id)
        }) {
            node.metadata
                .insert("validation_status".into(), "invalid".into());
            node.metadata
                .insert("diagnostic_code".into(), "content.duplicate_body_id".into());
            node.metadata
                .insert("diagnostic_severity".into(), "error".into());
            node.metadata.insert(
                "diagnostic_message".into(),
                format!("duplicate explicit Markdown body ID: {explicit_id}"),
            );
        }
    }
}

fn body_kind(tag: &Tag<'_>) -> Option<&'static str> {
    Some(match tag {
        Tag::Paragraph => "paragraph",
        Tag::Heading { .. } => "heading",
        Tag::BlockQuote(_) => "blockquote",
        Tag::CodeBlock(_) => "code_fence",
        Tag::HtmlBlock => "html_directive",
        Tag::FootnoteDefinition(_) => "footnote",
        Tag::Table(_) => "table",
        Tag::TableHead => "table_head",
        Tag::TableRow => "table_row",
        Tag::TableCell => "table_cell",
        Tag::Link { .. } => "link",
        Tag::Image { .. } => "image",
        _ => return None,
    })
}

fn end_kind(end: TagEnd) -> Option<&'static str> {
    Some(match end {
        TagEnd::Paragraph => "paragraph",
        TagEnd::Heading(_) => "heading",
        TagEnd::BlockQuote(_) => "blockquote",
        TagEnd::CodeBlock => "code_fence",
        TagEnd::HtmlBlock => "html_directive",
        TagEnd::FootnoteDefinition => "footnote",
        TagEnd::Table => "table",
        TagEnd::TableHead => "table_head",
        TagEnd::TableRow => "table_row",
        TagEnd::TableCell => "table_cell",
        TagEnd::Link => "link",
        TagEnd::Image => "image",
        _ => return None,
    })
}

fn tag_identity(
    tag: &Tag<'_>,
    content: &str,
    range: std::ops::Range<usize>,
) -> (Option<String>, Option<String>, Option<String>) {
    if let Tag::Heading { id, .. } = tag {
        let explicit = id
            .as_ref()
            .map(ToString::to_string)
            .or_else(|| explicit_inline_id(&content[range.clone()]));
        let anchor = explicit
            .clone()
            .or_else(|| Some(slugify_heading(&content[range])));
        (explicit, anchor, None)
    } else if let Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } = tag {
        (None, None, Some(dest_url.to_string()))
    } else {
        (None, None, None)
    }
}

fn explicit_inline_id(text: &str) -> Option<String> {
    let marker = text.rsplit_once("{#")?.1;
    Some(marker.split('}').next()?.trim().to_string()).filter(|id| !id.is_empty())
}

fn slugify_heading(text: &str) -> String {
    text.trim_start_matches('#')
        .trim()
        .trim_end_matches(['\r', '\n'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

fn ast_path(open: &OpenBodyNode) -> String {
    let own = format!("{}[{}]", open.kind, open.ordinal);
    if open.parent_path.is_empty() {
        own
    } else {
        format!("{}/{}", open.parent_path, own)
    }
}

fn body_node_id(path: &Path, open: &OpenBodyNode) -> NodeId {
    let contract_file = contract_path(path)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let stable = match &open.explicit_id {
        Some(id) => format!("{contract_file}::body::explicit:{}", percent_encode(id)),
        None => format!("{contract_file}::body::ast:{}", ast_path(open)),
    };
    NodeId {
        root: String::new(),
        file: path.to_path_buf(),
        name: stable,
        kind: NodeKind::MarkdownSection,
    }
}

fn make_body_node(
    path: &Path,
    content: &str,
    open: &OpenBodyNode,
    end: usize,
    explicit_ids: &mut BTreeMap<String, usize>,
) -> Node {
    let mut metadata = selector_metadata(
        path,
        content,
        open.start,
        end,
        open.kind,
        open.explicit_id.as_deref(),
    );
    let id = body_node_id(path, open);
    metadata.insert("body_node_id".into(), id.name.clone());
    if let Some(anchor) = &open.anchor {
        metadata.insert("anchor".into(), anchor.clone());
    }
    if let Some(oh_kind) = detect_oh_kind(path) {
        metadata.insert("oh_kind".into(), oh_kind);
    }
    if let Some(explicit) = &open.explicit_id {
        let count = explicit_ids.entry(explicit.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            metadata.insert("validation_status".into(), "invalid".into());
            metadata.insert("diagnostic_code".into(), "content.duplicate_body_id".into());
        }
    }
    let selected = &content[open.start..end];
    if open.kind == "image"
        && let Some(caption) = selected
            .split_once("![")
            .and_then(|(_, rest)| rest.split_once(']'))
            .map(|(caption, _)| caption)
        && !caption.is_empty()
    {
        metadata.insert("caption".into(), caption.to_string());
    }
    if open.kind == "link"
        && let Some(destination) = open.target.as_deref()
    {
        metadata.insert("link_target".into(), destination.to_string());
    }
    if open.kind == "link"
        && let Some(destination) = open.target.as_deref()
        && let Some((file, anchor)) = destination.split_once('#')
        && !anchor.is_empty()
        && !destination.starts_with("http://")
        && !destination.starts_with("https://")
        && !destination.starts_with("mailto:")
        && !destination.starts_with("data:")
        && !Path::new(file).is_absolute()
    {
        let target_file = if file.is_empty() {
            path.to_path_buf()
        } else {
            normalize_path(&path.parent().unwrap_or(Path::new("")).join(file))
        };
        metadata.insert("target_file".into(), target_file.display().to_string());
        metadata.insert("target_anchor".into(), anchor.to_string());
        if metadata.get("validation_status").map(String::as_str) == Some("valid") {
            metadata.insert("validation_status".into(), "unresolved".into());
        }
    }
    if matches!(open.kind, "html_comment" | "html_directive") {
        let directive = selected
            .trim()
            .trim_start_matches("<!--")
            .trim_start_matches('<')
            .trim()
            .split(|c: char| c == ':' || c == '>' || c.is_whitespace())
            .next()
            .unwrap_or("");
        if !directive.is_empty() {
            metadata.insert("directive_name".into(), directive.to_ascii_lowercase());
        }
    }
    Node {
        id,
        language: "markdown".into(),
        line_start: line_at(content, open.start),
        line_end: line_end_at(content, open.start, end),
        signature: open.kind.into(),
        body: content[open.start..end].to_string(),
        metadata,
        source: ExtractionSource::Markdown,
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_leaf_body_node(
    path: &Path,
    content: &str,
    kind: &'static str,
    range: std::ops::Range<usize>,
    explicit: Option<String>,
    parent_path: String,
    ordinal: usize,
    nodes: &mut Vec<Node>,
) {
    let open = OpenBodyNode {
        kind,
        start: range.start,
        ordinal,
        parent_path,
        explicit_id: explicit,
        anchor: None,
        target: None,
    };
    nodes.push(make_body_node(
        path,
        content,
        &open,
        range.end,
        &mut BTreeMap::new(),
    ));
}

fn selector_metadata(
    path: &Path,
    content: &str,
    start: usize,
    end: usize,
    kind: &str,
    explicit: Option<&str>,
) -> BTreeMap<String, String> {
    let end = end.min(content.len());
    let mut metadata = BTreeMap::new();
    metadata.insert("markdown_kind".into(), kind.into());
    if let Some(contract_path) = contract_path(path) {
        metadata.insert("file_path".into(), contract_path.display().to_string());
    } else {
        metadata.insert("file_path".into(), path.display().to_string());
        metadata.insert("validation_status".into(), "invalid".into());
        metadata.insert(
            "diagnostic_code".into(),
            "content.invalid_selector_path".into(),
        );
        metadata.insert("diagnostic_severity".into(), "error".into());
        metadata.insert(
            "diagnostic_message".into(),
            "selector file_path must be normalized and repository-relative".into(),
        );
    }
    metadata.insert("line_start".into(), line_at(content, start).to_string());
    metadata.insert(
        "line_end".into(),
        line_end_at(content, start, end).to_string(),
    );
    metadata.insert("byte_start".into(), start.to_string());
    metadata.insert("byte_end".into(), end.to_string());
    let line_start = content[..start.min(content.len())]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let name_col = content[line_start..start.min(content.len())]
        .encode_utf16()
        .count();
    metadata.insert("name_col".into(), name_col.to_string());
    metadata.insert(
        "snippet_hash".into(),
        blake3::hash(&content.as_bytes()[start..end])
            .to_hex()
            .to_string(),
    );
    metadata.insert("extractor_id".into(), MARKDOWN_EXTRACTOR_ID.into());
    metadata.insert("confidence".into(), "detected".into());
    metadata
        .entry("validation_status".into())
        .or_insert_with(|| "valid".into());
    if let Some(id) = explicit {
        metadata.insert("explicit_id".into(), id.into());
    }
    metadata
}

fn line_at(content: &str, byte: usize) -> usize {
    content[..byte.min(content.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

fn line_end_at(content: &str, start: usize, end: usize) -> usize {
    line_at(content, if end > start { end - 1 } else { end })
}

fn percent_encode(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (*byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn contract_path(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return None;
    }
    let normalized = normalize_path(path);
    if matches!(
        normalized.components().next(),
        Some(std::path::Component::ParentDir)
    ) {
        None
    } else {
        Some(normalized)
    }
}

/// Resolve exact Markdown anchors after all files have been extracted. Unresolved
/// candidates remain visible on their source link node with the contract diagnostic.
pub fn markdown_anchor_pass(all_nodes: &mut [Node]) -> Vec<Edge> {
    let mut candidates: BTreeMap<(String, PathBuf, String), Vec<NodeId>> = BTreeMap::new();
    for node in all_nodes
        .iter()
        .filter(|node| node.metadata.get("validation_status").map(String::as_str) == Some("valid"))
    {
        if let Some(anchor) = node.metadata.get("anchor") {
            candidates
                .entry((node.id.root.clone(), node.id.file.clone(), anchor.clone()))
                .or_default()
                .push(node.id.clone());
        }
    }
    let duplicate_ids: std::collections::HashSet<NodeId> = candidates
        .values()
        .filter(|ids| ids.len() > 1)
        .flatten()
        .cloned()
        .collect();
    for node in all_nodes
        .iter_mut()
        .filter(|node| duplicate_ids.contains(&node.id))
    {
        node.metadata
            .insert("validation_status".into(), "invalid".into());
        node.metadata
            .insert("diagnostic_code".into(), "content.duplicate_anchor".into());
        node.metadata
            .insert("diagnostic_severity".into(), "error".into());
        node.metadata.insert(
            "diagnostic_message".into(),
            "duplicate generated Markdown anchor".into(),
        );
    }
    let anchors: BTreeMap<_, _> = candidates
        .into_iter()
        .filter_map(|(key, ids)| (ids.len() == 1).then(|| (key, ids[0].clone())))
        .collect();
    let mut edges = Vec::new();
    for node in all_nodes.iter_mut() {
        let (Some(target_file), Some(target_anchor)) = (
            node.metadata.get("target_file").cloned(),
            node.metadata.get("target_anchor").cloned(),
        ) else {
            continue;
        };
        let key = (
            node.id.root.clone(),
            PathBuf::from(target_file),
            target_anchor,
        );
        if let Some(target) = anchors.get(&key) {
            node.metadata
                .insert("validation_status".into(), "valid".into());
            node.metadata.remove("diagnostic_code");
            node.metadata.remove("diagnostic_severity");
            node.metadata.remove("diagnostic_message");
            edges.push(Edge {
                from: node.id.clone(),
                to: target.clone(),
                kind: EdgeKind::References,
                source: ExtractionSource::Markdown,
                confidence: Confidence::Confirmed,
                evidence: Vec::new(),
            });
        } else {
            node.metadata
                .insert("validation_status".into(), "unresolved".into());
            node.metadata
                .insert("diagnostic_code".into(), "content.unresolved_anchor".into());
            node.metadata
                .insert("diagnostic_severity".into(), "error".into());
            node.metadata.insert(
                "diagnostic_message".into(),
                format!(
                    "Markdown anchor #{} does not resolve in {}",
                    key.2,
                    key.1.display()
                ),
            );
        }
    }
    edges
}

/// Emit `Defines` edges from parent heading sections to child heading sections.
///
/// A child section is one whose `parent_heading` metadata matches the `heading_text`
/// of another section in the same file. This mirrors tree-sitter's struct -> field edges.
fn emit_hierarchy_edges(nodes: &[Node], edges: &mut Vec<Edge>) {
    // Build a map from heading_text -> NodeId for lookup
    let heading_to_node: BTreeMap<&str, &NodeId> = nodes
        .iter()
        .filter_map(|n| {
            n.metadata
                .get("heading_text")
                .map(|ht| (ht.as_str(), &n.id))
        })
        .collect();

    for node in nodes {
        if let Some(parent_text) = node.metadata.get("parent_heading")
            && let Some(parent_id) = heading_to_node.get(parent_text.as_str())
        {
            edges.push(Edge {
                from: (*parent_id).clone(),
                to: node.id.clone(),
                kind: EdgeKind::Defines,
                source: ExtractionSource::Markdown,
                confidence: Confidence::Detected,
                evidence: Vec::new(),
            });
        }
    }
}

/// Frontmatter keys that reference other .oh/ artifact IDs.
const REFERENCE_KEYS: &[&str] = &["outcome", "signal", "guardrail", "endeavor"];

/// Emit `DependsOn` edges from the current artifact to referenced artifacts.
///
/// When frontmatter contains a key like `outcome: agent-alignment`, we emit a
/// DependsOn edge from this file's first section to a synthetic target node
/// representing the referenced artifact. The target uses the .oh/ path convention
/// (e.g., `.oh/outcomes/agent-alignment.md`).
fn emit_frontmatter_ref_edges(
    nodes: &[Node],
    frontmatter: &BTreeMap<String, String>,
    path: &Path,
    edges: &mut Vec<Edge>,
) {
    // Find the first non-frontmatter node (the document's main section)
    let source_node = nodes
        .iter()
        .find(|n| !n.metadata.contains_key("is_frontmatter"))
        .or_else(|| nodes.first());

    let source_id = match source_node {
        Some(n) => &n.id,
        None => return,
    };

    for (key, value) in frontmatter {
        if !REFERENCE_KEYS.contains(&key.as_str()) || value.is_empty() {
            continue;
        }

        // Determine the target path based on the key type
        let target_dir = match key.as_str() {
            "outcome" => "outcomes",
            "signal" => "signals",
            "guardrail" => "guardrails",
            "endeavor" => "metis",
            _ => continue,
        };

        let target_path = PathBuf::from(format!(".oh/{}/{}.md", target_dir, value));

        // Don't emit self-references (handles both relative and absolute source paths)
        if path == target_path || path.ends_with(&target_path) {
            continue;
        }

        let target_id = NodeId {
            root: String::new(),
            file: target_path,
            name: value.clone(),
            kind: NodeKind::MarkdownSection,
        };

        edges.push(Edge {
            from: source_id.clone(),
            to: target_id,
            kind: EdgeKind::DependsOn,
            source: ExtractionSource::Markdown,
            confidence: Confidence::Detected,
            evidence: Vec::new(),
        });
    }
}

/// Emit `References` edges for markdown links that point to local files.
///
/// For each `[text](./path.md)` link in a section, emit a References edge from
/// that section's node to a synthetic target node for the linked file.
/// Only emits edges for relative paths (not URLs starting with http/https/mailto).
fn emit_link_edges(
    nodes: &[Node],
    chunks: &[crate::types::MarkdownChunk],
    path: &Path,
    edges: &mut Vec<Edge>,
) {
    use std::collections::HashSet;

    for (node, chunk) in nodes.iter().zip(chunks.iter()) {
        let mut seen_targets: HashSet<PathBuf> = HashSet::new();

        for (_link_text, link_dest) in &chunk.links {
            // Skip external URLs and anchor-only links
            if link_dest.starts_with("http://")
                || link_dest.starts_with("https://")
                || link_dest.starts_with("mailto:")
                || link_dest.starts_with('#')
                || link_dest.is_empty()
            {
                continue;
            }

            // Strip anchor fragment from path
            let dest_path_str = link_dest.split('#').next().unwrap_or(link_dest);
            if dest_path_str.is_empty() || dest_path_str.starts_with('/') {
                continue;
            }

            // Resolve relative to the current file's directory
            let target_path = if let Some(parent) = path.parent() {
                normalize_path(&parent.join(dest_path_str))
            } else {
                PathBuf::from(dest_path_str)
            };

            // Only emit edges to markdown files (md/mdx), case-insensitive
            let is_markdown_target = target_path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| {
                    let lower = ext.to_ascii_lowercase();
                    lower == "md" || lower == "mdx"
                })
                .unwrap_or(false);
            if !is_markdown_target {
                continue;
            }

            // Deduplicate: skip if we already emitted an edge to this target from this node
            if !seen_targets.insert(target_path.clone()) {
                continue;
            }

            let target_name = target_path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            let target_id = NodeId {
                root: String::new(),
                file: target_path,
                name: target_name,
                kind: NodeKind::MarkdownSection,
            };

            edges.push(Edge {
                from: node.id.clone(),
                to: target_id,
                kind: EdgeKind::References,
                source: ExtractionSource::Markdown,
                confidence: Confidence::Detected,
                evidence: Vec::new(),
            });
        }
    }
}

/// Emit `References` edges from ADR markdown to exact Rust test functions declared in
/// ADR frontmatter (`validate.cargo_tests`).
///
/// The frontmatter stores exact `cargo test -- --list` names, which we resolve
/// against Rust test nodes via `metadata["test_path"]`. No leaf-name fallback: if
/// the exact Rust test path is not present, no edge is emitted.
///
/// Lookups are scoped by `NodeId.root` so multi-root scans never link an ADR in
/// one root to a Rust test in another (identical relative paths in two roots
/// would otherwise collide and either mis-link or be skipped as ambiguous).
pub fn adr_validation_pass(all_nodes: &[Node]) -> Vec<Edge> {
    let mut rust_tests: HashMap<(&str, &str), Vec<&Node>> = HashMap::new();
    let mut primary_markdown_nodes: HashMap<(&str, &Path), &NodeId> = HashMap::new();
    let mut frontmatter_nodes = Vec::new();

    for node in all_nodes {
        if node.id.kind == NodeKind::Function
            && node.language == "rust"
            && node.metadata.get("is_test").map(|value| value.as_str()) == Some("true")
            && let Some(test_path) = node.metadata.get("test_path")
        {
            rust_tests
                .entry((node.id.root.as_str(), test_path.as_str()))
                .or_default()
                .push(node);
        }

        if node.id.kind != NodeKind::MarkdownSection {
            continue;
        }
        if node
            .metadata
            .get("is_frontmatter")
            .map(|value| value.as_str())
            == Some("true")
        {
            frontmatter_nodes.push(node);
        } else {
            primary_markdown_nodes
                .entry((node.id.root.as_str(), node.id.file.as_path()))
                .or_insert(&node.id);
        }
    }

    let mut edges = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for node in frontmatter_nodes {
        let frontmatter: crate::adr::AdrValidationRefs =
            match serde_yaml::from_str::<serde_yaml::Value>(&node.body) {
                Ok(value) => match value.get("validate") {
                    Some(validate) => serde_yaml::from_value(validate.clone()).unwrap_or_default(),
                    None => continue,
                },
                Err(_) => continue,
            };

        let source_id = primary_markdown_nodes
            .get(&(node.id.root.as_str(), node.id.file.as_path()))
            .copied()
            .unwrap_or(&node.id);

        for cargo_test in frontmatter.cargo_tests {
            let Some(matches) = rust_tests.get(&(source_id.root.as_str(), cargo_test.as_str()))
            else {
                continue;
            };
            if matches.len() != 1 {
                continue;
            }
            let target_id = &matches[0].id;
            let edge_key = format!("{}->{}", source_id.to_stable_id(), target_id.to_stable_id());
            if !seen.insert(edge_key) {
                continue;
            }
            edges.push(Edge {
                from: (*source_id).clone(),
                to: target_id.clone(),
                kind: EdgeKind::References,
                source: ExtractionSource::Markdown,
                confidence: Confidence::Confirmed,
                evidence: Vec::new(),
            });
        }
    }

    edges
}

/// Emit `References` edges from extracted test functions back to ADR markdown nodes
/// using `metadata["adr_refs"]` captured during code extraction.
///
/// Lookups are scoped by `NodeId.root`: the ADR target must live in the same root
/// as the test that referenced it.
pub fn adr_backreference_pass(all_nodes: &[Node]) -> Vec<Edge> {
    let mut primary_markdown_nodes: HashMap<(&str, &Path), &NodeId> = HashMap::new();
    for node in all_nodes {
        if node.id.kind == NodeKind::MarkdownSection
            && node
                .metadata
                .get("is_frontmatter")
                .map(|value| value.as_str())
                != Some("true")
        {
            primary_markdown_nodes
                .entry((node.id.root.as_str(), node.id.file.as_path()))
                .or_insert(&node.id);
        }
    }

    let mut edges = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for node in all_nodes {
        if node.id.kind != NodeKind::Function
            || node.metadata.get("is_test").map(|value| value.as_str()) != Some("true")
        {
            continue;
        }
        let Some(raw_refs) = node.metadata.get("adr_refs") else {
            continue;
        };

        for adr_path in raw_refs
            .split(',')
            .map(|value| PathBuf::from(value.trim()))
            .filter(|p| !p.as_os_str().is_empty())
        {
            let Some(target_id) =
                primary_markdown_nodes.get(&(node.id.root.as_str(), adr_path.as_path()))
            else {
                continue;
            };
            let edge_key = format!("{}->{}", node.id.to_stable_id(), target_id.to_stable_id());
            if !seen.insert(edge_key) {
                continue;
            }
            edges.push(Edge {
                from: node.id.clone(),
                to: (*target_id).clone(),
                kind: EdgeKind::References,
                source: ExtractionSource::TreeSitter,
                confidence: Confidence::Detected,
                evidence: Vec::new(),
            });
        }
    }

    edges
}

/// Normalize a path by resolving `.` and `..` components without filesystem access.
/// Preserves leading `..` segments when there is nothing left to pop (out-of-repo links).
/// Never pops past a root directory or prefix component.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components: Vec<std::path::Component> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                // Only pop if the last component is a normal directory (not root, prefix, or ..)
                match components.last() {
                    Some(std::path::Component::Normal(_)) => {
                        components.pop();
                    }
                    _ => {
                        // Preserve leading .. or don't pop past root
                        if !matches!(
                            components.last(),
                            Some(std::path::Component::RootDir)
                                | Some(std::path::Component::Prefix(_))
                        ) {
                            components.push(component);
                        }
                        // If last is RootDir/Prefix, silently ignore (can't go above root)
                    }
                }
            }
            std::path::Component::CurDir => {}
            c => components.push(c),
        }
    }
    components.iter().collect()
}

#[derive(Debug, Deserialize)]
struct LocalKnowledgeFrontmatter {
    rna: Option<LocalKnowledgeNodeSpec>,
}

#[derive(Debug, Deserialize)]
struct LocalKnowledgeNodeSpec {
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    relationships: Vec<LocalKnowledgeRelationship>,
    #[serde(default)]
    metadata: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
struct LocalKnowledgeRelationship {
    kind: String,
    target: LocalKnowledgeTarget,
}

#[derive(Debug, Deserialize)]
struct LocalKnowledgeTarget {
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    file: Option<String>,
    /// Stable authorized Open Horizons identity. Mutually exclusive with the
    /// existing local `id`/`name` + optional `file` target form.
    #[serde(default)]
    uri: Option<String>,
}

/// Emits local knowledge nodes and advisory relationship candidates from frontmatter.
fn emit_local_knowledge_graph(
    path: &Path,
    content: &str,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let Some(yaml_block) = extract_frontmatter_yaml(content) else {
        return;
    };
    let Ok(frontmatter) = serde_yaml::from_str::<LocalKnowledgeFrontmatter>(yaml_block) else {
        return;
    };
    let Some(spec) = frontmatter.rna else {
        return;
    };

    let Some(local_id) = spec.id.as_deref().or(spec.name.as_deref()).map(str::trim) else {
        return;
    };
    if local_id.is_empty() || spec.kind.trim().is_empty() {
        return;
    }

    let display_name = spec.name.as_deref().unwrap_or(local_id).trim();
    let node_id = local_knowledge_node_id(path, spec.kind.trim(), local_id);
    let mut metadata = BTreeMap::new();
    metadata.insert("local_knowledge".to_string(), "true".to_string());
    metadata.insert("rna.kind".to_string(), spec.kind.trim().to_string());
    metadata.insert("rna.id".to_string(), local_id.to_string());
    metadata.insert("rna.name".to_string(), display_name.to_string());
    metadata.insert("rna.source_file".to_string(), path.display().to_string());
    for (key, value) in spec.metadata {
        metadata.insert(
            format!("rna.metadata.{}", key),
            yaml_value_to_string(&value),
        );
    }

    let line_end = content.lines().count().max(1);
    nodes.push(Node {
        id: node_id.clone(),
        language: "markdown".to_string(),
        line_start: 1,
        line_end,
        signature: format!("{} {}", spec.kind.trim(), display_name),
        body: strip_frontmatter(content).trim().to_string(),
        metadata,
        source: ExtractionSource::Markdown,
    });

    for relationship in spec.relationships {
        let Some(kind) = EdgeKind::from_label(&relationship.kind) else {
            continue;
        };
        if relationship.target.kind.trim().is_empty() {
            continue;
        }
        let (target_node, rule_id) = if let Some(uri) = relationship.target.uri.as_deref() {
            // Cloud identity remains a local placeholder target. Resolution is
            // an explicit advisory operation; extraction never performs I/O.
            // Keeping the URI as the stable node name preserves rename/archive
            // identity without synchronizing either system's graph.
            if relationship.target.id.is_some()
                || relationship.target.name.is_some()
                || relationship.target.file.is_some()
            {
                continue;
            }
            let Ok(reference) = crate::oh_reference::OhReference::parse(uri.trim()) else {
                continue;
            };
            if relationship.target.kind.trim() != reference.kind.as_str() {
                continue;
            }
            (
                NodeId {
                    root: String::new(),
                    file: PathBuf::from(".oh/external/open-horizons-v1"),
                    name: reference.uri,
                    kind: NodeKind::Other(reference.kind.as_str().to_string()),
                },
                "frontmatter-oh-reference-candidate@1",
            )
        } else {
            let Some(target_id) = relationship
                .target
                .id
                .as_deref()
                .or(relationship.target.name.as_deref())
                .map(str::trim)
            else {
                continue;
            };
            if target_id.is_empty() {
                continue;
            }

            let target_file = match relationship.target.file.as_deref() {
                Some(declared_file) => {
                    let declared_file = declared_file.trim();
                    if declared_file.is_empty() {
                        continue;
                    }
                    let declared_file = Path::new(declared_file);
                    if declared_file.is_absolute() {
                        continue;
                    }
                    let normalized = normalize_path(declared_file);
                    if matches!(
                        normalized.components().next(),
                        Some(std::path::Component::ParentDir)
                    ) {
                        continue;
                    }
                    normalized
                }
                None => path.to_path_buf(),
            };
            (
                local_knowledge_node_id(
                    &target_file,
                    relationship.target.kind.trim(),
                    target_id,
                ),
                "frontmatter-candidate@1",
            )
        };
        // Frontmatter nominates a candidate but is never relationship truth.
        // #715's pack rules may attach validated body evidence later.
        let confidence = Confidence::Detected;
        edges.push(Edge {
            from: node_id.clone(),
            to: target_node,
            kind,
            source: ExtractionSource::Markdown,
            confidence,
            evidence: vec![EdgeEvidence {
                selectors: Vec::new(),
                extractor_id: MARKDOWN_EXTRACTOR_ID.into(),
                pack_id: None,
                rule_id: rule_id.into(),
                confidence: Confidence::Detected,
                validation_status: ValidationStatus::Invalid,
                diagnostics: vec![EvidenceDiagnostic {
                    code: "content.metadata_without_body_evidence".into(),
                    severity: "error".into(),
                    file_path: path.to_path_buf(),
                    selector: None,
                    message:
                        "frontmatter nominates a relationship without supporting body evidence"
                            .into(),
                }],
            }],
        });
    }
}

fn local_knowledge_node_id(path: &Path, kind: &str, id: &str) -> NodeId {
    NodeId {
        root: String::new(),
        file: path.to_path_buf(),
        name: id.to_string(),
        kind: NodeKind::Other(kind.to_string()),
    }
}

fn extract_frontmatter_yaml(content: &str) -> Option<&str> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after_first = trimmed[3..].trim_start_matches(['\r', '\n']);
    let end_idx = after_first.find("\n---")?;
    Some(&after_first[..end_idx])
}

fn strip_frontmatter(content: &str) -> &str {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content;
    }
    let after_first = trimmed[3..].trim_start_matches(['\r', '\n']);
    let Some(end_idx) = after_first.find("\n---") else {
        return content;
    };
    after_first[end_idx + 4..].trim_start_matches(['\r', '\n'])
}

fn yaml_value_to_string(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::Null => String::new(),
        serde_yaml::Value::Bool(value) => value.to_string(),
        serde_yaml::Value::Number(value) => value.to_string(),
        serde_yaml::Value::String(value) => value.clone(),
        _ => serde_yaml::to_string(value)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

/// Detect the artifact kind for a markdown file based on its path.
///
/// Handles two families of paths:
///
/// **`.oh/` artifacts** — maps subdirectory to kind:
/// - `outcomes` → `"outcome"`
/// - `signals` → `"signal"`
/// - `guardrails` → `"guardrail"`
/// - `metis` → `"metis"`
/// - `knowledge` → `"knowledge"`
///
/// **Agent memory files** — detects common AI agent rule/memory locations:
/// - `.cursorrules` (file in repo root) → `"cursor-rule"`
/// - `.cursor/rules` (file or directory under `.cursor/`) → `"cursor-rule"`
/// - `.clinerules` (file in repo root) → `"cline-rule"`
/// - `.serena/memories/` (any file under `.serena/memories/`) → `"serena-memory"`
/// - `.github/copilot-instructions.md` → `"copilot-instruction"`
///
/// Returns `None` for all other paths.
fn detect_oh_kind(path: &Path) -> Option<String> {
    let components: Vec<_> = path.components().collect();
    let n = components.len();

    for (i, comp) in components.iter().enumerate() {
        let name = comp.as_os_str().to_string_lossy();

        // .oh/ artifact family
        if name == ".oh"
            && let Some(next) = components.get(i + 1)
        {
            let dir = next.as_os_str().to_string_lossy();
            return match dir.as_ref() {
                "outcomes" => Some("outcome".to_string()),
                "signals" => Some("signal".to_string()),
                "guardrails" => Some("guardrail".to_string()),
                "metis" => Some("metis".to_string()),
                "knowledge" => Some("knowledge".to_string()),
                _ => None,
            };
        }

        // .cursorrules — root-level file (component before this is the last)
        if name == ".cursorrules" && i == n - 1 {
            return Some("cursor-rule".to_string());
        }

        // .cursor/ — any file inside (covers both .cursor/rules file and .cursor/rules/*.md)
        if name == ".cursor" && i + 1 < n {
            return Some("cursor-rule".to_string());
        }

        // .clinerules — root-level file
        if name == ".clinerules" && i == n - 1 {
            return Some("cline-rule".to_string());
        }

        // .serena/memories/ — any file under this directory
        if name == ".serena"
            && let Some(next) = components.get(i + 1)
            && next.as_os_str() == "memories"
        {
            return Some("serena-memory".to_string());
        }

        // .github/copilot-instructions.md
        if name == ".github"
            && let Some(next) = components.get(i + 1)
            && next.as_os_str() == "copilot-instructions.md"
        {
            return Some("copilot-instruction".to_string());
        }
    }
    None
}

/// Extract YAML frontmatter from markdown content.
/// Expects `---\nkey: value\n---\n` at the start of the file.
fn extract_frontmatter(content: &str) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();

    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return result;
    }

    // Find the closing ---
    let after_first = &trimmed[3..];
    let after_first = after_first.trim_start_matches(['\r', '\n']);
    if let Some(end_idx) = after_first.find("\n---") {
        let yaml_block = &after_first[..end_idx];
        for line in yaml_block.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_string();
                let value = value
                    .trim()
                    .trim_matches('\'')
                    .trim_matches('"')
                    .to_string();
                if !key.is_empty() {
                    result.insert(key, value);
                }
            }
        }
    }

    result
}

/// Parse markdown source directly (avoids re-reading the file from disk
/// since the extractor framework already provides the content).
fn parse_markdown_file_from_source(source: &str, path: &Path) -> Vec<crate::types::MarkdownChunk> {
    crate::markdown::parse_markdown_source(source, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::ExtractorRegistry;
    use crate::graph::{Confidence, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};
    use crate::scanner::Scanner;
    use crate::server::store::{load_graph_from_lance, persist_graph_to_lance};
    use crate::service::{SearchContext, SearchParams, search};
    use std::collections::{HashMap, HashSet};
    use std::path::Path;

    fn legacy_section_count(result: &ExtractionResult) -> usize {
        result
            .nodes
            .iter()
            .filter(|node| !node.metadata.contains_key("markdown_kind"))
            .count()
    }

    #[test]
    fn test_markdown_extractor_basic() {
        let extractor = MarkdownExtractor::new();
        let content =
            "# Title\n\nIntro text.\n\n## Section A\n\nContent A.\n\n## Section B\n\nContent B.\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();

        let sections: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| !node.metadata.contains_key("markdown_kind"))
            .collect();
        assert_eq!(sections.len(), 3);

        // First node: Title section
        assert_eq!(sections[0].id.name, "Title");
        assert_eq!(
            sections[0].metadata.get("heading_level"),
            Some(&"1".to_string())
        );

        // Second node: Section A
        assert_eq!(sections[1].id.name, "Section A");
        assert_eq!(
            sections[1].metadata.get("heading_hierarchy"),
            Some(&"# Title > ## Section A".to_string())
        );

        // Third node: Section B
        assert_eq!(sections[2].id.name, "Section B");
    }

    #[test]
    fn test_markdown_extractor_with_frontmatter() {
        let extractor = MarkdownExtractor::new();
        let content = "---\nid: my-outcome\nstatus: active\ntitle: 'Test Outcome'\n---\n\n# My Outcome\n\nSome content.\n";
        let result = extractor.extract(Path::new("outcome.md"), content).unwrap();

        assert!(!result.nodes.is_empty());

        // Frontmatter should be on the first node
        let first = &result.nodes[0];
        assert_eq!(
            first.metadata.get("frontmatter.id"),
            Some(&"my-outcome".to_string())
        );
        assert_eq!(
            first.metadata.get("frontmatter.status"),
            Some(&"active".to_string())
        );
        assert_eq!(
            first.metadata.get("frontmatter.title"),
            Some(&"Test Outcome".to_string())
        );
    }

    #[test]
    fn test_markdown_local_knowledge_frontmatter_emits_custom_node_and_edge() {
        let extractor = MarkdownExtractor::new();
        let content = r#"---
rna:
  kind: quote
  id: quote.goodhart
  name: Goodhart quote
  metadata:
    public_use: verified
  relationships:
    - kind: supports
      confidence: confirmed
      target:
        kind: claim
        id: claim.proxy-risk
        file: ./.oh/knowledge/../knowledge/proxy-risk.md
    - kind: escapes_repo
      target:
        kind: claim
        id: claim.outside
        file: ../outside.md
---

# Goodhart Source

When a measure becomes a target, it ceases to be a good measure.
"#;
        let result = extractor
            .extract(Path::new(".oh/sources/goodhart.md"), content)
            .unwrap();

        let nodes_by_kind: HashMap<&str, &Node> = result
            .nodes
            .iter()
            .filter_map(|node| match &node.id.kind {
                NodeKind::Other(kind) => Some((kind.as_str(), node)),
                _ => None,
            })
            .collect();
        let quote = nodes_by_kind
            .get("quote")
            .copied()
            .expect("quote local knowledge node should be emitted");
        assert_eq!(quote.id.name, "quote.goodhart");
        assert_eq!(
            quote.metadata.get("local_knowledge"),
            Some(&"true".to_string())
        );
        assert_eq!(
            quote.metadata.get("rna.metadata.public_use"),
            Some(&"verified".to_string())
        );

        let edges_by_kind: HashMap<String, &Edge> = result
            .edges
            .iter()
            .map(|edge| (edge.kind.to_string(), edge))
            .collect();
        let edge = edges_by_kind
            .get("supports")
            .copied()
            .expect("custom supports edge should be emitted as a true edge kind");
        assert_eq!(edge.from.name, "quote.goodhart");
        assert_eq!(edge.to.name, "claim.proxy-risk");
        assert_eq!(edge.to.file, PathBuf::from(".oh/knowledge/proxy-risk.md"));
        assert!(matches!(&edge.to.kind, NodeKind::Other(kind) if kind == "claim"));
        assert_eq!(
            edge.confidence,
            Confidence::Detected,
            "frontmatter-only relationships nominate candidates but cannot confirm them"
        );
        assert_eq!(edge.evidence.len(), 1);
        assert!(edge.evidence[0].selectors.is_empty());
        assert_eq!(
            edge.evidence[0].diagnostics[0].code,
            "content.metadata_without_body_evidence"
        );
        assert!(
            !edges_by_kind.contains_key("escapes_repo"),
            "repo-local relationship targets must not escape the repository"
        );
    }

    #[test]
    fn test_markdown_local_knowledge_supports_oh_uri_without_changing_local_targets() {
        let extractor = MarkdownExtractor::new();
        let content = r#"---
rna:
  kind: claim
  id: claim.local
  relationships:
    - kind: informs
      target:
        kind: endeavor
        uri: oh://v1/endeavor/endeavor%3Ashared%3A1
    - kind: supports
      target:
        kind: claim
        id: claim.local-target
        file: .oh/knowledge/local.md
---

# Local claim

The reference is advisory.
"#;
        let result = extractor
            .extract(Path::new(".oh/knowledge/source.md"), content)
            .unwrap();

        let external = result
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Other("informs".into()))
            .expect("OH reference edge");
        assert_eq!(
            external.to.name,
            "oh://v1/endeavor/endeavor%3Ashared%3A1"
        );
        assert_eq!(
            external.to.file,
            PathBuf::from(".oh/external/open-horizons-v1")
        );
        assert!(matches!(&external.to.kind, NodeKind::Other(kind) if kind == "endeavor"));
        assert_eq!(
            external.evidence[0].rule_id,
            "frontmatter-oh-reference-candidate@1"
        );

        let local = result
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Other("supports".into()))
            .expect("existing local reference edge");
        assert_eq!(local.to.name, "claim.local-target");
        assert_eq!(local.to.file, PathBuf::from(".oh/knowledge/local.md"));
    }

    #[test]
    fn test_markdown_local_knowledge_rejects_oh_uri_kind_mismatch() {
        let extractor = MarkdownExtractor::new();
        let content = r#"---
rna:
  kind: claim
  id: claim.local
  relationships:
    - kind: informs
      target:
        kind: claim
        uri: oh://v1/endeavor/endeavor%3Ashared%3A1
---

# Local claim
"#;
        let result = extractor
            .extract(Path::new(".oh/knowledge/source.md"), content)
            .unwrap();

        assert!(
            result.edges.is_empty(),
            "a declaration whose target kind disagrees with its URI must not emit an edge"
        );
    }

    #[tokio::test]
    async fn test_local_knowledge_scan_persist_load_search_answers_claim_support_question() {
        let repo = tempfile::tempdir().expect("tempdir");
        let root = repo.path();
        std::fs::create_dir_all(root.join(".oh/sources")).expect("create sources dir");
        std::fs::create_dir_all(root.join(".oh/knowledge")).expect("create knowledge dir");
        std::fs::create_dir_all(root.join("manuscript")).expect("create manuscript dir");

        std::fs::write(
            root.join(".oh/sources/goodhart.md"),
            r#"---
rna:
  kind: quote
  id: quote.goodhart
  name: Goodhart quote
  metadata:
    public_use: verified
    source_url: https://example.test/goodhart
  relationships:
    - kind: supports
      confidence: confirmed
      target:
        kind: claim
        id: claim.proxy-risk
        file: .oh/knowledge/proxy-risk.md
---

# Goodhart Source

When a measure becomes a target, it ceases to be a good measure.
"#,
        )
        .expect("write source artifact");
        std::fs::write(
            root.join(".oh/knowledge/proxy-risk.md"),
            r#"---
rna:
  kind: claim
  id: claim.proxy-risk
  name: Proxy metrics become risky when treated as targets
---

# Proxy Risk Claim

Proxy metrics become unreliable when the organization optimizes the proxy rather than the underlying outcome.
"#,
        )
        .expect("write claim artifact");
        std::fs::write(
            root.join("manuscript/chapter-01.md"),
            r#"---
rna:
  kind: manuscript_section
  id: manuscript.chapter-01.proxy-risk
  name: Chapter 1 proxy-risk section
  relationships:
    - kind: consumes
      confidence: confirmed
      target:
        kind: claim
        id: claim.proxy-risk
        file: .oh/knowledge/proxy-risk.md
---

# Chapter 1

The manuscript uses the proxy-risk claim in the opening argument.
"#,
        )
        .expect("write manuscript artifact");

        let mut scanner = Scanner::new(root.to_path_buf()).expect("create scanner");
        let scan = scanner.scan().expect("scan local knowledge corpus");
        let extraction = ExtractorRegistry::with_builtins().extract_scan_result(root, &scan);

        persist_graph_to_lance(root, &extraction.nodes, &extraction.edges)
            .await
            .expect("persist graph");
        let loaded = load_graph_from_lance(root).await.expect("load graph");

        let nodes_by_name: HashMap<&str, &Node> = loaded
            .nodes
            .iter()
            .map(|node| (node.id.name.as_str(), node))
            .collect();
        let claim = nodes_by_name
            .get("claim.proxy-risk")
            .copied()
            .expect("claim node should survive scan/persist/load");
        let quote = nodes_by_name
            .get("quote.goodhart")
            .copied()
            .expect("quote node should survive scan/persist/load");
        assert_eq!(
            quote.source,
            ExtractionSource::Markdown,
            "Markdown provenance must survive scan/persist/load"
        );
        assert_eq!(
            quote.metadata.get("rna.metadata.public_use"),
            Some(&"verified".to_string()),
            "quote public-use verification must remain queryable as source metadata"
        );

        let supports = loaded.index.neighbors(
            &claim.stable_id(),
            Some(&[EdgeKind::Other("supports".to_string())]),
            petgraph::Direction::Incoming,
        );
        assert_eq!(
            supports,
            vec![quote.stable_id()],
            "claim support must be a real incoming custom edge, not metadata or a generic reference"
        );
        let supports_edge = loaded
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Other("supports".into()))
            .expect("persisted supports edge");
        assert_eq!(
            supports_edge.evidence[0].diagnostics[0].code,
            "content.metadata_without_body_evidence"
        );

        let business_context = crate::business_context::BusinessContextAdmission::default();
        let ctx = SearchContext {
            graph_state: &loaded,
            embed_index: None,
            repo_root: root,
            lsp_status: None,
            embed_status: None,
            root_filter: None,
            non_code_slugs: HashSet::new(),
            enrichment_jobs: Vec::new(),
            business_context: &business_context,
        };
        let answer = search(
            &SearchParams {
                node: Some(claim.stable_id()),
                mode: Some("neighbors".to_string()),
                direction: Some("incoming".to_string()),
                edge_types: Some(vec!["supports".to_string(), "consumes".to_string()]),
                compact: true,
                ..Default::default()
            },
            &ctx,
        )
        .await;

        assert!(
            answer.contains("#### Supports (1)") && answer.contains("quote.goodhart"),
            "search should answer what supports the claim: {answer}"
        );
        assert!(
            answer.contains("#### Consumes (1)")
                && answer.contains("manuscript.chapter-01.proxy-risk"),
            "search should answer which manuscript node consumes the claim: {answer}"
        );
        assert!(
            answer.contains("public_use") && answer.contains("verified"),
            "search must deliver persisted local knowledge metadata to agents: {answer}"
        );
        assert!(
            answer.contains("source_url") && answer.contains("https://example.test/goodhart"),
            "search must deliver persisted source metadata to agents: {answer}"
        );
    }

    #[test]
    fn test_markdown_extractor_preamble() {
        let extractor = MarkdownExtractor::new();
        let content = "Some preamble text.\n\n# First Heading\n\nBody.\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();

        assert_eq!(legacy_section_count(&result), 2);
        assert_eq!(result.nodes[0].id.name, "preamble");
        assert_eq!(
            result.nodes[0].metadata.get("heading_level"),
            Some(&"0".to_string())
        );
    }

    #[test]
    fn test_markdown_extractor_code_spans() {
        let extractor = MarkdownExtractor::new();
        let content = "# The `Config` struct\n\nUse `Config::new()` to create.\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();

        assert_eq!(legacy_section_count(&result), 1);
        let meta = &result.nodes[0].metadata;
        assert!(meta.get("code_spans").unwrap().contains("Config"));
        assert!(meta.get("code_spans").unwrap().contains("Config::new()"));
    }

    #[test]
    fn test_markdown_extractor_nested_headings() {
        let extractor = MarkdownExtractor::new();
        let content = "# Top\n\n## Sub\n\n### Deep\n\nDeep content.\n\n## Another Sub\n\nMore.\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();

        assert_eq!(legacy_section_count(&result), 4);

        // Deep section should have full hierarchy
        assert_eq!(result.nodes[2].id.name, "Deep");
        assert_eq!(
            result.nodes[2].metadata.get("heading_hierarchy"),
            Some(&"# Top > ## Sub > ### Deep".to_string())
        );

        // "Another Sub" should reset to level 2
        assert_eq!(result.nodes[3].id.name, "Another Sub");
        assert_eq!(
            result.nodes[3].metadata.get("heading_hierarchy"),
            Some(&"# Top > ## Another Sub".to_string())
        );
    }

    #[test]
    fn test_markdown_extractor_extensions() {
        let extractor = MarkdownExtractor::new();
        assert!(extractor.extensions().contains(&"md"));
        assert!(extractor.extensions().contains(&"mdx"));
    }

    #[test]
    fn test_frontmatter_extraction() {
        let fm = extract_frontmatter("---\nid: test\nstatus: active\n---\n\n# Hello\n");
        assert_eq!(fm.get("id"), Some(&"test".to_string()));
        assert_eq!(fm.get("status"), Some(&"active".to_string()));
    }

    #[test]
    fn test_frontmatter_no_frontmatter() {
        let fm = extract_frontmatter("# Just a heading\n\nContent.\n");
        assert!(fm.is_empty());
    }

    #[test]
    fn test_frontmatter_quoted_values() {
        let fm = extract_frontmatter("---\ntitle: 'My Title'\ndesc: \"A description\"\n---\n");
        assert_eq!(fm.get("title"), Some(&"My Title".to_string()));
        assert_eq!(fm.get("desc"), Some(&"A description".to_string()));
    }

    #[test]
    fn test_line_numbers() {
        let extractor = MarkdownExtractor::new();
        let content = "# Title\n\nLine 3.\n\n## Section\n\nLine 7.\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();

        assert_eq!(result.nodes[0].line_start, 1);
        assert_eq!(result.nodes[1].line_start, 5); // ## Section starts on line 5
    }

    #[test]
    fn test_detect_oh_kind_outcome() {
        assert_eq!(
            detect_oh_kind(Path::new(".oh/outcomes/my-outcome.md")),
            Some("outcome".to_string())
        );
    }

    #[test]
    fn test_detect_oh_kind_signal() {
        assert_eq!(
            detect_oh_kind(Path::new(".oh/signals/my-signal.md")),
            Some("signal".to_string())
        );
    }

    #[test]
    fn test_detect_oh_kind_guardrail() {
        assert_eq!(
            detect_oh_kind(Path::new(".oh/guardrails/my-guardrail.md")),
            Some("guardrail".to_string())
        );
    }

    #[test]
    fn test_detect_oh_kind_metis() {
        assert_eq!(
            detect_oh_kind(Path::new(".oh/metis/my-learning.md")),
            Some("metis".to_string())
        );
    }

    #[test]
    fn test_detect_oh_kind_not_oh() {
        assert_eq!(detect_oh_kind(Path::new("src/main.rs")), None);
        assert_eq!(detect_oh_kind(Path::new("docs/README.md")), None);
    }

    #[test]
    fn test_detect_oh_kind_unknown_subdir() {
        assert_eq!(detect_oh_kind(Path::new(".oh/sessions/123.md")), None);
        assert_eq!(detect_oh_kind(Path::new(".oh/.cache/data.md")), None);
    }

    // --- Agent memory oh_kind detection ---

    #[test]
    fn test_detect_oh_kind_cursorrules() {
        assert_eq!(
            detect_oh_kind(Path::new(".cursorrules")),
            Some("cursor-rule".to_string())
        );
        assert_eq!(
            detect_oh_kind(Path::new("/repo/.cursorrules")),
            Some("cursor-rule".to_string())
        );
    }

    #[test]
    fn test_detect_oh_kind_cursor_rules_file() {
        // .cursor/rules as a file
        assert_eq!(
            detect_oh_kind(Path::new(".cursor/rules")),
            Some("cursor-rule".to_string())
        );
        assert_eq!(
            detect_oh_kind(Path::new("/repo/.cursor/rules")),
            Some("cursor-rule".to_string())
        );
    }

    #[test]
    fn test_detect_oh_kind_cursor_rules_dir_contents() {
        // files inside .cursor/rules/ directory
        assert_eq!(
            detect_oh_kind(Path::new(".cursor/rules/my-rule.md")),
            Some("cursor-rule".to_string())
        );
        assert_eq!(
            detect_oh_kind(Path::new("/repo/.cursor/rules/python.md")),
            Some("cursor-rule".to_string())
        );
    }

    #[test]
    fn test_detect_oh_kind_clinerules() {
        assert_eq!(
            detect_oh_kind(Path::new(".clinerules")),
            Some("cline-rule".to_string())
        );
        assert_eq!(
            detect_oh_kind(Path::new("/repo/.clinerules")),
            Some("cline-rule".to_string())
        );
    }

    #[test]
    fn test_detect_oh_kind_serena_memory() {
        assert_eq!(
            detect_oh_kind(Path::new(".serena/memories/project-context.md")),
            Some("serena-memory".to_string())
        );
        assert_eq!(
            detect_oh_kind(Path::new("/repo/.serena/memories/architecture.md")),
            Some("serena-memory".to_string())
        );
    }

    #[test]
    fn test_detect_oh_kind_copilot_instructions() {
        assert_eq!(
            detect_oh_kind(Path::new(".github/copilot-instructions.md")),
            Some("copilot-instruction".to_string())
        );
        assert_eq!(
            detect_oh_kind(Path::new("/repo/.github/copilot-instructions.md")),
            Some("copilot-instruction".to_string())
        );
    }

    #[test]
    fn test_detect_oh_kind_github_other_files_not_tagged() {
        // Other .github/ files should not get copilot-instruction tag
        assert_eq!(detect_oh_kind(Path::new(".github/workflows/ci.yml")), None);
        assert_eq!(
            detect_oh_kind(Path::new(".github/PULL_REQUEST_TEMPLATE.md")),
            None
        );
    }

    #[test]
    fn test_detect_oh_kind_agent_memory_nodes_get_metadata() {
        let extractor = MarkdownExtractor::new();
        let content = "# Cursor Rules\n\nAlways write tests.\n";
        let result = extractor
            .extract(Path::new(".cursorrules"), content)
            .unwrap();
        assert!(!result.nodes.is_empty());
        for node in &result.nodes {
            assert_eq!(
                node.metadata.get("oh_kind"),
                Some(&"cursor-rule".to_string()),
                "node {} should have oh_kind=cursor-rule",
                node.id.name
            );
        }
    }

    #[test]
    fn test_detect_oh_kind_serena_memory_nodes_get_metadata() {
        let extractor = MarkdownExtractor::new();
        let content = "# Project Context\n\nThis is a Rust project.\n";
        let result = extractor
            .extract(Path::new(".serena/memories/project-context.md"), content)
            .unwrap();
        assert!(!result.nodes.is_empty());
        for node in &result.nodes {
            assert_eq!(
                node.metadata.get("oh_kind"),
                Some(&"serena-memory".to_string()),
                "node {} should have oh_kind=serena-memory",
                node.id.name
            );
        }
    }

    #[test]
    fn test_oh_artifact_gets_oh_kind_metadata() {
        let extractor = MarkdownExtractor::new();
        let content = "---\nid: test-outcome\nstatus: active\n---\n\n# My Outcome\n\nContent.\n";
        let result = extractor
            .extract(Path::new(".oh/outcomes/test-outcome.md"), content)
            .unwrap();
        assert!(!result.nodes.is_empty());
        for node in &result.nodes {
            assert_eq!(
                node.metadata.get("oh_kind"),
                Some(&"outcome".to_string()),
                "node {} should have oh_kind=outcome",
                node.id.name
            );
        }
    }

    #[test]
    fn test_non_oh_file_no_oh_kind() {
        let extractor = MarkdownExtractor::new();
        let content = "# Regular Doc\n\nContent.\n";
        let result = extractor
            .extract(Path::new("docs/readme.md"), content)
            .unwrap();
        for node in &result.nodes {
            assert!(
                node.metadata.get("oh_kind").is_none(),
                "non-.oh/ node should not have oh_kind metadata"
            );
        }
    }

    #[test]
    fn test_oh_artifact_with_absolute_path() {
        assert_eq!(
            detect_oh_kind(Path::new("/home/user/repo/.oh/metis/learning.md")),
            Some("metis".to_string())
        );
    }

    // --- Edge tests ---

    #[test]
    fn test_hierarchy_edges_parent_child() {
        let extractor = MarkdownExtractor::new();
        let content = "# Top\n\n## Child A\n\nContent A.\n\n## Child B\n\nContent B.\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();

        assert_eq!(legacy_section_count(&result), 3);

        // Should have 2 Defines edges: Top -> Child A, Top -> Child B
        let defines: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Defines)
            .collect();
        assert_eq!(
            defines.len(),
            2,
            "Expected 2 hierarchy edges, got {}",
            defines.len()
        );

        // Both edges should come from "Top"
        for edge in &defines {
            assert_eq!(edge.from.name, "Top");
        }

        let child_names: Vec<_> = defines.iter().map(|e| e.to.name.as_str()).collect();
        assert!(child_names.contains(&"Child A"));
        assert!(child_names.contains(&"Child B"));
    }

    #[test]
    fn test_hierarchy_edges_deep_nesting() {
        let extractor = MarkdownExtractor::new();
        let content = "# Top\n\n## Mid\n\n### Deep\n\nDeep content.\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();

        let defines: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Defines)
            .collect();
        assert_eq!(defines.len(), 2);

        // Top -> Mid
        assert!(
            defines
                .iter()
                .any(|e| e.from.name == "Top" && e.to.name == "Mid")
        );
        // Mid -> Deep
        assert!(
            defines
                .iter()
                .any(|e| e.from.name == "Mid" && e.to.name == "Deep")
        );
    }

    #[test]
    fn test_hierarchy_edges_no_children() {
        let extractor = MarkdownExtractor::new();
        let content = "# Solo\n\nJust one section.\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();

        let defines: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Defines)
            .collect();
        assert_eq!(
            defines.len(),
            0,
            "Single section should have no hierarchy edges"
        );
    }

    #[test]
    fn test_frontmatter_ref_edges_outcome() {
        let extractor = MarkdownExtractor::new();
        let content = "---\nid: agent-scoping-accuracy\noutcome: agent-alignment\n---\n\n# Agent Scoping Accuracy\n\nSignal content.\n";
        let result = extractor
            .extract(Path::new(".oh/signals/agent-scoping-accuracy.md"), content)
            .unwrap();

        let depends: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .collect();
        assert_eq!(
            depends.len(),
            1,
            "Should have 1 DependsOn edge for outcome ref"
        );

        let edge = &depends[0];
        assert_eq!(
            edge.to.file,
            PathBuf::from(".oh/outcomes/agent-alignment.md")
        );
        assert_eq!(edge.to.name, "agent-alignment");
    }

    #[test]
    fn test_frontmatter_ref_edges_no_refs() {
        let extractor = MarkdownExtractor::new();
        let content = "---\nid: my-outcome\nstatus: active\n---\n\n# My Outcome\n\nContent.\n";
        let result = extractor
            .extract(Path::new(".oh/outcomes/my-outcome.md"), content)
            .unwrap();

        let depends: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .collect();
        assert_eq!(
            depends.len(),
            0,
            "Non-reference frontmatter keys should not produce edges"
        );
    }

    #[test]
    fn test_link_edges_relative_path() {
        let extractor = MarkdownExtractor::new();
        let content = "# Overview\n\nSee [signals](./signals/agent-scoping.md) for details.\n";
        let result = extractor
            .extract(Path::new(".oh/outcomes/agent-alignment.md"), content)
            .unwrap();

        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();
        assert_eq!(refs.len(), 1, "Should have 1 References edge for link");
        assert_eq!(
            refs[0].to.file,
            PathBuf::from(".oh/outcomes/signals/agent-scoping.md")
        );
    }

    #[test]
    fn test_link_edges_parent_relative() {
        let extractor = MarkdownExtractor::new();
        let content = "# Signal\n\nSee [outcome](../outcomes/agent-alignment.md).\n";
        let result = extractor
            .extract(Path::new(".oh/signals/my-signal.md"), content)
            .unwrap();

        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0].to.file,
            PathBuf::from(".oh/outcomes/agent-alignment.md")
        );
    }

    #[test]
    fn test_link_edges_skip_external_urls() {
        let extractor = MarkdownExtractor::new();
        let content = "# Links\n\nSee [docs](https://example.com) and [mail](mailto:a@b.com).\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();

        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();
        assert_eq!(refs.len(), 0, "External URLs should not produce edges");
    }

    #[test]
    fn test_link_edges_skip_anchors() {
        let extractor = MarkdownExtractor::new();
        let content = "# Intro\n\nSee [below](#details).\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();

        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();
        assert_eq!(refs.len(), 0, "Anchor-only links should not produce edges");
    }

    #[test]
    fn test_link_edges_strip_anchor_from_path() {
        let extractor = MarkdownExtractor::new();
        let content = "# Doc\n\nSee [section](./other.md#heading).\n";
        let result = extractor
            .extract(Path::new("docs/readme.md"), content)
            .unwrap();

        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].to.file, PathBuf::from("docs/other.md"));
    }

    #[test]
    fn test_all_edge_types_combined() {
        let extractor = MarkdownExtractor::new();
        let content = "---\nid: my-signal\noutcome: agent-alignment\n---\n\n# My Signal\n\nSee [guardrail](../guardrails/no-lang.md).\n\n## Metrics\n\nDetails.\n";
        let result = extractor
            .extract(Path::new(".oh/signals/my-signal.md"), content)
            .unwrap();

        let defines: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Defines)
            .collect();
        let depends: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .collect();
        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();

        assert!(!defines.is_empty(), "Should have hierarchy edges");
        assert!(!depends.is_empty(), "Should have frontmatter ref edges");
        assert!(!refs.is_empty(), "Should have link edges");
    }

    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path(Path::new("a/b/../c")), PathBuf::from("a/c"));
        assert_eq!(normalize_path(Path::new("a/./b")), PathBuf::from("a/b"));
        assert_eq!(normalize_path(Path::new("./a/b")), PathBuf::from("a/b"));
    }

    #[test]
    fn test_normalize_path_preserves_leading_parent() {
        // Leading .. should be preserved when there's nothing to pop
        assert_eq!(
            normalize_path(Path::new("../outside.md")),
            PathBuf::from("../outside.md")
        );
        assert_eq!(
            normalize_path(Path::new("../../up.md")),
            PathBuf::from("../../up.md")
        );
    }

    #[test]
    fn test_normalize_path_absolute_stays_absolute() {
        // .. should not pop past root directory
        assert_eq!(
            normalize_path(Path::new("/foo/../../..")),
            PathBuf::from("/")
        );
        assert_eq!(
            normalize_path(Path::new("/a/b/../c")),
            PathBuf::from("/a/c")
        );
    }

    #[test]
    fn test_link_edges_skip_non_markdown_targets() {
        let extractor = MarkdownExtractor::new();
        let content = "# Doc\n\nSee [code](../src/lib.rs) and [license](../LICENSE).\n";
        let result = extractor
            .extract(Path::new("docs/readme.md"), content)
            .unwrap();
        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();
        assert_eq!(
            refs.len(),
            0,
            "Non-markdown targets should not produce edges"
        );
    }

    #[test]
    fn test_link_edges_dedup_same_target_different_anchors() {
        let extractor = MarkdownExtractor::new();
        let content = "# Doc\n\nSee [sec1](./other.md#one) and [sec2](./other.md#two).\n";
        let result = extractor
            .extract(Path::new("docs/readme.md"), content)
            .unwrap();
        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();
        assert_eq!(
            refs.len(),
            1,
            "Same target with different anchors should produce one edge"
        );
    }

    // --- Adversarial tests ---

    #[test]
    fn test_empty_markdown_no_edges() {
        let extractor = MarkdownExtractor::new();
        let result = extractor.extract(Path::new("empty.md"), "").unwrap();
        assert!(
            result.edges.is_empty(),
            "Empty markdown should produce no edges"
        );
    }

    #[test]
    fn test_frontmatter_only_no_hierarchy_edges() {
        let extractor = MarkdownExtractor::new();
        let content = "---\nid: test\nstatus: active\n---\n";
        let result = extractor.extract(Path::new("fm.md"), content).unwrap();
        let defines: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Defines)
            .collect();
        assert_eq!(
            defines.len(),
            0,
            "Frontmatter-only doc has no heading hierarchy"
        );
    }

    #[test]
    fn test_multiple_links_in_one_section() {
        let extractor = MarkdownExtractor::new();
        let content = "# Links\n\nSee [a](./a.md), [b](./b.md), and [c](https://ext.com).\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();
        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::References)
            .collect();
        // 2 local links, 1 external (skipped)
        assert_eq!(
            refs.len(),
            2,
            "Should emit edges for 2 local links, skip 1 external"
        );
    }

    #[test]
    fn test_frontmatter_empty_value_no_edge() {
        let extractor = MarkdownExtractor::new();
        let content = "---\nid: test\noutcome:\n---\n\n# Test\n\nContent.\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();
        let depends: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .collect();
        assert_eq!(
            depends.len(),
            0,
            "Empty frontmatter value should not produce edge"
        );
    }

    #[test]
    fn test_sibling_headings_at_same_level() {
        // Regression: ensure sibling headings don't create parent-child edges between each other
        let extractor = MarkdownExtractor::new();
        let content = "## A\n\nContent A.\n\n## B\n\nContent B.\n\n## C\n\nContent C.\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();
        let defines: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Defines)
            .collect();
        assert_eq!(
            defines.len(),
            0,
            "Same-level siblings should not have hierarchy edges"
        );
    }

    #[test]
    fn test_adr_validation_pass_links_frontmatter_to_exact_rust_test_path() {
        let adr_frontmatter = Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("docs/ADRs/002-arcswap-graph-concurrency.md"),
                name: "frontmatter".to_string(),
                kind: NodeKind::MarkdownSection,
            },
            language: "markdown".to_string(),
            line_start: 1,
            line_end: 6,
            signature: "[frontmatter]".to_string(),
            body: "id: 002-arcswap-graph-concurrency\nstatus: implementing\nvalidate:\n  cargo_tests:\n    - server::tests::test_arcswap_readers_see_consistent_snapshots\n".to_string(),
            metadata: BTreeMap::from([("is_frontmatter".to_string(), "true".to_string())]),
            source: ExtractionSource::Markdown,
        };
        let adr_section = Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("docs/ADRs/002-arcswap-graph-concurrency.md"),
                name: "ArcSwap for graph concurrency".to_string(),
                kind: NodeKind::MarkdownSection,
            },
            language: "markdown".to_string(),
            line_start: 8,
            line_end: 20,
            signature: "ArcSwap for graph concurrency".to_string(),
            body: "# ArcSwap for graph concurrency".to_string(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::Markdown,
        };
        let rust_test = Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("src/server/mod.rs"),
                name: "test_arcswap_readers_see_consistent_snapshots".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 10,
            signature: "async fn test_arcswap_readers_see_consistent_snapshots()".to_string(),
            body: String::new(),
            metadata: BTreeMap::from([
                ("is_test".to_string(), "true".to_string()),
                (
                    "test_path".to_string(),
                    "server::tests::test_arcswap_readers_see_consistent_snapshots".to_string(),
                ),
            ]),
            source: ExtractionSource::TreeSitter,
        };
        let python_test_same_leaf = Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("tests/test_server.py"),
                name: "test_arcswap_readers_see_consistent_snapshots".to_string(),
                kind: NodeKind::Function,
            },
            language: "python".to_string(),
            line_start: 1,
            line_end: 5,
            signature: "def test_arcswap_readers_see_consistent_snapshots()".to_string(),
            body: String::new(),
            metadata: BTreeMap::from([("is_test".to_string(), "true".to_string())]),
            source: ExtractionSource::TreeSitter,
        };

        let edges = adr_validation_pass(&[
            adr_frontmatter,
            adr_section.clone(),
            rust_test.clone(),
            python_test_same_leaf,
        ]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from, adr_section.id);
        assert_eq!(edges[0].to, rust_test.id);
        assert_eq!(edges[0].kind, EdgeKind::References);
    }

    #[test]
    fn test_adr_validation_pass_skips_when_exact_test_path_missing() {
        let adr_frontmatter = Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("docs/ADRs/001-event-bus-extraction-pipeline.md"),
                name: "frontmatter".to_string(),
                kind: NodeKind::MarkdownSection,
            },
            language: "markdown".to_string(),
            line_start: 1,
            line_end: 6,
            signature: "[frontmatter]".to_string(),
            body: "id: 001-event-bus-extraction-pipeline\nstatus: implemented\nvalidate:\n  cargo_tests:\n    - extract::event_bus::tests::test_depth_first_ordering\n".to_string(),
            metadata: BTreeMap::from([("is_frontmatter".to_string(), "true".to_string())]),
            source: ExtractionSource::Markdown,
        };
        let adr_section = Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("docs/ADRs/001-event-bus-extraction-pipeline.md"),
                name: "RNA Event Bus Architecture".to_string(),
                kind: NodeKind::MarkdownSection,
            },
            language: "markdown".to_string(),
            line_start: 8,
            line_end: 20,
            signature: "RNA Event Bus Architecture".to_string(),
            body: "# RNA Event Bus Architecture".to_string(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::Markdown,
        };
        let rust_test_wrong_path = Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("src/extract/event_bus.rs"),
                name: "test_depth_first_ordering".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 10,
            signature: "async fn test_depth_first_ordering()".to_string(),
            body: String::new(),
            metadata: BTreeMap::from([
                ("is_test".to_string(), "true".to_string()),
                (
                    "test_path".to_string(),
                    "other::tests::test_depth_first_ordering".to_string(),
                ),
            ]),
            source: ExtractionSource::TreeSitter,
        };

        let edges = adr_validation_pass(&[adr_frontmatter, adr_section, rust_test_wrong_path]);
        assert!(
            edges.is_empty(),
            "exact cargo test path must match test_path metadata"
        );
    }

    #[test]
    fn test_adr_backreference_pass_links_test_to_real_markdown_node() {
        let adr_section = Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("docs/ADRs/002-arcswap-graph-concurrency.md"),
                name: "ArcSwap for graph concurrency".to_string(),
                kind: NodeKind::MarkdownSection,
            },
            language: "markdown".to_string(),
            line_start: 8,
            line_end: 20,
            signature: "ArcSwap for graph concurrency".to_string(),
            body: "# ArcSwap for graph concurrency".to_string(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::Markdown,
        };
        let test_fn = Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("src/server/mod.rs"),
                name: "test_arcswap_readers_see_consistent_snapshots".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 10,
            signature: "async fn test_arcswap_readers_see_consistent_snapshots()".to_string(),
            body: String::new(),
            metadata: BTreeMap::from([
                ("is_test".to_string(), "true".to_string()),
                (
                    "adr_refs".to_string(),
                    "docs/ADRs/002-arcswap-graph-concurrency.md".to_string(),
                ),
            ]),
            source: ExtractionSource::TreeSitter,
        };

        let edges = adr_backreference_pass(&[adr_section.clone(), test_fn.clone()]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from, test_fn.id);
        assert_eq!(edges[0].to, adr_section.id);
        assert_eq!(edges[0].kind, EdgeKind::References);
    }

    #[test]
    fn test_adr_validation_pass_does_not_link_across_roots() {
        // Two roots, identical relative paths. Without root-scoped lookup, the
        // ADR in `app` would either ambiguously match both tests (skipped) or
        // mis-link to the test in `lib`.
        let make_adr = |root: &str| -> [Node; 2] {
            [
                Node {
                    id: NodeId {
                        root: root.to_string(),
                        file: PathBuf::from("docs/ADRs/001-shared.md"),
                        name: "frontmatter".to_string(),
                        kind: NodeKind::MarkdownSection,
                    },
                    language: "markdown".to_string(),
                    line_start: 1,
                    line_end: 4,
                    signature: "[frontmatter]".to_string(),
                    body: "validate:\n  cargo_tests:\n    - mymod::tests::test_thing\n".to_string(),
                    metadata: BTreeMap::from([("is_frontmatter".to_string(), "true".to_string())]),
                    source: ExtractionSource::Markdown,
                },
                Node {
                    id: NodeId {
                        root: root.to_string(),
                        file: PathBuf::from("docs/ADRs/001-shared.md"),
                        name: "Shared".to_string(),
                        kind: NodeKind::MarkdownSection,
                    },
                    language: "markdown".to_string(),
                    line_start: 6,
                    line_end: 12,
                    signature: "# Shared".to_string(),
                    body: String::new(),
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::Markdown,
                },
            ]
        };
        let make_test = |root: &str| Node {
            id: NodeId {
                root: root.to_string(),
                file: PathBuf::from("src/mymod.rs"),
                name: "test_thing".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 1,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::from([
                ("is_test".to_string(), "true".to_string()),
                (
                    "test_path".to_string(),
                    "mymod::tests::test_thing".to_string(),
                ),
            ]),
            source: ExtractionSource::TreeSitter,
        };

        let mut nodes: Vec<Node> = Vec::new();
        nodes.extend(make_adr("app"));
        nodes.extend(make_adr("lib"));
        nodes.push(make_test("app"));
        nodes.push(make_test("lib"));

        let edges = adr_validation_pass(&nodes);
        assert_eq!(
            edges.len(),
            2,
            "each root should produce exactly one in-root edge; got: {:?}",
            edges
        );
        for edge in &edges {
            assert_eq!(
                edge.from.root, edge.to.root,
                "adr_validation_pass must not link across NodeId.root boundaries"
            );
        }
    }

    #[test]
    fn test_adr_backreference_pass_does_not_link_across_roots() {
        // Identical relative paths in two roots. The test in `app` references
        // its own ADR; without root scoping it could resolve to the `lib` ADR
        // (or both, ambiguously).
        let make_adr = |root: &str| Node {
            id: NodeId {
                root: root.to_string(),
                file: PathBuf::from("docs/ADRs/001-shared.md"),
                name: "Shared".to_string(),
                kind: NodeKind::MarkdownSection,
            },
            language: "markdown".to_string(),
            line_start: 1,
            line_end: 5,
            signature: "# Shared".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::Markdown,
        };
        let make_test = |root: &str| Node {
            id: NodeId {
                root: root.to_string(),
                file: PathBuf::from("src/lib.rs"),
                name: "test_thing".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 1,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::from([
                ("is_test".to_string(), "true".to_string()),
                (
                    "adr_refs".to_string(),
                    "docs/ADRs/001-shared.md".to_string(),
                ),
            ]),
            source: ExtractionSource::TreeSitter,
        };

        let nodes = vec![
            make_adr("app"),
            make_adr("lib"),
            make_test("app"),
            make_test("lib"),
        ];

        let edges = adr_backreference_pass(&nodes);
        assert_eq!(edges.len(), 2, "expected one edge per root; got: {edges:?}");
        for edge in &edges {
            assert_eq!(
                edge.from.root, edge.to.root,
                "adr_backreference_pass must not link across NodeId.root boundaries"
            );
        }
    }

    #[test]
    fn timing_smoke_adr_passes_100k_nodes() {
        use std::time::Instant;

        let mut nodes = Vec::new();
        for i in 0..100_000 {
            nodes.push(Node {
                id: NodeId {
                    root: String::new(),
                    file: PathBuf::from(format!("src/module_{i}.rs")),
                    name: format!("symbol_{i}"),
                    kind: NodeKind::Function,
                },
                language: "rust".to_string(),
                line_start: 1,
                line_end: 1,
                signature: String::new(),
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            });
        }

        nodes.push(Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("docs/ADRs/001-event-bus-extraction-pipeline.md"),
                name: "frontmatter".to_string(),
                kind: NodeKind::MarkdownSection,
            },
            language: "markdown".to_string(),
            line_start: 1,
            line_end: 6,
            signature: "[frontmatter]".to_string(),
            body: "id: 001-event-bus-extraction-pipeline\nstatus: implemented\nvalidate:\n  cargo_tests:\n    - extract::event_bus::tests::test_depth_first_ordering\n".to_string(),
            metadata: BTreeMap::from([("is_frontmatter".to_string(), "true".to_string())]),
            source: ExtractionSource::Markdown,
        });
        nodes.push(Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("docs/ADRs/001-event-bus-extraction-pipeline.md"),
                name: "RNA Event Bus Architecture".to_string(),
                kind: NodeKind::MarkdownSection,
            },
            language: "markdown".to_string(),
            line_start: 8,
            line_end: 20,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::Markdown,
        });
        nodes.push(Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("src/extract/event_bus.rs"),
                name: "test_depth_first_ordering".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 1,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::from([
                ("is_test".to_string(), "true".to_string()),
                (
                    "test_path".to_string(),
                    "extract::event_bus::tests::test_depth_first_ordering".to_string(),
                ),
                (
                    "adr_refs".to_string(),
                    "docs/ADRs/001-event-bus-extraction-pipeline.md".to_string(),
                ),
            ]),
            source: ExtractionSource::TreeSitter,
        });

        // Functional assertions: forward + backward each emit exactly one edge
        // for the single ADR/test pair seeded into the 100k-node fixture. The
        // count proves both passes touched every node in the input (any early-exit
        // bug would lose the seeded match) without depending on wall-clock timing.
        let start = Instant::now();
        let forward = adr_validation_pass(&nodes);
        let backward = adr_backreference_pass(&nodes);
        let elapsed = start.elapsed();

        assert_eq!(forward.len(), 1, "forward pass must produce exactly 1 edge");
        assert_eq!(
            backward.len(),
            1,
            "backward pass must produce exactly 1 edge"
        );
        // Print elapsed for visibility but do not assert on it; wall-clock is
        // CI-flaky and the budget is enforced via the regression test in
        // `crate::extract::consumers::tests::adr_passes_timing_regression`.
        println!("ADR passes on 100k nodes (forward + backward): {elapsed:?}");
    }
    // --- Adversarial: agent memory detection boundary conditions ---

    #[test]
    fn test_cursor_settings_not_tagged_as_cursor_rule() {
        // .cursor/settings/ is NOT a rule file — but the current implementation
        // tags all .cursor/** files. This test documents that known behavior.
        // If this becomes a problem in practice, a more specific pattern can be used.
        // For now, intentionally accepting the broad match since .cursor/ is nearly
        // always used for rules only.
        let result = detect_oh_kind(Path::new(".cursor/settings/keybindings.json"));
        // Currently tagged — documented as known behavior, not a bug to fix now.
        assert_eq!(
            result,
            Some("cursor-rule".to_string()),
            ".cursor/** is broadly tagged as cursor-rule (documented behavior)"
        );
    }

    #[test]
    fn test_dotfile_not_in_agent_location_no_tag() {
        // Random dotfiles should not get agent memory tags
        assert_eq!(detect_oh_kind(Path::new(".editorconfig")), None);
        assert_eq!(detect_oh_kind(Path::new(".gitignore")), None);
        assert_eq!(detect_oh_kind(Path::new(".env")), None);
        assert_eq!(detect_oh_kind(Path::new(".rubocop.yml")), None);
    }

    #[test]
    fn test_serena_outside_memories_not_tagged() {
        // Only .serena/memories/ — not .serena/ itself or other subdirs
        assert_eq!(detect_oh_kind(Path::new(".serena/config.json")), None);
        assert_eq!(detect_oh_kind(Path::new(".serena/data/something.md")), None);
        // But memories/ subdir IS tagged
        assert_eq!(
            detect_oh_kind(Path::new(".serena/memories/note.md")),
            Some("serena-memory".to_string())
        );
    }

    #[test]
    fn test_cursorrules_not_tagged_when_not_root_component() {
        // A file named .cursorrules deep in a subdirectory should still be tagged
        // (detect_oh_kind scans all components, not just the last)
        let result = detect_oh_kind(Path::new("some/nested/.cursorrules"));
        assert_eq!(
            result,
            Some("cursor-rule".to_string()),
            "Nested .cursorrules should also be tagged"
        );
    }

    #[test]
    fn test_github_other_markdown_not_tagged_as_copilot() {
        // Only the specific copilot-instructions.md file gets the tag
        assert_eq!(detect_oh_kind(Path::new(".github/CONTRIBUTING.md")), None);
        assert_eq!(detect_oh_kind(Path::new(".github/SECURITY.md")), None);
        assert_eq!(
            detect_oh_kind(Path::new(".github/ISSUE_TEMPLATE/bug.md")),
            None
        );
    }

    #[test]
    fn body_ast_emits_required_source_backed_constructs() {
        let source = "# Title {#stable-title}\n\nParagraph with [link](#stable-title), ![caption](image.png), and note[^1].\n\n> quoted\n\n```rust\nfn main() {}\n```\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n[^1]: citation\n\n<!-- prompt: explain the result -->\n<!-- exercise: try another input -->\n";
        let mut result = MarkdownExtractor::new()
            .extract(Path::new("docs/example.md"), source)
            .unwrap();
        let anchor_edges = markdown_anchor_pass(&mut result.nodes);
        let kinds: HashSet<&str> = result
            .nodes
            .iter()
            .filter_map(|node| node.metadata.get("markdown_kind").map(String::as_str))
            .collect();

        for required in [
            "heading",
            "paragraph",
            "link",
            "image",
            "citation",
            "footnote",
            "blockquote",
            "code_fence",
            "table",
            "table_row",
            "table_cell",
            "html_comment",
            "prompt",
            "exercise",
            "caption",
        ] {
            assert!(kinds.contains(required), "missing {required}: {kinds:?}");
        }

        for node in result
            .nodes
            .iter()
            .filter(|node| node.metadata.contains_key("markdown_kind"))
        {
            for key in [
                "body_node_id",
                "file_path",
                "line_start",
                "line_end",
                "byte_start",
                "byte_end",
                "snippet_hash",
                "extractor_id",
                "confidence",
                "validation_status",
            ] {
                assert!(
                    node.metadata.contains_key(key),
                    "{} lacks {key}",
                    node.id.name
                );
            }
            let start: usize = node.metadata["byte_start"].parse().unwrap();
            let end: usize = node.metadata["byte_end"].parse().unwrap();
            assert_eq!(node.body, source[start..end]);
            assert_eq!(
                node.metadata["snippet_hash"],
                blake3::hash(source[start..end].as_bytes())
                    .to_hex()
                    .to_string()
            );
        }

        assert!(anchor_edges.iter().any(|edge| {
            edge.kind == EdgeKind::References && edge.confidence == Confidence::Confirmed
        }));
    }

    #[test]
    fn explicit_inline_id_preserves_identity_when_heading_moves() {
        let extractor = MarkdownExtractor::new();
        let before = extractor
            .extract(Path::new("doc.md"), "# Stable {#same-id}\n")
            .unwrap();
        let after = extractor
            .extract(Path::new("doc.md"), "Prelude\n\n# Stable {#same-id}\n")
            .unwrap();
        let explicit = |result: &ExtractionResult| {
            result
                .nodes
                .iter()
                .find(|node| node.metadata.get("explicit_id") == Some(&"same-id".to_string()))
                .unwrap()
                .id
                .name
                .clone()
        };
        assert_eq!(explicit(&before), explicit(&after));
        assert!(explicit(&before).ends_with("::body::explicit:same-id"));
        assert_eq!(percent_encode("same id/✓"), "same%20id%2F%E2%9C%93");
    }

    #[test]
    fn structural_identity_survives_text_edit_but_hash_changes() {
        let extractor = MarkdownExtractor::new();
        let before = extractor.extract(Path::new("doc.md"), "First.\n").unwrap();
        let after = extractor.extract(Path::new("doc.md"), "Second.\n").unwrap();
        let before_paragraph = before
            .nodes
            .iter()
            .find(|node| node.metadata.get("markdown_kind") == Some(&"paragraph".to_string()))
            .unwrap();
        let after_paragraph = after
            .nodes
            .iter()
            .find(|node| node.metadata.get("markdown_kind") == Some(&"paragraph".to_string()))
            .unwrap();
        assert_eq!(before_paragraph.id, after_paragraph.id);
        assert_ne!(
            before_paragraph.metadata["snippet_hash"],
            after_paragraph.metadata["snippet_hash"]
        );
    }

    #[test]
    fn unresolved_same_file_anchor_emits_contract_diagnostic() {
        let mut result = MarkdownExtractor::new()
            .extract(Path::new("doc.md"), "[missing](#does-not-exist)\n")
            .unwrap();
        assert!(markdown_anchor_pass(&mut result.nodes).is_empty());
        let diagnostic = result
            .nodes
            .iter()
            .find(|node| {
                node.metadata.get("diagnostic_code")
                    == Some(&"content.unresolved_anchor".to_string())
            })
            .expect("unresolved anchor diagnostic");
        assert_eq!(diagnostic.metadata["validation_status"], "unresolved");
        assert_eq!(diagnostic.metadata["diagnostic_severity"], "error");
        assert!(diagnostic.metadata["diagnostic_message"].contains("does not resolve"));
    }

    #[test]
    fn cross_file_anchor_resolves_only_against_exact_target() {
        let extractor = MarkdownExtractor::new();
        let mut nodes = extractor
            .extract(Path::new("docs/source.md"), "[target](target.md#exact)\n")
            .unwrap()
            .nodes;
        nodes.extend(
            extractor
                .extract(Path::new("docs/target.md"), "# Target {#exact}\n")
                .unwrap()
                .nodes,
        );
        let edges = markdown_anchor_pass(&mut nodes);
        assert_eq!(edges.len(), 1);
        assert!(edges[0].to.file.ends_with("docs/target.md"));
        let link = nodes
            .iter()
            .find(|node| node.metadata.get("target_anchor") == Some(&"exact".to_string()))
            .unwrap();
        assert_eq!(link.metadata["validation_status"], "valid");
        assert!(!link.metadata.contains_key("diagnostic_code"));
    }

    #[test]
    fn reference_style_anchor_uses_parser_resolved_destination() {
        let extractor = MarkdownExtractor::new();
        let mut nodes = extractor
            .extract(
                Path::new("docs/source.md"),
                "[target][ref]\n\n[ref]: target.md#exact\n",
            )
            .unwrap()
            .nodes;
        nodes.extend(
            extractor
                .extract(Path::new("docs/target.md"), "# Target {#exact}\n")
                .unwrap()
                .nodes,
        );
        assert_eq!(markdown_anchor_pass(&mut nodes).len(), 1);
    }

    #[test]
    fn selector_paths_reject_absolute_and_repo_escape_paths() {
        for path in [Path::new("/tmp/doc.md"), Path::new("../doc.md")] {
            let result = MarkdownExtractor::new().extract(path, "Body.\n").unwrap();
            let body = result
                .nodes
                .iter()
                .find(|node| node.metadata.get("markdown_kind") == Some(&"paragraph".to_string()))
                .unwrap();
            assert_eq!(body.metadata["validation_status"], "invalid");
            assert_eq!(
                body.metadata["diagnostic_code"],
                "content.invalid_selector_path"
            );
            assert!(body.metadata.contains_key("diagnostic_message"));
        }
    }

    #[test]
    fn merged_contract_fixtures_drive_extractor_validation() {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/content_source_contract");
        let positive = std::fs::read_to_string(root.join("body-backed-control.md")).unwrap();
        let mut positive_nodes = MarkdownExtractor::new()
            .extract(Path::new("body-backed-control.md"), &positive)
            .unwrap()
            .nodes;
        assert!(positive_nodes.iter().any(|node| {
            node.metadata.get("markdown_kind") == Some(&"paragraph".to_string())
                && node.metadata.get("validation_status") == Some(&"valid".to_string())
                && node.metadata.contains_key("snippet_hash")
        }));
        let broken = std::fs::read_to_string(root.join("broken-anchor.md")).unwrap();
        let mut broken_nodes = MarkdownExtractor::new()
            .extract(Path::new("broken-anchor.md"), &broken)
            .unwrap()
            .nodes;
        markdown_anchor_pass(&mut broken_nodes);
        assert!(broken_nodes.iter().any(|node| {
            node.metadata.get("diagnostic_code") == Some(&"content.unresolved_anchor".to_string())
        }));
        // Keep the mutable binding used above meaningful: the positive corpus
        // must not acquire an unresolved selector when it has no broken link.
        assert!(markdown_anchor_pass(&mut positive_nodes).is_empty());
    }

    #[test]
    fn duplicate_explicit_ids_are_invalid() {
        let result = MarkdownExtractor::new()
            .extract(Path::new("doc.md"), "# One {#dup}\n\n# Two {#dup}\n")
            .unwrap();
        assert!(result.nodes.iter().any(|node| {
            node.metadata.get("diagnostic_code") == Some(&"content.duplicate_body_id".to_string())
                && node.metadata.get("validation_status") == Some(&"invalid".to_string())
        }));
    }

    #[test]
    fn captions_use_exact_nonempty_alt_text_span() {
        let result = MarkdownExtractor::new()
            .extract(
                Path::new("doc.md"),
                "![caption](image.png) ![](empty.png)\n",
            )
            .unwrap();
        let captions: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| node.metadata.get("markdown_kind") == Some(&"caption".to_string()))
            .collect();
        assert_eq!(captions.len(), 1);
        assert_eq!(captions[0].body, "caption");
        assert_eq!(captions[0].line_start, 1);
    }

    #[test]
    fn selector_name_col_uses_lsp_utf16_code_units() {
        let result = MarkdownExtractor::new()
            .extract(Path::new("doc.md"), "🚀 Follow [source](src/app.py)\n")
            .unwrap();
        let link = result
            .nodes
            .iter()
            .find(|node| node.metadata.get("markdown_kind") == Some(&"link".to_string()))
            .unwrap();

        assert_eq!(link.metadata.get("name_col"), Some(&"10".to_string()));
        assert_ne!(link.metadata.get("name_col"), Some(&"12".to_string()));
    }

    #[test]
    fn duplicate_generated_anchors_are_diagnosed_not_resolved() {
        let mut nodes = MarkdownExtractor::new()
            .extract(Path::new("doc.md"), "# Same\n\n# Same\n\n[link](#same)\n")
            .unwrap()
            .nodes;
        assert!(markdown_anchor_pass(&mut nodes).is_empty());
        assert_eq!(
            nodes
                .iter()
                .filter(|node| {
                    node.metadata.get("diagnostic_code")
                        == Some(&"content.duplicate_anchor".to_string())
                })
                .count(),
            2
        );
    }

    #[test]
    fn external_fragment_links_do_not_become_repository_targets() {
        let result = MarkdownExtractor::new()
            .extract(
                Path::new("doc.md"),
                "[external](https://example.com/doc#part)\n",
            )
            .unwrap();
        let link = result
            .nodes
            .iter()
            .find(|node| node.metadata.get("markdown_kind") == Some(&"link".to_string()))
            .unwrap();
        assert!(!link.metadata.contains_key("target_anchor"));
    }

    #[test]
    fn repeated_citations_have_occurrence_identity_and_label_metadata() {
        let result = MarkdownExtractor::new()
            .extract(
                Path::new("doc.md"),
                "First[^n], second[^n].\n\n[^n]: note\n",
            )
            .unwrap();
        let citations: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| node.metadata.get("markdown_kind") == Some(&"citation".to_string()))
            .collect();
        assert_eq!(citations.len(), 2);
        assert_ne!(citations[0].id, citations[1].id);
        assert!(
            citations
                .iter()
                .all(|node| node.metadata.get("citation_label") == Some(&"n".to_string()))
        );
    }

    #[test]
    fn markdown_anchor_pass_timing_smoke_100k_nodes() {
        use std::time::Instant;
        let template = MarkdownExtractor::new()
            .extract(Path::new("doc.md"), "Body.\n")
            .unwrap()
            .nodes
            .into_iter()
            .find(|node| node.metadata.get("markdown_kind") == Some(&"paragraph".to_string()))
            .unwrap();
        let mut nodes = Vec::with_capacity(100_000);
        for index in 0..100_000 {
            let mut node = template.clone();
            node.id.name = format!("paragraph-{index}");
            nodes.push(node);
        }
        let started = Instant::now();
        let edges = markdown_anchor_pass(&mut nodes);
        println!(
            "markdown_anchor_pass on 100k nodes: {:?}",
            started.elapsed()
        );
        assert!(edges.is_empty());
        assert_eq!(nodes.len(), 100_000);
    }
}
