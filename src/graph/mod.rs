//! Unified graph model for code, topology, schema, and business context.
//!
//! This module provides the core types, persistent LanceDB storage, and
//! in-memory petgraph index for structural traversal. See solution-space-graph.md
//! for the design rationale (Option D: hybrid LanceDB + petgraph).

pub mod index;
pub mod store;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
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
    Module,
    Import,
    Const,
    Impl,
    ProtoMessage,
    SqlTable,
    ApiEndpoint,
    /// A merged PR (branch merge to base branch). The natural unit of meaningful change.
    PrMerge,
    Other(String),
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeKind::Function => write!(f, "function"),
            NodeKind::Struct => write!(f, "struct"),
            NodeKind::Trait => write!(f, "trait"),
            NodeKind::Enum => write!(f, "enum"),
            NodeKind::Module => write!(f, "module"),
            NodeKind::Import => write!(f, "import"),
            NodeKind::Const => write!(f, "const"),
            NodeKind::Impl => write!(f, "impl"),
            NodeKind::ProtoMessage => write!(f, "proto_message"),
            NodeKind::SqlTable => write!(f, "sql_table"),
            NodeKind::ApiEndpoint => write!(f, "api_endpoint"),
            NodeKind::PrMerge => write!(f, "pr_merge"),
            NodeKind::Other(s) => write!(f, "{}", s),
        }
    }
}

// ---------------------------------------------------------------------------
// Edge kinds
// ---------------------------------------------------------------------------

/// The kind of relationship between two nodes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Calls,
    Implements,
    DependsOn,
    ConnectsTo,
    Defines,
    HasField,
    Evolves,
    ReferencedBy,
    TopologyBoundary,
    /// PR modified this symbol/schema/component.
    Modified,
    /// PR affected this topology component.
    Affected,
    /// PR serves this outcome (from commit tags or file patterns).
    Serves,
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
            EdgeKind::TopologyBoundary => write!(f, "topology_boundary"),
            EdgeKind::Modified => write!(f, "modified"),
            EdgeKind::Affected => write!(f, "affected"),
            EdgeKind::Serves => write!(f, "serves"),
        }
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
    /// Same input → same key → safe to replay.
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
/// This is the canonical source record — independent of LanceDB layout.
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
}

impl Edge {
    /// Deterministic string ID for storage.
    pub fn stable_id(&self) -> String {
        format!(
            "{}->{}->{}",
            self.from.to_stable_id(),
            self.kind,
            self.to.to_stable_id()
        )
    }
}
