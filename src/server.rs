use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use rust_mcp_sdk::macros::{self, JsonSchema};
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::schema::{
    CallToolError, CallToolRequestParams, CallToolResult, ListToolsResult,
    PaginatedRequestParams, RpcError, TextContent,
};
use serde::{Deserialize, Serialize};

use crate::embed::EmbeddingIndex;
use crate::extract::{ExtractorRegistry, EnricherRegistry};
use crate::graph::{self, EdgeKind, Node, Edge};
use crate::graph::index::GraphIndex;
use crate::roots::{WorkspaceConfig, cache_state_path};
use crate::scanner::Scanner;
use crate::types::OhArtifactKind;
use crate::{code, git, markdown, oh, query};
use petgraph::Direction;
use tokio::sync::OnceCell;

// ── Tool input structs ──────────────────────────────────────────────

#[macros::mcp_tool(
    name = "oh_get_context",
    description = "Returns the full business context bundle: outcomes, signals, guardrails, metis, plus recent commits, code symbols, and markdown sections. This is the single read tool for all .oh/ artifacts."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhGetContext {}

#[macros::mcp_tool(
    name = "oh_search_context",
    description = "Semantic search over .oh/ artifacts, git commits, and optionally code symbols and markdown. Finds outcomes, guardrails, metis, signals, and commits relevant to a natural language query. Set include_code=true to also search code symbols, include_markdown=true for markdown sections."
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
}

