//! Shared service layer for CLI and MCP.
//!
//! Both interfaces are thin dispatchers to these functions. The service layer
//! defines the full capability surface -- adding a parameter here automatically
//! makes it available in both CLI and MCP.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::embed::EmbeddingIndex;
use crate::server::{EmbeddingStatus, EnrichmentJobRecord, GraphState, LspEnrichmentStatus};

pub mod graph;
pub mod progress;
pub mod repomap;
pub mod roots;
pub mod search;

// Re-export all public items so callers keep working without path changes.
pub use graph::{GraphParams, StatsResult, graph_query, stats};
pub use progress::{OutcomeProgressContext, OutcomeProgressParams, outcome_progress};
pub use repomap::{RepoMapContext, RepoMapParams, repo_map};
pub use roots::{list_roots, list_roots_from_slugs, list_roots_from_slugs_read_only};
#[cfg(test)]
pub use search::search;
pub use search::{search_delivery, search_result};

/// Interface-neutral inputs for advisory Open Horizons reference resolution.
#[derive(Clone, Debug, Default)]
pub struct ResolveReferencesParams {
    /// Canonical repository root used for declaration discovery and cache isolation.
    pub repo_root: PathBuf,
    /// Explicit canonical references; an empty list discovers repository declarations.
    pub references: Vec<String>,
    /// Optional expected kind applied to every explicit reference.
    pub expected_kind: Option<String>,
    /// Optional resolver endpoint override; otherwise the environment is consulted.
    pub resolver_url: Option<String>,
    /// Whether resolution must avoid network access.
    pub offline: bool,
    /// Optional cache freshness override in seconds.
    pub cache_ttl_seconds: Option<u64>,
}

/// Resolves Open Horizons references using the shared CLI/MCP policy.
///
/// This seam owns declaration discovery, kind validation, configuration,
/// credential lookup, resolver construction, cache freshness, and resolution
/// so transport adapters cannot drift. Only canonical reference identity and
/// expected kind can leave the process; graph/source/embedding data never do.
pub async fn resolve_references(
    params: ResolveReferencesParams,
) -> anyhow::Result<crate::oh_reference::ResolutionBatch> {
    use crate::oh_reference::{
        AdvisoryResolver, OhReferenceKind, OpenHorizonsReferenceConfig, ReqwestReferenceTransport,
        collect_reference_declarations, preflight_explicit_references, resolve_declarations,
    };

    let explicit_kind = params
        .expected_kind
        .as_deref()
        .map(|value| {
            OhReferenceKind::parse(value).ok_or_else(|| {
                anyhow::anyhow!(
                    "unsupported expected kind {value:?}; expected context, endeavor, metis, guardrail, dive_pack, or log"
                )
            })
        })
        .transpose()?;
    let discovery = if params.references.is_empty() {
        collect_reference_declarations(&params.repo_root)?
    } else {
        preflight_explicit_references(params.references, explicit_kind)
    };
    let config = OpenHorizonsReferenceConfig::load(&params.repo_root);
    let endpoint = params
        .resolver_url
        .or_else(|| std::env::var(crate::oh_reference::DEFAULT_RESOLVER_URL_ENV).ok());
    let api_key = std::env::var(crate::oh_reference::DEFAULT_API_KEY_ENV).ok();
    let resolver = AdvisoryResolver::new(
        ReqwestReferenceTransport::default(),
        endpoint,
        api_key,
        &params.repo_root,
        Duration::from_secs(params.cache_ttl_seconds.unwrap_or(config.cache_ttl_seconds)),
    );
    Ok(resolve_declarations(&resolver, discovery, explicit_kind, params.offline).await)
}

pub const CONVERGENCE_GUIDANCE: &str = "Convergence requires two or more explicitly bound source `nodes`, optional downstream `before`, and `direction`, `edge_types`, and `depth`; discover readable symbols and verify that any boundary is reachable under the same direction and edge filter, bind them through convergence resolution, then execute the returned stable IDs. Ambiguous, unresolved, coverage-unknown, unreachable-boundary, or empty-proof results never inject context and never fall back to lexical search.";

