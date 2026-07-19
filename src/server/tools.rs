//! MCP tool input structs and deprecated aliases.

use rust_mcp_sdk::macros::{self, JsonSchema};
use serde::de::{self, Unexpected, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

fn u32_from_f64_tolerant<E>(value: f64) -> Result<u32, E>
where
    E: de::Error,
{
    if !value.is_finite() {
        return Err(E::custom("expected a finite number for u32"));
    }
    if value < 0.0 {
        return Err(E::custom(format!(
            "expected a non-negative number for u32, got {value}"
        )));
    }
    if value.fract() != 0.0 {
        return Err(E::custom(format!(
            "expected a whole number for u32, got {value}"
        )));
    }
    if value > u32::MAX as f64 {
        return Err(E::custom(format!("number out of range for u32: {value}")));
    }
    Ok(value as u32)
}

fn deserialize_u32_tolerant<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    struct U32TolerantVisitor;

    impl<'de> Visitor<'de> for U32TolerantVisitor {
        type Value = u32;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a non-negative whole number within u32 range")
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            u32::try_from(value).map_err(|_| E::invalid_value(Unexpected::Unsigned(value), &self))
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            if value < 0 {
                return Err(E::invalid_value(Unexpected::Signed(value), &self));
            }
            u32::try_from(value as u64)
                .map_err(|_| E::invalid_value(Unexpected::Signed(value), &self))
        }

        fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            u32_from_f64_tolerant::<E>(value)
        }

        fn visit_f32<E>(self, value: f32) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            u32_from_f64_tolerant::<E>(f64::from(value))
        }
    }

    deserializer.deserialize_any(U32TolerantVisitor)
}

fn deserialize_option_u32_tolerant<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct U32Tolerant(#[serde(deserialize_with = "deserialize_u32_tolerant")] u32);

    Option::<U32Tolerant>::deserialize(deserializer).map(|value| value.map(|v| v.0))
}

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
    /// Repo path to query (e.g. worktree path); defaults to server repo
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

// ── Unified search tool ─────────────────────────────────────────────
// Unified search tool combining flat symbol search and graph traversal.

