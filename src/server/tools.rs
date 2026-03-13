//! MCP tool input structs and deprecated aliases.

use rust_mcp_sdk::macros::{self, JsonSchema};
use serde::{Deserialize, Serialize};

// ── Tool input structs ──────────────────────────────────────────────

#[macros::mcp_tool(
    name = "oh_search_context",
    description = "Semantic search across business context, commits, code, and markdown. Describe what you need in plain language. Returns results ranked 0-1 by relevance; test files are demoted. Enable include_code for ranked symbol search (exact name > contains > signature, production before tests), include_markdown for doc sections. For exact symbol name lookup use search_symbols instead. search_mode: hybrid (default, keyword+vector RRF), keyword (BM25 only), semantic (vector only). Results default to the primary workspace root; pass root: \"all\" for cross-root search."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhSearchContext {
    /// Natural language description of what you're looking for
    pub query: String,
    /// Optional: filter by artifact type (outcome, signal, guardrail, metis, commit)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_types: Option<Vec<String>>,
    /// Maximum results to return (default: 5)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// Also search code symbols by name/signature (default: false)
    #[serde(default)]
    pub include_code: Option<bool>,
    /// Also search markdown sections (default: false)
    #[serde(default)]
    pub include_markdown: Option<bool>,
    /// Search ranking mode: "hybrid" (default, keyword + vector RRF), "keyword" (BM25 only), "semantic" (vector only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_mode: Option<String>,
    /// Filter to a specific workspace root (by slug). Defaults to the primary root. Use "all" for cross-root search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}


#[macros::mcp_tool(
    name = "outcome_progress",
    description = "Track progress on a business outcome. Finds tagged commits, code symbols in changed files, and related markdown. Returns a navigable summary with stable Node IDs for use with search_symbols and graph_query. Set include_impact=true to add risk-classified blast radius showing which symbols are affected by the changes and how critical they are. Results default to the primary workspace root; pass root: \"all\" for cross-root search."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OutcomeProgress {
    /// The outcome ID (e.g. 'agent-alignment') from .oh/outcomes/
    pub outcome_id: String,
    /// When true, compute blast radius for changed symbols and classify risk as CRITICAL/HIGH/MEDIUM/LOW
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_impact: Option<bool>,
    /// Filter to a specific workspace root (by slug). Defaults to the primary root. Use "all" for cross-root search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

// ── Unified search tool ─────────────────────────────────────────────
// Combines the functionality of the former `search_symbols` (flat search)
// and `graph_query` (graph traversal) into a single tool. The old tools
// are kept as deprecated aliases that route here.

#[macros::mcp_tool(
    name = "search",
    description = "Find code symbols and trace their relationships. Without `mode`, performs flat ranked search (by name/signature). With `mode` (neighbors/impact/reachable/tests_for), performs graph traversal from matched symbols. `tests_for` finds which test functions call a symbol. Entry point: `query` (name or semantic search) or `node` (stable ID from previous results). Batch: `nodes` retrieves multiple IDs in one call. `compact: true` returns signature + location only (~25x fewer tokens). Filter by kind, language, file. Sort by relevance, complexity, or importance (PageRank). Results default to the primary workspace root; pass root: \"all\" for cross-root search."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct Search {
    /// Search query string — matched against symbol name and signature for flat search, or used as semantic search for graph traversal entry points
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Start from a known stable node ID (from previous search results). Takes precedence over query for graph traversal entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Graph traversal mode: "neighbors" (direct connections), "impact" (reverse dependents), "reachable" (forward BFS), "tests_for" (which test functions call this symbol). Omit for flat search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Maximum traversal depth (default: 1 for neighbors, 3 for impact/reachable). Only used with mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hops: Option<u32>,
    /// Direction for neighbors mode: "outgoing" (default), "incoming", "both". Only used with mode="neighbors".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// Filter edge types: calls, depends_on, implements, defines, etc. Only used with mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_types: Option<Vec<String>>,
    /// Filter by symbol kind (function, struct, trait, enum, module, import, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Filter by language (rust, python, typescript, go, markdown)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Filter by file path substring
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Filter to a specific workspace root (by slug). Defaults to the primary root. Use "all" for cross-root search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Number of results (flat search, default: 10) or entry points (traversal, default: 1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Sort results by "relevance" (default), "complexity" (descending cyclomatic), or "importance" (descending PageRank)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    /// Minimum cyclomatic complexity threshold. Only return functions with complexity >= this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_complexity: Option<u32>,
    /// If true, include only synthetic (inferred) constants. If false, exclude them. If absent, return all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthetic: Option<bool>,
    /// When true, return compact output: signature + line range + kind + file only (no body). ~25x token reduction for exploration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact: Option<bool>,
    /// Batch retrieve multiple nodes by stable ID. Returns combined results in a single response. Composes with compact and mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Vec<String>>,
    /// Search ranking mode for graph traversal entry-point resolution: "hybrid" (default, keyword + vector RRF), "keyword" (BM25 only), "semantic" (vector only). Only affects how entry nodes are found when using query + mode; flat search always uses name/signature matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_mode: Option<String>,
}

// ── Deprecated aliases (kept for one release cycle) ─────────────────

#[macros::mcp_tool(
    name = "search_symbols",
    description = "DEPRECATED: use `search` instead. Find code symbols by name or signature. This is an alias for `search` without a `mode` parameter."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SearchSymbols {
    /// Search query string (matched against symbol name and signature)
    pub query: String,
    /// Optional: filter by symbol kind (function, struct, trait, enum, module, import, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Optional: filter by language (rust, python, typescript, go, markdown)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Optional: filter by file path substring
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Optional: filter to a specific workspace root (by slug). Defaults to the primary root. Use "all" for cross-root search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Maximum results to return (default: 20)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// If true, include only synthetic (inferred) constants. If false, exclude them. If absent, return all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthetic: Option<bool>,
    /// Optional: minimum cyclomatic complexity threshold. Only return functions with complexity >= this value. When set, query can be empty to search all functions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_complexity: Option<u32>,
    /// Optional: sort results by "complexity" (descending). Default is relevance ranking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
}

impl SearchSymbols {
    /// Convert deprecated SearchSymbols into the unified Search struct.
    /// Preserves the old default limit of 20 (Search defaults to 10).
    pub(crate) fn into_search(self) -> Search {
        Search {
            query: Some(self.query),
            node: None,
            mode: None,
            hops: None,
            direction: None,
            edge_types: None,
            kind: self.kind,
            language: self.language,
            file: self.file,
            root: self.root,
            top_k: Some(self.limit.unwrap_or(20)),
            sort_by: self.sort,
            min_complexity: self.min_complexity,
            synthetic: self.synthetic,
            compact: None,
            nodes: None,
            search_mode: None,
        }
    }
}

#[macros::mcp_tool(
    name = "graph_query",
    description = "DEPRECATED: use `search` with a `mode` parameter instead. Trace code relationships from a symbol or natural language query."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GraphQuery {
    /// Stable ID from search_symbols results. Takes precedence over query if both provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Natural language query to find entry nodes via semantic search (e.g. "authentication handler", "database connection pool"). Used when node_id is not provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Query mode: "neighbors" (default), "impact" (reverse dependents), "reachable" (forward BFS)
    #[serde(default = "default_graph_mode")]
    pub mode: String,
    /// Direction for neighbors mode: "outgoing" (default — what does this call/implement/depend on?), "incoming" (what calls/implements/depends on this?), "both"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// Filter edge types: calls, depends_on, implements, defines, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_types: Option<Vec<String>>,
    /// Maximum hops to traverse (default: 1 for neighbors, 3 for impact/reachable)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_hops: Option<u32>,
    /// Maximum number of entry nodes from semantic search (default: 3). Only used with query parameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
}

impl GraphQuery {
    /// Convert deprecated GraphQuery into the unified Search struct.
    /// Preserves the old default top_k of 3 (Search defaults to 1 for traversal).
    pub(crate) fn into_search(self) -> Search {
        Search {
            query: self.query,
            node: self.node_id,
            mode: Some(self.mode),
            hops: self.max_hops,
            direction: self.direction,
            edge_types: self.edge_types,
            kind: None,
            language: None,
            file: None,
            root: None,
            top_k: Some(self.top_k.unwrap_or(3)),
            sort_by: None,
            min_complexity: None,
            synthetic: None,
            compact: None,
            nodes: None,
            search_mode: None,
        }
    }
}

fn default_graph_mode() -> String {
    "neighbors".to_string()
}

#[macros::mcp_tool(
    name = "list_roots",
    description = "Lists configured workspace roots with their type, path, and scan status."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ListRoots {}

#[macros::mcp_tool(
    name = "repo_map",
    description = "Repository orientation for agents. Returns top symbols by PageRank importance, hotspot files (most definitions), active business outcomes, and entry points (main/handler functions). One call replaces an exploratory loop of search calls. Use this when starting work on an unfamiliar codebase. Results default to the primary workspace root; pass root: \"all\" for cross-root search."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RepoMap {
    /// Number of top symbols to return (default: 15)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_n: Option<u32>,
    /// Filter to a specific workspace root (by slug). Defaults to the primary root. Use "all" for cross-root search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── GraphQuery deserialization edge cases ────────────────────────────

    fn parse_graph_query(v: serde_json::Value) -> Result<GraphQuery, serde_json::Error> {
        serde_json::from_value(v)
    }

    #[test]
    fn test_graph_query_neither_node_id_nor_query() {
        let gq = parse_graph_query(json!({})).unwrap();
        assert!(gq.node_id.is_none());
        assert!(gq.query.is_none());
        assert_eq!(gq.mode, "neighbors");
    }

    #[test]
    fn test_graph_query_both_node_id_and_query() {
        let gq = parse_graph_query(json!({
            "node_id": "test:src/lib.rs:foo:function",
            "query": "authentication handler"
        }))
        .unwrap();
        assert!(gq.node_id.is_some());
        assert!(gq.query.is_some());
    }

    #[test]
    fn test_graph_query_empty_string_query() {
        let gq = parse_graph_query(json!({"query": ""})).unwrap();
        assert_eq!(gq.query, Some("".to_string()));
        assert!(gq.node_id.is_none());
    }

    #[test]
    fn test_graph_query_whitespace_only_query() {
        let gq = parse_graph_query(json!({"query": "   \t\n  "})).unwrap();
        assert_eq!(gq.query, Some("   \t\n  ".to_string()));
    }

    #[test]
    fn test_graph_query_empty_string_node_id() {
        let gq = parse_graph_query(json!({
            "node_id": "",
            "query": "valid query"
        }))
        .unwrap();
        assert_eq!(gq.node_id, Some("".to_string()));
    }

    #[test]
    fn test_graph_query_top_k_zero() {
        let gq = parse_graph_query(json!({"query": "test", "top_k": 0})).unwrap();
        assert_eq!(gq.top_k, Some(0));
    }

    #[test]
    fn test_graph_query_top_k_very_large() {
        let gq = parse_graph_query(json!({"query": "test", "top_k": 999999})).unwrap();
        assert_eq!(gq.top_k, Some(999999));
    }

    #[test]
    fn test_graph_query_top_k_one() {
        let gq = parse_graph_query(json!({"query": "exact symbol", "top_k": 1})).unwrap();
        assert_eq!(gq.top_k, Some(1));
    }

    #[test]
    fn test_graph_query_null_fields_are_none() {
        let gq = parse_graph_query(json!({
            "node_id": null,
            "query": null,
            "top_k": null
        }))
        .unwrap();
        assert!(gq.node_id.is_none());
        assert!(gq.query.is_none());
        assert!(gq.top_k.is_none());
    }

    #[test]
    fn test_graph_query_unicode_query() {
        let gq = parse_graph_query(json!({"query": "日本語のクエリ"})).unwrap();
        assert_eq!(gq.query, Some("日本語のクエリ".to_string()));
    }

    #[test]
    fn test_graph_query_very_long_query() {
        let long = "a".repeat(10000);
        let gq = parse_graph_query(json!({"query": long})).unwrap();
        assert_eq!(gq.query.unwrap().len(), 10000);
    }

    #[test]
    fn test_graph_query_default_mode() {
        let gq = parse_graph_query(json!({})).unwrap();
        assert_eq!(gq.mode, "neighbors");
    }

    #[test]
    fn test_graph_query_top_k_ignored_with_node_id() {
        let gq = parse_graph_query(json!({
            "node_id": "test:src/lib.rs:foo:function",
            "top_k": 5
        }))
        .unwrap();
        assert_eq!(gq.top_k, Some(5));
    }

    #[test]
    fn test_graph_query_negative_top_k_rejected() {
        let result = parse_graph_query(json!({"query": "test", "top_k": -1}));
        assert!(result.is_err());
    }

    #[test]
    fn test_graph_query_top_k_overflow() {
        let result = parse_graph_query(json!({"query": "test", "top_k": 4294967296u64}));
        assert!(result.is_err());
    }

    #[test]
    fn test_graph_query_top_k_default_is_three() {
        let gq: GraphQuery = serde_json::from_value(json!({"query": "test"})).unwrap();
        let search = gq.into_search();
        assert_eq!(search.top_k, Some(3));
    }

    // ── Search struct deserialization ────────────────────────────────────

    fn parse_search(v: serde_json::Value) -> Result<Search, serde_json::Error> {
        serde_json::from_value(v)
    }

    #[test]
    fn test_search_flat_query_only() {
        let s = parse_search(json!({"query": "handle_call_tool_request"})).unwrap();
        assert_eq!(s.query, Some("handle_call_tool_request".to_string()));
        assert!(s.mode.is_none());
        assert!(s.node.is_none());
    }

    #[test]
    fn test_search_flat_with_filters() {
        let s = parse_search(json!({
            "query": "handler",
            "kind": "function",
            "language": "rust",
            "file": "server.rs"
        }))
        .unwrap();
        assert_eq!(s.kind, Some("function".to_string()));
        assert_eq!(s.language, Some("rust".to_string()));
        assert_eq!(s.file, Some("server.rs".to_string()));
    }

    #[test]
    fn test_search_traversal_query_neighbors() {
        let s = parse_search(json!({
            "query": "RnaHandler",
            "mode": "neighbors",
            "direction": "outgoing"
        }))
        .unwrap();
        assert_eq!(s.mode, Some("neighbors".to_string()));
        assert_eq!(s.direction, Some("outgoing".to_string()));
    }

    #[test]
    fn test_search_traversal_with_top_k() {
        let s = parse_search(json!({
            "query": "database",
            "mode": "impact",
            "top_k": 5
        }))
        .unwrap();
        assert_eq!(s.top_k, Some(5));
        assert_eq!(s.mode, Some("impact".to_string()));
    }

    #[test]
    fn test_search_traversal_from_node() {
        let s = parse_search(json!({
            "node": "test:src/server.rs:RnaHandler:struct",
            "mode": "neighbors"
        }))
        .unwrap();
        assert_eq!(s.node, Some("test:src/server.rs:RnaHandler:struct".to_string()));
    }

    #[test]
    fn test_search_impact_from_node() {
        let s = parse_search(json!({
            "node": "test:src/graph/mod.rs:NodeId:struct",
            "mode": "impact",
            "hops": 5
        }))
        .unwrap();
        assert_eq!(s.hops, Some(5));
    }

    #[test]
    fn test_search_flat_sort_by_complexity() {
        let s = parse_search(json!({
            "query": "",
            "sort_by": "complexity",
            "min_complexity": 10
        }))
        .unwrap();
        assert_eq!(s.sort_by, Some("complexity".to_string()));
        assert_eq!(s.min_complexity, Some(10));
    }

    #[test]
    fn test_search_flat_default_top_k_is_10() {
        let s = parse_search(json!({"query": "test"})).unwrap();
        assert!(s.top_k.is_none()); // default applied by handler, not struct
    }

    #[test]
    fn test_search_traversal_default_top_k_is_1() {
        let s = parse_search(json!({"query": "test", "mode": "neighbors"})).unwrap();
        assert!(s.top_k.is_none()); // default applied by handler
    }

    #[test]
    fn test_search_all_fields_empty() {
        let s = parse_search(json!({})).unwrap();
        assert!(s.query.is_none());
        assert!(s.mode.is_none());
        assert!(s.node.is_none());
    }

    #[test]
    fn test_search_hops_parameter() {
        let s = parse_search(json!({
            "node": "test:src/lib.rs:foo:function",
            "mode": "reachable",
            "hops": 5
        }))
        .unwrap();
        assert_eq!(s.hops, Some(5));
    }

    #[test]
    fn test_search_edge_types_filter() {
        let s = parse_search(json!({
            "node": "test:src/lib.rs:foo:function",
            "mode": "neighbors",
            "edge_types": ["calls", "implements"]
        }))
        .unwrap();
        assert_eq!(s.edge_types, Some(vec!["calls".to_string(), "implements".to_string()]));
    }

    #[test]
    fn test_search_extra_fields_ignored() {
        let s = parse_search(json!({
            "query": "test",
            "unknown_field": "should be ignored",
            "another_unknown": 42
        }));
        assert!(s.is_ok());
    }

    #[test]
    fn test_search_tests_for_mode_with_node() {
        let s = parse_search(json!({
            "node": "test:src/lib.rs:foo:function",
            "mode": "tests_for"
        })).unwrap();
        assert_eq!(s.mode, Some("tests_for".to_string()));
    }

    #[test]
    fn test_search_tests_for_mode_with_query() {
        let s = parse_search(json!({
            "query": "build_full_graph",
            "mode": "tests_for"
        })).unwrap();
        assert_eq!(s.mode, Some("tests_for".to_string()));
    }

    #[test]
    fn test_search_compact_param() {
        let s = parse_search(json!({"query": "test", "compact": true})).unwrap();
        assert_eq!(s.compact, Some(true));
    }

    #[test]
    fn test_search_compact_default_is_none() {
        let s = parse_search(json!({"query": "test"})).unwrap();
        assert!(s.compact.is_none());
    }

    #[test]
    fn test_search_nodes_param() {
        let s = parse_search(json!({
            "nodes": ["root:file:name:kind", "root:file2:name2:kind"]
        })).unwrap();
        assert_eq!(s.nodes, Some(vec![
            "root:file:name:kind".to_string(),
            "root:file2:name2:kind".to_string(),
        ]));
    }

    #[test]
    fn test_search_nodes_with_compact() {
        let s = parse_search(json!({
            "nodes": ["root:file:name:kind"],
            "compact": true
        })).unwrap();
        assert_eq!(s.compact, Some(true));
        assert!(s.nodes.is_some());
    }

    #[test]
    fn test_search_nodes_empty_array() {
        let s = parse_search(json!({"nodes": []})).unwrap();
        assert_eq!(s.nodes, Some(vec![]));
    }

    #[test]
    fn test_search_compact_with_traversal() {
        let s = parse_search(json!({
            "node": "root:file:name:kind",
            "mode": "neighbors",
            "compact": true
        })).unwrap();
        assert_eq!(s.compact, Some(true));
        assert_eq!(s.mode, Some("neighbors".to_string()));
    }

    #[test]
    fn test_search_symbols_into_search_has_no_compact_or_nodes() {
        let ss: SearchSymbols = serde_json::from_value(json!({"query": "test"})).unwrap();
        let s = ss.into_search();
        assert!(s.compact.is_none());
        assert!(s.nodes.is_none());
    }

    #[test]
    fn test_graph_query_into_search_has_no_compact_or_nodes() {
        let gq: GraphQuery = serde_json::from_value(json!({
            "query": "test",
            "mode": "neighbors"
        })).unwrap();
        let s = gq.into_search();
        assert!(s.compact.is_none());
        assert!(s.nodes.is_none());
    }

    #[test]
    fn test_search_mode_in_search_struct() {
        let s = parse_search(json!({
            "query": "test",
            "mode": "neighbors",
            "search_mode": "keyword"
        })).unwrap();
        assert_eq!(s.search_mode, Some("keyword".to_string()));
    }

    #[test]
    fn test_search_mode_absent_in_search_struct() {
        let s = parse_search(json!({"query": "test"})).unwrap();
        assert!(s.search_mode.is_none());
    }

    #[test]
    fn test_search_mode_in_oh_search_context_struct() {
        let ctx: OhSearchContext = serde_json::from_value(json!({
            "query": "test",
            "search_mode": "semantic"
        })).unwrap();
        assert_eq!(ctx.search_mode, Some("semantic".to_string()));
    }

    // ── Deprecated alias conversion ─────────────────────────────────────

    #[test]
    fn test_search_symbols_into_search() {
        let ss: SearchSymbols = serde_json::from_value(json!({
            "query": "RnaHandler",
            "kind": "struct",
            "language": "rust",
            "limit": 5,
            "synthetic": false,
            "min_complexity": 10,
            "sort": "complexity"
        }))
        .unwrap();
        let s = ss.into_search();
        assert_eq!(s.query, Some("RnaHandler".to_string()));
        assert!(s.node.is_none());
        assert!(s.mode.is_none());
        assert_eq!(s.kind, Some("struct".to_string()));
        assert_eq!(s.language, Some("rust".to_string()));
        assert_eq!(s.top_k, Some(5));
        assert_eq!(s.synthetic, Some(false));
        assert_eq!(s.min_complexity, Some(10));
        assert_eq!(s.sort_by, Some("complexity".to_string()));
    }

    #[test]
    fn test_search_symbols_into_search_preserves_default_limit() {
        let ss: SearchSymbols = serde_json::from_value(json!({
            "query": "test"
        }))
        .unwrap();
        let s = ss.into_search();
        assert_eq!(s.top_k, Some(20));
    }

    #[test]
    fn test_graph_query_into_search() {
        let gq: GraphQuery = serde_json::from_value(json!({
            "node_id": "test:src/lib.rs:foo:function",
            "query": "fallback query",
            "mode": "impact",
            "direction": "incoming",
            "edge_types": ["calls"],
            "max_hops": 5,
            "top_k": 10
        }))
        .unwrap();
        let s = gq.into_search();
        assert_eq!(s.node, Some("test:src/lib.rs:foo:function".to_string()));
        assert_eq!(s.query, Some("fallback query".to_string()));
        assert_eq!(s.mode, Some("impact".to_string()));
        assert_eq!(s.direction, Some("incoming".to_string()));
        assert_eq!(s.edge_types, Some(vec!["calls".to_string()]));
        assert_eq!(s.hops, Some(5));
        assert_eq!(s.top_k, Some(10));
    }

    #[test]
    fn test_graph_query_into_search_preserves_default_top_k() {
        let gq: GraphQuery = serde_json::from_value(json!({
            "query": "test"
        }))
        .unwrap();
        let s = gq.into_search();
        assert_eq!(s.top_k, Some(3));
    }
}
