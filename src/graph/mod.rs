//! Unified graph model for code, topology, schema, and business context.
//!
//! This module provides the core types, persistent LanceDB storage, and
//! in-memory petgraph index for structural traversal. See solution-space-graph.md
//! for the design rationale (Option D: hybrid LanceDB + petgraph).

pub mod index;
pub mod store;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Node identity
// ---------------------------------------------------------------------------

/// Uniquely identifies a node in the graph. The combination of root, file,
/// name, and kind produces a deterministic string ID for LanceDB upserts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId {
    /// Repository root identifier (supports multi-root workspaces).
    pub root: String,
    /// File where this node is defined.
    pub file: PathBuf,
    /// Name of the symbol / schema / component / artifact.
    pub name: String,
    /// What kind of node this is.
    pub kind: NodeKind,
}

impl NodeId {
    /// Deterministic string ID for storage and lookup.
    pub fn to_stable_id(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.root,
            self.file.display(),
            self.name,
            self.kind
        )
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_stable_id())
    }
}

// ---------------------------------------------------------------------------
// Node kinds
// ---------------------------------------------------------------------------

/// The kind of a graph node. Covers code symbols, schemas, topology, and
/// business artifacts. `Other(String)` is the escape hatch for new extractors.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Function,
    Struct,
    Trait,
    Enum,
    TypeAlias,
    Module,
    Import,
    Const,
    Impl,
    ProtoMessage,
    SqlTable,
    ApiEndpoint,
    /// A macro definition (Rust `macro_rules!`, C/C++ `#define`, etc.).
    Macro,
    /// A struct/class/enum field or record member.
    Field,
    /// A merged PR (branch merge to base branch). The natural unit of meaningful change.
    PrMerge,
    /// An enum variant (e.g. `Option::Some`, `Color::Red`).
    EnumVariant,
    /// A markdown section heading with its content.
    MarkdownSection,
    Other(String),
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeKind::Function => write!(f, "function"),
            NodeKind::Struct => write!(f, "struct"),
            NodeKind::Trait => write!(f, "trait"),
            NodeKind::Enum => write!(f, "enum"),
            NodeKind::TypeAlias => write!(f, "type_alias"),
            NodeKind::Module => write!(f, "module"),
            NodeKind::Import => write!(f, "import"),
            NodeKind::Const => write!(f, "const"),
            NodeKind::Impl => write!(f, "impl"),
            NodeKind::ProtoMessage => write!(f, "proto_message"),
            NodeKind::SqlTable => write!(f, "sql_table"),
            NodeKind::ApiEndpoint => write!(f, "api_endpoint"),
            NodeKind::Macro => write!(f, "macro"),
            NodeKind::Field => write!(f, "field"),
            NodeKind::PrMerge => write!(f, "pr_merge"),
            NodeKind::EnumVariant => write!(f, "enum_variant"),
            NodeKind::MarkdownSection => write!(f, "markdown_section"),
            NodeKind::Other(s) => write!(f, "{}", s),
        }
    }
}

impl NodeKind {
    /// Whether this node kind carries enough semantic signal to be worth embedding.
    /// Imports, consts, module declarations, impl blocks, and PR merges are noise
    /// for semantic search — the meaningful content lives in the symbols they contain
    /// or in commit messages (which are embedded separately).
    pub fn is_embeddable(&self) -> bool {
        match self {
            NodeKind::Function
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Enum
            | NodeKind::TypeAlias
            | NodeKind::Macro
            | NodeKind::ProtoMessage
            | NodeKind::SqlTable
            | NodeKind::ApiEndpoint
            | NodeKind::MarkdownSection
            | NodeKind::Other(_) => true,

            NodeKind::Import
            | NodeKind::Const
            | NodeKind::Module
            | NodeKind::Impl
            | NodeKind::Field
            | NodeKind::EnumVariant
            | NodeKind::PrMerge => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Edge kinds
// ---------------------------------------------------------------------------

/// The kind of relationship between two nodes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EdgeKind {
    Calls,
    Implements,
    DependsOn,
    ConnectsTo,
    Defines,
    HasField,
    Evolves,
    ReferencedBy,
    /// A concrete construction site constructs a locally declared struct.
    /// Currently emitted only for Rust struct literals; other extractors can opt in
    /// by emitting the same construction metadata when their semantics are defined.
    /// Direction: construction site → struct declaration.
    Constructs,
    /// A node references another (e.g., markdown link to another file).
    References,
    TopologyBoundary,
    /// PR modified this symbol/schema/component.
    Modified,
    /// PR affected this topology component.
    Affected,
    /// PR serves this outcome (from commit tags or file patterns).
    Serves,
    /// A test function covers a production symbol.
    /// Direction: test_fn → production_fn
    TestedBy,
    /// A symbol belongs to a module/package node.
    /// Direction: symbol → module
    BelongsTo,
    /// A public re-export edge (e.g. `pub use`, `export { X }`, `__all__`).
    /// Emitted when a module publicly re-exports a symbol defined elsewhere.
    ReExports,
    /// A subsystem or symbol uses a detected framework.
    /// Direction: subsystem/symbol → framework node
    UsesFramework,
    /// A symbol/handler produces events to a channel/topic.
    /// Direction: producer → channel
    Produces,
    /// A symbol/handler consumes events from a channel/topic.
    /// Direction: consumer → channel
    Consumes,
    /// Repo-local/domain-specific relationship kind.
    ///
    /// Custom edges use the persisted edge label directly (for example,
    /// `supports` or `requires_verification`) and intentionally share the
    /// conservative low default PageRank weight used by weak structural edges.
    Other(String),
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EdgeKind::Calls => write!(f, "calls"),
            EdgeKind::Implements => write!(f, "implements"),
            EdgeKind::DependsOn => write!(f, "depends_on"),
            EdgeKind::ConnectsTo => write!(f, "connects_to"),
            EdgeKind::Defines => write!(f, "defines"),
            EdgeKind::HasField => write!(f, "has_field"),
            EdgeKind::Evolves => write!(f, "evolves"),
            EdgeKind::ReferencedBy => write!(f, "referenced_by"),
            EdgeKind::Constructs => write!(f, "constructs"),
            EdgeKind::References => write!(f, "references"),
            EdgeKind::TopologyBoundary => write!(f, "topology_boundary"),
            EdgeKind::Modified => write!(f, "modified"),
            EdgeKind::Affected => write!(f, "affected"),
            EdgeKind::Serves => write!(f, "serves"),
            EdgeKind::TestedBy => write!(f, "tested_by"),
            EdgeKind::BelongsTo => write!(f, "belongs_to"),
            EdgeKind::ReExports => write!(f, "re_exports"),
            EdgeKind::UsesFramework => write!(f, "uses_framework"),
            EdgeKind::Produces => write!(f, "produces"),
            EdgeKind::Consumes => write!(f, "consumes"),
            EdgeKind::Other(s) => write!(f, "{}", s),
        }
    }
}