#[macros::mcp_tool(
    name = "search",
    description = "USE THIS INSTEAD OF Grep/Read for code understanding. Searches code symbols, docs, business artifacts, and commits in one call. Add `mode` for graph traversal (neighbors/impact/reachable/tests_for/cycles/path) — equivalent to the `graph` CLI command. Use `compact: true` to save tokens. Use `rerank: true` for natural language queries. Use `subsystem` to scope to a subsystem from repo_map. Use `target_subsystem` with mode to find cross-subsystem edges. Use `depth` with mode='neighbors' to walk N levels deep (e.g., module → members → their members). Use `limit` to control max results (flat default: 10, traversal default: 1). Use `include_body: true` to return function bodies; add `minify_body: true` to strip comments and shorten locals."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct Search {
    /// Search query (name, keyword, or natural language)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Stable node ID from previous results
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Traversal mode; omit/null/blank => flat. Trims whitespace. Values: neighbors, impact, reachable, tests_for, cycles, path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Max reachability depth for impact/reachable modes (default: 3). Controls how far the graph walk reaches. Not used for neighbors mode — use depth instead.
    #[serde(
        default,
        deserialize_with = "deserialize_option_u32_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub hops: Option<u32>,
    /// Multi-level neighbors traversal depth for neighbors mode (default: 1). Walk edges N levels deep, accumulating and deduplicating results per level. Only applies to neighbors mode; ignored for impact/reachable (use hops for those).
    #[serde(
        default,
        deserialize_with = "deserialize_option_u32_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub depth: Option<u32>,
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
    /// File path substring filter. With `line`, this becomes an exact or uniquely
    /// resolved repo/root-relative source path. Also accepts `path:line:column`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// First 1-based source line. Enables bounded current-filesystem source retrieval.
    #[serde(
        default,
        deserialize_with = "deserialize_option_u32_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub line: Option<u32>,
    /// Last 1-based source line (inclusive). Defaults to `line`; at most 200 lines.
    #[serde(
        default,
        deserialize_with = "deserialize_option_u32_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub end_line: Option<u32>,
    /// Workspace root slug; "all" for cross-root
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Max results (flat default: 10, traversal default: 1)
    #[serde(
        default,
        alias = "top_k",
        deserialize_with = "deserialize_option_u32_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub limit: Option<u32>,
    /// Sort: "relevance" (default), "complexity", "importance"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    /// Min cyclomatic complexity threshold
    #[serde(
        default,
        deserialize_with = "deserialize_option_u32_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
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
    /// Ranking: "hybrid" (default), "keyword", "semantic", or "strict".
    /// "strict" is the SWE-bench qualification lane: it requires the exact
    /// CI semantic bundle, a ready embedding index, observed Metal execution,
    /// hybrid retrieval without fallback, and successful reranking. It forces
    /// reranking even when `rerank` was omitted. Flat queries from that sealed
    /// bundle enter the same strict lane by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_mode: Option<String>,
    /// Cross-encoder reranking (~100-300ms). Defaults: MCP=true, CLI=false
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
    /// Filter to a specific subsystem (from repo_map)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subsystem: Option<String>,
    /// Cross-subsystem query: show only neighbors in this target subsystem. Use with mode="neighbors" to find edges between subsystems.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_subsystem: Option<String>,
    /// Repo path to query (e.g. worktree path); defaults to server repo
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Include function body in results (default: false)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_body: Option<bool>,
    /// Minify body: strip comments, shorten locals (default: false). Only applies when include_body=true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minify_body: Option<bool>,
    /// Show index stats footer (default: false for MCP, true for CLI)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbose: Option<bool>,
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
    #[serde(
        default,
        deserialize_with = "deserialize_option_u32_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub top_n: Option<u32>,
    /// Workspace root slug; "all" for cross-root
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Repo path to query (e.g. worktree path); defaults to server repo
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
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
    fn test_search_traversal_with_limit() {
        let s = parse_search(json!({
            "query": "database",
            "mode": "impact",
            "limit": 5
        }))
        .unwrap();
        assert_eq!(s.limit, Some(5));
        assert_eq!(s.mode, Some("impact".to_string()));
    }

    #[test]
    fn test_search_top_k_alias_still_works() {
        let s = parse_search(json!({
            "query": "database",
            "top_k": 5
        }))
        .unwrap();
        assert_eq!(s.limit, Some(5));
    }

    #[test]
    fn test_search_traversal_from_node() {
        let s = parse_search(json!({
            "node": "test:src/server.rs:RnaHandler:struct",
            "mode": "neighbors"
        }))
        .unwrap();
        assert_eq!(
            s.node,
            Some("test:src/server.rs:RnaHandler:struct".to_string())
        );
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
    fn test_search_flat_default_limit_is_10() {
        let s = parse_search(json!({"query": "test"})).unwrap();
        assert!(s.limit.is_none()); // default applied by handler, not struct
    }

    #[test]
    fn test_search_traversal_default_limit_is_1() {
        let s = parse_search(json!({"query": "test", "mode": "neighbors"})).unwrap();
        assert!(s.limit.is_none()); // default applied by handler
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
    fn test_search_depth_parameter() {
        let s = parse_search(json!({
            "node": "test:src/lib.rs:my_module:module",
            "mode": "neighbors",
            "depth": 2
        }))
        .unwrap();
        assert_eq!(s.depth, Some(2));
        assert_eq!(s.mode, Some("neighbors".to_string()));
    }

    #[test]
    fn test_search_depth_absent_defaults_none() {
        let s = parse_search(json!({
            "node": "test:src/lib.rs:foo:function",
            "mode": "neighbors"
        }))
        .unwrap();
        assert!(s.depth.is_none());
    }

    #[test]
    fn test_search_edge_types_filter() {
        let s = parse_search(json!({
            "node": "test:src/lib.rs:foo:function",
            "mode": "neighbors",
            "edge_types": ["calls", "implements"]
        }))
        .unwrap();
        assert_eq!(
            s.edge_types,
            Some(vec!["calls".to_string(), "implements".to_string()])
        );
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
        }))
        .unwrap();
        assert_eq!(s.mode, Some("tests_for".to_string()));
    }

    #[test]
    fn test_search_tests_for_mode_with_query() {
        let s = parse_search(json!({
            "query": "build_full_graph",
            "mode": "tests_for"
        }))
        .unwrap();
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
    fn test_search_include_body_default_is_none() {
        let s = parse_search(json!({"query": "test"})).unwrap();
        assert!(s.include_body.is_none());
    }

    #[test]
    fn test_search_include_body_true() {
        let s = parse_search(json!({"query": "test", "node": "x", "include_body": true})).unwrap();
        assert_eq!(s.include_body, Some(true));
    }

    #[test]
    fn test_search_minify_body_default_is_none() {
        let s = parse_search(json!({"query": "test"})).unwrap();
        assert!(s.minify_body.is_none());
    }

    #[test]
    fn test_search_minify_body_with_include_body() {
        let s =
            parse_search(json!({"node": "x", "include_body": true, "minify_body": true})).unwrap();
        assert_eq!(s.include_body, Some(true));
        assert_eq!(s.minify_body, Some(true));
    }

    #[test]
    fn test_search_nodes_param() {
        let s = parse_search(json!({
            "nodes": ["root:file:name:kind", "root:file2:name2:kind"]
        }))
        .unwrap();
        assert_eq!(
            s.nodes,
            Some(vec![
                "root:file:name:kind".to_string(),
                "root:file2:name2:kind".to_string(),
            ])
        );
    }

    #[test]
    fn test_search_nodes_with_compact() {
        let s = parse_search(json!({
            "nodes": ["root:file:name:kind"],
            "compact": true
        }))
        .unwrap();
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
        }))
        .unwrap();
        assert_eq!(s.compact, Some(true));
        assert_eq!(s.mode, Some("neighbors".to_string()));
    }

    #[test]
    fn test_search_mode_in_search_struct() {
        let s = parse_search(json!({
            "query": "test",
            "mode": "neighbors",
            "search_mode": "keyword"
        }))
        .unwrap();
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
        }))
        .unwrap();
        assert_eq!(
            s.artifact_types,
            Some(vec!["commit".to_string(), "outcome".to_string()])
        );
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
        }))
        .unwrap();
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
        }))
        .unwrap();
        assert_eq!(s.include_artifacts, Some(false));
        assert_eq!(s.artifact_types, Some(vec!["commit".to_string()]));
    }

    #[test]
    fn test_search_mode_with_flat_search_and_artifacts() {
        let s = parse_search(json!({
            "query": "test",
            "search_mode": "keyword",
            "include_artifacts": true
        }))
        .unwrap();
        assert_eq!(s.search_mode, Some("keyword".to_string()));
        assert_eq!(s.include_artifacts, Some(true));
        assert!(s.mode.is_none());
    }

    #[test]
    fn test_search_empty_artifact_types_array() {
        let s = parse_search(json!({
            "query": "test",
            "artifact_types": []
        }))
        .unwrap();
        assert_eq!(s.artifact_types, Some(vec![]));
    }

    // ── Subsystem parameter tests ────────────────────────────────────────

    #[test]
    fn test_search_subsystem_param() {
        let s = parse_search(json!({"query": "scan", "subsystem": "scanner"})).unwrap();
        assert_eq!(s.subsystem, Some("scanner".to_string()));
    }

    #[test]
    fn test_search_subsystem_default_is_none() {
        let s = parse_search(json!({"query": "test"})).unwrap();
        assert!(s.subsystem.is_none());
    }

    #[test]
    fn test_search_target_subsystem_param() {
        let s = parse_search(json!({"query": "handler", "mode": "neighbors", "node": "x", "target_subsystem": "embed"})).unwrap();
        assert_eq!(s.target_subsystem, Some("embed".to_string()));
    }

    #[test]
    fn test_search_target_subsystem_default_is_none() {
        let s = parse_search(json!({"query": "test"})).unwrap();
        assert!(s.target_subsystem.is_none());
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
            r#"Traversal mode; omit/null/blank => flat. Trims whitespace. Values: neighbors, impact, reachable, tests_for, cycles, path."#,
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
            "Cross-encoder reranking (~100-300ms). Defaults: MCP=true, CLI=false",
            "Search .oh/ artifacts and commits (default: true)",
            "Search markdown sections (default: true)",
            "Artifact filter: outcome, signal, guardrail, metis, commit",
            "Filter to a specific subsystem (from repo_map)",
            // RepoMap
            "Number of top symbols (default: 15)",
            // Shared
            r#"Workspace root slug; "all" for cross-root"#,
            "Repo path to query (e.g. worktree path); defaults to server repo",
        ];

        let max_len = 140;
        for desc in &descriptions {
            assert!(
                desc.len() <= max_len,
                "Description too long ({} chars, max {max_len}): {desc:?}",
                desc.len()
            );
        }
    }

    // ── repo parameter tests ─────────────────────────────────────────────

    #[test]
    fn test_search_repo_default_is_none() {
        let s = parse_search(json!({"query": "test"})).unwrap();
        assert!(s.repo.is_none());
    }

    #[test]
    fn test_search_repo_absolute_path() {
        let s = parse_search(json!({"query": "test", "repo": "/path/to/worktree"})).unwrap();
        assert_eq!(s.repo, Some("/path/to/worktree".to_string()));
    }

    #[test]
    fn test_search_repo_relative_path() {
        let s =
            parse_search(json!({"query": "test", "repo": ".claude/worktrees/my-feature"})).unwrap();
        assert_eq!(s.repo, Some(".claude/worktrees/my-feature".to_string()));
    }

    #[test]
    fn test_repo_map_repo_default_is_none() {
        let rm: super::RepoMap = serde_json::from_value(json!({})).unwrap();
        assert!(rm.repo.is_none());
    }

    #[test]
    fn test_repo_map_repo_with_path() {
        let rm: super::RepoMap =
            serde_json::from_value(json!({"repo": "/path/to/worktree"})).unwrap();
        assert_eq!(rm.repo, Some("/path/to/worktree".to_string()));
    }

    #[test]
    fn test_outcome_progress_repo_default_is_none() {
        let op: super::OutcomeProgress =
            serde_json::from_value(json!({"outcome_id": "agent-alignment"})).unwrap();
        assert!(op.repo.is_none());
    }

    #[test]
    fn test_outcome_progress_repo_with_path() {
        let op: super::OutcomeProgress = serde_json::from_value(
            json!({"outcome_id": "agent-alignment", "repo": "/path/to/worktree"}),
        )
        .unwrap();
        assert_eq!(op.repo, Some("/path/to/worktree".to_string()));
    }

    // ── u32 float tolerance tests (#629) ─────────────────────────────────
    // JSON has no integer type — 30 and 30.0 are both valid representations
    // of 30. MCP clients routinely serialize integers as floats.

    // -- Search.limit --

    #[test]
    fn test_limit_accepts_float_whole() {
        let s = parse_search(json!({"query": "test", "limit": 30.0})).unwrap();
        assert_eq!(s.limit, Some(30));
    }

    #[test]
    fn test_limit_accepts_integer() {
        let s = parse_search(json!({"query": "test", "limit": 30})).unwrap();
        assert_eq!(s.limit, Some(30));
    }

    #[test]
    fn test_limit_rejects_fractional_float() {
        assert!(parse_search(json!({"query": "test", "limit": 30.5})).is_err());
    }

    #[test]
    fn test_limit_null_is_none() {
        let s = parse_search(json!({"query": "test", "limit": null})).unwrap();
        assert!(s.limit.is_none());
    }

    #[test]
    fn test_limit_absent_is_none() {
        let s = parse_search(json!({"query": "test"})).unwrap();
        assert!(s.limit.is_none());
    }

    #[test]
    fn test_limit_rejects_negative_int() {
        assert!(parse_search(json!({"query": "test", "limit": -1})).is_err());
    }

    #[test]
    fn test_limit_rejects_negative_float() {
        assert!(parse_search(json!({"query": "test", "limit": -1.0})).is_err());
    }

    #[test]
    fn test_limit_rejects_string() {
        assert!(parse_search(json!({"query": "test", "limit": "thirty"})).is_err());
    }

    #[test]
    fn test_limit_rejects_bool() {
        assert!(parse_search(json!({"query": "test", "limit": true})).is_err());
    }

    #[test]
    fn test_limit_zero_float() {
        let s = parse_search(json!({"query": "test", "limit": 0.0})).unwrap();
        assert_eq!(s.limit, Some(0));
    }

    #[test]
    fn test_limit_u32_max() {
        let s = parse_search(json!({"query": "test", "limit": u32::MAX})).unwrap();
        assert_eq!(s.limit, Some(u32::MAX));
    }

    #[test]
    fn test_limit_overflow_u32() {
        let over = (u32::MAX as f64) + 1.0;
        assert!(parse_search(json!({"query": "test", "limit": over})).is_err());
    }

    // -- Search.limit via top_k alias --

    #[test]
    fn test_top_k_alias_accepts_float_whole() {
        let s = parse_search(json!({"query": "test", "top_k": 30.0})).unwrap();
        assert_eq!(s.limit, Some(30));
    }

    // -- Search.hops --

    #[test]
    fn test_hops_accepts_float_whole() {
        let s = parse_search(json!({"node": "x", "mode": "impact", "hops": 5.0})).unwrap();
        assert_eq!(s.hops, Some(5));
    }

    #[test]
    fn test_hops_rejects_fractional() {
        assert!(parse_search(json!({"node": "x", "mode": "impact", "hops": 3.7})).is_err());
    }

    // -- Search.depth --

    #[test]
    fn test_depth_accepts_float_whole() {
        let s = parse_search(json!({"node": "x", "mode": "neighbors", "depth": 2.0})).unwrap();
        assert_eq!(s.depth, Some(2));
    }

    #[test]
    fn test_depth_rejects_fractional() {
        assert!(parse_search(json!({"node": "x", "mode": "neighbors", "depth": 1.5})).is_err());
    }

    // -- Search.min_complexity --

    #[test]
    fn test_min_complexity_accepts_float_whole() {
        let s = parse_search(json!({"query": "test", "min_complexity": 10.0})).unwrap();
        assert_eq!(s.min_complexity, Some(10));
    }

    #[test]
    fn test_min_complexity_rejects_fractional() {
        assert!(parse_search(json!({"query": "test", "min_complexity": 5.5})).is_err());
    }

    // -- RepoMap.top_n --

    fn parse_repo_map(v: serde_json::Value) -> Result<super::RepoMap, serde_json::Error> {
        serde_json::from_value(v)
    }

    #[test]
    fn test_repo_map_top_n_accepts_float_whole() {
        let rm = parse_repo_map(json!({"top_n": 15.0})).unwrap();
        assert_eq!(rm.top_n, Some(15));
    }

    #[test]
    fn test_repo_map_top_n_accepts_integer() {
        let rm = parse_repo_map(json!({"top_n": 15})).unwrap();
        assert_eq!(rm.top_n, Some(15));
    }

    #[test]
    fn test_repo_map_top_n_rejects_fractional() {
        assert!(parse_repo_map(json!({"top_n": 15.5})).is_err());
    }

    #[test]
    fn test_repo_map_top_n_null_is_none() {
        let rm = parse_repo_map(json!({"top_n": null})).unwrap();
        assert!(rm.top_n.is_none());
    }

    #[test]
    fn test_repo_map_top_n_absent_is_none() {
        let rm = parse_repo_map(json!({})).unwrap();
        assert!(rm.top_n.is_none());
    }

    // -- Multi-field: all u32 fields as floats in one request --

    #[test]
    fn test_all_u32_fields_as_floats() {
        let s = parse_search(json!({
            "query": "test",
            "limit": 10.0,
            "hops": 3.0,
            "depth": 2.0,
            "min_complexity": 5.0
        }))
        .unwrap();
        assert_eq!(s.limit, Some(10));
        assert_eq!(s.hops, Some(3));
        assert_eq!(s.depth, Some(2));
        assert_eq!(s.min_complexity, Some(5));
    }
}