/// Interface-agnostic search parameters.
///
/// The manual `Default` impl intentionally sets `include_artifacts` and
/// `include_markdown` to `true`, matching the MCP tool's `default_true()`
/// defaults. `derive(Default)` would leave them `false`, diverging from
/// `from_mcp_search()` and silently disabling artifact/markdown search for
/// any code that constructs `SearchParams::default()`.
#[derive(Clone, Debug)]
pub struct SearchParams {
    pub query: Option<String>,
    pub node: Option<String>,
    pub mode: Option<String>,
    pub hops: Option<u32>,
    pub depth: Option<u32>,
    pub direction: Option<String>,
    pub edge_types: Option<Vec<String>>,
    pub kind: Option<String>,
    pub language: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub end_line: Option<u32>,
    pub root: Option<String>,
    pub limit: Option<usize>,
    pub sort_by: Option<String>,
    pub min_complexity: Option<u32>,
    pub synthetic: Option<bool>,
    pub compact: bool,
    pub nodes: Option<Vec<String>>,
    pub before: Option<String>,
    pub search_mode: Option<String>,
    pub rerank: bool,
    pub include_artifacts: bool,
    pub include_markdown: bool,
    pub artifact_types: Option<Vec<String>>,
    pub subsystem: Option<String>,
    pub target_subsystem: Option<String>,
    pub include_body: bool,
    pub minify_body: bool,
    pub verbose: bool,
    /// Agent-facing output (default) or exhaustive evidence projection.
    pub projection: Option<String>,
    /// Source body policy for projected/task-context results.
    pub body_policy: Option<String>,
    /// Hard cap on the final rendered UTF-8 byte length.
    pub max_output_bytes: Option<usize>,
    /// Hard cap on the explicitly estimated rendered token count.
    pub max_output_tokens: Option<usize>,
    /// Hard cap on source bytes contributed by any one selected record.
    pub max_body_bytes: Option<usize>,
    /// Hard cap on source bytes across all selected records after coalescing.
    pub max_total_body_bytes: Option<usize>,
    /// Opt-in context mode: `task` or `graph-delta-beta`.
    pub context_mode: Option<String>,
    /// Requested task-context evidence roles.
    pub context_roles: Option<Vec<String>>,
    /// Requested task-context query facets.
    pub context_facets: Option<Vec<String>>,
    /// Bounded unified diff or structured edit sketch for graph-delta beta.
    pub proposal: Option<String>,
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            query: None,
            node: None,
            mode: None,
            hops: None,
            depth: None,
            direction: None,
            edge_types: None,
            kind: None,
            language: None,
            file: None,
            line: None,
            end_line: None,
            root: None,
            limit: None,
            sort_by: None,
            min_complexity: None,
            synthetic: None,
            compact: false,
            nodes: None,
            before: None,
            search_mode: None,
            rerank: false,
            // Default to true to match MCP tool defaults (`default_true()` on the
            // `Search` struct). `derive(Default)` would leave these `false`, causing
            // `SearchParams::default()` callers to silently get code-only search.
            include_artifacts: true,
            include_markdown: true,
            artifact_types: None,
            subsystem: None,
            target_subsystem: None,
            include_body: false,
            minify_body: false,
            verbose: false,
            projection: None,
            body_policy: None,
            max_output_bytes: None,
            max_output_tokens: None,
            max_body_bytes: None,
            max_total_body_bytes: None,
            context_mode: None,
            context_roles: None,
            context_facets: None,
            proposal: None,
        }
    }
}