impl EdgeKind {
    /// Parse an edge label into a built-in or repo-local edge kind.
    pub fn from_label(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }

        Some(match s {
            "calls" => EdgeKind::Calls,
            "implements" => EdgeKind::Implements,
            "depends_on" => EdgeKind::DependsOn,
            "connects_to" => EdgeKind::ConnectsTo,
            "defines" => EdgeKind::Defines,
            "has_field" => EdgeKind::HasField,
            "evolves" => EdgeKind::Evolves,
            "referenced_by" => EdgeKind::ReferencedBy,
            "constructs" => EdgeKind::Constructs,
            "references" => EdgeKind::References,
            "topology_boundary" => EdgeKind::TopologyBoundary,
            "modified" => EdgeKind::Modified,
            "affected" => EdgeKind::Affected,
            "serves" => EdgeKind::Serves,
            "tested_by" => EdgeKind::TestedBy,
            "belongs_to" => EdgeKind::BelongsTo,
            "re_exports" => EdgeKind::ReExports,
            "uses_framework" => EdgeKind::UsesFramework,
            "produces" => EdgeKind::Produces,
            "consumes" => EdgeKind::Consumes,
            other => EdgeKind::Other(other.to_string()),
        })
    }
}

impl Serialize for EdgeKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for EdgeKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        EdgeKind::from_label(&s)
            .ok_or_else(|| serde::de::Error::custom("edge kind must not be empty"))
    }
}

// ---------------------------------------------------------------------------
// Extraction metadata
// ---------------------------------------------------------------------------

/// How a node or edge was discovered.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionSource {
    TreeSitter,
    Lsp,
    Schema,
    /// Extracted from git history (merge commits, diff analysis).
    Git,
    /// Extracted from markdown parsing (pulldown-cmark).
    Markdown,
}

impl fmt::Display for ExtractionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExtractionSource::TreeSitter => write!(f, "tree_sitter"),
            ExtractionSource::Lsp => write!(f, "lsp"),
            ExtractionSource::Schema => write!(f, "schema"),
            ExtractionSource::Git => write!(f, "git"),
            ExtractionSource::Markdown => write!(f, "markdown"),
        }
    }
}

/// Confidence level for an extracted relationship.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Automatically detected by an extractor.
    Detected,
    /// Confirmed by a human or a higher-confidence source.
    Confirmed,
}

/// Lifecycle state for source evidence backing a content-derived edge.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    Valid,
    Stale,
    Unresolved,
    Invalid,
}

/// An exact repository body span that supports an edge.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EvidenceSelector {
    /// Workspace root slug for the selected file. Legacy persisted selectors
    /// may omit this; they are accepted only when their file/body identity is
    /// unique across the loaded graph.
    #[serde(default)]
    pub root_id: String,
    pub file_path: PathBuf,
    pub line_start: usize,
    pub line_end: usize,
    pub byte_start: usize,
    pub byte_end: usize,
    pub body_node_id: String,
    pub snippet_hash: String,
    pub snippet: String,
}

/// Generic, domain-neutral provenance for a content-derived relationship.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EdgeEvidence {
    pub selectors: Vec<EvidenceSelector>,
    pub extractor_id: String,
    pub pack_id: Option<String>,
    pub rule_id: String,
    /// Confidence declared by the producing extractor/pack. Validation status
    /// determines whether the containing edge may currently retain that trust;
    /// the declared value remains intact so a later source restoration can
    /// recover confidence without regenerating the edge.
    pub confidence: Confidence,
    pub validation_status: ValidationStatus,
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Confidence::Detected => write!(f, "detected"),
            Confidence::Confirmed => write!(f, "confirmed"),
        }
    }
}

// ---------------------------------------------------------------------------
// Scope — workspace/root/repo identity for source registration
// ---------------------------------------------------------------------------

/// Identifies the scope a fact belongs to. Supports future Context Assembler
/// source registration without rewriting extractors.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Scope {
    /// Workspace identifier (user-level, spans roots).
    pub workspace_id: Option<String>,
    /// Root identifier (e.g., "zettelkasten", "project-x").
    pub root_id: String,
    /// Git repo identifier (remote URL or local path hash), if git-aware.
    pub repo_id: Option<String>,
    /// Branch name, if relevant.
    pub branch: Option<String>,
    /// Commit SHA at extraction time, if git-aware.
    pub commit_sha: Option<String>,
}