#[macros::mcp_tool(
    name = "oh_record",
    description = "Record a business artifact: metis (learning), signal (measurement), guardrail (constraint), or update an outcome. The 'type' field determines which fields are used."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhRecord {
    /// Artifact type: "metis", "signal", "guardrail", "outcome"
    #[serde(rename = "type")]
    pub record_type: String,
    /// Slug/ID (required for all types)
    pub slug: String,
    /// Title (metis, signal)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Body/description (metis, signal, guardrail)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Related outcome ID (metis, signal, guardrail)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Signal type: slo, metric, qualitative (signals only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_type: Option<String>,
    /// Threshold (signals only, e.g. "p95 < 200ms")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<String>,
    /// Guardrail severity: candidate, soft, hard (guardrails only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    /// Guardrail statement (guardrails only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement: Option<String>,
    /// Outcome status update: active, achieved, paused, abandoned (outcomes only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Outcome mechanism update (outcomes only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mechanism: Option<String>,
    /// Outcome files patterns (outcomes only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

#[macros::mcp_tool(
    name = "oh_init",
    description = "Scaffolds .oh/ directory structure for a repo. Reads CLAUDE.md, README.md, and recent git history to propose an initial outcome, signal, and guardrails. Idempotent — won't overwrite existing files."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhInit {
    /// Optional: name for the primary outcome (auto-detected from README/CLAUDE.md if omitted)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_name: Option<String>,
}

#[macros::mcp_tool(
    name = "outcome_progress",
    description = "The real intersection query: given an outcome ID, finds related commits (by [outcome:X] tags and file pattern matches), code symbols in changed files, and markdown mentioning the outcome. This joins layers structurally, not by keyword."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OutcomeProgress {
    /// The outcome ID (e.g. 'agent-alignment') from .oh/outcomes/
    pub outcome_id: String,
}

#[macros::mcp_tool(
    name = "search_symbols",
    description = "Searches code symbols across all languages (Rust, Python, TypeScript, Go) from the workspace graph. Returns functions, structs, traits, classes, interfaces with file location and edges. Multi-language, graph-aware search."
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
    /// Optional: filter to a specific workspace root (by slug, e.g. "zettelkasten")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Maximum results to return (default: 20)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[macros::mcp_tool(
    name = "graph_query",
    description = "Query the code graph: find neighbors (what calls/depends on a symbol), impact analysis (what depends on this), or reachable nodes within N hops. Use after search_symbols to explore relationships."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GraphQuery {
    /// Stable ID from search_symbols results
    pub node_id: String,
    /// Query mode: "neighbors" (default), "impact" (reverse dependents), "reachable" (forward BFS)
    #[serde(default = "default_graph_mode")]
    pub mode: String,
    /// Direction for neighbors mode: "outgoing" (default), "incoming", "both"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// Filter edge types: calls, depends_on, implements, defines, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_types: Option<Vec<String>>,
    /// Maximum hops to traverse (default: 1 for neighbors, 3 for impact/reachable)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_hops: Option<u32>,
}

fn default_graph_mode() -> String {
    "neighbors".to_string()
}

#[macros::mcp_tool(
    name = "git_history",
    description = "Search git history: find commits by message query, or view the change history of a specific file. Provide 'query' to search commit messages, or 'file' to get file history."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GitHistory {
    /// Search query (matched against commit messages)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// File path to get history for (alternative to query search)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Maximum number of results to return (default: 20 for file, 50 for query)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[macros::mcp_tool(
    name = "list_roots",
    description = "Lists configured workspace roots with their type, path, and scan status."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ListRoots {}

// ── Graph state ─────────────────────────────────────────────────────

/// In-memory graph state: extraction results + petgraph index.
/// Lazily initialized on first graph-aware tool call.
pub struct GraphState {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub index: GraphIndex,
}

// ── ServerHandler ───────────────────────────────────────────────────

pub struct RnaHandler {
    pub repo_root: PathBuf,
    pub embed_index: OnceCell<EmbeddingIndex>,
    pub graph_index: OnceCell<GraphState>,
    /// Whether business context has been injected into a tool response.
    pub context_injected: std::sync::atomic::AtomicBool,
}

impl Default for RnaHandler {
    fn default() -> Self {
        Self {
            repo_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            embed_index: OnceCell::new(),
            graph_index: OnceCell::new(),
            context_injected: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl RnaHandler {
    async fn get_index(&self) -> anyhow::Result<&EmbeddingIndex> {
        self.embed_index
            .get_or_try_init(|| async {
                let index = EmbeddingIndex::new(&self.repo_root).await?;
                let count = index.index_all(&self.repo_root).await?;
                tracing::info!("Indexed {} .oh/ artifacts for semantic search", count);
                Ok(index)
            })
            .await
    }

    async fn get_graph(&self) -> anyhow::Result<&GraphState> {
        self.graph_index
            .get_or_try_init(|| async {
                // Load workspace config and merge with --repo as primary root
                let workspace = WorkspaceConfig::load()
                    .with_primary_root(self.repo_root.clone());
                let resolved_roots = workspace.resolved_roots();

                let registry = ExtractorRegistry::with_builtins();
                let mut all_nodes: Vec<Node> = Vec::new();
                let mut all_edges: Vec<Edge> = Vec::new();

                // 3. Scan all roots (multi-root support)
                for resolved_root in &resolved_roots {
                    let root_slug = &resolved_root.slug;
                    let root_path = &resolved_root.path;
                    let excludes = resolved_root.config.effective_excludes();

                    let is_primary = resolved_root.path == self.repo_root;
                    let mut scanner = if is_primary {
                        Scanner::with_excludes(root_path.clone(), excludes)?
                    } else {
                        let state_path = cache_state_path(root_slug);
                        Scanner::with_excludes_and_state_path(
                            root_path.clone(),
                            excludes,
                            state_path,
                        )?
                    };

                    let scan_result = scanner.scan()?;
                    tracing::info!(
                        "Scanned root '{}' ({}): {} new, {} changed, {} deleted in {:?}",
                        root_slug,
                        resolved_root.config.root_type,
                        scan_result.new_files.len(),
                        scan_result.changed_files.len(),
                        scan_result.deleted_files.len(),
                        scan_result.scan_duration
                    );

                    let all_files = scanner.all_known_files();
                    let full_scan = crate::scanner::ScanResult {
                        changed_files: Vec::new(),
                        new_files: all_files,
                        deleted_files: Vec::new(),
                        scan_duration: std::time::Duration::ZERO,
                    };
                    let mut extraction = registry.extract_scan_result(root_path, &full_scan);

                    for node in &mut extraction.nodes {
                        node.id.root = root_slug.clone();
                    }
                    for edge in &mut extraction.edges {
                        edge.from.root = root_slug.clone();
                        edge.to.root = root_slug.clone();
                    }

                    tracing::info!(
                        "Extracted from '{}': {} nodes, {} edges",
                        root_slug,
                        extraction.nodes.len(),
                        extraction.edges.len()
                    );

                    all_nodes.extend(extraction.nodes);
                    all_edges.extend(extraction.edges);
                }

                // 4. Extract PR merges from git history
                match git::pr_merges::extract_pr_merges(&self.repo_root, Some(100)) {
                    Ok((pr_nodes, pr_edges)) => {
                        let modified_edges =
                            git::pr_merges::link_pr_to_symbols(&pr_nodes, &all_nodes);
                        tracing::info!(
                            "PR merges: {} nodes, {} edges, {} Modified links",
                            pr_nodes.len(),
                            pr_edges.len(),
                            modified_edges.len()
                        );
                        all_nodes.extend(pr_nodes);
                        all_edges.extend(pr_edges);
                        all_edges.extend(modified_edges);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to extract PR merges: {}", e);
                    }
                }

                // 5. Build petgraph index
                let mut index = GraphIndex::new();
                index.rebuild_from_edges(&all_edges);
                for node in &all_nodes {
                    index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                }

                // 6. Phase 2: Run enrichers (LSP) synchronously
                let languages: Vec<String> = all_nodes
                    .iter()
                    .map(|n| n.language.clone())
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();

                let enricher_registry = EnricherRegistry::with_builtins();
                let enrichment = enricher_registry
                    .enrich_all(&all_nodes, &index, &languages)
                    .await;

                if !enrichment.added_edges.is_empty() {
                    tracing::info!(
                        "Enrichment added {} edges",
                        enrichment.added_edges.len()
                    );
                    for edge in &enrichment.added_edges {
                        let from_id = edge.from.to_stable_id();
                        let to_id = edge.to.to_stable_id();
                        index.add_edge(
                            &from_id,
                            &edge.from.kind.to_string(),
                            &to_id,
                            &edge.to.kind.to_string(),
                            edge.kind.clone(),
                        );
                    }
                    all_edges.extend(enrichment.added_edges);
                }

                for (node_id, patches) in &enrichment.updated_nodes {
                    if let Some(node) = all_nodes.iter_mut().find(|n| n.stable_id() == *node_id) {
                        for (key, value) in patches {
                            node.metadata.insert(key.clone(), value.clone());
                        }
                    }
                }

                tracing::info!(
                    "Graph built: {} nodes, {} edges across {} root(s)",
                    all_nodes.len(),
                    all_edges.len(),
                    resolved_roots.len()
                );

                Ok(GraphState {
                    nodes: all_nodes,
                    edges: all_edges,
                    index,
                })
            })
            .await
    }
}

fn text_result(s: String) -> CallToolResult {
    CallToolResult::text_content(vec![TextContent::new(s, None, None)])
}

/// Build a concise business context preamble from .oh/ artifacts.
fn build_context_preamble(root: &Path) -> String {
    let artifacts = match oh::load_oh_artifacts(root) {
        Ok(a) => a,
        Err(_) => return String::new(),
    };

    if artifacts.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();

    // Active outcomes (just names + status)
    let outcomes: Vec<_> = artifacts.iter().filter(|a| a.kind == OhArtifactKind::Outcome).collect();
    if !outcomes.is_empty() {
        let mut section = String::from("**Active outcomes:**\n");
        for o in &outcomes {
            let status = o.frontmatter.get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            section.push_str(&format!("- {} ({})\n", o.id(), status));
        }
        parts.push(section);
    }

    // Hard/soft guardrails (just statements, no full body)
    let guardrails: Vec<_> = artifacts.iter().filter(|a| a.kind == OhArtifactKind::Guardrail).collect();
    if !guardrails.is_empty() {
        let mut section = String::from("**Guardrails:**\n");
        for g in &guardrails {
            let severity = g.frontmatter.get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("candidate");
            let id = g.id();
            let statement = g.frontmatter.get("statement")
                .and_then(|v| v.as_str())
                .unwrap_or(&id);
            section.push_str(&format!("- [{}] {}\n", severity, statement));
        }
        parts.push(section);
    }

    // Recent metis (last 3 titles only)
    let metis: Vec<_> = artifacts.iter().filter(|a| a.kind == OhArtifactKind::Metis).collect();
    if !metis.is_empty() {
        let mut section = String::from("**Recent learnings:**\n");
        for m in metis.iter().rev().take(3) {
            let id = m.id();
            let title = m.frontmatter.get("title")
                .and_then(|v| v.as_str())
                .unwrap_or(&id);
            section.push_str(&format!("- {}\n", title));
        }
        parts.push(section);
    }

    format!("---\n# Business Context (auto-injected on first tool call)\n\n{}\n---\n\n", parts.join("\n"))
}

#[async_trait]
impl rust_mcp_sdk::mcp_server::ServerHandler for RnaHandler {
    async fn handle_list_tools_request(
        &self,
        _request: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            tools: vec![
                OhGetContext::tool(),
                OhSearchContext::tool(),
                OhRecord::tool(),
                OhInit::tool(),
                OutcomeProgress::tool(),
                SearchSymbols::tool(),
                GraphQuery::tool(),
                GitHistory::tool(),
                ListRoots::tool(),
            ],
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<CallToolResult, CallToolError> {
        let root = &self.repo_root;

        // Inject business context preamble on first tool call
        let preamble = if !self.context_injected.swap(true, std::sync::atomic::Ordering::Relaxed) {
            let ctx = build_context_preamble(root);
            if !ctx.is_empty() {
                tracing::info!("Injecting business context preamble on first tool call");
                Some(ctx)
            } else {
                None
            }
        } else {
            None
        };

        let mut result = match params.name.as_str() {
            "oh_get_context" => match query::get_full_context(root) {
                Ok(mut result) => {
                    let sym_total = result.code_symbols.len();
                    let chunk_total = result.markdown_chunks.len();
                    result.code_symbols.truncate(50);
                    result.markdown_chunks.truncate(50);
                    let mut md = result.to_markdown();
                    if sym_total > 50 || chunk_total > 50 {
                        md.push_str(&format!(
                            "\n_Showing {} of {} symbols, {} of {} markdown sections._\n",
                            result.code_symbols.len(), sym_total,
                            result.markdown_chunks.len(), chunk_total,
                        ));
                    }
                    Ok(text_result(md))
                }
                Err(e) => Ok(text_result(format!("Error: {}", e))),
            },

            "oh_search_context" => {
                let args: OhSearchContext = parse_args(params.arguments)?;
                let limit = args.limit.unwrap_or(5) as usize;
                let include_code = args.include_code.unwrap_or(false);
                let include_markdown = args.include_markdown.unwrap_or(false);

                let mut sections: Vec<String> = Vec::new();

                // Always search .oh/ artifacts via embedding index
                match self.get_index().await {
                    Ok(index) => {
                        match index.search(&args.query, args.artifact_types.as_deref(), limit).await {
                            Ok(results) => {
                                if !results.is_empty() {
                                    let md: String = results
                                        .iter()
                                        .map(|r| r.to_markdown())
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    sections.push(format!(
                                        "### Artifacts ({} result(s))\n\n{}",
                                        results.len(),
                                        md
                                    ));
                                }
                            }
                            Err(e) => sections.push(format!("Artifact search error: {}", e)),
                        }
                    }
                    Err(e) => sections.push(format!("Index error: {}", e)),
                }

                // Optionally search code symbols
                if include_code {
                    match code::extract_symbols(root) {
                        Ok(symbols) => {
                            let matches = code::search_symbols(&symbols, &args.query);
                            if !matches.is_empty() {
                                let md = matches
                                    .iter()
                                    .take(limit)
                                    .map(|s| s.to_markdown())
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                sections.push(format!(
                                    "### Code symbols ({} result(s))\n\n{}",
                                    matches.len().min(limit),
                                    md
                                ));
                            }
                        }
                        Err(e) => sections.push(format!("Code search error: {}", e)),
                    }
                }

                // Optionally search markdown
                if include_markdown {
                    match markdown::extract_markdown_chunks(root) {
                        Ok(chunks) => {
                            let matches = markdown::search_chunks(&chunks, &args.query);
                            if !matches.is_empty() {
                                let md = matches
                                    .iter()
                                    .take(limit)
                                    .map(|c| c.to_markdown())
                                    .collect::<Vec<_>>()
                                    .join("\n\n---\n\n");
                                sections.push(format!(
                                    "### Markdown ({} result(s))\n\n{}",
                                    matches.len().min(limit),
                                    md
                                ));
                            }
                        }
                        Err(e) => sections.push(format!("Markdown search error: {}", e)),
                    }
                }

                if sections.is_empty() {
                    Ok(text_result(format!(
                        "No results found matching \"{}\".",
                        args.query
                    )))
                } else {
                    Ok(text_result(format!(
                        "## Semantic search: \"{}\"\n\n{}",
                        args.query,
                        sections.join("\n\n")
                    )))
                }
            }

            "oh_record" => {
                let args: OhRecord = parse_args(params.arguments)?;
                match args.record_type.as_str() {
                    "metis" => {
                        let title = args.title.unwrap_or_else(|| args.slug.clone());
                        let body = args.body.unwrap_or_default();
                        let mut fm = BTreeMap::new();
                        fm.insert(
                            "id".to_string(),
                            serde_yaml::Value::String(args.slug.clone()),
                        );
                        fm.insert(
                            "title".to_string(),
                            serde_yaml::Value::String(title),
                        );
                        if let Some(ref outcome) = args.outcome {
                            fm.insert(
                                "outcome".to_string(),
                                serde_yaml::Value::String(outcome.clone()),
                            );
                        }
                        match oh::write_metis(root, &args.slug, &fm, &body) {
                            Ok(path) => Ok(text_result(format!(
                                "Recorded metis at `{}`",
                                path.display()
                            ))),
                            Err(e) => Ok(text_result(format!("Error writing metis: {}", e))),
                        }
                    }
                    "signal" => {
                        let body = args.body.unwrap_or_default();
                        let outcome = args.outcome.unwrap_or_default();
                        let signal_type = args.signal_type.unwrap_or_else(|| "slo".to_string());
                        let threshold = args.threshold.unwrap_or_default();
                        let mut fm = BTreeMap::new();
                        fm.insert("id".into(), serde_yaml::Value::String(args.slug.clone()));
                        fm.insert("outcome".into(), serde_yaml::Value::String(outcome));
                        fm.insert("type".into(), serde_yaml::Value::String(signal_type));
                        fm.insert("threshold".into(), serde_yaml::Value::String(threshold));
                        match oh::write_artifact(root, "signals", &args.slug, &fm, &body) {
                            Ok(path) => Ok(text_result(format!("Recorded signal at `{}`", path.display()))),
                            Err(e) => Ok(text_result(format!("Error: {}", e))),
                        }
                    }
                    "guardrail" => {
                        let body = args.body.unwrap_or_default();
                        let severity = args.severity.unwrap_or_else(|| "candidate".to_string());
                        let statement = args.statement.unwrap_or_else(|| args.slug.clone());
                        let mut fm = BTreeMap::new();
                        fm.insert("id".into(), serde_yaml::Value::String(args.slug.clone()));
                        fm.insert("severity".into(), serde_yaml::Value::String(severity));
                        fm.insert("statement".into(), serde_yaml::Value::String(statement));
                        if let Some(ref outcome) = args.outcome {
                            fm.insert("outcome".into(), serde_yaml::Value::String(outcome.clone()));
                        }
                        match oh::write_artifact(root, "guardrails", &args.slug, &fm, &body) {
                            Ok(path) => Ok(text_result(format!("Recorded guardrail at `{}`", path.display()))),
                            Err(e) => Ok(text_result(format!("Error: {}", e))),
                        }
                    }
                    "outcome" => {
                        let mut updates = BTreeMap::new();
                        if let Some(status) = args.status {
                            updates.insert("status".into(), serde_yaml::Value::String(status));
                        }
                        if let Some(mechanism) = args.mechanism {
                            updates.insert("mechanism".into(), serde_yaml::Value::String(mechanism));
                        }
                        if let Some(files) = args.files {
                            let seq: Vec<serde_yaml::Value> = files.into_iter().map(serde_yaml::Value::String).collect();
                            updates.insert("files".into(), serde_yaml::Value::Sequence(seq));
                        }
                        if updates.is_empty() {
                            return Ok(text_result("No fields to update.".into()));
                        }
                        match oh::update_artifact(root, "outcomes", &args.slug, &updates) {
                            Ok(path) => Ok(text_result(format!("Updated outcome at `{}`", path.display()))),
                            Err(e) => Ok(text_result(format!("Error: {}", e))),
                        }
                    }
                    other => Ok(text_result(format!(
                        "Unknown record type: \"{}\". Use \"metis\", \"signal\", \"guardrail\", or \"outcome\".",
                        other
                    ))),
                }
            }

            "oh_init" => {
                let args: OhInit = parse_args(params.arguments)?;
                match oh_init_impl(root, args.outcome_name.as_deref()) {
                    Ok(msg) => Ok(text_result(msg)),
                    Err(e) => Ok(text_result(format!("Error: {}", e))),
                }
            }

            "outcome_progress" => {
                let args: OutcomeProgress = parse_args(params.arguments)?;
                match query::outcome_progress(root, &args.outcome_id) {
                    Ok(result) => {
                        let mut md = result.to_markdown();

                        // Append PR merge section from the graph
                        if let Ok(graph_state) = self.get_graph().await {
                            let file_patterns: Vec<String> = result
                                .outcomes
                                .first()
                                .and_then(|o| o.frontmatter.get("files"))
                                .and_then(|v| v.as_sequence())
                                .map(|seq| {
                                    seq.iter()
                                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                        .collect()
                                })
                                .unwrap_or_default();

                            let pr_nodes = query::find_pr_merges_for_outcome(
                                &graph_state.nodes,
                                &graph_state.edges,
                                &args.outcome_id,
                                &file_patterns,
                            );
                            let pr_md = query::format_pr_merges_markdown(&pr_nodes);
                            if !pr_md.is_empty() {
                                md.push('\n');
                                md.push_str(&pr_md);
                            }
                        }

                        Ok(text_result(md))
                    }
                    Err(e) => Ok(text_result(format!("Error: {}", e))),
                }
            }

            "search_symbols" => {
                let args: SearchSymbols = parse_args(params.arguments)?;
                match self.get_graph().await {
                    Ok(graph_state) => {
                        let limit = args.limit.unwrap_or(20) as usize;
                        let query_lower = args.query.to_lowercase();

                        let mut matches: Vec<&Node> = graph_state
                            .nodes
                            .iter()
                            .filter(|n| {
                                let name_match = n.id.name.to_lowercase().contains(&query_lower)
                                    || n.signature.to_lowercase().contains(&query_lower);
                                if !name_match {
                                    return false;
                                }
                                if let Some(ref kind_filter) = args.kind {
                                    if n.id.kind.to_string().to_lowercase() != kind_filter.to_lowercase() {
                                        return false;
                                    }
                                }
                                if let Some(ref lang_filter) = args.language {
                                    if n.language.to_lowercase() != lang_filter.to_lowercase() {
                                        return false;
                                    }
                                }
                                if let Some(ref file_filter) = args.file {
                                    let path_str = n.id.file.to_string_lossy();
                                    if !path_str.contains(file_filter.as_str()) {
                                        return false;
                                    }
                                }
                                if let Some(ref root_filter) = args.root {
                                    if n.id.root.to_lowercase() != root_filter.to_lowercase() {
                                        return false;
                                    }
                                }
                                true
                            })
                            .collect();

                        matches.truncate(limit);

                        if matches.is_empty() {
                            Ok(text_result(format!(
                                "No symbols matching \"{}\".",
                                args.query
                            )))
                        } else {
                            let md: String = matches
                                .iter()
                                .map(|n| {
                                    let stable_id = n.stable_id();
                                    // Find edges involving this node
                                    let outgoing = graph_state.index.neighbors(
                                        &stable_id,
                                        None,
                                        Direction::Outgoing,
                                    );
                                    let incoming = graph_state.index.neighbors(
                                        &stable_id,
                                        None,
                                        Direction::Incoming,
                                    );
                                    let mut entry = format!(
                                        "- **{}** `{}` ({}) `{}`:{}-{}\n  ID: `{}`",
                                        n.id.kind, n.id.name, n.language,
                                        n.id.file.display(),
                                        n.line_start, n.line_end,
                                        stable_id,
                                    );
                                    if !n.signature.is_empty() {
                                        entry.push_str(&format!("\n  Sig: `{}`", n.signature));
                                    }
                                    if !outgoing.is_empty() {
                                        entry.push_str(&format!("\n  Out: {} edge(s)", outgoing.len()));
                                    }
                                    if !incoming.is_empty() {
                                        entry.push_str(&format!("\n  In: {} edge(s)", incoming.len()));
                                    }
                                    entry
                                })
                                .collect::<Vec<_>>()
                                .join("\n\n");
                            Ok(text_result(format!(
                                "## Symbol search: \"{}\"\n\n{} result(s)\n\n{}",
                                args.query,
                                matches.len(),
                                md
                            )))
                        }
                    }
                    Err(e) => Ok(text_result(format!("Graph error: {}", e))),
                }
            }

            "graph_query" => {
                let args: GraphQuery = parse_args(params.arguments)?;
                match self.get_graph().await {
                    Ok(graph_state) => {
                        let edge_filter = args.edge_types.as_ref().map(|types| {
                            types
                                .iter()
                                .filter_map(|t| parse_edge_kind(t))
                                .collect::<Vec<_>>()
                        });
                        let edge_filter_slice = edge_filter.as_deref();

                        match args.mode.as_str() {
                            "neighbors" => {
                                let max_hops = args.max_hops.unwrap_or(1) as usize;
                                let direction = args.direction.as_deref().unwrap_or("outgoing");

                                let mut all_ids: Vec<String> = Vec::new();

                                match direction {
                                    "outgoing" => {
                                        if max_hops == 1 {
                                            all_ids = graph_state.index.neighbors(
                                                &args.node_id,
                                                edge_filter_slice,
                                                Direction::Outgoing,
                                            );
                                        } else {
                                            all_ids = graph_state.index.reachable(
                                                &args.node_id,
                                                max_hops,
                                                edge_filter_slice,
                                            );
                                        }
                                    }
                                    "incoming" => {
                                        if max_hops == 1 {
                                            all_ids = graph_state.index.neighbors(
                                                &args.node_id,
                                                edge_filter_slice,
                                                Direction::Incoming,
                                            );
                                        } else {
                                            all_ids = graph_state.index.impact(&args.node_id, max_hops);
                                        }
                                    }
                                    "both" => {
                                        let out = if max_hops == 1 {
                                            graph_state.index.neighbors(
                                                &args.node_id,
                                                edge_filter_slice,
                                                Direction::Outgoing,
                                            )
                                        } else {
                                            graph_state.index.reachable(
                                                &args.node_id,
                                                max_hops,
                                                edge_filter_slice,
                                            )
                                        };
                                        let inc = if max_hops == 1 {
                                            graph_state.index.neighbors(
                                                &args.node_id,
                                                edge_filter_slice,
                                                Direction::Incoming,
                                            )
                                        } else {
                                            graph_state.index.impact(&args.node_id, max_hops)
                                        };
                                        all_ids.extend(out);
                                        all_ids.extend(inc);
                                        all_ids.sort();
                                        all_ids.dedup();
                                    }
                                    _ => {
                                        return Ok(text_result(format!(
                                            "Invalid direction: \"{}\". Use \"outgoing\", \"incoming\", or \"both\".",
                                            direction
                                        )));
                                    }
                                }

                                if all_ids.is_empty() {
                                    Ok(text_result(format!(
                                        "No {} neighbors for `{}`.",
                                        direction, args.node_id
                                    )))
                                } else {
                                    let md = format_neighbor_nodes(&graph_state.nodes, &all_ids);
                                    Ok(text_result(format!(
                                        "## Graph neighbors ({}) of `{}`\n\n{} result(s)\n\n{}",
                                        direction,
                                        args.node_id,
                                        all_ids.len(),
                                        md
                                    )))
                                }
                            }
                            "impact" => {
                                let max_hops = args.max_hops.unwrap_or(3) as usize;
                                let impacted = graph_state.index.impact(&args.node_id, max_hops);

                                if impacted.is_empty() {
                                    Ok(text_result(format!(
                                        "No dependents found for `{}` within {} hops.",
                                        args.node_id, max_hops
                                    )))
                                } else {
                                    let md = format_neighbor_nodes(&graph_state.nodes, &impacted);
                                    Ok(text_result(format!(
                                        "## Impact analysis for `{}`\n\n{} dependent(s) within {} hop(s)\n\n{}",
                                        args.node_id,
                                        impacted.len(),
                                        max_hops,
                                        md
                                    )))
                                }
                            }
                            "reachable" => {
                                let max_hops = args.max_hops.unwrap_or(3) as usize;
                                let reachable = graph_state.index.reachable(
                                    &args.node_id,
                                    max_hops,
                                    edge_filter_slice,
                                );

                                if reachable.is_empty() {
                                    Ok(text_result(format!(
                                        "No reachable nodes from `{}` within {} hops.",
                                        args.node_id, max_hops
                                    )))
                                } else {
                                    let md = format_neighbor_nodes(&graph_state.nodes, &reachable);
                                    Ok(text_result(format!(
                                        "## Reachable from `{}`\n\n{} node(s) within {} hop(s)\n\n{}",
                                        args.node_id,
                                        reachable.len(),
                                        max_hops,
                                        md
                                    )))
                                }
                            }
                            other => {
                                Ok(text_result(format!(
                                    "Unknown mode: \"{}\". Use \"neighbors\", \"impact\", or \"reachable\".",
                                    other
                                )))
                            }
                        }
                    }
                    Err(e) => Ok(text_result(format!("Graph error: {}", e))),
                }
            }

            "git_history" => {
                let args: GitHistory = parse_args(params.arguments)?;
                if let Some(ref file_path_str) = args.file {
                    // File history mode
                    let max = args.limit.unwrap_or(20) as usize;
                    let file_path = Path::new(file_path_str);
                    if file_path.is_absolute() || file_path_str.contains("..") {
                        return Ok(text_result("Error: path must be relative and cannot contain '..'".to_string()));
                    }
                    match git::file_history(root, file_path, max) {
                        Ok(commits) => {
                            if commits.is_empty() {
                                Ok(text_result(format!(
                                    "No commits found for `{}`.",
                                    file_path_str
                                )))
                            } else {
                                let md = commits
                                    .iter()
                                    .map(|c| c.to_markdown())
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                Ok(text_result(format!(
                                    "## File history: `{}`\n\n{} commit(s)\n\n{}",
                                    file_path_str,
                                    commits.len(),
                                    md
                                )))
                            }
                        }
                        Err(e) => Ok(text_result(format!("Error: {}", e))),
                    }
                } else if let Some(ref query) = args.query {
                    // Commit search mode
                    let max = args.limit.unwrap_or(50) as usize;
                    match git::search_commits(root, query, max) {
                        Ok(commits) => {
                            if commits.is_empty() {
                                Ok(text_result(format!(
                                    "No commits matching \"{}\".",
                                    query
                                )))
                            } else {
                                let md = commits
                                    .iter()
                                    .map(|c| c.to_markdown())
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                Ok(text_result(format!(
                                    "## Commit search: \"{}\"\n\n{} result(s)\n\n{}",
                                    query,
                                    commits.len(),
                                    md
                                )))
                            }
                        }
                        Err(e) => Ok(text_result(format!("Error: {}", e))),
                    }
                } else {
                    Ok(text_result("Provide either 'query' (to search commit messages) or 'file' (to view file history).".to_string()))
                }
            }

            "list_roots" => {
                let workspace = WorkspaceConfig::load()
                    .with_primary_root(self.repo_root.clone());
                let resolved = workspace.resolved_roots();

                if resolved.is_empty() {
                    Ok(text_result("No workspace roots configured.".to_string()))
                } else {
                    let md: String = resolved
                        .iter()
                        .enumerate()
                        .map(|(i, r)| {
                            let primary = if i == 0 { " (primary)" } else { "" };
                            format!(
                                "- **{}**{}: `{}` (type: {}, git: {})",
                                r.slug,
                                primary,
                                r.path.display(),
                                r.config.root_type,
                                r.config.git_aware,
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok(text_result(format!(
                        "## Workspace Roots\n\n{} root(s)\n\n{}",
                        resolved.len(),
                        md
                    )))
                }
            }

            _ => Err(CallToolError::unknown_tool(&params.name)),
        };

        // Prepend business context preamble to first successful tool result
        if let (Some(preamble), Ok(tool_result)) = (preamble, &mut result) {
            tool_result.content.insert(
                0,
                TextContent::new(preamble, None, None).into(),
            );
        }

        result
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

fn parse_args<T: serde::de::DeserializeOwned>(
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<T, CallToolError> {
    let value = arguments
        .map(serde_json::Value::Object)
        .unwrap_or(serde_json::Value::Null);
    serde_json::from_value(value)
        .map_err(|e| CallToolError::from_message(format!("Invalid arguments: {}", e)))
}

fn oh_init_impl(repo_root: &Path, outcome_name: Option<&str>) -> anyhow::Result<String> {
    use std::fs;

    let oh_dir = repo_root.join(".oh");
    let mut created = Vec::new();
    let mut skipped = Vec::new();

    // Create directory structure
    for subdir in &["outcomes", "signals", "guardrails", "metis"] {
        let dir = oh_dir.join(subdir);
        if !dir.exists() {
            fs::create_dir_all(&dir)?;
            created.push(format!(".oh/{}/", subdir));
        }
    }

    // Try to detect project name from README or CLAUDE.md
    let project_name = outcome_name
        .map(|s| s.to_string())
        .or_else(|| detect_project_name(repo_root))
        .unwrap_or_else(|| "project-goal".to_string());

    let slug = project_name
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
        .trim_matches('-')
        .to_string();

    // Scaffold outcome
    let outcome_path = oh_dir.join("outcomes").join(format!("{}.md", slug));
    if outcome_path.exists() {
        skipped.push(format!(".oh/outcomes/{}.md (exists)", slug));
    } else {
        let mut fm = BTreeMap::new();
        fm.insert("id".into(), serde_yaml::Value::String(slug.clone()));
        fm.insert("status".into(), serde_yaml::Value::String("active".into()));
        fm.insert(
            "mechanism".into(),
            serde_yaml::Value::String("(describe how this outcome is achieved)".into()),
        );
        fm.insert(
            "files".into(),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::String("src/*".into())]),
        );
        oh::write_artifact(
            repo_root,
            "outcomes",
            &slug,
            &fm,
            &format!("# {}\n\n(Describe the desired outcome here.)\n\n## Signals\n- (what signals indicate progress?)\n\n## Constraints\n- (what guardrails apply?)", project_name),
        )?;
        created.push(format!(".oh/outcomes/{}.md", slug));
    }

    // Scaffold signal
    let signal_slug = format!("{}-progress", slug);
    let signal_path = oh_dir.join("signals").join(format!("{}.md", signal_slug));
    if signal_path.exists() {
        skipped.push(format!(".oh/signals/{}.md (exists)", signal_slug));
    } else {
        let mut fm = BTreeMap::new();
        fm.insert("id".into(), serde_yaml::Value::String(signal_slug.clone()));
        fm.insert("outcome".into(), serde_yaml::Value::String(slug.clone()));
        fm.insert("type".into(), serde_yaml::Value::String("slo".into()));
        fm.insert(
            "threshold".into(),
            serde_yaml::Value::String("(define measurable threshold)".into()),
        );
        oh::write_artifact(
            repo_root,
            "signals",
            &signal_slug,
            &fm,
            &format!("# {} Progress\n\n(How do you measure progress toward this outcome?)", project_name),
        )?;
        created.push(format!(".oh/signals/{}.md", signal_slug));
    }

    // Scaffold lightweight guardrail
    let gr_path = oh_dir.join("guardrails").join("lightweight.md");
    if gr_path.exists() {
        skipped.push(".oh/guardrails/lightweight.md (exists)".into());
    } else {
        let mut fm = BTreeMap::new();
        fm.insert("id".into(), serde_yaml::Value::String("lightweight".into()));
        fm.insert("severity".into(), serde_yaml::Value::String("hard".into()));
        oh::write_artifact(
            repo_root,
            "guardrails",
            "lightweight",
            &fm,
            "# Lightweight Adoption\n\nAdding an outcome is writing a markdown file, not configuring a system. If this harness is heavier than adding a section to CLAUDE.md, adoption will fail.",
        )?;
        created.push(".oh/guardrails/lightweight.md".into());
    }

    // Build result message
    let mut msg = String::from("## .oh/ initialized\n\n");
    if !created.is_empty() {
        msg.push_str("### Created\n");
        for f in &created {
            msg.push_str(&format!("- `{}`\n", f));
        }
    }
    if !skipped.is_empty() {
        msg.push_str("\n### Skipped\n");
        for f in &skipped {
            msg.push_str(&format!("- `{}`\n", f));
        }
    }
    msg.push_str(&format!(
        "\n### Next steps\n1. Edit `.oh/outcomes/{}.md` — describe your outcome\n2. Edit `.oh/signals/{}.md` — define how to measure progress\n3. Add `files:` patterns to the outcome frontmatter\n4. Start tagging commits with `[outcome:{}]`\n",
        slug, signal_slug, slug
    ));
    Ok(msg)
}

fn detect_project_name(repo_root: &Path) -> Option<String> {
    // Try Cargo.toml name field
    let cargo_path = repo_root.join("Cargo.toml");
    if cargo_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&cargo_path) {
            for line in content.lines() {
                if let Some(name) = line.strip_prefix("name") {
                    let name = name.trim().trim_start_matches('=').trim().trim_matches('"');
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }
    // Try package.json name field
    let pkg_path = repo_root.join("package.json");
    if pkg_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&pkg_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                    return Some(name.to_string());
                }
            }
        }
    }
    // Try directory name
    repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
}

/// Parse an edge kind string into an EdgeKind enum variant.
fn parse_edge_kind(s: &str) -> Option<EdgeKind> {
    match s.to_lowercase().as_str() {
        "calls" => Some(EdgeKind::Calls),
        "implements" => Some(EdgeKind::Implements),
        "depends_on" => Some(EdgeKind::DependsOn),
        "connects_to" => Some(EdgeKind::ConnectsTo),
        "defines" => Some(EdgeKind::Defines),
        "has_field" => Some(EdgeKind::HasField),
        "evolves" => Some(EdgeKind::Evolves),
        "referenced_by" => Some(EdgeKind::ReferencedBy),
        "topology_boundary" => Some(EdgeKind::TopologyBoundary),
        "modified" => Some(EdgeKind::Modified),
        "affected" => Some(EdgeKind::Affected),
        "serves" => Some(EdgeKind::Serves),
        _ => None,
    }
}

/// Format a list of node IDs into markdown, enriched with node details if available.
fn format_neighbor_nodes(nodes: &[graph::Node], ids: &[String]) -> String {
    ids.iter()
        .map(|id| {
            if let Some(node) = nodes.iter().find(|n| n.stable_id() == *id) {
                format!(
                    "- **{}** `{}` ({}) `{}`:{}-{}",
                    node.id.kind,
                    node.id.name,
                    node.language,
                    node.id.file.display(),
                    node.line_start,
                    node.line_end,
                )
            } else {
                format!("- `{}`", id)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
