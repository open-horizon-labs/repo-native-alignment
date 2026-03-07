use std::collections::HashMap;
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

use crate::types::OhArtifactKind;
use crate::{code, git, markdown, oh, query};

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
    description = "Searches code symbols (functions, structs, traits, enums, etc.) by case-insensitive substring match against name, signature, and body"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SearchCode {
    /// Search query string
    pub query: String,
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
    name = "query",
    description = "The intersection query: searches across all layers (.oh/ artifacts, markdown, code symbols, git commits) and returns a unified result"
)]
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct Query {
    /// Search query string to match across all layers
    pub query: String,
}

// ── ServerHandler ───────────────────────────────────────────────────

pub struct RnaHandler {
    pub repo_root: PathBuf,
}

impl Default for RnaHandler {
    fn default() -> Self {
        Self {
            repo_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
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
                SearchMarkdown::tool(),
                SearchCode::tool(),
                SearchCommits::tool(),
                FileHistory::tool(),
                Query::tool(),
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
                Ok(result) => Ok(text_result(result.to_markdown())),
                Err(e) => Ok(text_result(format!("Error: {}", e))),
            },

            "oh_record_metis" => {
                let args: OhRecordMetis = parse_args(params.arguments)?;
                let mut fm = HashMap::new();
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
                        let matches = code::search_symbols(&symbols, &args.query);
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

            "query" => {
                let args: Query = parse_args(params.arguments)?;
                match query::query_all(root, &args.query) {
                    Ok(result) => Ok(text_result(result.to_markdown())),
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

fn ok_markdown(result: Result<String, anyhow::Error>) -> Result<CallToolResult, CallToolError> {
    match result {
        Ok(md) => Ok(text_result(md)),
        Err(e) => Ok(text_result(format!("Error: {}", e))),
    }
}