// ---------------------------------------------------------------------------
// Source envelope — canonical record wrapper for replay/outbox
// ---------------------------------------------------------------------------

/// Canonical source envelope wrapping every extracted fact (node or edge).
/// Designed for deterministic replay and future FEED publishing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceEnvelope<T> {
    /// Unique event ID (UUID v4).
    pub event_id: String,
    /// Deterministic idempotency key derived from content.
    /// Same input -> same key -> safe to replay.
    pub idempotency_key: String,
    /// Source identifier: "code.workspace:v1"
    pub source: String,
    /// Schema version for the payload.
    pub source_schema_version: String,
    /// Fact type: "code.symbol", "code.edge", "code.pr_merge", etc.
    pub fact_type: String,
    /// When this fact was extracted.
    pub event_time: SystemTime,
    /// Scope context.
    pub scope: Scope,
    /// The actual payload.
    pub payload: T,
}

impl<T> SourceEnvelope<T> {
    /// Wrap a payload in a source envelope with standard defaults.
    pub fn wrap(payload: T, fact_type: &str, idempotency_key: String, scope: Scope) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            idempotency_key,
            source: "code.workspace:v1".to_string(),
            source_schema_version: "0.1.0".to_string(),
            fact_type: fact_type.to_string(),
            event_time: SystemTime::now(),
            scope,
            payload,
        }
    }
}

// ---------------------------------------------------------------------------
// Core structs
// ---------------------------------------------------------------------------

/// A node in the code graph: a symbol, schema element, component, or artifact.
/// This is the canonical source record -- independent of LanceDB layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    /// Programming language (e.g., "rust", "python", "protobuf").
    pub language: String,
    pub line_start: usize,
    pub line_end: usize,
    /// The declaration / signature line(s).
    pub signature: String,
    /// Full body text of the node.
    pub body: String,
    /// Arbitrary key-value metadata for extractor-specific data.
    pub metadata: BTreeMap<String, String>,
    /// Which extractor produced this node.
    pub source: ExtractionSource,
}

impl Node {
    /// Deterministic string ID, delegated to `NodeId`.
    pub fn stable_id(&self) -> String {
        self.id.to_stable_id()
    }
}

/// A directed edge in the code graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
    pub source: ExtractionSource,
    pub confidence: Confidence,
    /// Ordered source evidence. Ordinary code edges leave this empty; content
    /// edges retain material selectors even when validation later downgrades them.
    #[serde(default)]
    pub evidence: Vec<EdgeEvidence>,
}

impl Edge {
    /// Deterministic string ID for storage.
    pub fn stable_id(&self) -> String {
        let base = format!(
            "{}->{}->{}",
            self.from.to_stable_id(),
            self.kind,
            self.to.to_stable_id()
        );
        if self.evidence.is_empty() {
            base
        } else {
            // Lifecycle state and confidence may change after revalidation; they
            // are deliberately excluded from identity. The rendered snippet is
            // refreshed from current source after validation and is display-only,
            // while line/byte coordinates may shift without changing the body
            // node. Identity therefore uses stable body identity/content plus
            // source/rule provenance.
            let identity: Vec<_> = self
                .evidence
                .iter()
                .map(|e| {
                    let selectors: Vec<_> = e
                        .selectors
                        .iter()
                        .map(|selector| {
                            (
                                &selector.root_id,
                                &selector.file_path,
                                &selector.body_node_id,
                                &selector.snippet_hash,
                            )
                        })
                        .collect();
                    (selectors, &e.extractor_id, &e.pack_id, &e.rule_id)
                })
                .collect();
            let encoded = serde_json::to_vec(&identity).unwrap_or_default();
            format!("{base}->evidence:{}", blake3::hash(&encoded).to_hex())
        }
    }

    /// Revalidate content evidence against the current extracted Markdown body nodes.
    /// Any non-valid selector synchronously removes confirmed confidence.
    pub fn revalidate_evidence(&mut self, nodes: &[Node]) {
        let index = EvidenceNodeIndex::new(nodes);
        self.revalidate_evidence_with_index(&index);
    }

    fn revalidate_evidence_with_index(&mut self, index: &EvidenceNodeIndex<'_>) {
        let was_lifecycle_downgraded = self.confidence == Confidence::Detected
            && self.evidence.iter().any(|evidence| {
                evidence.confidence == Confidence::Confirmed
                    && evidence.validation_status != ValidationStatus::Valid
            });
        let all_evidence_confirmed = self
            .evidence
            .iter()
            .all(|evidence| evidence.confidence == Confidence::Confirmed);
        let mut all_evidence_valid = true;
        for evidence in &mut self.evidence {
            let custom_provenance_is_valid = !matches!(self.kind, EdgeKind::Other(_))
                || (!evidence.extractor_id.trim().is_empty()
                    && !evidence.rule_id.trim().is_empty()
                    && evidence
                        .pack_id
                        .as_deref()
                        .is_some_and(|id| !id.trim().is_empty()));
            let mut status = if custom_provenance_is_valid {
                ValidationStatus::Valid
            } else {
                ValidationStatus::Invalid
            };
            if custom_provenance_is_valid {
                for selector in &mut evidence.selectors {
                    let matching = index.get(selector);
                    let Some(node) = matching else {
                        status = ValidationStatus::Unresolved;
                        break;
                    };
                    let hash = blake3::hash(node.body.as_bytes()).to_hex().to_string();
                    let ranges_match = node.line_start == selector.line_start
                        && node.line_end == selector.line_end
                        && node.metadata.get("byte_start").and_then(|v| v.parse().ok())
                            == Some(selector.byte_start)
                        && node.metadata.get("byte_end").and_then(|v| v.parse().ok())
                            == Some(selector.byte_end);
                    if hash != selector.snippet_hash || !ranges_match {
                        status = ValidationStatus::Stale;
                        break;
                    }
                    // The snippet is display data, not authority. Refresh it only
                    // after the selector's current hash and source ranges validate
                    // so MCP output cannot render producer-supplied stale text as
                    // valid evidence.
                    selector.snippet.clone_from(&node.body);
                }
            }
            if evidence.selectors.is_empty() {
                status = ValidationStatus::Invalid;
            }
            evidence.validation_status = status;
            if evidence.validation_status != ValidationStatus::Valid {
                all_evidence_valid = false;
            }
        }
        if !self.evidence.is_empty() {
            if all_evidence_valid && all_evidence_confirmed {
                if was_lifecycle_downgraded {
                    self.confidence = Confidence::Confirmed;
                }
            } else {
                // Effective edge trust cannot exceed either lifecycle validity
                // or the confidence declared by every supporting evidence record.
                self.confidence = Confidence::Detected;
            }
        }
    }
}

