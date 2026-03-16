//! MCP tool input structs and deprecated aliases.

use rust_mcp_sdk::macros::{self, JsonSchema};
use serde::{Deserialize, Serialize};

// ── Tool input structs ──────────────────────────────────────────────

#[macros::mcp_tool(
    name = "outcome_progress",
    description = "Track progress on a business outcome. Finds tagged commits, changed symbols, and related docs. Set include_impact=true for risk-classified blast radius."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OutcomeProgress {
    /// Outcome ID (e.g. "agent-alignment")
    pub outcome_id: String,
    /// Add risk-classified blast radius (default: false)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_impact: Option<bool>,
    /// Workspace root slug; "all" for cross-root
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

// ── Unified search tool ─────────────────────────────────────────────
// Unified search tool combining flat symbol search and graph traversal.
// Deprecated aliases (`search_symbols`, `graph_query`) are kept below
// and route here.

#[macros::mcp_tool(
    name = "search",
    description = "USE THIS INSTEAD OF Grep/Read for code understanding. Searches code symbols, docs, business artifacts, and commits in one call. Add `mode` for graph traversal (neighbors/impact/reachable/tests_for). Use `compact: true` to save tokens. Use `rerank: true` for natural language queries."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct Search {
    /// Search query (name, keyword, or natural language)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Stable node ID from previous results
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Traversal: "neighbors", "impact", "reachable", "tests_for"; omit for flat search
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Max traversal depth (default: 1 neighbors, 3 impact/reachable)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hops: Option<u32>,
    /// Neighbors direction: "outgoing" (default), "incoming", "both"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// Edge filter: calls, depends_on, implements, defines, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_types: Option<Vec<String>>,
    /// Symbol kind: function, struct, trait, enum, type_alias, module, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Language: rust, python, typescript, go, markdown
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// File path substring filter
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Workspace root slug; "all" for cross-root
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Max results (flat default: 10, traversal default: 1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Sort: "relevance" (default), "complexity", "importance"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    /// Min cyclomatic complexity threshold
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_complexity: Option<u32>,
    /// Filter synthetic (inferred) constants: true=only, false=exclude
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthetic: Option<bool>,
    /// Compact output: signature + location only (~25x fewer tokens)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact: Option<bool>,
    /// Batch-retrieve multiple node IDs in one call
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Vec<String>>,
    /// Ranking: "hybrid" (default), "keyword", "semantic"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_mode: Option<String>,
    /// Cross-encoder reranking for precision (~100-300ms); default: false
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank: Option<bool>,
    /// Search .oh/ artifacts and commits (default: true)
    #[serde(default = "default_true")]
    pub include_artifacts: Option<bool>,
    /// Search markdown sections (default: true)
    #[serde(default = "default_true")]
    pub include_markdown: Option<bool>,
    /// Artifact filter: outcome, signal, guardrail, metis, commit
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_types: Option<Vec<String>>,
}

fn default_true() -> Option<bool> {
    Some(true)
}

#[macros::mcp_tool(
    name = "list_roots",
    description = "Lists configured workspace roots with their type, path, and scan status."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ListRoots {}

#[macros::mcp_tool(
    name = "repo_map",
    description = "Codebase orientation. Top symbols by importance, hotspot files, active outcomes, entry points. Use when starting on an unfamiliar codebase."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RepoMap {
    /// Number of top symbols (default: 15)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_n: Option<u32>,
    /// Workspace root slug; "all" for cross-root
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn test_search_include_artifacts_default_true() {
        let s = parse_search(json!({"query": "test"})).unwrap();
        assert_eq!(s.include_artifacts, Some(true));
    }

    #[test]
    fn test_search_include_artifacts_explicit_false() {
        let s = parse_search(json!({"query": "test", "include_artifacts": false})).unwrap();
        assert_eq!(s.include_artifacts, Some(false));
    }

    #[test]
    fn test_search_include_markdown_default_true() {
        let s = parse_search(json!({"query": "test"})).unwrap();
        assert_eq!(s.include_markdown, Some(true));
    }

    #[test]
    fn test_search_include_markdown_explicit_false() {
        let s = parse_search(json!({"query": "test", "include_markdown": false})).unwrap();
        assert_eq!(s.include_markdown, Some(false));
    }

    #[test]
    fn test_search_artifact_types_filter() {
        let s = parse_search(json!({
            "query": "test",
            "artifact_types": ["commit", "outcome"]
        })).unwrap();
        assert_eq!(s.artifact_types, Some(vec!["commit".to_string(), "outcome".to_string()]));
    }

    #[test]
    fn test_search_artifact_types_absent() {
        let s = parse_search(json!({"query": "test"})).unwrap();
        assert!(s.artifact_types.is_none());
    }

    #[test]
    fn test_search_code_only_mode() {
        let s = parse_search(json!({
            "query": "handler",
            "include_artifacts": false,
            "include_markdown": false
        })).unwrap();
        assert_eq!(s.include_artifacts, Some(false));
        assert_eq!(s.include_markdown, Some(false));
    }

    #[test]
    fn test_search_include_artifacts_null_becomes_none() {
        let s = parse_search(json!({"query": "test", "include_artifacts": null})).unwrap();
        assert_eq!(s.include_artifacts, None);
    }

    #[test]
    fn test_search_artifact_types_with_artifacts_disabled() {
        let s = parse_search(json!({
            "query": "test",
            "include_artifacts": false,
            "artifact_types": ["commit"]
        })).unwrap();
        assert_eq!(s.include_artifacts, Some(false));
        assert_eq!(s.artifact_types, Some(vec!["commit".to_string()]));
    }

    #[test]
    fn test_search_mode_with_flat_search_and_artifacts() {
        let s = parse_search(json!({
            "query": "test",
            "search_mode": "keyword",
            "include_artifacts": true
        })).unwrap();
        assert_eq!(s.search_mode, Some("keyword".to_string()));
        assert_eq!(s.include_artifacts, Some(true));
        assert!(s.mode.is_none());
    }

    #[test]
    fn test_search_empty_artifact_types_array() {
        let s = parse_search(json!({
            "query": "test",
            "artifact_types": []
        })).unwrap();
        assert_eq!(s.artifact_types, Some(vec![]));
    }

    // ── Rerank parameter tests ───────────────────────────────────────────

    #[test]
    fn test_search_rerank_default_is_none() {
        let s = parse_search(json!({"query": "test"})).unwrap();
        assert!(s.rerank.is_none());
    }

    #[test]
    fn test_search_rerank_true() {
        let s = parse_search(json!({"query": "test", "rerank": true})).unwrap();
        assert_eq!(s.rerank, Some(true));
    }

    #[test]
    fn test_search_rerank_false() {
        let s = parse_search(json!({"query": "test", "rerank": false})).unwrap();
        assert_eq!(s.rerank, Some(false));
    }

    // ── Schema description length guardrail ───────────────────────────────
    // Doc comments on struct fields become JSON schema descriptions via JsonSchema derive.
    // This test ensures no parameter description regresses to multi-sentence verbosity.
    // We test the source strings directly since schemars isn't a direct dependency.

    #[test]
    fn test_param_descriptions_are_slim() {
        // All parameter doc comments from tools.rs, extracted as string literals.
        // If you add a parameter, add its description here.
        let descriptions = vec![
            // OutcomeProgress
            r#"Outcome ID (e.g. "agent-alignment")"#,
            "Add risk-classified blast radius (default: false)",
            // Search
            "Search query (name, keyword, or natural language)",
            "Stable node ID from previous results",
            r#"Traversal: "neighbors", "impact", "reachable", "tests_for"; omit for flat search"#,
            "Max traversal depth (default: 1 neighbors, 3 impact/reachable)",
            r#"Neighbors direction: "outgoing" (default), "incoming", "both""#,
            "Edge filter: calls, depends_on, implements, defines, etc.",
            "Symbol kind: function, struct, trait, enum, type_alias, module, etc.",
            "Language: rust, python, typescript, go, markdown",
            "File path substring filter",
            "Max results (flat default: 10, traversal default: 1)",
            r#"Sort: "relevance" (default), "complexity", "importance""#,
            "Min cyclomatic complexity threshold",
            "Filter synthetic (inferred) constants: true=only, false=exclude",
            "Compact output: signature + location only (~25x fewer tokens)",
            "Batch-retrieve multiple node IDs in one call",
            r#"Ranking: "hybrid" (default), "keyword", "semantic""#,
            "Cross-encoder reranking for precision (~100-300ms); default: false",
            "Search .oh/ artifacts and commits (default: true)",
            "Search markdown sections (default: true)",
            "Artifact filter: outcome, signal, guardrail, metis, commit",
            // RepoMap
            "Number of top symbols (default: 15)",
            // Shared
            r#"Workspace root slug; "all" for cross-root"#,
        ];

        let max_len = 80;
        for desc in &descriptions {
            assert!(
                desc.len() <= max_len,
                "Description too long ({} chars, max {max_len}): {desc:?}",
                desc.len()
            );
        }
    }
}