fn non_blank_optional(value: &Option<String>) -> Option<String> {
    value.as_deref().and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn non_empty_string_vec(value: &Option<Vec<String>>) -> Option<Vec<String>> {
    let values: Vec<String> = value
        .as_ref()?
        .iter()
        .filter_map(|s| {
            let trimmed = s.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .collect();

    (!values.is_empty()).then_some(values)
}

impl SearchParams {
    pub fn normalized_mode(&self) -> Option<&str> {
        self.mode
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Convert from MCP `Search` tool struct.
    pub fn from_mcp_search(args: &crate::server::tools::Search) -> Self {
        Self {
            query: args
                .query
                .as_ref()
                .filter(|query| !query.trim().is_empty())
                .cloned(),
            node: non_blank_optional(&args.node),
            mode: non_blank_optional(&args.mode),
            hops: args.hops,
            depth: args.depth,
            direction: non_blank_optional(&args.direction),
            edge_types: non_empty_string_vec(&args.edge_types),
            kind: non_blank_optional(&args.kind),
            language: non_blank_optional(&args.language),
            file: non_blank_optional(&args.file),
            line: args.line,
            end_line: args.end_line,
            root: non_blank_optional(&args.root),
            limit: args.limit.map(|k| k as usize),
            sort_by: non_blank_optional(&args.sort_by),
            min_complexity: args.min_complexity,
            synthetic: args.synthetic,
            compact: args.compact.unwrap_or(false),
            nodes: non_empty_string_vec(&args.nodes),
            before: non_blank_optional(&args.before),
            search_mode: non_blank_optional(&args.search_mode),
            rerank: args.rerank.unwrap_or(true),
            include_artifacts: args.include_artifacts.unwrap_or(true),
            include_markdown: args.include_markdown.unwrap_or(true),
            artifact_types: non_empty_string_vec(&args.artifact_types),
            subsystem: non_blank_optional(&args.subsystem),
            target_subsystem: non_blank_optional(&args.target_subsystem),
            include_body: args.include_body.unwrap_or(false),
            minify_body: args.minify_body.unwrap_or(false),
            verbose: args.verbose.unwrap_or(false),
            projection: non_blank_optional(&args.projection),
            body_policy: non_blank_optional(&args.body_policy),
            max_output_bytes: args.max_output_bytes.map(|value| value as usize),
            max_output_tokens: args.max_output_tokens.map(|value| value as usize),
            max_body_bytes: args.max_body_bytes.map(|value| value as usize),
            max_total_body_bytes: args.max_total_body_bytes.map(|value| value as usize),
            context_mode: non_blank_optional(&args.context_mode),
            context_roles: non_empty_string_vec(&args.context_roles),
            context_facets: non_empty_string_vec(&args.context_facets),
            proposal: args.proposal.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::tools::Search;
    use serde_json::json;

    #[tokio::test]
    async fn shared_reference_service_resolves_explicit_offline_input_advisorially() {
        let repo = tempfile::tempdir().unwrap();
        let output = resolve_references(ResolveReferencesParams {
            repo_root: repo.path().to_path_buf(),
            references: vec!["oh://v1/context/context-service".to_string()],
            expected_kind: Some("context".to_string()),
            offline: true,
            ..ResolveReferencesParams::default()
        })
        .await
        .unwrap();

        assert_eq!(output.resolutions.len(), 1);
        assert_eq!(
            output.resolutions[0].resolution.state,
            crate::oh_reference::AdvisoryState::Unavailable
        );
        assert_eq!(
            output.resolutions[0].resolution.reference,
            "oh://v1/context/context-service"
        );
    }

    #[tokio::test]
    async fn shared_reference_service_rejects_an_unsupported_expected_kind() {
        let repo = tempfile::tempdir().unwrap();
        let error = resolve_references(ResolveReferencesParams {
            repo_root: repo.path().to_path_buf(),
            references: vec!["oh://v1/context/context-service".to_string()],
            expected_kind: Some("claim".to_string()),
            offline: true,
            ..ResolveReferencesParams::default()
        })
        .await
        .unwrap_err();

        assert!(error.to_string().contains("unsupported expected kind"));
    }

    #[test]
    fn from_mcp_search_treats_blank_mode_as_absent() {
        let search: Search = serde_json::from_value(json!({
            "query": "parse_search",
            "mode": ""
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.query.as_deref(), Some("parse_search"));
        assert!(params.mode.is_none());
    }

    #[test]
    fn from_mcp_search_treats_whitespace_only_mode_as_absent() {
        let search: Search = serde_json::from_value(json!({
            "query": "parse_search",
            "mode": "   "
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.query.as_deref(), Some("parse_search"));
        assert!(params.mode.is_none());
    }

    #[test]
    fn from_mcp_search_trims_mode() {
        let search: Search = serde_json::from_value(json!({
            "node": "repo:src/lib.rs:thing:function",
            "mode": " neighbors "
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.mode.as_deref(), Some("neighbors"));
    }

    #[test]
    fn from_mcp_search_preserves_source_span_contract() {
        let search: Search = serde_json::from_value(json!({
            "file": " src/main.rs ",
            "line": 12,
            "end_line": 14,
            "root": " secondary "
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.file.as_deref(), Some("src/main.rs"));
        assert_eq!(params.line, Some(12));
        assert_eq!(params.end_line, Some(14));
        assert_eq!(params.root.as_deref(), Some("secondary"));
    }

    #[test]
    fn from_mcp_search_preserves_invalid_non_blank_mode() {
        let search: Search = serde_json::from_value(json!({
            "node": "repo:src/lib.rs:thing:function",
            "mode": " bogus "
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.mode.as_deref(), Some("bogus"));
    }

    #[test]
    fn from_mcp_search_treats_empty_nodes_as_absent() {
        let search: Search = serde_json::from_value(json!({
            "query": "get_graph",
            "nodes": [],
            "edge_types": [],
            "artifact_types": []
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.query.as_deref(), Some("get_graph"));
        assert!(params.nodes.is_none());
        assert!(params.edge_types.is_none());
        assert!(params.artifact_types.is_none());
    }

    #[test]
    fn from_mcp_search_trims_and_drops_blank_nodes() {
        let search: Search = serde_json::from_value(json!({
            "nodes": ["  repo:src/lib.rs:thing:function  ", "", "   "]
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(
            params.nodes,
            Some(vec!["repo:src/lib.rs:thing:function".to_string()])
        );
    }

    #[test]
    fn from_mcp_search_preserves_the_convergence_contract() {
        let search: Search = serde_json::from_value(json!({
            "mode": " convergence ",
            "nodes": [" Request.prepare ", " Session.prepare_request "],
            "before": " PreparedRequest.prepare_method ",
            "direction": " outgoing ",
            "edge_types": [" calls "],
            "depth": 6,
            "max_output_bytes": 12000,
            "max_output_tokens": 3000
        }))
        .unwrap();
        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.mode.as_deref(), Some("convergence"));
        assert_eq!(
            params.nodes,
            Some(vec!["Request.prepare".into(), "Session.prepare_request".into()])
        );
        assert_eq!(
            params.before.as_deref(),
            Some("PreparedRequest.prepare_method")
        );
        assert_eq!(params.direction.as_deref(), Some("outgoing"));
        assert_eq!(params.edge_types, Some(vec!["calls".into()]));
        assert_eq!(params.depth, Some(6));
        assert_eq!(params.max_output_bytes, Some(12_000));
        assert_eq!(params.max_output_tokens, Some(3_000));
    }

    #[test]
    fn from_mcp_search_treats_blank_scalar_filters_as_absent() {
        let search: Search = serde_json::from_value(json!({
            "query": "  ",
            "node": "",
            "kind": "   ",
            "language": "",
            "file": " ",
            "direction": "   ",
            "sort_by": "",
            "search_mode": " ",
            "subsystem": "",
            "target_subsystem": "  "
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert!(params.query.is_none());
        assert!(params.node.is_none());
        assert!(params.kind.is_none());
        assert!(params.language.is_none());
        assert!(params.file.is_none());
        assert!(params.direction.is_none());
        assert!(params.sort_by.is_none());
        assert!(params.search_mode.is_none());
        assert!(params.subsystem.is_none());
        assert!(params.target_subsystem.is_none());
    }

    #[test]
    fn from_mcp_search_preserves_non_blank_query_bytes() {
        let query = "  MiXeD/Δοκιμή/東京  ";
        let search: Search = serde_json::from_value(json!({
            "query": query
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.query.as_deref(), Some(query));
    }

    #[test]
    fn from_mcp_search_trims_vector_filters() {
        let search: Search = serde_json::from_value(json!({
            "edge_types": [" calls ", "", "   "],
            "artifact_types": [" outcome ", ""]
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.edge_types, Some(vec!["calls".to_string()]));
        assert_eq!(params.artifact_types, Some(vec!["outcome".to_string()]));
    }

    #[test]
    fn from_mcp_search_preserves_projection_and_task_context_contract() {
        let proposal = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let search: Search = serde_json::from_value(json!({
            "query": "Fix `render` and its named test",
            "projection": " evidence ",
            "body_policy": " focused_span ",
            "max_output_bytes": 12000,
            "max_output_tokens": 3000,
            "max_body_bytes": 2048,
            "max_total_body_bytes": 8192,
            "context_mode": " task ",
            "context_roles": [" editable_source ", "", "test"],
            "context_facets": [" behavior ", "api_or_state"],
            "proposal": proposal
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.projection.as_deref(), Some("evidence"));
        assert_eq!(params.body_policy.as_deref(), Some("focused_span"));
        assert_eq!(params.max_output_bytes, Some(12000));
        assert_eq!(params.max_output_tokens, Some(3000));
        assert_eq!(params.max_body_bytes, Some(2048));
        assert_eq!(params.max_total_body_bytes, Some(8192));
        assert_eq!(params.context_mode.as_deref(), Some("task"));
        assert_eq!(
            params.context_roles,
            Some(vec!["editable_source".to_string(), "test".to_string()])
        );
        assert_eq!(
            params.context_facets,
            Some(vec!["behavior".to_string(), "api_or_state".to_string()])
        );
        assert_eq!(params.proposal.as_deref(), Some(proposal));
    }

    #[test]
    fn search_context_controls_default_to_ordinary_agent_search() {
        let params = SearchParams::default();

        assert!(params.projection.is_none());
        assert!(params.body_policy.is_none());
        assert!(params.max_output_bytes.is_none());
        assert!(params.max_output_tokens.is_none());
        assert!(params.max_body_bytes.is_none());
        assert!(params.max_total_body_bytes.is_none());
        assert!(params.context_mode.is_none());
        assert!(params.context_roles.is_none());
        assert!(params.context_facets.is_none());
        assert!(params.proposal.is_none());
    }
}

/// Runtime context for search operations.
pub struct SearchContext<'a> {
    pub graph_state: &'a GraphState,
    pub embed_index: Option<&'a EmbeddingIndex>,
    pub repo_root: &'a Path,
    pub lsp_status: Option<&'a LspEnrichmentStatus>,
    pub embed_status: Option<&'a EmbeddingStatus>,
    pub root_filter: Option<String>,
    pub non_code_slugs: HashSet<String>,
    pub enrichment_jobs: Vec<EnrichmentJobRecord>,
    pub business_context: &'a crate::business_context::BusinessContextAdmission,
}

/// Returns true when a graph node's root passes the active root filter.
///
/// `None` filter matches any root. When a filter is set, the node must match
/// the slug (case-insensitive), OR be the synthetic "external" root (so that
/// external dependencies always appear in traversal results), OR be a
/// non-code slug (e.g., a memory root that stores markdown rather than code).
pub fn node_passes_root_filter(
    node_root: &str,
    root_filter: &Option<String>,
    non_code_slugs: &HashSet<String>,
) -> bool {
    match root_filter {
        None => true,
        Some(slug) => {
            node_root.eq_ignore_ascii_case(slug)
                || node_root == "external"
                || non_code_slugs.contains(node_root)
        }
    }
}

/// Returns true when an embedding search result passes the active root filter.
///
/// Non-code results (artifacts, markdown) always pass — they are not root-
/// scoped the same way code symbols are. Code results delegate to
/// `node_passes_root_filter` using the root prefix of the result ID.
pub fn search_result_passes_root_filter(
    result: &crate::embed::SearchResult,
    root_filter: &Option<String>,
    non_code_slugs: &HashSet<String>,
) -> bool {
    if root_filter.is_none() {
        return true;
    }
    if !result.kind.starts_with("code:") {
        return true;
    }
    node_passes_root_filter(
        result.id.split(':').next().unwrap_or(""),
        root_filter,
        non_code_slugs,
    )
}