struct EvidenceNodeIndex<'a> {
    scoped: HashMap<(String, PathBuf, String), &'a Node>,
    legacy_unique: HashMap<(PathBuf, String), Option<&'a Node>>,
}

impl<'a> EvidenceNodeIndex<'a> {
    fn new(nodes: &'a [Node]) -> Self {
        let mut scoped = HashMap::new();
        let mut legacy_unique = HashMap::new();
        for node in nodes {
            let Some(body_node_id) = node.metadata.get("body_node_id") else {
                continue;
            };
            scoped.insert(
                (
                    node.id.root.clone(),
                    node.id.file.clone(),
                    body_node_id.clone(),
                ),
                node,
            );
            legacy_unique
                .entry((node.id.file.clone(), body_node_id.clone()))
                .and_modify(|candidate| *candidate = None)
                .or_insert(Some(node));
        }
        Self {
            scoped,
            legacy_unique,
        }
    }

    fn get(&self, selector: &EvidenceSelector) -> Option<&'a Node> {
        if selector.root_id.is_empty() {
            self.legacy_unique
                .get(&(selector.file_path.clone(), selector.body_node_id.clone()))
                .copied()
                .flatten()
        } else {
            self.scoped
                .get(&(
                    selector.root_id.clone(),
                    selector.file_path.clone(),
                    selector.body_node_id.clone(),
                ))
                .copied()
        }
    }
}

/// Revalidate a graph's evidence in O(nodes + selectors), avoiding an edge×node scan.
///
/// Returns the edges whose confidence or evidence lifecycle changed so
/// incremental writers can upsert the downgraded records.
pub fn revalidate_edge_evidence(edges: &mut [Edge], nodes: &[Node]) -> Vec<Edge> {
    let index = EvidenceNodeIndex::new(nodes);
    let mut changed = Vec::new();
    for edge in edges {
        let previous_confidence = edge.confidence.clone();
        let previous_evidence = edge.evidence.clone();
        edge.revalidate_evidence_with_index(&index);
        if edge.confidence != previous_confidence || edge.evidence != previous_evidence {
            changed.push(edge.clone());
        }
    }
    changed
}

// ---------------------------------------------------------------------------
// Edge deduplication
// ---------------------------------------------------------------------------

/// A pre-built index mapping `(file, line)` pairs to the nodes that span
/// that line. Built once, then used for repeated lookups.
pub type FileLineIndex<'a> = HashMap<(PathBuf, usize), Vec<&'a Node>>;

/// Build a `HashSet` of existing edge stable IDs for O(1) dedup lookups.
///
/// Enrichers that produce edges from a second source (LSP, SCIP, etc.) can
/// use this set to skip edges that were already discovered by tree-sitter or
/// another enricher. The key is `Edge::stable_id()` which encodes
/// `from -> kind -> to` directionality, so A->B and B->A are distinct.
///
/// # Example
/// ```ignore
/// let existing = edge_id_set(&existing_edges);
/// for candidate in new_edges {
///     if !existing.contains(&candidate.stable_id()) {
///         result.push(candidate);
///     }
/// }
/// ```
pub fn edge_id_set(edges: &[Edge]) -> HashSet<String> {
    edges.iter().map(|e| e.stable_id()).collect()
}

// ---------------------------------------------------------------------------
// Node line-range lookup
// ---------------------------------------------------------------------------

/// Build a `FileLineIndex` from a slice of nodes.
///
/// For each node, every line in `[line_start..=line_end]` gets an entry
/// pointing back to that node. This enables O(1) lookup of "which nodes
/// contain this line?" -- useful for mapping diagnostics, coverage data,
/// or external references back to graph nodes.
pub fn build_file_line_index<'a>(nodes: &'a [Node]) -> FileLineIndex<'a> {
    let mut index: FileLineIndex<'a> = HashMap::new();
    for node in nodes {
        for line in node.line_start..=node.line_end {
            index
                .entry((node.id.file.clone(), line))
                .or_default()
                .push(node);
        }
    }
    index
}

