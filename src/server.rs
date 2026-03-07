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
use crate::types::OhArtifactKind;
use crate::{code, git, markdown, oh, query};
use tokio::sync::OnceCell;

// ── Tool input structs ──────────────────────────────────────────────

#[macros::mcp_tool(
    name = "oh_get_outcomes",
    description = "Returns all business outcomes from .oh/outcomes/"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhGetOutcomes {}

#[macros::mcp_tool(
    name = "oh_get_signals",
    description = "Returns all SLO signals from .oh/signals/"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhGetSignals {}

#[macros::mcp_tool(
    name = "oh_get_guardrails",
    description = "Returns all guardrails/constraints from .oh/guardrails/"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhGetGuardrails {}

#[macros::mcp_tool(
    name = "oh_get_metis",
    description = "Returns all metis (learnings/decisions) from .oh/metis/"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhGetMetis {}

#[macros::mcp_tool(
    name = "oh_get_context",
    description = "Returns the full business context bundle: outcomes, signals, guardrails, metis, plus recent commits, code symbols, and markdown sections"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhGetContext {}

#[macros::mcp_tool(
    name = "oh_record_metis",
    description = "Records a new metis (learning/decision) entry in .oh/metis/"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhRecordMetis {
    /// URL-safe slug used as the filename (e.g. 'prefer-lancedb')
    pub slug: String,
    /// Human-readable title for the metis entry
    pub title: String,
    /// Markdown body describing the learning or decision
    pub body: String,
    /// Optional outcome ID this metis relates to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

#[macros::mcp_tool(
    name = "oh_record_signal",
    description = "Records a signal observation (SLO measurement, progress indicator) in .oh/signals/"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhRecordSignal {
    /// URL-safe slug used as the filename (e.g. 'p95-latency')
    pub slug: String,
    /// The outcome this signal measures
    pub outcome: String,
    /// Signal type: slo, metric, qualitative
    #[serde(default = "default_signal_type")]
    pub signal_type: String,
    /// Threshold or definition (e.g. "p95 < 200ms")
    pub threshold: String,
    /// Markdown body with details, measurement method, current state
    pub body: String,
}

fn default_signal_type() -> String {
    "slo".to_string()
}

#[macros::mcp_tool(
    name = "oh_update_outcome",
    description = "Updates fields on an existing outcome in .oh/outcomes/. Merges provided fields with existing frontmatter; body is preserved."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhUpdateOutcome {
    /// The outcome slug/ID to update (e.g. 'agent-alignment')
    pub slug: String,
    /// Optional: new status (e.g. 'active', 'achieved', 'paused', 'abandoned')
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Optional: new or updated mechanism description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mechanism: Option<String>,
    /// Optional: updated file patterns (replaces existing)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

#[macros::mcp_tool(
    name = "oh_record_guardrail_candidate",
    description = "Proposes a guardrail candidate from experience. Guardrails are born from regret, not theory. Human promotes to hard/soft guardrail."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhRecordGuardrailCandidate {
    /// URL-safe slug (e.g. 'no-breaking-api-changes')
    pub slug: String,
    /// The constraint statement
    pub statement: String,
    /// Severity: candidate (default), soft, hard
    #[serde(default = "default_severity")]
    pub severity: String,
    /// What outcome this guardrail protects
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Markdown body: rationale, what happened, override protocol
    pub body: String,
}

fn default_severity() -> String {
    "candidate".to_string()
}

#[macros::mcp_tool(
    name = "oh_search_context",
    description = "Semantic search over .oh/ artifacts and recent git commits. Finds outcomes, guardrails, metis, signals, and commits relevant to a natural language query. Uses local embeddings — no API key needed."
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
}

#[macros::mcp_tool(
    name = "search_markdown",
    description = "Searches all markdown files in the repo by case-insensitive substring match against headings and content"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SearchMarkdown {
    /// Search query string
    pub query: String,
}

#[macros::mcp_tool(
    name = "search_code",
    description = "Searches code symbols (functions, structs, traits, enums, etc.) by name and signature. Supports optional kind filter (function, struct, trait, impl, enum, const, module) and file glob filter."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SearchCode {
    /// Search query string (matched against symbol name and signature)
    pub query: String,
    /// Optional: filter by symbol kind (function, struct, trait, impl, enum, const, module)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Optional: filter by file path glob (e.g. "src/server*", "src/oh/*")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

#[macros::mcp_tool(
    name = "search_commits",
    description = "Searches git commit messages by case-insensitive substring match"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SearchCommits {
    /// Search query string
    pub query: String,
    /// Maximum number of commits to return (default: 50)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_count: Option<u64>,
}

#[macros::mcp_tool(
    name = "file_history",
    description = "Returns git commit history for a specific file path"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FileHistory {
    /// File path relative to the repository root
    pub path: String,
    /// Maximum number of commits to return (default: 20)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_count: Option<u64>,
}

#[macros::mcp_tool(
    name = "search_all",
    description = "Multi-source substring search across all layers (.oh/ artifacts, markdown, code symbols, git commits). Note: this is a keyword union, not a relational intersection."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SearchAll {
    /// Search query string to match across all layers
    pub query: String,
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
    name = "oh_init",
    description = "Scaffolds .oh/ directory structure for a repo. Reads CLAUDE.md, README.md, and recent git history to propose an initial outcome, signal, and guardrails. Idempotent — won't overwrite existing files."
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OhInit {
    /// Optional: name for the primary outcome (auto-detected from README/CLAUDE.md if omitted)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_name: Option<String>,
}

// ── ServerHandler ───────────────────────────────────────────────────

pub struct RnaHandler {
    pub repo_root: PathBuf,
    pub embed_index: OnceCell<EmbeddingIndex>,
}

impl Default for RnaHandler {
    fn default() -> Self {
        Self {
            repo_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            embed_index: OnceCell::new(),
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
}

fn text_result(s: String) -> CallToolResult {
    CallToolResult::text_content(vec![TextContent::new(s, None, None)])
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
                OhGetOutcomes::tool(),
                OhGetSignals::tool(),
                OhGetGuardrails::tool(),
                OhGetMetis::tool(),
                OhGetContext::tool(),
                OhRecordMetis::tool(),
                OhSearchContext::tool(),
                OhRecordSignal::tool(),
                OhUpdateOutcome::tool(),
                OhRecordGuardrailCandidate::tool(),
                SearchMarkdown::tool(),
                SearchCode::tool(),
                SearchCommits::tool(),
                FileHistory::tool(),
                SearchAll::tool(),
                OutcomeProgress::tool(),
                OhInit::tool(),
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

        match params.name.as_str() {
            "oh_get_outcomes" => ok_markdown(get_artifacts_by_kind(root, OhArtifactKind::Outcome)),
            "oh_get_signals" => ok_markdown(get_artifacts_by_kind(root, OhArtifactKind::Signal)),
            "oh_get_guardrails" => {
                ok_markdown(get_artifacts_by_kind(root, OhArtifactKind::Guardrail))
            }
            "oh_get_metis" => ok_markdown(get_artifacts_by_kind(root, OhArtifactKind::Metis)),

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

            "oh_record_metis" => {
                let args: OhRecordMetis = parse_args(params.arguments)?;
                let mut fm = BTreeMap::new();
                fm.insert(
                    "id".to_string(),
                    serde_yaml::Value::String(args.slug.clone()),
                );
                fm.insert(
                    "title".to_string(),
                    serde_yaml::Value::String(args.title.clone()),
                );
                if let Some(ref outcome) = args.outcome {
                    fm.insert(
                        "outcome".to_string(),
                        serde_yaml::Value::String(outcome.clone()),
                    );
                }

                match oh::write_metis(root, &args.slug, &fm, &args.body) {
                    Ok(path) => Ok(text_result(format!(
                        "Recorded metis at `{}`",
                        path.display()
                    ))),
                    Err(e) => Ok(text_result(format!("Error writing metis: {}", e))),
                }
            }

            "oh_search_context" => {
                let args: OhSearchContext = parse_args(params.arguments)?;
                let limit = args.limit.unwrap_or(5) as usize;
                match self.get_index().await {
                    Ok(index) => {
                        match index.search(&args.query, args.artifact_types.as_deref(), limit).await {
                            Ok(results) => {
                                if results.is_empty() {
                                    Ok(text_result(format!(
                                        "No .oh/ artifacts found matching \"{}\".",
                                        args.query
                                    )))
                                } else {
                                    let md: String = results
                                        .iter()
                                        .map(|r| r.to_markdown())
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    Ok(text_result(format!(
                                        "## Semantic search: \"{}\"\n\n{} result(s)\n\n{}",
                                        args.query,
                                        results.len(),
                                        md
                                    )))
                                }
                            }
                            Err(e) => Ok(text_result(format!("Search error: {}", e))),
                        }
                    }
                    Err(e) => Ok(text_result(format!("Index error: {}", e))),
                }
            }

            "oh_record_signal" => {
                let args: OhRecordSignal = parse_args(params.arguments)?;
                let mut fm = BTreeMap::new();
                fm.insert("id".into(), serde_yaml::Value::String(args.slug.clone()));
                fm.insert("outcome".into(), serde_yaml::Value::String(args.outcome));
                fm.insert("type".into(), serde_yaml::Value::String(args.signal_type));
                fm.insert("threshold".into(), serde_yaml::Value::String(args.threshold));

                match oh::write_artifact(root, "signals", &args.slug, &fm, &args.body) {
                    Ok(path) => Ok(text_result(format!("Recorded signal at `{}`", path.display()))),
                    Err(e) => Ok(text_result(format!("Error: {}", e))),
                }
            }

            "oh_update_outcome" => {
                let args: OhUpdateOutcome = parse_args(params.arguments)?;
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

            "oh_record_guardrail_candidate" => {
                let args: OhRecordGuardrailCandidate = parse_args(params.arguments)?;
                let mut fm = BTreeMap::new();
                fm.insert("id".into(), serde_yaml::Value::String(args.slug.clone()));
                fm.insert("severity".into(), serde_yaml::Value::String(args.severity));
                fm.insert("statement".into(), serde_yaml::Value::String(args.statement));
                if let Some(ref outcome) = args.outcome {
                    fm.insert("outcome".into(), serde_yaml::Value::String(outcome.clone()));
                }

                match oh::write_artifact(root, "guardrails", &args.slug, &fm, &args.body) {
                    Ok(path) => Ok(text_result(format!("Recorded guardrail candidate at `{}`", path.display()))),
                    Err(e) => Ok(text_result(format!("Error: {}", e))),
                }
            }

            "search_markdown" => {
                let args: SearchMarkdown = parse_args(params.arguments)?;
                match markdown::extract_markdown_chunks(root) {
                    Ok(chunks) => {
                        let matches = markdown::search_chunks(&chunks, &args.query);
                        if matches.is_empty() {
                            Ok(text_result(format!(
                                "No markdown matches for \"{}\".",
                                args.query
                            )))
                        } else {
                            let md = matches
                                .iter()
                                .map(|c| c.to_markdown())
                                .collect::<Vec<_>>()
                                .join("\n\n---\n\n");
                            Ok(text_result(format!(
                                "## Markdown search: \"{}\"\n\n{} result(s)\n\n---\n\n{}",
                                args.query,
                                matches.len(),
                                md
                            )))
                        }
                    }
                    Err(e) => Ok(text_result(format!("Error: {}", e))),
                }
            }

            "search_code" => {
                let args: SearchCode = parse_args(params.arguments)?;
                match code::extract_symbols(root) {
                    Ok(symbols) => {
                        let mut matches = code::search_symbols(&symbols, &args.query);

                        // Apply kind filter
                        if let Some(ref kind_filter) = args.kind {
                            let k = kind_filter.to_lowercase();
                            matches.retain(|s| s.kind.to_string().to_lowercase() == k);
                        }

                        // Apply file glob filter
                        if let Some(ref file_filter) = args.file {
                            matches.retain(|s| {
                                let path_str = s.file_path.to_string_lossy();
                                git::glob_match_public(file_filter, &path_str)
                            });
                        }

                        if matches.is_empty() {
                            Ok(text_result(format!(
                                "No code symbol matches for \"{}\".",
                                args.query
                            )))
                        } else {
                            let md = matches
                                .iter()
                                .map(|s| s.to_markdown())
                                .collect::<Vec<_>>()
                                .join("\n");
                            Ok(text_result(format!(
                                "## Code search: \"{}\"\n\n{} result(s)\n\n{}",
                                args.query,
                                matches.len(),
                                md
                            )))
                        }
                    }
                    Err(e) => Ok(text_result(format!("Error: {}", e))),
                }
            }

            "search_commits" => {
                let args: SearchCommits = parse_args(params.arguments)?;
                let max = args.max_count.unwrap_or(50) as usize;
                match git::search_commits(root, &args.query, max) {
                    Ok(commits) => {
                        if commits.is_empty() {
                            Ok(text_result(format!(
                                "No commits matching \"{}\".",
                                args.query
                            )))
                        } else {
                            let md = commits
                                .iter()
                                .map(|c| c.to_markdown())
                                .collect::<Vec<_>>()
                                .join("\n");
                            Ok(text_result(format!(
                                "## Commit search: \"{}\"\n\n{} result(s)\n\n{}",
                                args.query,
                                commits.len(),
                                md
                            )))
                        }
                    }
                    Err(e) => Ok(text_result(format!("Error: {}", e))),
                }
            }

            "file_history" => {
                let args: FileHistory = parse_args(params.arguments)?;
                let max = args.max_count.unwrap_or(20) as usize;
                let file_path = Path::new(&args.path);
                if file_path.is_absolute() || args.path.contains("..") {
                    return Ok(text_result("Error: path must be relative and cannot contain '..'".to_string()));
                }
                match git::file_history(root, file_path, max) {
                    Ok(commits) => {
                        if commits.is_empty() {
                            Ok(text_result(format!(
                                "No commits found for `{}`.",
                                args.path
                            )))
                        } else {
                            let md = commits
                                .iter()
                                .map(|c| c.to_markdown())
                                .collect::<Vec<_>>()
                                .join("\n");
                            Ok(text_result(format!(
                                "## File history: `{}`\n\n{} commit(s)\n\n{}",
                                args.path,
                                commits.len(),
                                md
                            )))
                        }
                    }
                    Err(e) => Ok(text_result(format!("Error: {}", e))),
                }
            }

            "search_all" => {
                let args: SearchAll = parse_args(params.arguments)?;
                match query::query_all(root, &args.query) {
                    Ok(result) => Ok(text_result(result.to_markdown())),
                    Err(e) => Ok(text_result(format!("Error: {}", e))),
                }
            }

            "outcome_progress" => {
                let args: OutcomeProgress = parse_args(params.arguments)?;
                match query::outcome_progress(root, &args.outcome_id) {
                    Ok(result) => Ok(text_result(result.to_markdown())),
                    Err(e) => Ok(text_result(format!("Error: {}", e))),
                }
            }

            "oh_init" => {
                let args: OhInit = parse_args(params.arguments)?;
                match oh_init_impl(root, args.outcome_name.as_deref()) {
                    Ok(msg) => Ok(text_result(msg)),
                    Err(e) => Ok(text_result(format!("Error: {}", e))),
                }
            }

            _ => Err(CallToolError::unknown_tool(&params.name)),
        }
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

fn get_artifacts_by_kind(
    repo_root: &Path,
    kind: OhArtifactKind,
) -> Result<String, anyhow::Error> {
    let all = oh::load_oh_artifacts(repo_root)?;
    let filtered: Vec<_> = all.into_iter().filter(|a| a.kind == kind).collect();
    Ok(oh::artifacts_to_markdown(&filtered))
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

fn ok_markdown(result: Result<String, anyhow::Error>) -> Result<CallToolResult, CallToolError> {
    match result {
        Ok(md) => Ok(text_result(md)),
        Err(e) => Ok(text_result(format!("Error: {}", e))),
    }
}