/// Find the narrowest enclosing node at a given file + line.
///
/// Given a `FileLineIndex`, returns the `NodeId` of the tightest-enclosing
/// node (smallest `line_end - line_start`) that matches the allowed kinds.
/// Only considers `Function`, `Impl`, and `Struct` kinds -- skips `Module`,
/// `Import`, and other kinds that span large ranges without semantic
/// enclosure meaning.
///
/// Use case: mapping an external reference (LSP location, SCIP occurrence,
/// diagnostic) to the function or struct that "owns" it.
pub fn find_enclosing_node(index: &FileLineIndex<'_>, file: &Path, line: usize) -> Option<NodeId> {
    index.get(&(file.to_path_buf(), line)).and_then(|nodes| {
        nodes
            .iter()
            .filter(|n| {
                matches!(
                    n.id.kind,
                    NodeKind::Function | NodeKind::Impl | NodeKind::Struct
                )
            })
            .min_by_key(|n| n.line_end.saturating_sub(n.line_start))
            .map(|n| n.id.clone())
    })
}

/// Find the narrowest node at a given file + line, considering only
/// semantically meaningful kinds (Function, Struct, Trait, Enum, Const).
///
/// Unlike `find_enclosing_node`, this is used for exact-match lookups
/// (e.g., "what symbol is defined at this line?") rather than enclosure.
pub fn find_node_at(index: &FileLineIndex<'_>, file: &Path, line: usize) -> Option<NodeId> {
    index.get(&(file.to_path_buf(), line)).and_then(|nodes| {
        nodes
            .iter()
            .filter(|n| {
                matches!(
                    n.id.kind,
                    NodeKind::Function
                        | NodeKind::Struct
                        | NodeKind::Trait
                        | NodeKind::Enum
                        | NodeKind::TypeAlias
                        | NodeKind::Const
                        | NodeKind::Macro
                        | NodeKind::EnumVariant
                )
            })
            .min_by_key(|n| n.line_end.saturating_sub(n.line_start))
            .map(|n| n.id.clone())
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(
        file: &str,
        name: &str,
        kind: NodeKind,
        line_start: usize,
        line_end: usize,
    ) -> Node {
        Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind,
            },
            language: "rust".to_string(),
            line_start,
            line_end,
            signature: format!("fn {}()", name),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_edge(from_name: &str, to_name: &str, kind: EdgeKind) -> Edge {
        Edge {
            from: NodeId {
                root: String::new(),
                file: PathBuf::from("src/lib.rs"),
                name: from_name.to_string(),
                kind: NodeKind::Function,
            },
            to: NodeId {
                root: String::new(),
                file: PathBuf::from("src/lib.rs"),
                name: to_name.to_string(),
                kind: NodeKind::Function,
            },
            kind,
            source: ExtractionSource::TreeSitter,
            confidence: Confidence::Detected,
            evidence: Vec::new(),
        }
    }

    fn evidence_for(node: &Node, hash: String) -> EdgeEvidence {
        EdgeEvidence {
            selectors: vec![EvidenceSelector {
                root_id: node.id.root.clone(),
                file_path: node.id.file.clone(),
                line_start: node.line_start,
                line_end: node.line_end,
                byte_start: node.metadata["byte_start"].parse().unwrap(),
                byte_end: node.metadata["byte_end"].parse().unwrap(),
                body_node_id: node.metadata["body_node_id"].clone(),
                snippet_hash: hash,
                snippet: node.body.clone(),
            }],
            extractor_id: "markdown-ast@1".into(),
            pack_id: Some("test-pack@1".into()),
            rule_id: "supports-body@1".into(),
            confidence: Confidence::Confirmed,
            validation_status: ValidationStatus::Valid,
        }
    }

    #[test]
    fn evidence_distinguishes_otherwise_identical_edges() {
        let mut first = make_edge("claim", "fact", EdgeKind::Other("supports".into()));
        let mut second = first.clone();
        let mut node = make_node(
            "chapter.md",
            "body",
            NodeKind::Other("paragraph".into()),
            3,
            3,
        );
        node.id.file = PathBuf::from("chapter.md");
        node.body = "first span".into();
        node.metadata.insert(
            "body_node_id".into(),
            "chapter.md::body::ast:paragraph[0]".into(),
        );
        node.metadata.insert("byte_start".into(), "10".into());
        node.metadata.insert("byte_end".into(), "20".into());
        first.evidence = vec![evidence_for(
            &node,
            blake3::hash(node.body.as_bytes()).to_hex().to_string(),
        )];
        node.body = "other span".into();
        second.evidence = vec![evidence_for(
            &node,
            blake3::hash(node.body.as_bytes()).to_hex().to_string(),
        )];
        assert_ne!(first.stable_id(), second.stable_id());
    }

    #[test]
    fn evidence_location_shift_preserves_edge_identity() {
        let mut node = make_node(
            "chapter.md",
            "body",
            NodeKind::Other("paragraph".into()),
            3,
            3,
        );
        node.id.file = PathBuf::from("chapter.md");
        node.body = "stable evidence".into();
        node.metadata.insert(
            "body_node_id".into(),
            "chapter.md::body::ast:paragraph[0]".into(),
        );
        node.metadata.insert("byte_start".into(), "10".into());
        node.metadata.insert("byte_end".into(), "25".into());
        let mut edge = make_edge("claim", "fact", EdgeKind::Other("supports".into()));
        edge.evidence = vec![evidence_for(
            &node,
            blake3::hash(node.body.as_bytes()).to_hex().to_string(),
        )];
        let original_id = edge.stable_id();

        edge.evidence[0].selectors[0].line_start = 30;
        edge.evidence[0].selectors[0].line_end = 30;
        edge.evidence[0].selectors[0].byte_start = 410;
        edge.evidence[0].selectors[0].byte_end = 425;

        assert_eq!(
            edge.stable_id(),
            original_id,
            "offset-only source movement must remain a same-ID edge update"
        );
    }

    #[test]
    fn editing_body_downgrades_confirmed_edge_to_stale() {
        let mut node = make_node(
            "chapter.md",
            "body",
            NodeKind::Other("paragraph".into()),
            3,
            3,
        );
        node.id.file = PathBuf::from("chapter.md");
        node.body = "current evidence".into();
        node.metadata.insert(
            "body_node_id".into(),
            "chapter.md::body::ast:paragraph[0]".into(),
        );
        node.metadata.insert("byte_start".into(), "10".into());
        node.metadata.insert("byte_end".into(), "26".into());
        let old_hash = blake3::hash(b"old evidence").to_hex().to_string();
        let mut edge = make_edge("claim", "fact", EdgeKind::Other("supports".into()));
        edge.confidence = Confidence::Confirmed;
        edge.evidence = vec![evidence_for(&node, old_hash)];
        edge.revalidate_evidence(&[node]);
        assert_eq!(edge.confidence, Confidence::Detected);
        assert_eq!(
            edge.evidence[0].confidence,
            Confidence::Confirmed,
            "source-declared confidence must survive lifecycle downgrades"
        );
        assert_eq!(edge.evidence[0].validation_status, ValidationStatus::Stale);
    }

    #[test]
    fn valid_evidence_refreshes_untrusted_display_snippet() {
        let mut node = make_node(
            "chapter.md",
            "body",
            NodeKind::Other("paragraph".into()),
            3,
            3,
        );
        node.id.file = PathBuf::from("chapter.md");
        node.body = "current source-backed evidence".into();
        node.metadata.insert(
            "body_node_id".into(),
            "chapter.md::body::ast:paragraph[0]".into(),
        );
        node.metadata.insert("byte_start".into(), "10".into());
        node.metadata.insert("byte_end".into(), "40".into());

        let mut edge = make_edge("claim", "fact", EdgeKind::Other("supports".into()));
        edge.confidence = Confidence::Confirmed;
        edge.evidence = vec![evidence_for(
            &node,
            blake3::hash(node.body.as_bytes()).to_hex().to_string(),
        )];
        edge.evidence[0].selectors[0].snippet = "untrusted producer text".into();
        let stable_id_before_refresh = edge.stable_id();

        edge.revalidate_evidence(&[node.clone()]);

        assert_eq!(edge.confidence, Confidence::Confirmed);
        assert_eq!(edge.evidence[0].validation_status, ValidationStatus::Valid);
        assert_eq!(edge.evidence[0].selectors[0].snippet, node.body);
        assert_eq!(
            edge.stable_id(),
            stable_id_before_refresh,
            "refreshing display-only snippet text must not churn durable edge identity"
        );
    }

    #[test]
    fn custom_evidence_requires_nonblank_provenance_but_generic_evidence_does_not_require_pack() {
        let mut node = make_node(
            "chapter.md",
            "body",
            NodeKind::Other("paragraph".into()),
            3,
            3,
        );
        node.id.file = PathBuf::from("chapter.md");
        node.body = "source-backed evidence".into();
        node.metadata.insert(
            "body_node_id".into(),
            "chapter.md::body::ast:paragraph[0]".into(),
        );
        node.metadata.insert("byte_start".into(), "10".into());
        node.metadata.insert("byte_end".into(), "32".into());
        let hash = blake3::hash(node.body.as_bytes()).to_hex().to_string();

        let mut custom = make_edge("claim", "fact", EdgeKind::Other("supports".into()));
        custom.confidence = Confidence::Confirmed;
        custom.evidence = vec![evidence_for(&node, hash.clone())];
        custom.evidence[0].extractor_id = " ".into();
        custom.evidence[0].rule_id.clear();
        custom.evidence[0].pack_id = Some(" ".into());
        custom.revalidate_evidence(&[node.clone()]);
        assert_eq!(custom.confidence, Confidence::Detected);
        assert_eq!(
            custom.evidence[0].validation_status,
            ValidationStatus::Invalid
        );

        let mut generic = make_edge("claim", "fact", EdgeKind::Calls);
        generic.confidence = Confidence::Confirmed;
        generic.evidence = vec![evidence_for(&node, hash)];
        generic.evidence[0].pack_id = None;
        generic.revalidate_evidence(&[node]);
        assert_eq!(generic.confidence, Confidence::Confirmed);
        assert_eq!(
            generic.evidence[0].validation_status,
            ValidationStatus::Valid
        );
    }

    #[test]
    fn evidence_validation_is_scoped_to_workspace_root() {
        let mut first = make_node(
            "chapter.md",
            "body",
            NodeKind::Other("paragraph".into()),
            3,
            3,
        );
        first.id.root = "first-root".into();
        first.id.file = PathBuf::from("chapter.md");
        first.body = "first root evidence".into();
        first.metadata.insert(
            "body_node_id".into(),
            "chapter.md::body::ast:paragraph[0]".into(),
        );
        first.metadata.insert("byte_start".into(), "10".into());
        first.metadata.insert("byte_end".into(), "29".into());

        let mut second = first.clone();
        second.id.root = "second-root".into();
        second.body = "second root changed".into();

        let mut edge = make_edge("claim", "fact", EdgeKind::Other("supports".into()));
        edge.confidence = Confidence::Confirmed;
        edge.evidence = vec![evidence_for(
            &first,
            blake3::hash(first.body.as_bytes()).to_hex().to_string(),
        )];

        let changed = revalidate_edge_evidence(&mut [edge.clone()], &[second, first]);
        assert!(
            changed.is_empty(),
            "same relative selector in another root must not invalidate exact-root evidence"
        );

        edge.evidence[0].selectors[0].root_id.clear();
        edge.revalidate_evidence(&[
            {
                let mut node = make_node(
                    "chapter.md",
                    "body",
                    NodeKind::Other("paragraph".into()),
                    3,
                    3,
                );
                node.id.root = "first-root".into();
                node.id.file = PathBuf::from("chapter.md");
                node.body = "first root evidence".into();
                node.metadata.insert(
                    "body_node_id".into(),
                    "chapter.md::body::ast:paragraph[0]".into(),
                );
                node.metadata.insert("byte_start".into(), "10".into());
                node.metadata.insert("byte_end".into(), "29".into());
                node
            },
            {
                let mut node = make_node(
                    "chapter.md",
                    "body",
                    NodeKind::Other("paragraph".into()),
                    3,
                    3,
                );
                node.id.root = "second-root".into();
                node.id.file = PathBuf::from("chapter.md");
                node.body = "second root changed".into();
                node.metadata.insert(
                    "body_node_id".into(),
                    "chapter.md::body::ast:paragraph[0]".into(),
                );
                node.metadata.insert("byte_start".into(), "10".into());
                node.metadata.insert("byte_end".into(), "29".into());
                node
            },
        ]);
        assert_eq!(
            edge.evidence[0].validation_status,
            ValidationStatus::Unresolved,
            "legacy unscoped selectors must fail closed when multiple roots match"
        );
        assert_eq!(edge.confidence, Confidence::Detected);
    }

    // -- Macro tests --

    #[test]
    fn test_macro_is_embeddable() {
        assert!(
            NodeKind::Macro.is_embeddable(),
            "Macro should be embeddable"
        );
    }

    #[test]
    fn test_macro_display() {
        assert_eq!(format!("{}", NodeKind::Macro), "macro");
    }

    #[test]
    fn test_find_node_at_finds_macro() {
        let nodes = vec![make_node("src/lib.rs", "my_macro", NodeKind::Macro, 5, 10)];
        let index = build_file_line_index(&nodes);

        let result = find_node_at(&index, &PathBuf::from("src/lib.rs"), 7);
        assert!(result.is_some(), "find_node_at should find Macro nodes");
        assert_eq!(result.unwrap().name, "my_macro");
    }

    // -- TypeAlias tests --

    #[test]
    fn test_type_alias_is_embeddable() {
        assert!(
            NodeKind::TypeAlias.is_embeddable(),
            "TypeAlias should be embeddable"
        );
    }

    #[test]
    fn test_type_alias_display() {
        assert_eq!(format!("{}", NodeKind::TypeAlias), "type_alias");
    }

    #[test]
    fn test_find_node_at_finds_type_alias() {
        let nodes = vec![make_node("src/lib.rs", "Result", NodeKind::TypeAlias, 5, 5)];
        let index = build_file_line_index(&nodes);

        let result = find_node_at(&index, &PathBuf::from("src/lib.rs"), 5);
        assert!(result.is_some(), "find_node_at should find TypeAlias nodes");
        assert_eq!(result.unwrap().name, "Result");
    }

    #[test]
    fn test_find_node_at_finds_enum_variant() {
        let nodes = vec![make_node(
            "src/lib.rs",
            "Active",
            NodeKind::EnumVariant,
            10,
            10,
        )];
        let index = build_file_line_index(&nodes);

        let result = find_node_at(&index, &PathBuf::from("src/lib.rs"), 10);
        assert!(
            result.is_some(),
            "find_node_at should find EnumVariant nodes"
        );
        assert_eq!(result.unwrap().name, "Active");
    }

    #[test]
    fn test_find_node_at_enum_variant_vs_enum_same_line() {
        // Adversarial: enum variant on same line as enclosing enum.
        // find_node_at should prefer the narrower span (variant).
        let nodes = vec![
            make_node("src/lib.rs", "Status", NodeKind::Enum, 5, 10),
            make_node("src/lib.rs", "Active", NodeKind::EnumVariant, 7, 7),
        ];
        let index = build_file_line_index(&nodes);

        // On line 7 (variant line), should find the variant (narrower span)
        let result = find_node_at(&index, &PathBuf::from("src/lib.rs"), 7);
        assert!(result.is_some());
        assert_eq!(
            result.unwrap().name,
            "Active",
            "Should prefer EnumVariant (span 1) over Enum (span 5) on variant line"
        );

        // On line 5 (enum declaration line, no variant), should find the enum
        let result = find_node_at(&index, &PathBuf::from("src/lib.rs"), 5);
        assert!(result.is_some());
        assert_eq!(
            result.unwrap().name,
            "Status",
            "Should find Enum on enum declaration line"
        );
    }

    // -- EdgeKind tests --

    #[test]
    fn test_builtin_edge_kind_json_serialization_stays_string_label() {
        assert_eq!(
            serde_json::to_string(&EdgeKind::DependsOn).expect("serialize built-in edge kind"),
            "\"depends_on\""
        );
        assert_eq!(
            serde_json::from_str::<EdgeKind>("\"depends_on\"")
                .expect("deserialize built-in edge kind"),
            EdgeKind::DependsOn
        );
    }

    #[test]
    fn test_custom_edge_kind_display_and_json_serialization_use_label() {
        let edge_kind = EdgeKind::Other("requires_verification".to_string());

        assert_eq!(edge_kind.to_string(), "requires_verification");
        assert_eq!(
            serde_json::to_string(&edge_kind).expect("serialize custom edge kind"),
            "\"requires_verification\"",
            "custom edge serialization must expose the relationship label, not an opaque Other wrapper"
        );
        assert_eq!(
            serde_json::from_str::<EdgeKind>("\"requires_verification\"")
                .expect("deserialize custom edge kind"),
            edge_kind,
            "custom edge deserialization must preserve the relationship label"
        );
    }

    // -- edge_id_set tests --

    #[test]
    fn test_edge_id_set_empty() {
        let set = edge_id_set(&[]);
        assert!(set.is_empty());
    }

    #[test]
    fn test_edge_id_set_contains_stable_ids() {
        let edges = vec![
            make_edge("foo", "bar", EdgeKind::Calls),
            make_edge("bar", "baz", EdgeKind::DependsOn),
        ];
        let set = edge_id_set(&edges);
        assert_eq!(set.len(), 2);
        assert!(set.contains(&edges[0].stable_id()));
        assert!(set.contains(&edges[1].stable_id()));
    }

    #[test]
    fn test_edge_id_set_directional() {
        // A->B and B->A should be distinct entries
        let edges = vec![
            make_edge("foo", "bar", EdgeKind::Calls),
            make_edge("bar", "foo", EdgeKind::Calls),
        ];
        let set = edge_id_set(&edges);
        assert_eq!(set.len(), 2, "A->B and B->A must be distinct");
    }

    #[test]
    fn test_edge_id_set_dedup_same_edge() {
        // Two identical edges should produce one set entry
        let edges = vec![
            make_edge("foo", "bar", EdgeKind::Calls),
            make_edge("foo", "bar", EdgeKind::Calls),
        ];
        let set = edge_id_set(&edges);
        assert_eq!(set.len(), 1, "Duplicate edges should collapse in the set");
    }

    // -- build_file_line_index + find_enclosing_node tests --

    #[test]
    fn test_build_file_line_index() {
        let nodes = vec![
            make_node("src/main.rs", "main", NodeKind::Function, 1, 10),
            make_node("src/main.rs", "helper", NodeKind::Function, 12, 20),
        ];
        let index = build_file_line_index(&nodes);

        // Line 5 should map to "main"
        let at_5 = index.get(&(PathBuf::from("src/main.rs"), 5));
        assert!(at_5.is_some());
        assert_eq!(at_5.unwrap().len(), 1);
        assert_eq!(at_5.unwrap()[0].id.name, "main");

        // Line 15 should map to "helper"
        let at_15 = index.get(&(PathBuf::from("src/main.rs"), 15));
        assert!(at_15.is_some());
        assert_eq!(at_15.unwrap()[0].id.name, "helper");

        // Line 25 should be absent
        let at_25 = index.get(&(PathBuf::from("src/main.rs"), 25));
        assert!(at_25.is_none());
    }

    #[test]
    fn test_find_enclosing_node_prefers_narrowest() {
        let nodes = vec![
            make_node("src/lib.rs", "outer", NodeKind::Function, 1, 20),
            make_node("src/lib.rs", "inner", NodeKind::Function, 5, 10),
        ];
        let index = build_file_line_index(&nodes);

        // Line 7 is in both, should return inner (narrower)
        let result = find_enclosing_node(&index, &PathBuf::from("src/lib.rs"), 7);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "inner");

        // Line 15 is only in outer
        let result = find_enclosing_node(&index, &PathBuf::from("src/lib.rs"), 15);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "outer");
    }

    #[test]
    fn test_find_enclosing_node_skips_module() {
        let nodes = vec![
            make_node("src/lib.rs", "my_module", NodeKind::Module, 1, 100),
            make_node("src/lib.rs", "real_fn", NodeKind::Function, 5, 10),
        ];
        let index = build_file_line_index(&nodes);

        // Line 7: both Module and Function present, only Function matches
        let result = find_enclosing_node(&index, &PathBuf::from("src/lib.rs"), 7);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "real_fn");

        // Line 50: only Module present, should return None
        let result = find_enclosing_node(&index, &PathBuf::from("src/lib.rs"), 50);
        assert!(result.is_none(), "Module-only lines should return None");
    }

    #[test]
    fn test_find_enclosing_node_skips_import() {
        let nodes = vec![make_node("src/lib.rs", "use_std", NodeKind::Import, 1, 1)];
        let index = build_file_line_index(&nodes);

        let result = find_enclosing_node(&index, &PathBuf::from("src/lib.rs"), 1);
        assert!(
            result.is_none(),
            "Import nodes should not be enclosing nodes"
        );
    }

    // -- find_node_at tests --

    #[test]
    fn test_find_node_at_basic() {
        let nodes = vec![
            make_node("src/main.rs", "main", NodeKind::Function, 1, 10),
            make_node("src/main.rs", "helper", NodeKind::Function, 12, 20),
        ];
        let index = build_file_line_index(&nodes);

        let result = find_node_at(&index, &PathBuf::from("src/main.rs"), 5);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "main");

        let result = find_node_at(&index, &PathBuf::from("src/main.rs"), 15);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "helper");

        let result = find_node_at(&index, &PathBuf::from("src/main.rs"), 25);
        assert!(result.is_none());
    }

    #[test]
    fn test_find_node_at_skips_module() {
        let nodes = vec![
            make_node("src/lib.rs", "my_module", NodeKind::Module, 1, 100),
            make_node("src/lib.rs", "real_fn", NodeKind::Function, 5, 10),
        ];
        let index = build_file_line_index(&nodes);

        let result = find_node_at(&index, &PathBuf::from("src/lib.rs"), 7);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "real_fn");

        // Module-only line
        let result = find_node_at(&index, &PathBuf::from("src/lib.rs"), 50);
        assert!(result.is_none());
    }
}
