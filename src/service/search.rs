//! Flat search, graph traversal, and batch node retrieval.
//!
//! The `search` function is the unified entry point that dispatches to
//! `search_flat`, `search_traversal`, or `search_batch` depending on the
//! parameters supplied by the caller.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Once;

use crate::embed::{EMBEDDING_MODEL_NAME, SearchFilters, SearchMode, SearchOutcome};
use crate::graph::index::GraphIndex;
use crate::graph::{EdgeKind, ExtractionSource, Node, NodeKind};
use crate::ranking;
use crate::server::handlers::parse_search_mode;
use crate::server::helpers::{
    format_capability_readiness, format_freshness_full, format_neighbors_grouped_with_root,
    format_node_entry_with_root, strip_root_prefix,
};
use crate::server::state::{CapabilityReadinessState, GraphState, LspEnrichmentStatus};
use crate::server::store::parse_edge_kind;
use crate::server::{EnrichmentCapability, EnrichmentJobState, EnrichmentScope};

use super::{
    SearchContext, SearchParams, node_passes_root_filter, search_result_passes_root_filter,
};

/// When impact results exceed this node-count threshold, render a
/// subsystem-grouped summary instead of listing every node.
const IMPACT_SUMMARY_NODE_THRESHOLD: usize = 30;

/// Even when the node count is below the node threshold, if the rendered output
/// exceeds this character limit we retroactively switch to the summary view.
/// This catches cases where a small number of nodes with verbose bodies (non-
/// compact mode) still produce huge responses (e.g., 157K chars for ~80 nodes).
const IMPACT_SUMMARY_CHAR_THRESHOLD: usize = 40_000;

const MAX_SOURCE_SPAN_LINES: u32 = 200;
const MAX_SOURCE_SPAN_BYTES: usize = 64 * 1024;
const MAX_SOURCE_PATH_ENTRIES: usize = 50_000;
const MAX_SOURCE_CANDIDATES: usize = 20;

fn read_bounded_source_lines(path: &Path, start: u32, end: u32) -> Result<Vec<String>, String> {
    let mut file = fs::File::open(path).map_err(|error| format!("cannot read file: {error}"))?;
    let mut buffer = [0_u8; 8192];
    let mut selected = Vec::<Vec<u8>>::new();
    let mut current = Vec::new();
    let mut selected_bytes = 0_usize;
    let mut line_number = 1_u32;
    let mut saw_any = false;
    let mut last_was_newline = false;
    let mut completed = false;
    let mut utf8_pending = Vec::new();

    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read file: {error}"))?;
        if count == 0 {
            break;
        }
        if buffer[..count].contains(&0) {
            return Err("binary (contains NUL bytes)".to_string());
        }
        utf8_pending.extend_from_slice(&buffer[..count]);
        match std::str::from_utf8(&utf8_pending) {
            Ok(_) => utf8_pending.clear(),
            Err(error) if error.error_len().is_some() => {
                return Err("binary or is not valid UTF-8".to_string());
            }
            Err(error) => {
                let incomplete_start = error.valid_up_to();
                utf8_pending.drain(..incomplete_start);
            }
        }
        for &byte in &buffer[..count] {
            saw_any = true;
            last_was_newline = byte == b'\n';
            if completed {
                continue;
            }
            if (start..=end).contains(&line_number) && byte != b'\n' {
                selected_bytes += 1;
                if selected_bytes > MAX_SOURCE_SPAN_BYTES {
                    return Err(format!(
                        "requested source text exceeds the hard maximum of {MAX_SOURCE_SPAN_BYTES} bytes"
                    ));
                }
                current.push(byte);
            }
            if byte == b'\n' {
                if (start..=end).contains(&line_number) {
                    if current.last() == Some(&b'\r') {
                        current.pop();
                    }
                    selected.push(std::mem::take(&mut current));
                }
                if line_number == end {
                    completed = true;
                    continue;
                }
                line_number = line_number.saturating_add(1);
            }
        }
    }
    if !utf8_pending.is_empty() {
        return Err("binary or is not valid UTF-8".to_string());
    }

    let available_lines = if completed {
        end
    } else if !saw_any {
        0
    } else if last_was_newline {
        line_number.saturating_sub(1)
    } else {
        if (start..=end).contains(&line_number) {
            if current.last() == Some(&b'\r') {
                current.pop();
            }
            selected.push(current);
        }
        line_number
    };
    if start > available_lines {
        return Err(format!(
            "line {start} is out of range (file has {available_lines} lines)"
        ));
    }
    if end > available_lines {
        return Err(format!(
            "end line {end} is out of range (file has {available_lines} lines)"
        ));
    }
    selected
        .into_iter()
        .map(|line| String::from_utf8(line).map_err(|_| "binary or is not valid UTF-8".to_string()))
        .collect()
}

fn compiler_location(file: &Option<String>) -> Option<(String, u32)> {
    let value = file.as_deref()?;
    let (before_column, column) = value.rsplit_once(':')?;
    column.parse::<u32>().ok()?;
    let (path, line) = before_column.rsplit_once(':')?;
    let line = line.parse::<u32>().ok()?;
    (!path.is_empty()).then(|| (path.to_string(), line))
}

fn source_roots(params: &SearchParams, repo_root: &Path) -> Vec<(String, PathBuf)> {
    let workspace = crate::roots::WorkspaceConfig::load()
        .with_primary_root(repo_root.to_path_buf())
        .with_declared_roots(repo_root);
    let mut roots: Vec<_> = workspace
        .resolved_roots()
        .into_iter()
        .filter(|root| match params.root.as_deref() {
            Some(selected) if selected.eq_ignore_ascii_case("all") => true,
            Some(selected) => root.slug.eq_ignore_ascii_case(selected),
            None => root.path == repo_root,
        })
        .map(|root| (root.slug, root.path))
        .collect();
    if roots.is_empty() && params.root.is_none() {
        roots.push((
            crate::roots::RootConfig::code_project(repo_root.to_path_buf()).slug(),
            repo_root.to_path_buf(),
        ));
    }
    roots.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    roots
}

fn collect_suffix_matches(
    dir: &Path,
    suffix: &Path,
    matches: &mut Vec<PathBuf>,
    visited: &mut usize,
) -> Result<(), String> {
    if matches.len() > MAX_SOURCE_CANDIDATES {
        return Ok(());
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        *visited += 1;
        if *visited > MAX_SOURCE_PATH_ENTRIES {
            return Err(format!(
                "source path lookup exceeded the hard traversal maximum of {MAX_SOURCE_PATH_ENTRIES} filesystem entries; provide a more exact path"
            ));
        }
        let path = entry.path();
        if matches!(
            entry.file_name().to_str(),
            Some(".git" | "target" | "node_modules" | ".cache")
        ) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            if path.ends_with(suffix) {
                matches.push(path);
            }
        } else if file_type.is_dir() {
            collect_suffix_matches(&path, suffix, matches, visited)?;
        } else if file_type.is_file() && path.ends_with(suffix) {
            matches.push(path);
        }
        if matches.len() > MAX_SOURCE_CANDIDATES {
            return Ok(());
        }
    }
    Ok(())
}

fn source_span(params: &SearchParams, repo_root: &Path) -> String {
    let Some(raw_file) = params
        .file
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return "Source span lookup requires `file` plus `line` (or `file` as path:line:column)."
            .to_string();
    };
    let parsed_location = compiler_location(&params.file);
    let (file, start) = match (params.line, parsed_location) {
        (Some(line), Some((path, _))) => (path, line),
        (Some(line), None) => (raw_file.to_string(), line),
        (None, Some((path, line))) => (path, line),
        (None, None) => {
            return "Source span lookup requires `line`, or a compiler-style `file` value such as src/lib.rs:42:7.".to_string();
        }
    };
    let end = params.end_line.unwrap_or(start);
    if start == 0 || end == 0 {
        return "Source line numbers are 1-based; `line` and `end_line` must be at least 1."
            .to_string();
    }
    if end < start {
        return format!("Invalid source span {start}-{end}: `end_line` must be >= `line`.");
    }
    let count = u64::from(end) - u64::from(start) + 1;
    if count > u64::from(MAX_SOURCE_SPAN_LINES) {
        return format!(
            "Source span {start}-{end} requests {count} lines; the hard maximum is {MAX_SOURCE_SPAN_LINES}. Narrow the range."
        );
    }

    let requested = PathBuf::from(&file);
    if requested.is_absolute()
        || requested.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return format!(
            "Rejected source path `{file}`: use a repo/root-relative path without parent traversal."
        );
    }

    let roots = source_roots(params, repo_root);
    if roots.is_empty() {
        return format!(
            "Selected root `{}` is not configured or is unavailable.",
            params.root.as_deref().unwrap_or_default()
        );
    }
    let mut candidates = Vec::new();
    let mut visited = 0;
    let mut omitted_candidates = false;
    for (slug, root) in roots {
        let Ok(canonical_root) = fs::canonicalize(&root) else {
            continue;
        };
        let exact = root.join(&requested);
        let mut paths = if exact.symlink_metadata().is_ok() {
            vec![exact]
        } else {
            let mut suffix_matches = Vec::new();
            if let Err(error) =
                collect_suffix_matches(&root, &requested, &mut suffix_matches, &mut visited)
            {
                return format!("Source path lookup failed: {error}.");
            }
            if suffix_matches.len() > MAX_SOURCE_CANDIDATES {
                suffix_matches.truncate(MAX_SOURCE_CANDIDATES);
                omitted_candidates = true;
            }
            suffix_matches
        };
        paths.sort();
        for path in paths {
            match fs::canonicalize(&path) {
                Ok(canonical) if canonical.starts_with(&canonical_root) => {
                    candidates.push((slug.clone(), canonical_root.clone(), canonical));
                    if candidates.len() > MAX_SOURCE_CANDIDATES {
                        omitted_candidates = true;
                        break;
                    }
                }
                Ok(_) => {
                    return format!(
                        "Rejected source path `{file}`: the resolved path escapes root `{}` through a symlink.",
                        root.display()
                    );
                }
                Err(error) => {
                    return format!("Cannot resolve source path `{}`: {error}.", path.display());
                }
            }
        }
        if candidates.len() > MAX_SOURCE_CANDIDATES {
            candidates.truncate(MAX_SOURCE_CANDIDATES);
            break;
        }
    }
    candidates.sort_by(|a, b| a.2.cmp(&b.2));
    candidates.dedup_by(|a, b| a.2 == b.2);
    if candidates.is_empty() {
        return format!("Source file `{file}` was not found in the selected repository/root.");
    }
    if candidates.len() > 1 {
        let paths = candidates
            .iter()
            .map(|(slug, root, path)| {
                format!(
                    "- [{slug}] {}",
                    path.strip_prefix(root).unwrap_or(path).display()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let omitted = if omitted_candidates {
            format!("\n- … additional matches omitted after {MAX_SOURCE_CANDIDATES} candidates")
        } else {
            String::new()
        };
        return format!(
            "Source path `{file}` is ambiguous. Provide one exact repo/root-relative path. Candidates:\n{paths}{omitted}"
        );
    }
    let (slug, root, path) = candidates.pop().unwrap();
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => return format!("Cannot inspect source file `{}`: {error}.", path.display()),
    };
    if !metadata.is_file() {
        return format!("Source path `{file}` is not a regular file.");
    }
    let lines = match read_bounded_source_lines(&path, start, end) {
        Ok(lines) => lines,
        Err(error) => return format!("Cannot read source file `{file}`: {error}."),
    };
    let numbered = lines
        .iter()
        .enumerate()
        .map(|(offset, line)| format!("{:>6} | {line}", start as usize + offset))
        .collect::<Vec<_>>()
        .join("\n");
    let relative = path.strip_prefix(&root).unwrap_or(&path);
    let longest_backtick_run = numbered
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest_backtick_run.saturating_add(1).max(3));
    format!(
        "## Source span\n\n- **Root:** `{slug}` (`{}`)\n- **File:** `{}`\n- **Lines:** {start}-{end}\n- **Provenance:** current filesystem state (may differ from the last indexed snapshot)\n- **Bound:** at most {MAX_SOURCE_SPAN_LINES} lines and {MAX_SOURCE_SPAN_BYTES} source bytes per request\n\n{fence}text\n{numbered}\n{fence}",
        root.display(),
        relative.display()
    )
}

fn format_verbose_readiness(
    gs: &GraphState,
    ctx: &SearchContext<'_>,
    semantic_index_attached: bool,
    semantic_index_available: bool,
) -> String {
    let should_infer_lsp_status = ctx
        .lsp_status
        .map(|status| status.call_reference_readiness().state != CapabilityReadinessState::Ready)
        .unwrap_or(true);
    let inferred_lsp_status = if should_infer_lsp_status {
        let persisted_lsp_edges = gs
            .edges
            .iter()
            .filter(|edge| {
                edge.source == ExtractionSource::Lsp
                    && matches!(edge.kind, EdgeKind::Calls | EdgeKind::ReferencedBy)
            })
            .count();
        let completed_repo_order = ctx
            .enrichment_jobs
            .iter()
            .filter(|job| {
                job.capability == EnrichmentCapability::CallReferences
                    && job.scope == EnrichmentScope::Repo
                    && job.state == EnrichmentJobState::Completed
                    && job.completed_at.is_some()
                    && job.failure.is_none()
            })
            .map(|job| (job.updated_at, job.revision))
            .max();
        if let Some(degraded_job) = ctx
            .enrichment_jobs
            .iter()
            .filter(|job| {
                job.capability == EnrichmentCapability::CallReferences
                    && job.state == EnrichmentJobState::Degraded
                    && completed_repo_order
                        .is_none_or(|completed| (job.updated_at, job.revision) > completed)
            })
            .max_by_key(|job| (job.updated_at, job.revision))
        {
            let status = LspEnrichmentStatus::default();
            status.set_degraded(
                persisted_lsp_edges,
                degraded_job
                    .failure
                    .as_deref()
                    .unwrap_or("call-reference enrichment finalized with degraded output"),
            );
            Some(status)
        } else if completed_repo_order.is_some() {
            let status = LspEnrichmentStatus::default();
            status.set_complete(persisted_lsp_edges);
            Some(status)
        } else if let Some(scoped_job) = ctx
            .enrichment_jobs
            .iter()
            .filter(|job| {
                job.capability == EnrichmentCapability::CallReferences
                    && job.scope != EnrichmentScope::Repo
                    && job.state == EnrichmentJobState::Completed
                    && job.completed_at.is_some()
                    && job.failure.is_none()
            })
            .max_by_key(|job| (job.updated_at, job.revision))
        {
            let status = LspEnrichmentStatus::default();
            let scoped_edge_count = scoped_job.counters.edge_count.unwrap_or(0);
            status.set_complete_scoped(
                scoped_edge_count,
                scoped_edge_count,
                format!("{} scope", scoped_job.scope.stable_key()),
            );
            Some(status)
        } else if let Some(job) = ctx
            .enrichment_jobs
            .iter()
            .filter(|job| {
                job.capability == EnrichmentCapability::CallReferences
                    && !matches!(
                        job.state,
                        EnrichmentJobState::Completed
                            | EnrichmentJobState::Degraded
                            | EnrichmentJobState::Cancelled
                            | EnrichmentJobState::Superseded
                    )
            })
            .max_by_key(|job| (job.updated_at, job.revision))
        {
            let status = LspEnrichmentStatus::default();
            match job.state {
                EnrichmentJobState::Failed => status.set_failed(
                    job.failure
                        .as_deref()
                        .unwrap_or("call-reference enrichment failed"),
                ),
                EnrichmentJobState::Queued
                | EnrichmentJobState::Running
                | EnrichmentJobState::Persisting => status.set_running(),
                EnrichmentJobState::Completed
                | EnrichmentJobState::Degraded
                | EnrichmentJobState::Cancelled
                | EnrichmentJobState::Superseded => unreachable!("terminal states filtered above"),
            }
            Some(status)
        } else {
            None
        }
    } else {
        None
    };
    let lsp_status = inferred_lsp_status.as_ref().or(ctx.lsp_status);

    format!(
        "{}{}{}",
        format_freshness_full(
            gs.nodes.len(),
            gs.last_scan_completed_at,
            lsp_status,
            ctx.embed_status,
        ),
        format_capability_readiness(
            gs.nodes.len(),
            lsp_status,
            ctx.embed_status,
            semantic_index_attached,
            semantic_index_available,
        ),
        format_enrichment_jobs(ctx),
    )
}

fn format_enrichment_jobs(ctx: &SearchContext<'_>) -> String {
    if ctx.enrichment_jobs.is_empty() {
        return String::new();
    }

    let mut lines = vec!["\n\nEnrichment jobs:".to_string()];
    for job in &ctx.enrichment_jobs {
        let state = format!("{:?}", job.state).to_lowercase();
        let phase = job.phase.as_deref().unwrap_or("unknown");
        let failure = job
            .failure
            .as_ref()
            .map(|msg| format!("; failure: {}", msg))
            .unwrap_or_default();
        lines.push(format!(
            "- `{}` {} {} scope={} phase={} updated={}{}",
            job.job_id,
            job.capability.as_str(),
            state,
            job.scope.stable_key(),
            phase,
            job.updated_at,
            failure
        ));
    }
    lines.join("\n")
}

/// Unified search entry point. Returns formatted markdown.
pub async fn search(params: &SearchParams, ctx: &SearchContext<'_>) -> String {
    if params.line.is_some()
        || params.end_line.is_some()
        || compiler_location(&params.file).is_some()
    {
        let params = params.clone();
        let repo_root = ctx.repo_root.to_path_buf();
        return tokio::task::spawn_blocking(move || source_span(&params, &repo_root))
            .await
            .unwrap_or_else(|error| format!("Source span lookup task failed: {error}."));
    }

    let query = params
        .query
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let node = params
        .node
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let semantic_index_attached = ctx.embed_index.is_some();
    let semantic_index_available = if params.verbose {
        match ctx.embed_index {
            Some(index) => index.has_table().await.unwrap_or(false),
            None => false,
        }
    } else {
        false
    };

    if let Some(ref node_ids) = params.nodes {
        let node_ids: Vec<&str> = node_ids
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if node_ids.is_empty() {
            return "Empty nodes list. Provide at least one stable node ID.".to_string();
        }
        // depth > 1 is not supported for batched traversal (nodes=[...]).
        // Use node= (single node) instead, or call search separately for each node.
        if params.depth.unwrap_or(1) > 1 && params.normalized_mode() == Some("neighbors") {
            return "depth > 1 is not supported with nodes=[...] batched traversal. Use node= for a single entry point with depth traversal.".to_string();
        }
        return search_batch(
            &node_ids,
            params,
            ctx,
            semantic_index_attached,
            semantic_index_available,
        );
    }

    if params.normalized_mode().is_some() {
        search_traversal(
            params,
            query,
            node,
            ctx,
            semantic_index_attached,
            semantic_index_available,
        )
        .await
    } else if query.is_none() && node.is_some() {
        let node_ids = vec![node.unwrap()];
        search_batch(
            &node_ids,
            params,
            ctx,
            semantic_index_attached,
            semantic_index_available,
        )
    } else {
        search_flat(
            params,
            query,
            ctx,
            semantic_index_attached,
            semantic_index_available,
        )
        .await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EmbeddingScorerDiagnostic {
    failure: &'static str,
    mode: SearchMode,
}

thread_local! {
    static REDACT_SCORER_PANIC: Cell<bool> = const { Cell::new(false) };
}

static INSTALL_SCORER_PANIC_HOOK: Once = Once::new();

#[cfg(test)]
static REDACTED_SCORER_PANICS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn install_scorer_panic_hook() {
    INSTALL_SCORER_PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            let redact = REDACT_SCORER_PANIC.try_with(Cell::get).unwrap_or(false);
            if redact {
                #[cfg(test)]
                REDACTED_SCORER_PANICS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tracing::error!(
                    component = "embedding_scorer",
                    model = EMBEDDING_MODEL_NAME,
                    index = "attached",
                    failure = "task_panic",
                    "Embedding scorer panicked; panic payload redacted"
                );
            } else {
                previous(panic_info);
            }
        }));
    });
}

struct ScorerPanicRedactionGuard {
    previous: bool,
}

impl ScorerPanicRedactionGuard {
    fn enter() -> Self {
        let previous = REDACT_SCORER_PANIC.with(|redact| redact.replace(true));
        Self { previous }
    }
}

impl Drop for ScorerPanicRedactionGuard {
    fn drop(&mut self) {
        REDACT_SCORER_PANIC.with(|redact| redact.set(self.previous));
    }
}

async fn poll_scorer_with_content_safe_panic_hook<F>(scorer: F) -> anyhow::Result<SearchOutcome>
where
    F: std::future::Future<Output = anyhow::Result<SearchOutcome>>,
{
    let mut scorer = Box::pin(scorer);
    std::future::poll_fn(move |cx| {
        let _redaction = ScorerPanicRedactionGuard::enter();
        scorer.as_mut().poll(cx)
    })
    .await
}

impl EmbeddingScorerDiagnostic {
    fn render(&self) -> String {
        let mode = match self.mode {
            SearchMode::Hybrid => "hybrid",
            SearchMode::Keyword => "keyword",
            SearchMode::Semantic => "semantic",
        };
        format!(
            "### Search degradation\n\n\
             Embedding scorer unavailable: `component=embedding_scorer \
             model={EMBEDDING_MODEL_NAME} index=attached mode={mode} failure={}`. \
             RNA returned bounded lexical/graph results and kept the mapped graph available. \
             Rebuild the embeddings capability for this root, then retry semantic search.",
            self.failure
        )
    }
}

async fn isolate_embedding_scorer<F>(
    scorer: F,
    mode: SearchMode,
) -> Result<SearchOutcome, EmbeddingScorerDiagnostic>
where
    F: std::future::Future<Output = anyhow::Result<SearchOutcome>> + Send + 'static,
{
    install_scorer_panic_hook();
    match tokio::spawn(poll_scorer_with_content_safe_panic_hook(scorer)).await {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(_)) => {
            tracing::warn!(
                component = "embedding_scorer",
                model = EMBEDDING_MODEL_NAME,
                index = "attached",
                failure = "search_error",
                "Embedding scorer failed; using bounded lexical/graph fallback"
            );
            Err(EmbeddingScorerDiagnostic {
                failure: "search_error",
                mode,
            })
        }
        Err(join_error) => {
            let failure = if join_error.is_cancelled() {
                "task_cancelled"
            } else {
                "task_panic"
            };
            tracing::warn!(
                component = "embedding_scorer",
                model = EMBEDDING_MODEL_NAME,
                index = "attached",
                failure,
                "Embedding scorer task failed; using bounded lexical/graph fallback"
            );
            Err(EmbeddingScorerDiagnostic { failure, mode })
        }
    }
}

#[cfg(test)]
tokio::task_local! {
    static TEST_EMBEDDING_SCORER_PANIC: String;
}

#[cfg(test)]
pub(crate) async fn with_test_embedding_scorer_panic<T>(
    payload: String,
    future: impl std::future::Future<Output = T>,
) -> T {
    TEST_EMBEDDING_SCORER_PANIC.scope(payload, future).await
}

#[cfg(test)]
fn test_embedding_scorer_panic_payload() -> Option<String> {
    TEST_EMBEDDING_SCORER_PANIC.try_with(Clone::clone).ok()
}

#[cfg(not(test))]
fn test_embedding_scorer_panic_payload() -> Option<String> {
    None
}

struct FlatCodeSymbolSearch<'a> {
    matches: Vec<&'a Node>,
    scorer_diagnostic: Option<EmbeddingScorerDiagnostic>,
}

async fn search_flat(
    params: &SearchParams,
    query: Option<&str>,
    ctx: &SearchContext<'_>,
    semantic_index_attached: bool,
    semantic_index_available: bool,
) -> String {
    let sort_by_complexity = params.sort_by.as_deref() == Some("complexity");
    let sort_by_importance = params.sort_by.as_deref() == Some("importance");
    let complexity_search = params.min_complexity.is_some() || sort_by_complexity;
    let has_kind_filter = params.kind.is_some();
    let has_file_filter = params.file.is_some();
    let has_synthetic_filter = params.synthetic.is_some();
    let has_subsystem_filter = params.subsystem.is_some();
    let has_browse_filter =
        has_kind_filter || has_file_filter || has_synthetic_filter || has_subsystem_filter;

    let query_str = query.unwrap_or("");
    if query_str.is_empty() && !complexity_search && !sort_by_importance && !has_browse_filter {
        return "Empty query. Please describe what you're looking for (or use kind, file, synthetic, min_complexity, sort_by=\"complexity\", or sort_by=\"importance\").".to_string();
    }

    let search_mode = parse_search_mode(params.search_mode.as_deref());
    let limit = params.limit.unwrap_or(10);
    let mut sections: Vec<String> = Vec::new();
    let graph_state = ctx.graph_state;

    // Try embedding-ranked code symbol search first; fall back to name/signature matching.
    let FlatCodeSymbolSearch {
        matches,
        mut scorer_diagnostic,
    } = flat_code_symbol_search_with_diagnostics(
        query_str,
        search_mode,
        limit,
        params,
        graph_state,
        ctx,
        sort_by_complexity,
        sort_by_importance,
    )
    .await;

    if !matches.is_empty() {
        let strip = ctx.root_filter.as_deref();
        let md: String = matches
            .iter()
            .map(|n| {
                format_node_entry_with_root(
                    n,
                    &graph_state.index,
                    params.compact,
                    strip,
                    false,
                    false,
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        sections.push(format!(
            "### Code symbols ({} result(s))\n\n{}",
            matches.len(),
            md
        ));
    }

    if params.include_artifacts
        && !query_str.is_empty()
        && let Some(embed_idx) = ctx.embed_index
    {
        let scorer = {
            let embed_idx = embed_idx.clone();
            let query = query_str.to_string();
            let artifact_types = params.artifact_types.clone();
            async move {
                embed_idx
                    .search_with_mode(&query, artifact_types.as_deref(), limit, search_mode)
                    .await
            }
        };
        match isolate_embedding_scorer(scorer, search_mode).await {
            Ok(SearchOutcome::Results(results)) => {
                let filtered: Vec<_> = results
                    .into_iter()
                    .filter(|r| !r.kind.starts_with("code:"))
                    .filter(|r| {
                        search_result_passes_root_filter(r, &ctx.root_filter, &ctx.non_code_slugs)
                    })
                    .collect();
                if !filtered.is_empty() {
                    let md: String = filtered
                        .iter()
                        .map(|r| r.to_markdown())
                        .collect::<Vec<_>>()
                        .join("\n");
                    sections.push(format!(
                        "### Artifacts ({} result(s))\n\n{}",
                        filtered.len(),
                        md
                    ));
                }
            }
            Ok(SearchOutcome::NotReady) => {
                sections.push("Embedding index: building -- artifact results will appear shortly. Retry in a few seconds.".to_string());
            }
            Err(diagnostic) => {
                scorer_diagnostic.get_or_insert(diagnostic);
            }
        }
    }

    if let Some(diagnostic) = scorer_diagnostic {
        sections.push(diagnostic.render());
    }

    if params.include_markdown
        && !query_str.is_empty()
        && let Ok(chunks) = crate::markdown::extract_markdown_chunks(ctx.repo_root)
    {
        let filtered_chunks: Vec<_> = if let Some(ref slug) = ctx.root_filter {
            let workspace = crate::roots::WorkspaceConfig::load()
                .with_primary_root(ctx.repo_root.to_path_buf())
                .with_worktrees(ctx.repo_root)
                .with_claude_memory(ctx.repo_root)
                .with_agent_memories(ctx.repo_root)
                .with_declared_roots(ctx.repo_root);
            let root_path = workspace
                .resolved_roots()
                .into_iter()
                .find(|r| r.slug.eq_ignore_ascii_case(slug))
                .map(|r| r.path);
            if let Some(rp) = root_path {
                chunks
                    .into_iter()
                    .filter(|c| c.file_path.starts_with(&rp))
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            chunks
        };
        let scored = crate::markdown::search_chunks_ranked(&filtered_chunks, query_str);
        if !scored.is_empty() {
            let md = scored
                .iter()
                .take(limit)
                .map(|sc| format!("- (score: {:.2}) {}", sc.score, sc.chunk.to_markdown()))
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");
            sections.push(format!(
                "### Markdown ({} result(s))\n\n{}",
                scored.len().min(limit),
                md
            ));
        }
    }

    let freshness = if params.verbose {
        format_verbose_readiness(
            graph_state,
            ctx,
            semantic_index_attached,
            semantic_index_available,
        )
    } else {
        String::new()
    };
    if sections.is_empty() {
        format!("No results matching \"{}\".{}", query_str, freshness)
    } else {
        format!(
            "## Search: \"{}\"\n\n{}{}",
            query_str,
            sections.join("\n\n"),
            freshness
        )
    }
}

/// Find code symbols for flat search, using embedding index when available.
///
/// Strategy:
/// 1. If query is non-empty and embed index is available, use `search_with_mode`
///    to get semantically-ranked code symbols, then resolve to graph nodes.
/// 2. Fall back to name/signature string matching if embed is unavailable or not ready.
/// 3. Apply post-filters (kind, language, file, root, synthetic, min_complexity).
/// 4. Apply sort_by overrides (complexity, importance) if requested; otherwise
///    preserve embed ranking or use name-match ranking for fallback results.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn flat_code_symbol_search<'a>(
    query_str: &str,
    search_mode: SearchMode,
    limit: usize,
    params: &SearchParams,
    graph_state: &'a GraphState,
    ctx: &SearchContext<'_>,
    sort_by_complexity: bool,
    sort_by_importance: bool,
) -> Vec<&'a Node> {
    flat_code_symbol_search_with_diagnostics(
        query_str,
        search_mode,
        limit,
        params,
        graph_state,
        ctx,
        sort_by_complexity,
        sort_by_importance,
    )
    .await
    .matches
}

#[allow(clippy::too_many_arguments)]
async fn flat_code_symbol_search_with_diagnostics<'a>(
    query_str: &str,
    search_mode: SearchMode,
    limit: usize,
    params: &SearchParams,
    graph_state: &'a GraphState,
    ctx: &SearchContext<'_>,
    sort_by_complexity: bool,
    sort_by_importance: bool,
) -> FlatCodeSymbolSearch<'a> {
    let query_lower = query_str.to_lowercase();
    let complexity_search = params.min_complexity.is_some() || sort_by_complexity;

    // Detect path/name split query (e.g. "auth/handlers/validate" → path="auth/handlers", name="validate").
    // When present, embed search uses only the name part; name-matching filters by both.
    let path_name = parse_path_name_query(query_str);
    let (path_filter_lower, name_filter_lower): (Option<String>, Option<String>) =
        if let Some((p, n)) = path_name {
            (Some(p.to_lowercase()), Some(n.to_lowercase()))
        } else {
            (None, None)
        };
    // The string forwarded to the embed index: name-part only for path/name queries
    // so the embedding attends to the symbol name rather than the slash-separated path.
    let embed_query_str: &str = name_filter_lower.as_deref().unwrap_or(query_str);

    // Build O(1) lookup map: stable_id -> index into graph_state.nodes.
    // Replaces O(N) linear scans per result when resolving embed results.
    let node_index_map = graph_state.node_index_map();

    // Closure: does a node pass path/name + all active filters?
    let node_passes_filters = |n: &Node| -> bool {
        if complexity_search && n.id.kind != NodeKind::Function {
            return false;
        }
        if let Some(ref kf) = params.kind
            && n.id.kind.to_string().to_lowercase() != kf.to_lowercase()
        {
            return false;
        }
        if let Some(ref lf) = params.language
            && n.language.to_lowercase() != lf.to_lowercase()
        {
            return false;
        }
        if let Some(ref ff) = params.file
            && !n.id.file.to_string_lossy().contains(ff.as_str())
        {
            return false;
        }
        if !node_passes_root_filter(&n.id.root, &ctx.root_filter, &ctx.non_code_slugs) {
            return false;
        }
        if let Some(sf) = params.synthetic
            && (n
                .metadata
                .get("synthetic")
                .map(|s| s == "true")
                .unwrap_or(false))
                != sf
        {
            return false;
        }
        if let Some(min_cc) = params.min_complexity {
            let Some(cc) = n
                .metadata
                .get("cyclomatic")
                .and_then(|s| s.parse::<u32>().ok())
            else {
                return false;
            };
            if cc < min_cc {
                return false;
            }
        }
        if let Some(ref sub) = params.subsystem {
            let node_sub = n
                .metadata
                .get(crate::server::SUBSYSTEM_KEY)
                .map(|s| s.as_str())
                .unwrap_or("");
            if !subsystem_matches(node_sub, sub) {
                return false;
            }
        }
        // Path/name split filter: when query contained `/`, require both file-path
        // and name to match their respective parts.
        if let (Some(pf), Some(nf)) = (&path_filter_lower, &name_filter_lower) {
            let file_match =
                n.id.file
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(pf.as_str());
            let name_match = n.id.name.to_lowercase().contains(nf.as_str());
            if !file_match || !name_match {
                return false;
            }
        }
        true
    };

    // When reranking is requested, over-fetch more candidates so the
    // cross-encoder has a wider pool to re-score.
    let rerank_over_fetch = if params.rerank { limit.max(20) } else { limit };

    // Build scalar pre-filters for LanceDB (#400).
    // When filters are active, LanceDB applies them before vector scoring so
    // only matching rows compete for the top-K slots. This gives correct
    // "top-K within filter" semantics instead of "globally top-3K, then discard."
    let embed_filters = SearchFilters {
        subsystem: params.subsystem.clone(),
        file: params.file.clone(),
        language: params.language.clone(),
        min_complexity: params.min_complexity,
    };
    let has_embed_filters = embed_filters.to_sql().is_some();
    let mut scorer_diagnostic = None;

    // Try embed-ranked search for code symbols when query is non-empty.
    // For path/name queries use only the name part so the embedding attends to
    // the symbol name rather than a slash-delimited path string.
    let mut used_embed = false;
    let mut matches: Vec<&Node> = if !query_str.is_empty() {
        if let Some(panic_payload) = test_embedding_scorer_panic_payload() {
            let scorer = async move {
                tokio::task::yield_now().await;
                panic!("{panic_payload}");
                #[allow(unreachable_code)]
                Ok(SearchOutcome::NotReady)
            };
            match isolate_embedding_scorer(scorer, search_mode).await {
                Err(diagnostic) => scorer_diagnostic = Some(diagnostic),
                Ok(_) => unreachable!("injected scorer panic must degrade"),
            }
            Vec::new()
        } else if let Some(embed_idx) = ctx.embed_index {
            // With scalar pre-filters active, fetch exactly rerank_over_fetch rows —
            // only matching rows are scored, so no over-fetch needed.
            // Without filters, keep the 3x over-fetch to allow for graph-side
            // filtering (root filter, synthetic, kind filter) and reranking.
            let over_fetch = if has_embed_filters {
                rerank_over_fetch
            } else {
                rerank_over_fetch * 3
            };
            let scorer = {
                let embed_idx = embed_idx.clone();
                let embed_query = embed_query_str.to_string();
                let embed_filters = embed_filters.clone();
                async move {
                    embed_idx
                        .search_with_filters(
                            &embed_query,
                            None,
                            over_fetch,
                            search_mode,
                            &embed_filters,
                        )
                        .await
                }
            };
            match isolate_embedding_scorer(scorer, search_mode).await {
                Ok(SearchOutcome::Results(results)) => {
                    used_embed = true;
                    // Keep only code results, resolve to graph nodes via HashMap (O(1)), apply filters.
                    // node_passes_filters already handles the path/name split check.
                    results
                        .iter()
                        .filter(|r| r.kind.starts_with("code:"))
                        .filter_map(|r| graph_state.node_by_stable_id(&r.id, node_index_map))
                        .filter(|n| node_passes_filters(n))
                        .take(rerank_over_fetch)
                        .collect()
                }
                // Embedding index not ready -- fall through to name/signature fallback.
                Ok(SearchOutcome::NotReady) => Vec::new(),
                Err(diagnostic) => {
                    scorer_diagnostic = Some(diagnostic);
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    // Supplement or fallback: structured text matching over symbol declarations.
    //
    // The wide-net stage must retrieve known facts before ranking can help.
    // Exact name/signature matches still dominate, but compound capability-style
    // queries also match high-signal terms against non-function declaration text
    // and extraction metadata. Function bodies stay out of generic symbol search.
    let text_terms = query_terms(query_str);
    if !used_embed {
        matches = graph_state
            .nodes
            .iter()
            .filter(|n| {
                if complexity_search && n.id.kind != NodeKind::Function {
                    return false;
                }
                if !query_lower.is_empty() && path_name.is_none() {
                    // Plain query: cast a wider lexical net over name, signature, body, and metadata.
                    // Path/name queries are handled inside node_passes_filters.
                    if !node_matches_text_query(n, &query_lower, &text_terms) {
                        return false;
                    }
                }
                node_passes_filters(n)
            })
            .collect();
    } else if !query_lower.is_empty() {
        // Embed search was used -- supplement with name/signature matches
        // that the embedding missed. Deduplicate by stable_id so embed-ranked
        // results keep their position; supplements are appended at the end.
        //
        // Cap supplements to avoid blowing up the reranker candidate pool
        // and reserve slots so supplements survive the downstream truncate.
        let supplement_budget = limit.min(10);
        let seen: std::collections::HashSet<String> =
            matches.iter().map(|n| n.stable_id()).collect();
        let name_supplements: Vec<&Node> = graph_state
            .nodes
            .iter()
            .filter(|n| {
                if seen.contains(&n.stable_id()) {
                    return false;
                }
                if path_name.is_none() {
                    // Plain query: cast the same lexical net for supplements so exact/code-expression
                    // matches survive embedding misses.
                    if !node_matches_text_query(n, &query_lower, &text_terms) {
                        return false;
                    }
                }
                node_passes_filters(n)
            })
            .collect();
        if !name_supplements.is_empty() {
            // Sort supplements by text-match quality, then cap to budget.
            // For path/name queries use only the name part for ranking.
            let sort_key = name_filter_lower.as_deref().unwrap_or(&query_lower);
            let mut sorted_supplements = name_supplements;
            sort_symbol_text_matches(
                &mut sorted_supplements,
                sort_key,
                &text_terms,
                &graph_state.index,
            );
            sorted_supplements.truncate(supplement_budget);
            // Evict tail embed results to make room so supplements survive
            // the final truncate(limit).
            let reserved = sorted_supplements.len();
            if matches.len() + reserved > limit {
                matches.truncate(limit.saturating_sub(reserved));
            }
            matches.extend(sorted_supplements);
        }
    }

    // Apply sort overrides or default ranking.
    if sort_by_complexity {
        matches.retain(|n| {
            n.metadata
                .get("cyclomatic")
                .and_then(|s| s.parse::<u32>().ok())
                .is_some()
        });
        matches.sort_by(|a, b| {
            let ca = a
                .metadata
                .get("cyclomatic")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let cb = b
                .metadata
                .get("cyclomatic")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            cb.cmp(&ca)
        });
    } else if sort_by_importance {
        matches.sort_by(|a, b| {
            let ia = a
                .metadata
                .get("importance")
                .and_then(|s| s.parse::<f64>().ok());
            let ib = b
                .metadata
                .get("importance")
                .and_then(|s| s.parse::<f64>().ok());
            match (ia, ib) {
                (Some(a), Some(b)) => b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });
    } else if !used_embed {
        // Only apply text-match ranking for fallback results; embed results
        // are already ranked by the embedding index.
        // For path/name queries use only the name part for ranking.
        let sort_key = name_filter_lower.as_deref().unwrap_or(&query_lower);
        sort_symbol_text_matches(&mut matches, sort_key, &text_terms, &graph_state.index);
    }

    // Cross-encoder reranking: re-score the top candidates using a cross-encoder
    // model that attends to (query, document) pairs jointly. This produces more
    // precise relevance scores than bi-encoder similarity alone.
    // Skip reranking when an explicit sort_by mode is active (complexity,
    // importance) -- the caller's sort request takes precedence.
    let use_relevance_sort = !sort_by_complexity && !sort_by_importance;
    if params.rerank && use_relevance_sort && !query_str.is_empty() && matches.len() > 1 {
        use crate::rerank::{RerankCandidate, rerank_results};

        let candidates: Vec<RerankCandidate> = matches
            .iter()
            .enumerate()
            .map(|(i, node)| {
                // Build reranking text from signature + body (the full context
                // the cross-encoder should attend to).
                let text = if node.body.is_empty() {
                    node.signature.clone()
                } else {
                    format!("{}\n{}", node.signature, node.body)
                };
                RerankCandidate {
                    text,
                    original_index: i,
                }
            })
            .collect();

        // Run reranking on a blocking thread to avoid blocking the Tokio
        // executor during ONNX model inference (and possible first-time
        // model download/initialization).
        let query_owned = query_str.to_string();
        let rerank_result =
            tokio::task::spawn_blocking(move || rerank_results(&query_owned, &candidates)).await;

        match rerank_result {
            Ok(Ok(reranked)) => {
                let original_matches = matches.clone();
                matches = reranked
                    .iter()
                    .filter_map(|r| original_matches.get(r.original_index).copied())
                    .collect();
                tracing::debug!(
                    "Reranked {} candidates for query \"{}\"",
                    reranked.len(),
                    query_str
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    "Cross-encoder reranking failed, using original order: {}",
                    e
                );
                // Fall through with original ordering -- reranking is best-effort.
            }
            Err(e) => {
                tracing::warn!("Reranking task panicked, using original order: {}", e);
            }
        }
    }

    matches.truncate(limit);
    FlatCodeSymbolSearch {
        matches,
        scorer_diagnostic,
    }
}

async fn search_traversal(
    params: &SearchParams,
    query: Option<&str>,
    node: Option<&str>,
    ctx: &SearchContext<'_>,
    semantic_index_attached: bool,
    semantic_index_available: bool,
) -> String {
    let mode = params.normalized_mode().unwrap_or("neighbors");
    let top_k = params.limit.unwrap_or(1).clamp(1, 50);

    // ── cycles mode ─────────────────────────────────────────────────────────
    // No entry-point resolution needed: we run tarjan_scc over the full graph.
    // If `node` is provided, return only the ring containing that node.
    // Otherwise return all rings (useful for a global circular-dependency audit).
    if mode == "cycles" {
        let gs = ctx.graph_state;
        let edge_filter = params.edge_types.as_ref().map(|types| {
            types
                .iter()
                .filter_map(|t| parse_edge_kind(t))
                .collect::<Vec<_>>()
        });
        let edge_filter_slice = edge_filter.as_deref();
        let freshness = if params.verbose {
            format_verbose_readiness(gs, ctx, semantic_index_attached, semantic_index_available)
        } else {
            String::new()
        };
        let strip = ctx.root_filter.as_deref();

        if let Some(node_id) = node {
            let resolved = gs.resolve_node_id(node_id);
            if gs.index.get_node(&resolved).is_none() {
                return format!(
                    "Node `{}` not found in graph. Use search to find valid node IDs.{freshness}",
                    strip_root_prefix(&resolved, strip),
                );
            }
            return match gs.index.cycle_for_node(&resolved, edge_filter_slice) {
                Some(ring) => {
                    let labels: Vec<String> = ring
                        .iter()
                        .map(|id| format!("`{}`", strip_root_prefix(id, strip)))
                        .collect();
                    format!(
                        "## Cycle containing `{}`\n\n{} node(s) in ring\n\n{}{freshness}",
                        strip_root_prefix(&resolved, strip),
                        labels.len(),
                        labels.join(" → ") + " → ...",
                    )
                }
                None => format!(
                    "`{}` is not part of any circular dependency.{freshness}",
                    strip_root_prefix(&resolved, strip),
                ),
            };
        }

        // No node specified: return all rings.
        let rings = gs.index.detect_cycles(edge_filter_slice);
        if rings.is_empty() {
            let scope = match edge_filter_slice {
                Some(kinds) if !kinds.is_empty() => {
                    let labels: Vec<String> = kinds.iter().map(|k| format!("{k}")).collect();
                    format!("filtered edges: {}", labels.join(", "))
                }
                _ => "default coupling graph (Calls + DependsOn)".to_string(),
            };
            return format!(
                "## Circular dependency analysis\n\nNo cycles detected in the {scope}.{freshness}"
            );
        }
        let mut out = format!(
            "## Circular dependency analysis\n\n{} ring(s) detected\n\n",
            rings.len()
        );
        for (i, ring) in rings.iter().enumerate() {
            let labels: Vec<String> = ring
                .iter()
                .map(|id| format!("`{}`", strip_root_prefix(id, strip)))
                .collect();
            out.push_str(&format!(
                "### Ring {}: {} nodes\n{}\n\n",
                i + 1,
                ring.len(),
                labels.join(" → ") + " → ..."
            ));
        }
        out.push_str(&freshness);
        return out;
    }

    // ── path mode ────────────────────────────────────────────────────────────
    // Computes the shortest directed call path from `node` (start) to `query`
    // (destination). Both are resolved via the usual name-matching machinery.
    // Returns the ordered hop list: start → hop1 → hop2 → ... → destination.
    if mode == "path" {
        if node.is_none() || query.is_none() {
            return "path mode requires both node= (start) and query= (destination).".to_string();
        }
        let gs = ctx.graph_state;
        let from_raw = node.unwrap();
        let to_raw = query.unwrap();
        let from_id = gs.resolve_node_id(from_raw);
        let to_id = gs.resolve_node_id(to_raw);
        let edge_filter = params.edge_types.as_ref().map(|types| {
            types
                .iter()
                .filter_map(|t| parse_edge_kind(t))
                .collect::<Vec<_>>()
        });
        let edge_filter_slice = edge_filter.as_deref();
        let freshness = if params.verbose {
            format_verbose_readiness(gs, ctx, semantic_index_attached, semantic_index_available)
        } else {
            String::new()
        };
        let strip = ctx.root_filter.as_deref();

        if gs.index.get_node(&from_id).is_none() {
            return format!(
                "Start node `{}` not found in graph. Use search to find valid node IDs.{freshness}",
                strip_root_prefix(&from_id, strip),
            );
        }
        if gs.index.get_node(&to_id).is_none() {
            return format!(
                "Destination node `{}` not found in graph. Use search to find valid node IDs.{freshness}",
                strip_root_prefix(&to_id, strip),
            );
        }

        return match gs.index.shortest_path(&from_id, &to_id, edge_filter_slice) {
            None => format!(
                "No directed call path from `{}` to `{}`.{freshness}",
                strip_root_prefix(&from_id, strip),
                strip_root_prefix(&to_id, strip),
            ),
            Some(hops) if hops.is_empty() => format!(
                "`{}` and `{}` are the same node — no path needed.{freshness}",
                strip_root_prefix(&from_id, strip),
                strip_root_prefix(&to_id, strip),
            ),
            Some(hops) => {
                let hop_count = hops.len(); // number of edges = number of directed calls
                let all_nodes: Vec<String> = std::iter::once(from_id.clone())
                    .chain(hops.iter().cloned())
                    .collect();
                let labels: Vec<String> = all_nodes
                    .iter()
                    .map(|id| format!("`{}`", strip_root_prefix(id, strip)))
                    .collect();
                format!(
                    "## Call path: {} → {}\n\n{} hop(s)\n\n{}{freshness}",
                    strip_root_prefix(&from_id, strip),
                    strip_root_prefix(&to_id, strip),
                    hop_count,
                    labels.join(" → "),
                )
            }
        };
    }

    if node.is_none() && query.is_none() {
        return "Either query or node is required. Provide a search query or a stable node ID."
            .to_string();
    }

    let search_mode = parse_search_mode(params.search_mode.as_deref());
    let (entry_node_ids, entry_header): (Vec<String>, String) = if let Some(node_id) = node {
        // Resolve short IDs (without root prefix) to full stable IDs.
        // Search results display `src/file.rs:name:kind` but graph needs `root:src/file.rs:name:kind`.
        let resolved = ctx.graph_state.resolve_node_id(node_id);
        // If resolve_node_id couldn't find the node AND the node_id contains `/`,
        // try path/name resolution before falling through.  This lets callers use
        // `node="auth/handlers/validate"` without knowing the full stable ID.
        if ctx.graph_state.index.get_node(&resolved).is_none()
            && parse_path_name_query(node_id).is_some()
        {
            let name_matches = resolve_entry_points_by_name(node_id, top_k, params, ctx);
            if !name_matches.is_empty() {
                let mut header = format!(
                    "### Matched entry nodes for \"{}\" (path/name match)\n\n",
                    node_id
                );
                let strip = ctx.root_filter.as_deref();
                let ids: Vec<String> = name_matches
                    .iter()
                    .map(|n| {
                        let stable_id = n.id.to_stable_id();
                        let display = strip_root_prefix(&stable_id, strip);
                        header
                            .push_str(&format!("- `{}` -- {} {}\n", display, n.id.kind, n.id.name));
                        stable_id
                    })
                    .collect();
                header.push('\n');
                (ids, header)
            } else {
                (vec![resolved], String::new())
            }
        } else {
            (vec![resolved], String::new())
        }
    } else if let Some(query_text) = query {
        // Try name matching against graph nodes first (#290).
        // This ensures `search("SearchParams", kind: "struct", mode: "neighbors")`
        // finds the struct by name, not by semantic similarity to random markdown.
        let name_matches = resolve_entry_points_by_name(query_text, top_k, params, ctx);
        if !name_matches.is_empty() {
            let mut header = format!(
                "### Matched entry nodes for \"{}\" (name match)\n\n",
                query_text
            );
            let strip = ctx.root_filter.as_deref();
            let ids: Vec<String> = name_matches
                .iter()
                .map(|n| {
                    let stable_id = n.id.to_stable_id();
                    let display = strip_root_prefix(&stable_id, strip);
                    header.push_str(&format!("- `{}` -- {} {}\n", display, n.id.kind, n.id.name));
                    stable_id
                })
                .collect();
            header.push('\n');
            (ids, header)
        } else if let Some(embed_idx) = ctx.embed_index {
            // Fall back to embed index for natural-language queries where name matching
            // finds nothing.
            match embed_idx.search_with_mode(query_text, None, top_k.min(50) * 3, search_mode).await {
                Ok(SearchOutcome::Results(results)) if !results.is_empty() => {
                    let node_index_map_for_entry = ctx.graph_state.node_index_map();
                    let code_results: Vec<_> = results.into_iter()
                        .filter(|r| r.kind.starts_with("code:"))
                        .filter(|r| search_result_passes_root_filter(r, &ctx.root_filter, &ctx.non_code_slugs))
                        .filter(|r| {
                            if let Some(ref sub) = params.subsystem {
                                ctx.graph_state.node_by_stable_id(&r.id, node_index_map_for_entry)
                                    .and_then(|n| n.metadata.get(crate::server::SUBSYSTEM_KEY))
                                    .map(|s| subsystem_matches(s, sub))
                                    .unwrap_or(false)
                            } else {
                                true
                            }
                        })
                        .take(top_k).collect();
                    if code_results.is_empty() { return format!("No code symbols matched query \"{}\". Try a different query or use node parameter.", query_text); }
                    let mut header = format!("### Matched entry nodes for \"{}\"\n\n", query_text);
                    let strip = ctx.root_filter.as_deref();
                    let ids: Vec<String> = code_results.iter().map(|r| { let display = strip_root_prefix(&r.id, strip); header.push_str(&format!("- `{}` -- {} (score: {:.2})\n", display, r.title, r.score)); r.id.clone() }).collect();
                    header.push('\n');
                    (ids, header)
                }
                Ok(SearchOutcome::NotReady) => return "Embedding index: building -- semantic graph queries will work shortly. Use node parameter instead, or retry in a few seconds.".to_string(),
                Ok(_) => return format!("No code symbols matched query \"{}\". Try a different query or use node parameter.", query_text),
                Err(e) => return format!("Semantic search failed: {}. Use node parameter instead.", e),
            }
        } else {
            return "No matching graph nodes found and embedding index not available. Use node parameter instead.".to_string();
        }
    } else {
        unreachable!()
    };

    let gs = ctx.graph_state;
    let valid_entry_ids: Vec<&String> = entry_node_ids
        .iter()
        .filter(|id| gs.index.get_node(id).is_some())
        .collect();
    if valid_entry_ids.is_empty() {
        let id_list = entry_node_ids
            .iter()
            .map(|id| format!("`{}`", id))
            .collect::<Vec<_>>()
            .join(", ");
        return format!(
            "{}No graph nodes found for {}. The node(s) may not have edges in the graph. Try search to find valid node IDs.",
            entry_header, id_list
        );
    }

    let edge_filter = params.edge_types.as_ref().map(|types| {
        types
            .iter()
            .filter_map(|t| parse_edge_kind(t))
            .collect::<Vec<_>>()
    });
    let edge_filter_slice = edge_filter.as_deref();

    // Collect grouped results across all entry nodes.
    // Deduplication is per-edge-kind: the same node may legitimately appear
    // under multiple relationship kinds, so we only deduplicate within a kind.
    use crate::server::handlers::run_traversal_grouped;
    let mut merged_groups: std::collections::BTreeMap<crate::graph::EdgeKind, Vec<String>> =
        std::collections::BTreeMap::new();
    // Per-kind seen sets for O(1) membership checks (avoids O(N²) Vec.contains in hot path).
    let mut merged_seen: std::collections::BTreeMap<crate::graph::EdgeKind, HashSet<String>> =
        std::collections::BTreeMap::new();
    let entry_set: HashSet<&str> = valid_entry_ids.iter().map(|s| s.as_str()).collect();

    // depth > 1 in neighbors mode: iterative BFS walking N levels deep.
    // Each level uses the previous level's results as the new frontier.
    // Nodes seen at earlier levels are not revisited (dedup across levels).
    let traversal_depth = if mode == "neighbors" {
        params.depth.unwrap_or(1).max(1)
    } else {
        1
    };

    if traversal_depth > 1 {
        // BFS: track visited nodes to avoid revisiting across levels.
        // Entry nodes are seeded into visited so they don't appear in results.
        let mut visited: HashSet<String> = valid_entry_ids.iter().map(|s| (*s).clone()).collect();
        let mut frontier: Vec<String> = valid_entry_ids.iter().map(|s| (*s).clone()).collect();

        for _ in 0..traversal_depth {
            if frontier.is_empty() {
                break;
            }
            let mut next_frontier: Vec<String> = Vec::new();
            for node_id in &frontier {
                match run_traversal_grouped(
                    &gs.index,
                    node_id,
                    mode,
                    Some(1),
                    params.direction.as_deref(),
                    edge_filter_slice,
                ) {
                    Ok(groups) => {
                        for (kind, ids) in groups {
                            let seen = merged_seen.entry(kind.clone()).or_default();
                            let entry = merged_groups.entry(kind).or_default();
                            for id in ids {
                                // visited: cross-level dedup; seen: intra-level O(1) per-kind dedup.
                                if !visited.contains(&id) && seen.insert(id.clone()) {
                                    entry.push(id.clone());
                                    next_frontier.push(id.clone());
                                }
                            }
                        }
                    }
                    Err(msg) => return msg,
                }
            }
            // Mark all newly-discovered nodes visited before next level.
            for id in &next_frontier {
                visited.insert(id.clone());
            }
            frontier = next_frontier;
        }
    } else {
        for node_id in &valid_entry_ids {
            match run_traversal_grouped(
                &gs.index,
                node_id,
                mode,
                params.hops,
                params.direction.as_deref(),
                edge_filter_slice,
            ) {
                Ok(groups) => {
                    for (kind, ids) in groups {
                        let seen = merged_seen.entry(kind.clone()).or_default();
                        let entry = merged_groups.entry(kind).or_default();
                        for id in ids {
                            if !entry_set.contains(id.as_str()) && seen.insert(id.clone()) {
                                entry.push(id);
                            }
                        }
                    }
                }
                Err(msg) => return msg,
            }
        }
    }

    // Build O(1) lookup map for stable_id -> node index.
    let node_index_map = gs.node_index_map();

    // Apply tests_for filtering
    if mode == "tests_for" {
        for ids in merged_groups.values_mut() {
            ids.retain(|id| {
                gs.node_by_stable_id(id, node_index_map)
                    .map(ranking::is_test_file)
                    .unwrap_or(false)
            });
        }
    }
    // Apply subsystem filter to traversal results (within-subsystem query).
    // When only `subsystem` is set, restrict neighbors to the same subsystem.
    if let Some(ref sub) = params.subsystem {
        for ids in merged_groups.values_mut() {
            ids.retain(|id| {
                gs.node_by_stable_id(id, node_index_map)
                    .and_then(|n| n.metadata.get(crate::server::SUBSYSTEM_KEY))
                    .map(|s| subsystem_matches(s, sub))
                    .unwrap_or(false)
            });
        }
    }
    // Apply target_subsystem filter (cross-subsystem query).
    // When set, keep only neighbors whose subsystem matches the target.
    // This enables queries like "what connects node X to the server subsystem?"
    if let Some(ref target_sub) = params.target_subsystem {
        for ids in merged_groups.values_mut() {
            ids.retain(|id| {
                gs.node_by_stable_id(id, node_index_map)
                    .and_then(|n| n.metadata.get(crate::server::SUBSYSTEM_KEY))
                    .map(|s| subsystem_matches(s, target_sub))
                    .unwrap_or(false)
            });
        }
    }
    // Remove empty groups after filtering
    merged_groups.retain(|_, ids| !ids.is_empty());

    // Count total displayable results
    let total_count: usize = merged_groups
        .values()
        .map(|ids| {
            ids.iter()
                .filter(|id| {
                    gs.node_by_stable_id(id, node_index_map)
                        .map(|n| !crate::server::helpers::is_hidden_traversal_kind(&n.id.kind))
                        .unwrap_or(true)
                })
                .count()
        })
        .sum();

    let strip = ctx.root_filter.as_deref();
    let entry_label = if valid_entry_ids.len() == 1 {
        format!("`{}`", strip_root_prefix(valid_entry_ids[0], strip))
    } else {
        format!("{} entry nodes", valid_entry_ids.len())
    };
    let direction = params.direction.as_deref().unwrap_or("outgoing");
    let freshness = if params.verbose {
        format_verbose_readiness(gs, ctx, semantic_index_attached, semantic_index_available)
    } else {
        String::new()
    };

    if total_count == 0 {
        let mode_desc = match mode {
            "neighbors" => format!("No {} neighbors for {}.", direction, entry_label),
            "impact" => format!(
                "No dependents found for {} within {} hops.",
                entry_label,
                params.hops.unwrap_or(3)
            ),
            "reachable" => format!(
                "No reachable nodes from {} within {} hops.",
                entry_label,
                params.hops.unwrap_or(3)
            ),
            "tests_for" => format!(
                "No test functions found calling {}. Either no tests exist for this symbol, or the call edges haven't been extracted (check LSP status).",
                entry_label
            ),
            _ => format!("No results for {}.", entry_label),
        };
        format!("{}{}{}", entry_header, mode_desc, freshness)
    } else {
        // For large impact results (>100 unique nodes), show only the subsystem summary
        // instead of listing every node. This prevents 162K+ char responses that
        // overflow MCP response limits and are unreadable by agents.
        // Use unique node count (not per-bucket total_count) because the same node
        // can appear under multiple edge kinds in merged_groups.
        let unique_impact_count = if mode == "impact" {
            let mut seen: HashSet<&str> = HashSet::new();
            for ids in merged_groups.values() {
                for id in ids {
                    if let Some(node) = gs.node_by_stable_id(id, node_index_map)
                        && !crate::server::helpers::is_hidden_traversal_kind(&node.id.kind)
                    {
                        seen.insert(id.as_str());
                    }
                }
            }
            seen.len()
        } else {
            0
        };
        let large_by_count =
            mode == "impact" && unique_impact_count > IMPACT_SUMMARY_NODE_THRESHOLD;

        // Helper: build the summary-only response for large impact results.
        let build_summary = |entry_header: &str, entry_label: &str, freshness: &str| -> String {
            let subsystem_breakdown =
                format_impact_subsystem_breakdown(&merged_groups, gs, node_index_map, strip);
            let subsystem_count = count_affected_subsystems(&merged_groups, gs, node_index_map);
            let heading = if subsystem_count == 0 {
                format!(
                    "## Impact of {}\n\n{} dependent(s) within {} hop(s) (result summarized — use `subsystem` filter to drill down)\n\n",
                    entry_label,
                    unique_impact_count,
                    params.hops.unwrap_or(3),
                )
            } else {
                format!(
                    "## Impact of {} ({} subsystems affected)\n\n{} dependent(s) within {} hop(s)\n{}\n",
                    entry_label,
                    subsystem_count,
                    unique_impact_count,
                    params.hops.unwrap_or(3),
                    subsystem_breakdown,
                )
            };
            format!("{}{}{}", entry_header, heading, freshness)
        };

        if large_by_count {
            // Node count alone exceeds threshold — skip rendering the full list.
            build_summary(&entry_header, &entry_label, &freshness)
        } else {
            let heading = match mode {
                "neighbors" => format!(
                    "## Graph neighbors ({}) of {}\n\n{} result(s)\n\n",
                    direction, entry_label, total_count
                ),
                "impact" => format!(
                    "## Impact analysis for {}\n\n{} dependent(s) within {} hop(s)\n\n",
                    entry_label,
                    total_count,
                    params.hops.unwrap_or(3)
                ),
                "reachable" => format!(
                    "## Reachable from {}\n\n{} node(s) within {} hop(s)\n\n",
                    entry_label,
                    total_count,
                    params.hops.unwrap_or(3)
                ),
                "tests_for" => format!(
                    "## Test coverage for {}\n\n{} test function(s)\n\n",
                    entry_label, total_count
                ),
                _ => String::new(),
            };

            let md = format_neighbors_grouped_with_root(
                &gs.nodes,
                &merged_groups,
                &gs.index,
                params.compact,
                strip,
                params.include_body,
                params.minify_body,
            );

            // For impact mode, append a subsystem breakdown showing which subsystems
            // are affected and through which interface function the impact propagates.
            let subsystem_section = if mode == "impact" {
                format_impact_subsystem_breakdown(&merged_groups, gs, node_index_map, strip)
            } else {
                String::new()
            };

            let full_output = format!(
                "{}{}{}{}{}",
                entry_header, heading, md, subsystem_section, freshness
            );

            // Safety net: if the rendered output exceeds the character threshold,
            // retroactively switch to the summary view. This catches cases where
            // a moderate number of nodes (below the node threshold) still produce
            // enormous output due to verbose non-compact rendering.
            if mode == "impact" && full_output.len() > IMPACT_SUMMARY_CHAR_THRESHOLD {
                build_summary(&entry_header, &entry_label, &freshness)
            } else {
                full_output
            }
        }
    }
}

/// Group impact results by subsystem metadata and format as a summary section.
///
/// For each affected subsystem, reports the symbol count and the first node in
/// that subsystem (the "entry point" through which impact propagates).
fn format_impact_subsystem_breakdown(
    merged_groups: &std::collections::BTreeMap<crate::graph::EdgeKind, Vec<String>>,
    gs: &GraphState,
    node_index_map: &std::collections::HashMap<String, usize>,
    strip: Option<&str>,
) -> String {
    // Collect all unique result node IDs across edge-kind groups, deduplicated.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut subsystem_nodes: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for ids in merged_groups.values() {
        for id in ids {
            if !seen.insert(id.clone()) {
                continue; // Skip duplicates across edge-kind buckets
            }
            if let Some(node) = gs.node_by_stable_id(id, node_index_map) {
                if crate::server::helpers::is_hidden_traversal_kind(&node.id.kind) {
                    continue;
                }
                if let Some(sub) = node.metadata.get(crate::server::SUBSYSTEM_KEY) {
                    subsystem_nodes
                        .entry(sub.clone())
                        .or_default()
                        .push(id.clone());
                }
            }
        }
    }

    if subsystem_nodes.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::new();
    for (subsystem, ids) in &subsystem_nodes {
        // The first node in this subsystem is the interface through which impact enters
        let entry_point = ids
            .first()
            .and_then(|id| gs.node_by_stable_id(id, node_index_map))
            .map(|n| {
                let display = strip_root_prefix(&n.stable_id(), strip);
                format!(", entry point: `{}`", display)
            })
            .unwrap_or_default();
        lines.push(format!(
            "- **{}** ({} symbol(s){})",
            subsystem,
            ids.len(),
            entry_point
        ));
    }

    format!("\n\n### Affected subsystems\n\n{}\n", lines.join("\n"))
}

/// Count the number of distinct subsystems affected by impact results.
fn count_affected_subsystems(
    merged_groups: &std::collections::BTreeMap<crate::graph::EdgeKind, Vec<String>>,
    gs: &GraphState,
    node_index_map: &std::collections::HashMap<String, usize>,
) -> usize {
    let mut seen_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut subsystems: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for ids in merged_groups.values() {
        for id in ids {
            if !seen_ids.insert(id.as_str()) {
                continue;
            }
            if let Some(node) = gs.node_by_stable_id(id, node_index_map) {
                if crate::server::helpers::is_hidden_traversal_kind(&node.id.kind) {
                    continue;
                }
                if let Some(sub) = node.metadata.get(crate::server::SUBSYSTEM_KEY) {
                    subsystems.insert(sub.as_str());
                }
            }
        }
    }
    subsystems.len()
}

fn search_batch(
    node_ids: &[&str],
    params: &SearchParams,
    ctx: &SearchContext<'_>,
    semantic_index_attached: bool,
    semantic_index_available: bool,
) -> String {
    use crate::server::handlers::run_traversal_grouped;
    let gs = ctx.graph_state;
    let freshness = if params.verbose {
        format_verbose_readiness(gs, ctx, semantic_index_attached, semantic_index_available)
    } else {
        String::new()
    };
    // Build O(1) lookup map and root slugs once for the entire batch.
    let node_index_map = gs.node_index_map();
    let roots = GraphState::root_slugs_from_index_map(node_index_map);
    if let Some(mode) = params.normalized_mode() {
        let edge_filter = params.edge_types.as_ref().map(|types| {
            types
                .iter()
                .filter_map(|t| parse_edge_kind(t))
                .collect::<Vec<_>>()
        });
        let edge_filter_slice = edge_filter.as_deref();
        let mut sections: Vec<String> = Vec::new();
        let strip = ctx.root_filter.as_deref();
        for &nid in node_ids {
            // Resolve short IDs (without root prefix) to full stable IDs.
            let resolved_nid = GraphState::resolve_node_id_fast(nid, node_index_map, &roots);
            let display_nid = strip_root_prefix(&resolved_nid, strip);
            if gs.index.get_node(&resolved_nid).is_none() {
                sections.push(format!("### `{}`\n\nNode not found in graph.", display_nid));
                continue;
            }
            match run_traversal_grouped(
                &gs.index,
                &resolved_nid,
                mode,
                params.hops,
                params.direction.as_deref(),
                edge_filter_slice,
            ) {
                Ok(mut groups) => {
                    // Remove self-references
                    for ids in groups.values_mut() {
                        ids.retain(|id| id != resolved_nid.as_str());
                    }
                    if mode == "tests_for" {
                        for ids in groups.values_mut() {
                            ids.retain(|id| {
                                gs.node_by_stable_id(id, node_index_map)
                                    .map(ranking::is_test_file)
                                    .unwrap_or(false)
                            });
                        }
                    }
                    groups.retain(|_, ids| !ids.is_empty());
                    let total: usize = groups
                        .values()
                        .map(|ids| {
                            ids.iter()
                                .filter(|id| {
                                    gs.node_by_stable_id(id, node_index_map)
                                        .map(|n| {
                                            !crate::server::helpers::is_hidden_traversal_kind(
                                                &n.id.kind,
                                            )
                                        })
                                        .unwrap_or(true)
                                })
                                .count()
                        })
                        .sum();
                    if total == 0 {
                        sections.push(format!("### `{}`\n\nNo {} results.", display_nid, mode));
                    } else {
                        let md = format_neighbors_grouped_with_root(
                            &gs.nodes,
                            &groups,
                            &gs.index,
                            params.compact,
                            strip,
                            params.include_body,
                            params.minify_body,
                        );
                        sections.push(format!(
                            "### `{}`\n\n{} result(s)\n\n{}",
                            display_nid, total, md
                        ));
                    }
                }
                Err(msg) => sections.push(format!("### `{}`\n\n{}", display_nid, msg)),
            }
        }
        format!(
            "## Batch {} for {} node(s)\n\n{}{}",
            mode,
            node_ids.len(),
            sections.join("\n\n"),
            freshness
        )
    } else {
        let mut found = Vec::new();
        let mut missing = Vec::new();
        for &nid in node_ids {
            let resolved = GraphState::resolve_node_id_fast(nid, node_index_map, &roots);
            if let Some(node) = gs.node_by_stable_id(&resolved, node_index_map) {
                found.push(node);
            } else {
                missing.push(nid);
            }
        }
        let strip = ctx.root_filter.as_deref();
        if found.is_empty() {
            return format!(
                "No nodes found for {}. Try search to find valid node IDs.{}",
                node_ids
                    .iter()
                    .map(|id| format!("`{}`", strip_root_prefix(id, strip)))
                    .collect::<Vec<_>>()
                    .join(", "),
                freshness
            );
        }
        let md: String = found
            .iter()
            .map(|n| {
                format_node_entry_with_root(
                    n,
                    &gs.index,
                    params.compact,
                    strip,
                    params.include_body,
                    params.minify_body,
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut result = format!("## Batch retrieve: {} found\n\n{}", found.len(), md);
        if !missing.is_empty() {
            result.push_str(&format!(
                "\n\n**Missing:** {}",
                missing
                    .iter()
                    .map(|id| format!("`{}`", strip_root_prefix(id, strip)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        result.push_str(&freshness);
        result
    }
}

/// Parse a path/name query like `"auth/handlers/validate"` into
/// `Some(("auth/handlers", "validate"))`. Returns `None` if the query
/// contains no `/` — plain queries must be handled by normal name matching.
///
/// Splits at the **last** `/` so that deep paths like `"src/auth/handlers/validate"`
/// produce `path_part = "src/auth/handlers"` and `name_part = "validate"`.
fn parse_path_name_query(query: &str) -> Option<(&str, &str)> {
    let slash_pos = query.rfind('/')?;
    let path_part = &query[..slash_pos];
    let name_part = &query[slash_pos + 1..];
    // Reject degenerate splits (empty name or empty path) — fall back to
    // plain matching.
    if path_part.is_empty() || name_part.is_empty() {
        return None;
    }
    Some((path_part, name_part))
}

/// Resolve traversal entry points by exact name/signature matching against graph nodes.
///
/// Applies kind, language, file, and root filters. Returns matching nodes sorted
/// by name-match quality (exact > contains). This is used as the primary entry
/// point resolution strategy for traversal queries (#290), with the embed index
/// as a fallback for natural-language queries where name matching finds nothing.
///
/// When `query` contains `/`, the query is parsed as `path_part/name_part` and
/// both the file path and name are filtered simultaneously. Plain queries (no `/`)
/// behave identically to today.
fn resolve_entry_points_by_name<'a>(
    query: &str,
    limit: usize,
    params: &SearchParams,
    ctx: &SearchContext<'a>,
) -> Vec<&'a Node> {
    let gs = ctx.graph_state;

    // Detect path/name split query (e.g. "auth/handlers/validate").
    let path_name = parse_path_name_query(query);
    let (query_lower, path_filter_lower, name_filter_lower): (
        String,
        Option<String>,
        Option<String>,
    ) = if let Some((path_part, name_part)) = path_name {
        (
            query.to_lowercase(),
            Some(path_part.to_lowercase()),
            Some(name_part.to_lowercase()),
        )
    } else {
        (query.to_lowercase(), None, None)
    };

    let mut matches: Vec<&Node> = gs
        .nodes
        .iter()
        .filter(|n| {
            // Name/file matching: path/name split vs. plain.
            if let (Some(pf), Some(nf)) = (&path_filter_lower, &name_filter_lower) {
                // Both file path and name must match.
                let file_match =
                    n.id.file
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(pf.as_str());
                let name_match = n.id.name.to_lowercase().contains(nf.as_str());
                if !file_match || !name_match {
                    return false;
                }
            } else {
                // Plain name or signature match.
                let name_match = n.id.name.to_lowercase().contains(&query_lower)
                    || n.signature.to_lowercase().contains(&query_lower);
                if !name_match {
                    return false;
                }
            }

            // Apply filters (kind, language, file, root).
            if let Some(ref kf) = params.kind
                && n.id.kind.to_string().to_lowercase() != kf.to_lowercase()
            {
                return false;
            }
            if let Some(ref lf) = params.language
                && n.language.to_lowercase() != lf.to_lowercase()
            {
                return false;
            }
            if let Some(ref ff) = params.file
                && !n.id.file.to_string_lossy().contains(ff.as_str())
            {
                return false;
            }
            if !node_passes_root_filter(&n.id.root, &ctx.root_filter, &ctx.non_code_slugs) {
                return false;
            }
            if let Some(ref sub) = params.subsystem {
                let node_sub = n
                    .metadata
                    .get(crate::server::SUBSYSTEM_KEY)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                if !subsystem_matches(node_sub, sub) {
                    return false;
                }
            }
            true
        })
        .collect();

    // Sort: exact name match first, then contains.
    // For path/name queries use the name part for exact-match comparison.
    let effective_query = name_filter_lower.as_deref().unwrap_or(&query_lower);
    matches.sort_by(|a, b| {
        let a_exact = a.id.name.to_lowercase() == effective_query
            || a.id.name.eq_ignore_ascii_case(query)
            || a.signature.eq_ignore_ascii_case(query);
        let b_exact = b.id.name.to_lowercase() == effective_query
            || b.id.name.eq_ignore_ascii_case(query)
            || b.signature.eq_ignore_ascii_case(query);
        b_exact.cmp(&a_exact)
    });

    matches.truncate(limit);
    matches
}

/// Match a node's subsystem metadata against a filter value.
///
/// Supports hierarchical matching: `subsystem="extract"` matches nodes whose
/// subsystem is exactly "extract" (case-insensitive) OR starts with "extract/"
/// (i.e., any child sub-module). `subsystem="extract/enrich"` matches only
/// nodes in that specific sub-module.
fn subsystem_matches(node_subsystem: &str, filter: &str) -> bool {
    if node_subsystem.eq_ignore_ascii_case(filter) {
        return true;
    }
    // Parent-level match: filter="extract" should match node_subsystem="extract/Node".
    // Check without allocating: node_subsystem must start with filter + "/" (case-insensitive).
    if node_subsystem.len() > filter.len() {
        let (head, tail) = node_subsystem.split_at(filter.len());
        return head.eq_ignore_ascii_case(filter) && tail.starts_with('/');
    }
    false
}

fn query_terms(query: &str) -> Vec<String> {
    query
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter_map(|raw| {
            let term = raw.trim().to_lowercase();
            if term.len() < 3 {
                return None;
            }
            if matches!(
                term.as_str(),
                "add"
                    | "and"
                    | "are"
                    | "but"
                    | "can"
                    | "does"
                    | "for"
                    | "from"
                    | "have"
                    | "how"
                    | "need"
                    | "registered"
                    | "the"
                    | "this"
                    | "what"
                    | "when"
                    | "where"
                    | "which"
                    | "why"
                    | "with"
            ) {
                return None;
            }
            Some(term)
        })
        .collect()
}

fn node_search_text_lower(n: &Node) -> String {
    let mut text = format!(
        "{} {} {} {}",
        n.id.name,
        n.signature,
        n.id.file.display(),
        n.language
    )
    .to_lowercase();
    if matches!(
        n.id.kind,
        NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Enum
            | NodeKind::TypeAlias
            | NodeKind::Const
            | NodeKind::ProtoMessage
            | NodeKind::SqlTable
            | NodeKind::ApiEndpoint
            | NodeKind::Macro
            | NodeKind::Field
            | NodeKind::EnumVariant
            | NodeKind::Other(_)
    ) && !n.body.is_empty()
    {
        text.push(' ');
        text.push_str(&n.body.to_lowercase());
    }
    for (key, value) in &n.metadata {
        text.push(' ');
        text.push_str(&key.to_lowercase());
        text.push(' ');
        text.push_str(&value.to_lowercase());
    }
    text
}

fn symbol_text_match_score(n: &Node, query_lower: &str, terms: &[String]) -> usize {
    let name = n.id.name.to_lowercase();
    let signature = n.signature.to_lowercase();
    if !query_lower.is_empty() {
        if name == query_lower {
            return 10_000;
        }
        if name.contains(query_lower) {
            return 8_000;
        }
        if signature.contains(query_lower) {
            return 6_000;
        }
        let search_text = node_search_text_lower(n);
        if search_text.contains(query_lower) {
            return 7_000;
        }
    }

    if terms.is_empty() {
        return 0;
    }

    let search_text = node_search_text_lower(n);
    let matched_term_score: usize = terms
        .iter()
        .filter(|term| search_text.contains(term.as_str()))
        .map(|term| {
            if term.contains('_') || term.len() >= 12 {
                2_000
            } else {
                100
            }
        })
        .sum();
    if matched_term_score == 0 {
        return 0;
    }

    let name_hits = terms
        .iter()
        .filter(|term| name.contains(term.as_str()))
        .count();
    let signature_hits = terms
        .iter()
        .filter(|term| signature.contains(term.as_str()))
        .count();

    matched_term_score + name_hits * 250 + signature_hits * 10
}

fn node_matches_text_query(n: &Node, query_lower: &str, terms: &[String]) -> bool {
    symbol_text_match_score(n, query_lower, terms) > 0
}

fn sort_symbol_text_matches(
    matches: &mut [&Node],
    query_lower: &str,
    terms: &[String],
    index: &GraphIndex,
) {
    let scores: HashMap<*const Node, usize> = matches
        .iter()
        .map(|node| {
            (
                *node as *const Node,
                symbol_text_match_score(node, query_lower, terms),
            )
        })
        .collect();

    matches.sort_by(|a, b| {
        let score_cmp = scores
            .get(&(*b as *const Node))
            .copied()
            .unwrap_or_default()
            .cmp(
                &scores
                    .get(&(*a as *const Node))
                    .copied()
                    .unwrap_or_default(),
            );
        if score_cmp != std::cmp::Ordering::Equal {
            return score_cmp;
        }

        let mut pair = [*a, *b];
        ranking::sort_symbol_matches(&mut pair, query_lower, index);
        if std::ptr::eq(pair[0], *a) {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::index::GraphIndex;
    use crate::graph::{ExtractionSource, NodeId};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    fn make_node(name: &str, kind: NodeKind, file: &str) -> Node {
        Node {
            id: NodeId {
                kind,
                name: name.to_string(),
                file: PathBuf::from(file),
                root: "local".to_string(),
            },
            language: "rust".to_string(),
            signature: format!("fn {}", name),
            line_start: 0,
            line_end: 10,
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_graph_state(nodes: Vec<Node>) -> GraphState {
        let index = GraphIndex::new();
        GraphState::new(nodes, vec![], index, None, std::collections::HashSet::new())
    }

    fn make_graph_state_with_edges(nodes: Vec<Node>, edges: Vec<crate::graph::Edge>) -> GraphState {
        let mut index = GraphIndex::new();
        index.rebuild_from_edges(&edges);
        for node in &nodes {
            index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
        }
        GraphState::new(nodes, edges, index, None, std::collections::HashSet::new())
    }

    fn make_edge(from: &Node, to: &Node, kind: crate::graph::EdgeKind) -> crate::graph::Edge {
        crate::graph::Edge {
            from: from.id.clone(),
            to: to.id.clone(),
            kind,
            source: ExtractionSource::TreeSitter,
            confidence: crate::graph::Confidence::Detected,
        }
    }

    fn make_search_context<'a>(
        graph_state: &'a GraphState,
        repo_root: &'a Path,
    ) -> SearchContext<'a> {
        SearchContext {
            graph_state,
            embed_index: None,
            repo_root,
            lsp_status: None,
            embed_status: None,
            root_filter: None,
            non_code_slugs: HashSet::new(),
            enrichment_jobs: Vec::new(),
        }
    }

    #[test]
    fn test_search_params_default() {
        let p = SearchParams::default();
        assert!(p.query.is_none());
        assert!(!p.compact);
        assert!(!p.rerank);
    }

    #[tokio::test]
    async fn source_span_reads_compiler_location_without_index_node() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src/main.rs"),
            "fn main() {\n    let value = Thing { field: 1 };\n}\n",
        )
        .unwrap();
        let graph = make_graph_state(vec![]);
        let ctx = make_search_context(&graph, tmp.path());
        let params = SearchParams {
            file: Some("src/main.rs:2:17".into()),
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(result.contains("2 |     let value = Thing { field: 1 };"));
        assert!(result.contains("current filesystem state"));
        assert!(result.contains("**Root:**"));

        let explicit_line = SearchParams {
            file: Some("src/main.rs:1:1".into()),
            line: Some(2),
            ..Default::default()
        };
        assert!(
            search(&explicit_line, &ctx)
                .await
                .contains("2 |     let value")
        );
    }

    #[tokio::test]
    async fn source_span_rejects_ambiguous_suffix_and_lists_sorted_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["a", "b"] {
            std::fs::create_dir(tmp.path().join(dir)).unwrap();
            std::fs::write(tmp.path().join(dir).join("same.rs"), "one\n").unwrap();
        }
        let graph = make_graph_state(vec![]);
        let ctx = make_search_context(&graph, tmp.path());
        let params = SearchParams {
            file: Some("same.rs".into()),
            line: Some(1),
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(result.contains("is ambiguous"));
        assert!(result.find("a/same.rs").unwrap() < result.find("b/same.rs").unwrap());

        for index in 0..25 {
            let dir = tmp.path().join(format!("many-{index:02}"));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("common.rs"), "one\n").unwrap();
        }
        let capped = SearchParams {
            file: Some("common.rs".into()),
            line: Some(1),
            ..Default::default()
        };
        let capped_result = search(&capped, &ctx).await;
        assert!(capped_result.contains("additional matches omitted after 20 candidates"));
        assert_eq!(
            capped_result
                .lines()
                .filter(|line| line.starts_with("- ["))
                .count(),
            MAX_SOURCE_CANDIDATES
        );
    }

    #[tokio::test]
    async fn source_span_honors_specific_and_all_workspace_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("primary");
        let secondary = tmp.path().join("secondary");
        std::fs::create_dir_all(primary.join(".oh")).unwrap();
        std::fs::create_dir_all(&secondary).unwrap();
        std::fs::write(
            primary.join(".oh/config.toml"),
            "[workspace.roots]\nsecondary = \"../secondary\"\n",
        )
        .unwrap();
        std::fs::write(primary.join("same.rs"), "primary\n").unwrap();
        std::fs::write(secondary.join("same.rs"), "secondary\n").unwrap();
        let graph = make_graph_state(vec![]);
        let ctx = make_search_context(&graph, &primary);

        let all = SearchParams {
            file: Some("same.rs".into()),
            line: Some(1),
            root: Some("all".into()),
            ..Default::default()
        };
        let ambiguous = search(&all, &ctx).await;
        assert!(ambiguous.contains("is ambiguous"));
        assert!(ambiguous.contains("[primary] same.rs"), "{ambiguous}");
        assert!(ambiguous.contains("[secondary] same.rs"), "{ambiguous}");
        let selected = SearchParams {
            root: Some("secondary".into()),
            ..all
        };
        let result = search(&selected, &ctx).await;
        assert!(result.contains("1 | secondary"));
        assert!(result.contains("**Root:** `secondary`"));
    }

    #[tokio::test]
    async fn source_span_rejects_traversal_and_range_overflow() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("huge.rs"),
            vec![b'x'; MAX_SOURCE_SPAN_BYTES + 1],
        )
        .unwrap();
        let graph = make_graph_state(vec![]);
        let ctx = make_search_context(&graph, tmp.path());
        let traversal = SearchParams {
            file: Some("../secret".into()),
            line: Some(1),
            ..Default::default()
        };
        let oversized = SearchParams {
            file: Some("anything.rs".into()),
            line: Some(1),
            end_line: Some(MAX_SOURCE_SPAN_LINES + 1),
            ..Default::default()
        };

        assert!(search(&traversal, &ctx).await.contains("parent traversal"));
        assert!(
            search(&oversized, &ctx)
                .await
                .contains("hard maximum is 200")
        );
        let overflow_probe = SearchParams {
            file: Some("anything.rs".into()),
            line: Some(1),
            end_line: Some(u32::MAX),
            ..Default::default()
        };
        assert!(
            search(&overflow_probe, &ctx)
                .await
                .contains("hard maximum is 200")
        );
        let huge_line = SearchParams {
            file: Some("huge.rs".into()),
            line: Some(1),
            ..Default::default()
        };
        assert!(
            search(&huge_line, &ctx)
                .await
                .contains("hard maximum of 65536 bytes")
        );

        let mut matches = Vec::new();
        let mut visited = MAX_SOURCE_PATH_ENTRIES;
        let traversal_error = collect_suffix_matches(
            tmp.path(),
            Path::new("missing.rs"),
            &mut matches,
            &mut visited,
        )
        .unwrap_err();
        assert!(traversal_error.contains("hard traversal maximum of 50000"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn source_span_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), tmp.path().join("escape.rs")).unwrap();
        let graph = make_graph_state(vec![]);
        let ctx = make_search_context(&graph, tmp.path());
        let params = SearchParams {
            file: Some("escape.rs".into()),
            line: Some(1),
            ..Default::default()
        };

        assert!(search(&params, &ctx).await.contains("escapes root"));
    }

    #[tokio::test]
    async fn source_span_reports_directory_binary_and_out_of_range() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("folder")).unwrap();
        std::fs::write(tmp.path().join("binary.bin"), b"GIF89a\0payload").unwrap();
        std::fs::write(
            tmp.path().join("binary-after.rs"),
            b"valid source\n\0binary tail",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("invalid-after.rs"),
            b"valid source\n\xff\xfe",
        )
        .unwrap();
        std::fs::write(tmp.path().join("short.rs"), "one\n").unwrap();
        let graph = make_graph_state(vec![]);
        let ctx = make_search_context(&graph, tmp.path());
        let lookup = |file: &str, line| SearchParams {
            file: Some(file.into()),
            line: Some(line),
            ..Default::default()
        };

        assert!(
            search(&lookup("folder", 1), &ctx)
                .await
                .contains("not a regular file")
        );
        assert!(
            search(&lookup("binary.bin", 1), &ctx)
                .await
                .contains("binary (contains NUL bytes)")
        );
        assert!(
            search(&lookup("binary-after.rs", 1), &ctx)
                .await
                .contains("binary (contains NUL bytes)")
        );
        assert!(
            search(&lookup("invalid-after.rs", 1), &ctx)
                .await
                .contains("not valid UTF-8")
        );
        assert!(
            search(&lookup("short.rs", 2), &ctx)
                .await
                .contains("out of range")
        );
        assert!(
            search(&lookup("missing.rs", 1), &ctx)
                .await
                .contains("was not found")
        );
    }

    #[tokio::test]
    async fn source_span_uses_a_safe_markdown_fence() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("fenced.md"), "```\ncontent\n").unwrap();
        let graph = make_graph_state(vec![]);
        let ctx = make_search_context(&graph, tmp.path());
        let params = SearchParams {
            file: Some("fenced.md".into()),
            line: Some(1),
            end_line: Some(2),
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(result.contains("````text\n     1 | ```\n     2 | content\n````"));
    }

    #[tokio::test]
    async fn test_search_blank_mode_is_flat_search() {
        let nodes = vec![make_node("auth_handler", NodeKind::Function, "src/auth.rs")];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("auth".into()),
            mode: Some("   ".into()),
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(result.contains("## Search: \"auth\""));
        assert!(result.contains("auth_handler"));
        assert!(!result.contains("Unknown mode"));
    }

    #[tokio::test]
    async fn test_search_trims_traversal_mode_at_service_boundary() {
        let caller = make_node("caller", NodeKind::Function, "src/caller.rs");
        let callee = make_node("callee", NodeKind::Function, "src/callee.rs");
        let edge = make_edge(&caller, &callee, crate::graph::EdgeKind::Calls);
        let gs = make_graph_state_with_edges(vec![caller.clone(), callee], vec![edge]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            node: Some(caller.stable_id()),
            mode: Some(" neighbors ".into()),
            compact: true,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(result.contains("## Graph neighbors"));
        assert!(result.contains("callee"));
        assert!(!result.contains("Unknown mode"));
    }

    #[tokio::test]
    async fn test_search_neighbors_displays_custom_edge_label() {
        let quote = make_node(
            "quote.goodhart",
            NodeKind::Other("quote".to_string()),
            ".oh/sources/goodhart.md",
        );
        let claim = make_node(
            "claim.proxy-risk",
            NodeKind::Other("claim".to_string()),
            ".oh/knowledge/proxy-risk.md",
        );
        let edge = make_edge(
            &quote,
            &claim,
            crate::graph::EdgeKind::Other("supports".to_string()),
        );
        let gs = make_graph_state_with_edges(vec![quote.clone(), claim], vec![edge]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            node: Some(quote.stable_id()),
            mode: Some("neighbors".into()),
            edge_types: Some(vec!["supports".to_string()]),
            compact: true,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(
            result.contains("#### Supports (1)"),
            "custom edge label should render: {}",
            result
        );
        assert!(result.contains("claim.proxy-risk"));
    }

    #[tokio::test]
    async fn test_verbose_readiness_uses_persisted_lsp_edges_without_live_status() {
        let caller = make_node("caller", NodeKind::Function, "src/caller.rs");
        let callee = make_node("callee", NodeKind::Function, "src/callee.rs");
        let mut edge = make_edge(&caller, &callee, crate::graph::EdgeKind::Calls);
        edge.source = ExtractionSource::Lsp;
        let gs = make_graph_state_with_edges(vec![caller, callee], vec![edge]);
        let tmp = tempfile::tempdir().expect("temp repo");
        let repo_root = tmp.path().to_path_buf();
        let ledger = crate::server::EnrichmentJobLedger::default();
        let job_id = match ledger
            .begin_job(
                &repo_root,
                crate::server::EnrichmentCapability::CallReferences,
                crate::server::EnrichmentScope::Repo,
                crate::server::EnrichmentTrigger::Explicit,
                None,
            )
            .expect("begin call-reference job")
        {
            crate::server::JobStart::Started(job) => job.job_id,
            crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
        };
        ledger.mark_completed(&repo_root, &job_id, 2, 1);
        let mut ctx = make_search_context(&gs, &repo_root);
        ctx.enrichment_jobs = ledger.recent_jobs(&repo_root, 5);
        let params = SearchParams {
            query: Some("caller".into()),
            verbose: true,
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(
            result.contains("LSP call/reference coverage**: ready"),
            "got: {}",
            result
        );
        assert!(
            result.contains("global dead-code prerequisites**: ready"),
            "got: {}",
            result
        );
    }

    #[tokio::test]
    async fn test_verbose_readiness_does_not_promote_lsp_edges_without_completed_job() {
        let caller = make_node("caller", NodeKind::Function, "src/caller.rs");
        let callee = make_node("callee", NodeKind::Function, "src/callee.rs");
        let mut edge = make_edge(&caller, &callee, crate::graph::EdgeKind::Calls);
        edge.source = ExtractionSource::Lsp;
        let gs = make_graph_state_with_edges(vec![caller, callee], vec![edge]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("caller".into()),
            verbose: true,
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(
            result.contains("LSP call/reference coverage**: unavailable"),
            "got: {}",
            result
        );
        assert!(
            result.contains("global dead-code prerequisites**: unavailable"),
            "got: {}",
            result
        );
    }

    #[tokio::test]
    async fn test_verbose_readiness_reports_scoped_lsp_coverage_as_partial() {
        let caller = make_node("caller", NodeKind::Function, "src/caller.rs");
        let callee = make_node("callee", NodeKind::Function, "src/callee.rs");
        let mut edge = make_edge(&caller, &callee, crate::graph::EdgeKind::Calls);
        edge.source = ExtractionSource::Lsp;
        let gs = make_graph_state_with_edges(vec![caller, callee], vec![edge]);
        let tmp = tempfile::tempdir().expect("temp repo");
        let repo_root = tmp.path().to_path_buf();
        let ledger = crate::server::EnrichmentJobLedger::default();
        let job_id = match ledger
            .begin_job(
                &repo_root,
                crate::server::EnrichmentCapability::CallReferences,
                crate::server::EnrichmentScope::ChangedFiles,
                crate::server::EnrichmentTrigger::Explicit,
                None,
            )
            .expect("begin changed-file call-reference job")
        {
            crate::server::JobStart::Started(job) => job.job_id,
            crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
        };
        ledger.mark_completed(&repo_root, &job_id, 2, 1);
        let mut ctx = make_search_context(&gs, &repo_root);
        ctx.enrichment_jobs = ledger.recent_jobs(&repo_root, 5);
        let params = SearchParams {
            query: Some("caller".into()),
            verbose: true,
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(
            result.contains("LSP call/reference coverage**: partial/degraded"),
            "got: {}",
            result
        );
        assert!(
            result.contains("repo-wide coverage is not proven"),
            "got: {}",
            result
        );
        assert!(
            result.contains("global dead-code prerequisites**: partial/degraded"),
            "got: {}",
            result
        );
        assert!(
            result.contains("review-readiness scoped context**: partial/degraded"),
            "got: {}",
            result
        );
    }

    #[tokio::test]
    async fn test_verbose_readiness_reports_failed_scoped_lsp_job_as_degraded() {
        let caller = make_node("caller", NodeKind::Function, "src/caller.rs");
        let gs = make_graph_state_with_edges(vec![caller], vec![]);
        let tmp = tempfile::tempdir().expect("temp repo");
        let repo_root = tmp.path().to_path_buf();
        let ledger = crate::server::EnrichmentJobLedger::default();
        let job_id = match ledger
            .begin_job(
                &repo_root,
                crate::server::EnrichmentCapability::CallReferences,
                crate::server::EnrichmentScope::Root("large-root".to_string()),
                crate::server::EnrichmentTrigger::Explicit,
                Some("large-root".to_string()),
            )
            .expect("begin root-scoped call-reference job")
        {
            crate::server::JobStart::Started(job) => job.job_id,
            crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
        };
        ledger.mark_failed(&repo_root, &job_id, "language server exited");
        let mut ctx = make_search_context(&gs, &repo_root);
        ctx.enrichment_jobs = ledger.recent_jobs(&repo_root, 5);
        let params = SearchParams {
            query: Some("caller".into()),
            verbose: true,
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(
            result.contains("LSP call/reference coverage**: failed"),
            "got: {}",
            result
        );
        assert!(result.contains("language server exited"), "got: {}", result);
        assert!(
            result.contains("review-readiness scoped context**: partial/degraded"),
            "got: {}",
            result
        );
    }

    #[tokio::test]
    async fn test_verbose_readiness_reports_live_degraded_lsp_diagnostic() {
        let caller = make_node("caller", NodeKind::Function, "src/caller.rs");
        let gs = make_graph_state(vec![caller]);
        let tmp = tempfile::tempdir().expect("temp repo");
        let status = crate::server::state::LspEnrichmentStatus::default();
        status.set_degraded(4, "forced no-progress abort after 7 attempted nodes");
        let mut ctx = make_search_context(&gs, tmp.path());
        ctx.lsp_status = Some(&status);
        let params = SearchParams {
            query: Some("caller".into()),
            verbose: true,
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(
            result.contains("LSP call/reference coverage**: partial/degraded"),
            "got: {result}"
        );
        assert!(
            result.contains("degraded after finalization with 4 partial call/reference edges"),
            "got: {result}"
        );
        assert!(
            result.contains("forced no-progress abort after 7 attempted nodes"),
            "got: {result}"
        );
        assert!(!result.contains("LSP call/reference coverage**: ready"));
        assert!(!result.contains("no supported language server detected"));
    }

    #[tokio::test]
    async fn test_verbose_readiness_restores_degraded_lsp_diagnostic_from_job_ledger() {
        let caller = make_node("caller", NodeKind::Function, "src/caller.rs");
        let callee = make_node("callee", NodeKind::Function, "src/callee.rs");
        let mut edge = make_edge(&caller, &callee, crate::graph::EdgeKind::Calls);
        edge.source = ExtractionSource::Lsp;
        let gs = make_graph_state_with_edges(vec![caller, callee], vec![edge]);
        let tmp = tempfile::tempdir().expect("temp repo");
        let ledger = crate::server::EnrichmentJobLedger::default();
        let job_id = match ledger
            .begin_job(
                tmp.path(),
                crate::server::EnrichmentCapability::CallReferences,
                crate::server::EnrichmentScope::Repo,
                crate::server::EnrichmentTrigger::Explicit,
                None,
            )
            .expect("begin call-reference job")
        {
            crate::server::JobStart::Started(job) => job.job_id,
            crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
        };
        ledger.mark_degraded(
            tmp.path(),
            &job_id,
            2,
            1,
            "forced no-progress abort after 7 attempted nodes",
        );
        let mut ctx = make_search_context(&gs, tmp.path());
        ctx.enrichment_jobs = ledger.recent_jobs(tmp.path(), 5);
        let params = SearchParams {
            query: Some("caller".into()),
            verbose: true,
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(
            result.contains("LSP call/reference coverage**: partial/degraded"),
            "got: {result}"
        );
        assert!(
            result.contains("forced no-progress abort after 7 attempted nodes"),
            "got: {result}"
        );
        assert!(!result.contains("LSP call/reference coverage**: ready"));
    }

    #[tokio::test]
    async fn test_newer_repo_success_supersedes_historical_degraded_job() {
        let caller = make_node("caller", NodeKind::Function, "src/caller.rs");
        let callee = make_node("callee", NodeKind::Function, "src/callee.rs");
        let mut edge = make_edge(&caller, &callee, crate::graph::EdgeKind::Calls);
        edge.source = ExtractionSource::Lsp;
        let gs = make_graph_state_with_edges(vec![caller, callee], vec![edge]);
        let tmp = tempfile::tempdir().expect("temp repo");
        let ledger = crate::server::EnrichmentJobLedger::default();
        let degraded = match ledger
            .begin_job(
                tmp.path(),
                crate::server::EnrichmentCapability::CallReferences,
                crate::server::EnrichmentScope::ChangedFiles,
                crate::server::EnrichmentTrigger::BackgroundScan,
                None,
            )
            .expect("begin degraded job")
        {
            crate::server::JobStart::Started(job) => job.job_id,
            crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
        };
        ledger.mark_degraded(tmp.path(), &degraded, 2, 1, "historical abort");
        let completed = match ledger
            .begin_job(
                tmp.path(),
                crate::server::EnrichmentCapability::CallReferences,
                crate::server::EnrichmentScope::Repo,
                crate::server::EnrichmentTrigger::Explicit,
                None,
            )
            .expect("begin successful repo job")
        {
            crate::server::JobStart::Started(job) => job.job_id,
            crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
        };
        ledger.mark_completed(tmp.path(), &completed, 2, 1);
        let mut ctx = make_search_context(&gs, tmp.path());
        ctx.enrichment_jobs = ledger.recent_jobs(tmp.path(), 5);
        let result = search(
            &SearchParams {
                query: Some("caller".into()),
                verbose: true,
                include_artifacts: false,
                include_markdown: false,
                ..Default::default()
            },
            &ctx,
        )
        .await;

        assert!(
            result.contains("LSP call/reference coverage**: ready"),
            "got: {result}"
        );
        assert!(
            !result.contains("LSP call/reference coverage**: partial/degraded"),
            "got: {result}"
        );
    }

    #[tokio::test]
    async fn test_newer_zero_edge_repo_success_supersedes_historical_degraded_job() {
        let caller = make_node("caller", NodeKind::Function, "src/caller.rs");
        let gs = make_graph_state(vec![caller]);
        let tmp = tempfile::tempdir().expect("temp repo");
        let ledger = crate::server::EnrichmentJobLedger::default();
        let degraded = match ledger
            .begin_job(
                tmp.path(),
                crate::server::EnrichmentCapability::CallReferences,
                crate::server::EnrichmentScope::ChangedFiles,
                crate::server::EnrichmentTrigger::BackgroundScan,
                None,
            )
            .expect("begin degraded job")
        {
            crate::server::JobStart::Started(job) => job.job_id,
            crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
        };
        ledger.mark_degraded(tmp.path(), &degraded, 1, 0, "historical abort");
        let completed = match ledger
            .begin_job(
                tmp.path(),
                crate::server::EnrichmentCapability::CallReferences,
                crate::server::EnrichmentScope::Repo,
                crate::server::EnrichmentTrigger::Explicit,
                None,
            )
            .expect("begin successful zero-edge repo job")
        {
            crate::server::JobStart::Started(job) => job.job_id,
            crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
        };
        ledger.mark_completed(tmp.path(), &completed, 1, 0);
        let mut ctx = make_search_context(&gs, tmp.path());
        ctx.enrichment_jobs = ledger.recent_jobs(tmp.path(), 5);

        let result = search(
            &SearchParams {
                query: Some("caller".into()),
                verbose: true,
                include_artifacts: false,
                include_markdown: false,
                ..Default::default()
            },
            &ctx,
        )
        .await;

        assert!(
            result.contains("LSP completed with 0 persisted call/reference edges"),
            "got: {result}"
        );
        assert!(
            !result.contains("degraded after finalization"),
            "got: {result}"
        );
    }

    #[tokio::test]
    async fn test_verbose_readiness_uses_persisted_lsp_edges_when_live_status_is_only_server_found()
    {
        let caller = make_node("caller", NodeKind::Function, "src/caller.rs");
        let callee = make_node("callee", NodeKind::Function, "src/callee.rs");
        let mut edge = make_edge(&caller, &callee, crate::graph::EdgeKind::Calls);
        edge.source = ExtractionSource::Lsp;
        let gs = make_graph_state_with_edges(vec![caller, callee], vec![edge]);
        let tmp = tempfile::tempdir().expect("temp repo");
        let repo_root = tmp.path().to_path_buf();
        let ledger = crate::server::EnrichmentJobLedger::default();
        let job_id = match ledger
            .begin_job(
                &repo_root,
                crate::server::EnrichmentCapability::CallReferences,
                crate::server::EnrichmentScope::Repo,
                crate::server::EnrichmentTrigger::Explicit,
                None,
            )
            .expect("begin call-reference job")
        {
            crate::server::JobStart::Started(job) => job.job_id,
            crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
        };
        ledger.mark_completed(&repo_root, &job_id, 2, 1);
        let live_status = LspEnrichmentStatus::default();
        live_status.set_server_found();
        let mut ctx = make_search_context(&gs, &repo_root);
        ctx.lsp_status = Some(&live_status);
        ctx.enrichment_jobs = ledger.recent_jobs(&repo_root, 5);

        let params = SearchParams {
            query: Some("caller".into()),
            verbose: true,
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(
            result.contains("LSP call/reference coverage**: ready"),
            "got: {}",
            result
        );
        assert!(
            result.contains("global dead-code prerequisites**: ready"),
            "got: {}",
            result
        );
    }

    #[tokio::test]
    async fn test_verbose_readiness_uses_completed_job_beyond_recent_display_limit() {
        let caller = make_node("caller", NodeKind::Function, "src/caller.rs");
        let callee = make_node("callee", NodeKind::Function, "src/callee.rs");
        let mut edge = make_edge(&caller, &callee, crate::graph::EdgeKind::Calls);
        edge.source = ExtractionSource::Lsp;
        let gs = make_graph_state_with_edges(vec![caller, callee], vec![edge]);
        let tmp = tempfile::tempdir().expect("temp repo");
        let repo_root = tmp.path().to_path_buf();
        let ledger = crate::server::EnrichmentJobLedger::default();
        let completed_call_refs = match ledger
            .begin_job(
                &repo_root,
                crate::server::EnrichmentCapability::CallReferences,
                crate::server::EnrichmentScope::Repo,
                crate::server::EnrichmentTrigger::Explicit,
                None,
            )
            .expect("begin call-reference job")
        {
            crate::server::JobStart::Started(job) => job.job_id,
            crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
        };
        ledger.mark_completed(&repo_root, &completed_call_refs, 2, 1);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        for index in 0..5 {
            let job_id = match ledger
                .begin_job(
                    &repo_root,
                    crate::server::EnrichmentCapability::Embeddings,
                    crate::server::EnrichmentScope::Root(format!("root-{index}")),
                    crate::server::EnrichmentTrigger::Explicit,
                    None,
                )
                .expect("begin unrelated job")
            {
                crate::server::JobStart::Started(job) => job.job_id,
                crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
            };
            ledger.mark_completed(&repo_root, &job_id, 1, 1);
        }
        assert!(
            !ledger
                .recent_jobs(&repo_root, 5)
                .iter()
                .any(|job| job.job_id == completed_call_refs),
            "test setup should push call-reference proof outside the recent display window"
        );
        let mut ctx = make_search_context(&gs, &repo_root);
        ctx.enrichment_jobs = ledger.all_jobs(&repo_root);

        let params = SearchParams {
            query: Some("caller".into()),
            verbose: true,
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(
            result.contains("LSP call/reference coverage**: ready"),
            "got: {}",
            result
        );
        assert!(
            result.contains("global dead-code prerequisites**: ready"),
            "got: {}",
            result
        );
    }

    #[tokio::test]
    async fn test_verbose_readiness_does_not_promote_superseded_completed_job() {
        let caller = make_node("caller", NodeKind::Function, "src/caller.rs");
        let callee = make_node("callee", NodeKind::Function, "src/callee.rs");
        let mut edge = make_edge(&caller, &callee, crate::graph::EdgeKind::Calls);
        edge.source = ExtractionSource::Lsp;
        let gs = make_graph_state_with_edges(vec![caller, callee], vec![edge]);
        let tmp = tempfile::tempdir().expect("temp repo");
        let repo_root = tmp.path().to_path_buf();
        let ledger = crate::server::EnrichmentJobLedger::default();
        let job_id = match ledger
            .begin_job(
                &repo_root,
                crate::server::EnrichmentCapability::CallReferences,
                crate::server::EnrichmentScope::Repo,
                crate::server::EnrichmentTrigger::Explicit,
                None,
            )
            .expect("begin call-reference job")
        {
            crate::server::JobStart::Started(job) => job.job_id,
            crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
        };
        ledger.mark_completed(&repo_root, &job_id, 2, 1);
        let mut ctx = make_search_context(&gs, &repo_root);
        ctx.enrichment_jobs = ledger.recent_jobs(&repo_root, 5);
        ctx.enrichment_jobs[0].state = EnrichmentJobState::Superseded;

        let params = SearchParams {
            query: Some("caller".into()),
            verbose: true,
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(
            result.contains("LSP call/reference coverage**: unavailable"),
            "got: {}",
            result
        );
        assert!(
            result.contains("global dead-code prerequisites**: unavailable"),
            "got: {}",
            result
        );
    }

    #[tokio::test]
    async fn test_verbose_readiness_prefers_live_ready_status_over_stale_job_metadata() {
        let caller = make_node("caller", NodeKind::Function, "src/caller.rs");
        let callee = make_node("callee", NodeKind::Function, "src/callee.rs");
        let gs = make_graph_state_with_edges(vec![caller, callee], vec![]);
        let tmp = tempfile::tempdir().expect("temp repo");
        let repo_root = tmp.path().to_path_buf();
        let ledger = crate::server::EnrichmentJobLedger::default();
        let completed_job = match ledger
            .begin_job(
                &repo_root,
                crate::server::EnrichmentCapability::CallReferences,
                crate::server::EnrichmentScope::Repo,
                crate::server::EnrichmentTrigger::Explicit,
                None,
            )
            .expect("begin completed call-reference job")
        {
            crate::server::JobStart::Started(job) => job.job_id,
            crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
        };
        ledger.mark_completed(&repo_root, &completed_job, 2, 7);
        let _replacement_job = match ledger
            .begin_job(
                &repo_root,
                crate::server::EnrichmentCapability::CallReferences,
                crate::server::EnrichmentScope::Repo,
                crate::server::EnrichmentTrigger::Explicit,
                None,
            )
            .expect("begin replacement call-reference job")
        {
            crate::server::JobStart::Started(job) => job.job_id,
            crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
        };
        let live_status = LspEnrichmentStatus::default();
        live_status.set_complete(7);
        let mut ctx = make_search_context(&gs, &repo_root);
        ctx.lsp_status = Some(&live_status);
        ctx.enrichment_jobs = ledger.all_jobs(&repo_root);

        let params = SearchParams {
            query: Some("caller".into()),
            verbose: true,
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(
            result.contains("LSP call/reference coverage**: ready"),
            "got: {}",
            result
        );
        assert!(
            result.contains("global dead-code prerequisites**: ready"),
            "got: {}",
            result
        );
    }

    #[tokio::test]
    async fn test_search_preserves_invalid_non_blank_mode_failure() {
        let node = make_node("caller", NodeKind::Function, "src/caller.rs");
        let gs = make_graph_state_with_edges(vec![node.clone()], vec![]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            node: Some(node.stable_id()),
            mode: Some(" bogus ".into()),
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(result.contains("Unknown mode: \"bogus\""));
    }

    #[test]
    fn test_node_passes_root_filter_all() {
        assert!(node_passes_root_filter("any", &None, &HashSet::new()));
    }
    #[test]
    fn test_node_passes_root_filter_match() {
        assert!(node_passes_root_filter(
            "my-root",
            &Some("my-root".into()),
            &HashSet::new()
        ));
    }
    #[test]
    fn test_node_passes_root_filter_external() {
        assert!(node_passes_root_filter(
            "external",
            &Some("my-root".into()),
            &HashSet::new()
        ));
    }
    #[test]
    fn test_node_passes_root_filter_reject() {
        assert!(!node_passes_root_filter(
            "other",
            &Some("my-root".into()),
            &HashSet::new()
        ));
    }

    // ── flat_code_symbol_search tests ──────────────────────────────────

    /// Without embed index, flat search falls back to name/signature matching.
    #[tokio::test]
    async fn test_scorer_panic_becomes_content_safe_actionable_diagnostic() {
        let redacted_before = REDACTED_SCORER_PANICS.load(std::sync::atomic::Ordering::Relaxed);
        let repository_content = "secret-query src/private/customer.rs";
        let diagnostic = match isolate_embedding_scorer(
            async move {
                tokio::task::yield_now().await;
                panic!("{repository_content}");
                #[allow(unreachable_code)]
                Ok(SearchOutcome::NotReady)
            },
            SearchMode::Semantic,
        )
        .await
        {
            Err(diagnostic) => diagnostic,
            Ok(_) => panic!("panicking scorer must degrade"),
        };

        let rendered = diagnostic.render();
        assert!(rendered.contains("component=embedding_scorer"));
        assert!(rendered.contains(&format!("model={EMBEDDING_MODEL_NAME}")));
        assert!(rendered.contains("index=attached"));
        assert!(rendered.contains("mode=semantic"));
        assert!(rendered.contains("failure=task_panic"));
        assert!(rendered.contains("bounded lexical/graph results"));
        assert!(!rendered.contains("secret-query"));
        assert!(!rendered.contains("customer.rs"));
        assert!(
            REDACTED_SCORER_PANICS.load(std::sync::atomic::Ordering::Relaxed) > redacted_before,
            "the panic hook must redact the payload before Tokio converts the panic to JoinError"
        );
    }

    #[tokio::test]
    async fn test_scorer_error_does_not_echo_repository_content() {
        let diagnostic = match isolate_embedding_scorer(
            async {
                Err(anyhow::anyhow!(
                    "failed near src/private/customer.rs for secret-query"
                ))
            },
            SearchMode::Hybrid,
        )
        .await
        {
            Err(diagnostic) => diagnostic,
            Ok(_) => panic!("failing scorer must degrade"),
        };

        let rendered = diagnostic.render();
        assert!(rendered.contains("failure=search_error"));
        assert!(!rendered.contains("secret-query"));
        assert!(!rendered.contains("customer.rs"));
    }

    #[tokio::test]
    async fn test_mapped_graph_fallback_remains_bounded_after_scorer_panic() {
        let nodes = (0..5)
            .map(|index| {
                make_node(
                    &format!("auth_handler_{index}"),
                    NodeKind::Function,
                    &format!("src/auth_{index}.rs"),
                )
            })
            .collect();
        let gs = make_graph_state(nodes);
        let stable_ids_before: Vec<String> = gs.nodes.iter().map(Node::stable_id).collect();

        let repo_root = PathBuf::from("/tmp/live-worktree-map");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("auth".into()),
            limit: Some(2),
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };
        let search = with_test_embedding_scorer_panic(
            "auth src/private/customer.rs".to_string(),
            flat_code_symbol_search_with_diagnostics(
                "auth",
                SearchMode::Hybrid,
                2,
                &params,
                &gs,
                &ctx,
                false,
                false,
            ),
        )
        .await;

        let diagnostic = search
            .scorer_diagnostic
            .expect("scorer panic should be delivered with fallback results")
            .render();
        assert!(diagnostic.contains("failure=task_panic"));
        assert!(!diagnostic.contains("auth"));
        assert!(!diagnostic.contains("customer.rs"));

        assert_eq!(
            search.matches.len(),
            2,
            "fallback must respect the requested limit"
        );
        assert_eq!(
            gs.nodes.iter().map(Node::stable_id).collect::<Vec<_>>(),
            stable_ids_before,
            "scorer failure must not invalidate the mapped graph"
        );
    }

    #[tokio::test]
    async fn test_flat_search_fallback_name_matching() {
        let nodes = vec![
            make_node("auth_handler", NodeKind::Function, "src/auth.rs"),
            make_node("db_connect", NodeKind::Function, "src/db.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("auth".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "auth",
            SearchMode::Hybrid,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "auth_handler");
    }

    /// Fallback matches against signature too.
    #[tokio::test]
    async fn test_flat_search_fallback_signature_matching() {
        let mut node = make_node("process", NodeKind::Function, "src/proc.rs");
        node.signature = "fn process(auth_token: &str)".to_string();
        let gs = make_graph_state(vec![node]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("auth_token".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "auth_token",
            SearchMode::Hybrid,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "process");
    }

    #[tokio::test]
    async fn test_flat_search_fallback_body_matching_for_parser_registration() {
        let mut node = make_node("PYTHON_CONFIG", NodeKind::Const, "src/extract/configs.rs");
        node.body = r#"pub static PYTHON_CONFIG: LangConfig = LangConfig {
    language_fn: || tree_sitter_python::LANGUAGE.into(),
    language_name: "python",
};"#
        .to_string();
        let gs = make_graph_state(vec![node]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("where is tree_sitter_python::LANGUAGE registered".into()),
            file: Some("src/extract".into()),
            language: Some("rust".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "where is tree_sitter_python::LANGUAGE registered",
            SearchMode::Keyword,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "PYTHON_CONFIG");
    }

    #[tokio::test]
    async fn test_flat_search_fallback_compound_query_matches_config_symbol() {
        let mut lang_config = make_node("LangConfig", NodeKind::Struct, "src/extract/generic.rs");
        lang_config.body = "pub struct LangConfig { pub language_fn: fn() -> tree_sitter::Language, pub extensions: &'static [&'static str] }".to_string();
        let unrelated = make_node("ExtractorRegistry", NodeKind::Struct, "src/extract/mod.rs");
        let gs = make_graph_state(vec![unrelated, lang_config]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("LangConfig language parser tree_sitter extractor suffixes".into()),
            file: Some("src/extract".into()),
            language: Some("rust".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "LangConfig language parser tree_sitter extractor suffixes",
            SearchMode::Keyword,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;

        assert!(!results.is_empty());
        assert_eq!(results[0].id.name, "LangConfig");
    }

    /// Kind filter works with fallback path.
    #[tokio::test]
    async fn test_flat_search_fallback_kind_filter() {
        let nodes = vec![
            make_node("Config", NodeKind::Struct, "src/config.rs"),
            make_node("config_init", NodeKind::Function, "src/config.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("config".into()),
            kind: Some("struct".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "config",
            SearchMode::Hybrid,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "Config");
    }

    /// Empty query with kind=framework returns framework nodes (#601).
    #[tokio::test]
    async fn test_flat_search_empty_query_kind_framework() {
        let func_node = make_node("main", NodeKind::Function, "src/main.rs");
        let mut fw_node = make_node(
            "tokio",
            NodeKind::Other("framework".to_string()),
            "frameworks/tokio",
        );
        fw_node.language = String::new();
        fw_node.signature = "framework tokio".to_string();
        let gs = make_graph_state(vec![func_node, fw_node]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            kind: Some("framework".into()),
            ..Default::default()
        };

        let results =
            flat_code_symbol_search("", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false)
                .await;

        assert_eq!(
            results.len(),
            1,
            "Expected 1 framework node, got {}",
            results.len()
        );
        assert_eq!(results[0].id.name, "tokio");
    }

    /// Language filter works with fallback path.
    #[tokio::test]
    async fn test_flat_search_fallback_language_filter() {
        let mut py_node = make_node("handler", NodeKind::Function, "src/handler.py");
        py_node.language = "python".to_string();
        let rs_node = make_node("handler", NodeKind::Function, "src/handler.rs");
        let gs = make_graph_state(vec![py_node, rs_node]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("handler".into()),
            language: Some("python".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "handler",
            SearchMode::Hybrid,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].language, "python");
    }

    /// File filter works with fallback path.
    #[tokio::test]
    async fn test_flat_search_fallback_file_filter() {
        let nodes = vec![
            make_node("parse", NodeKind::Function, "src/parser.rs"),
            make_node("parse", NodeKind::Function, "src/config.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("parse".into()),
            file: Some("parser".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "parse",
            SearchMode::Hybrid,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;

        assert_eq!(results.len(), 1);
        assert!(results[0].id.file.to_string_lossy().contains("parser"));
    }

    /// sort_by=complexity works with fallback path.
    #[tokio::test]
    async fn test_flat_search_sort_by_complexity() {
        let mut low = make_node("simple", NodeKind::Function, "a.rs");
        low.metadata.insert("cyclomatic".into(), "2".into());
        let mut high = make_node("complex", NodeKind::Function, "b.rs");
        high.metadata.insert("cyclomatic".into(), "15".into());
        let gs = make_graph_state(vec![low, high]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            kind: Some("function".into()),
            sort_by: Some("complexity".into()),
            ..Default::default()
        };

        let results =
            flat_code_symbol_search("", SearchMode::Hybrid, 10, &params, &gs, &ctx, true, false)
                .await;

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id.name, "complex");
        assert_eq!(results[1].id.name, "simple");
    }

    /// sort_by=importance works with fallback path.
    #[tokio::test]
    async fn test_flat_search_sort_by_importance() {
        let mut low = make_node("leaf", NodeKind::Function, "a.rs");
        low.metadata.insert("importance".into(), "0.01".into());
        let mut high = make_node("hub", NodeKind::Function, "b.rs");
        high.metadata.insert("importance".into(), "0.95".into());
        let gs = make_graph_state(vec![low, high]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("".into()),
            kind: Some("function".into()),
            sort_by: Some("importance".into()),
            ..Default::default()
        };

        let results =
            flat_code_symbol_search("", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, true)
                .await;

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id.name, "hub");
    }

    /// search_mode is parsed correctly for all variants.
    #[test]
    fn test_search_mode_parsing_coverage() {
        assert!(matches!(parse_search_mode(None), SearchMode::Hybrid));
        assert!(matches!(
            parse_search_mode(Some("hybrid")),
            SearchMode::Hybrid
        ));
        assert!(matches!(
            parse_search_mode(Some("keyword")),
            SearchMode::Keyword
        ));
        assert!(matches!(
            parse_search_mode(Some("semantic")),
            SearchMode::Semantic
        ));
        assert!(matches!(
            parse_search_mode(Some("SEMANTIC")),
            SearchMode::Semantic
        ));
        assert!(matches!(
            parse_search_mode(Some("unknown")),
            SearchMode::Hybrid
        ));
    }

    /// Empty query with no filters returns empty results (via the search function).
    #[tokio::test]
    async fn test_flat_search_empty_query_no_filters() {
        let gs = make_graph_state(vec![make_node("foo", NodeKind::Function, "a.rs")]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        let result = search(&params, &ctx).await;
        assert!(
            result.contains("Empty query"),
            "Should reject empty query without filters"
        );
    }

    /// Verify full search function respects search_mode parameter in output
    /// (no error, produces results via fallback when embed is absent).
    #[tokio::test]
    async fn test_flat_search_with_search_mode_no_embed() {
        let nodes = vec![make_node("auth_handler", NodeKind::Function, "src/auth.rs")];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("auth".into()),
            search_mode: Some("semantic".into()),
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;
        assert!(
            result.contains("auth_handler"),
            "Fallback should find by name even with search_mode=semantic"
        );
        assert!(
            result.contains("Code symbols"),
            "Should have code symbols section"
        );
    }

    /// min_complexity filter works with the new code path.
    #[tokio::test]
    async fn test_flat_search_min_complexity_filter() {
        let mut simple = make_node("simple", NodeKind::Function, "a.rs");
        simple.metadata.insert("cyclomatic".into(), "2".into());
        let mut complex = make_node("complex", NodeKind::Function, "b.rs");
        complex.metadata.insert("cyclomatic".into(), "10".into());
        let gs = make_graph_state(vec![simple, complex]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            min_complexity: Some(5),
            kind: Some("function".into()),
            ..Default::default()
        };

        let results =
            flat_code_symbol_search("", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false)
                .await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "complex");
    }

    // ── Adversarial tests (seeded from dissent) ──────────────────────

    /// Dissent #1: Multiple filters stacked -- kind + language + file should
    /// all compose correctly in fallback path.
    #[tokio::test]
    async fn test_flat_search_stacked_filters() {
        let mut target = make_node("handler", NodeKind::Function, "src/api/handler.rs");
        target.language = "rust".to_string();
        let mut wrong_kind = make_node("handler", NodeKind::Struct, "src/api/handler.rs");
        wrong_kind.language = "rust".to_string();
        let mut wrong_lang = make_node("handler", NodeKind::Function, "src/api/handler.py");
        wrong_lang.language = "python".to_string();
        let wrong_file = make_node("handler", NodeKind::Function, "src/db/handler.rs");
        let gs = make_graph_state(vec![target, wrong_kind, wrong_lang, wrong_file]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("handler".into()),
            kind: Some("function".into()),
            language: Some("rust".into()),
            file: Some("api".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "handler",
            SearchMode::Hybrid,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;

        assert_eq!(
            results.len(),
            1,
            "Only one node should pass all three filters"
        );
        assert_eq!(results[0].id.name, "handler");
        assert!(results[0].id.file.to_string_lossy().contains("api"));
    }

    /// Dissent #2: Limit respected when more results available.
    #[tokio::test]
    async fn test_flat_search_limit_respected() {
        let nodes: Vec<Node> = (0..20)
            .map(|i| make_node(&format!("fn_{}", i), NodeKind::Function, "src/lib.rs"))
            .collect();
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("fn".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "fn",
            SearchMode::Hybrid,
            5,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;

        assert_eq!(
            results.len(),
            5,
            "Should respect limit of 5 even with 20 matches"
        );
    }

    /// Dissent #3: Root filter rejects non-matching roots in fallback.
    #[tokio::test]
    async fn test_flat_search_root_filter_fallback() {
        let mut local = make_node("handler", NodeKind::Function, "src/handler.rs");
        local.id.root = "my-project".to_string();
        let mut other = make_node("handler", NodeKind::Function, "src/handler.rs");
        other.id.root = "other-project".to_string();
        let gs = make_graph_state(vec![local, other]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = SearchContext {
            graph_state: &gs,
            embed_index: None,
            repo_root: &repo_root,
            lsp_status: None,
            embed_status: None,
            root_filter: Some("my-project".into()),
            non_code_slugs: HashSet::new(),
            enrichment_jobs: Vec::new(),
        };
        let params = SearchParams {
            query: Some("handler".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "handler",
            SearchMode::Hybrid,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;

        assert_eq!(
            results.len(),
            1,
            "Should only return nodes from matching root"
        );
        assert_eq!(results[0].id.root, "my-project");
    }

    /// Dissent #3: synthetic filter works correctly.
    #[tokio::test]
    async fn test_flat_search_synthetic_filter() {
        let mut synth = make_node("CONSTANT", NodeKind::Const, "src/lib.rs");
        synth.metadata.insert("synthetic".into(), "true".into());
        let real = make_node("real_fn", NodeKind::Function, "src/lib.rs");
        let gs = make_graph_state(vec![synth, real]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // Only synthetic
        let params = SearchParams {
            kind: Some("const".into()),
            synthetic: Some(true),
            ..Default::default()
        };
        let results =
            flat_code_symbol_search("", SearchMode::Hybrid, 10, &params, &gs, &ctx, false, false)
                .await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "CONSTANT");

        // Only non-synthetic
        let params2 = SearchParams {
            synthetic: Some(false),
            kind: Some("function".into()),
            ..Default::default()
        };
        let results2 = flat_code_symbol_search(
            "",
            SearchMode::Hybrid,
            10,
            &params2,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;
        assert_eq!(results2.len(), 1);
        assert_eq!(results2[0].id.name, "real_fn");
    }

    // ── Adversarial: rerank parameter ──────────────────────────────────

    /// Rerank=true with only one match: the reranking block requires
    /// `matches.len() > 1`, so with a single match the reranker is not
    /// invoked, keeping this test hermetic (no model download in CI).
    /// This validates the over-fetch logic and parameter plumbing.
    #[tokio::test]
    async fn test_flat_search_rerank_true_no_embed() {
        let nodes = vec![make_node("auth_handler", NodeKind::Function, "src/auth.rs")];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("auth".into()),
            rerank: true,
            ..Default::default()
        };

        // Without embed index, falls back to name matching.
        // Single match means reranking block is skipped (len() > 1 guard),
        // so no model loading occurs.
        let results = flat_code_symbol_search(
            "auth",
            SearchMode::Hybrid,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;
        assert!(
            !results.is_empty(),
            "Rerank=true should not prevent results from appearing"
        );
    }

    /// Rerank=false should not trigger any reranking code path.
    #[tokio::test]
    async fn test_flat_search_rerank_false_default() {
        let nodes = vec![make_node("foo", NodeKind::Function, "src/lib.rs")];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("foo".into()),
            rerank: false,
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "foo",
            SearchMode::Hybrid,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "foo");
    }

    // ── Empty query guard tests (#213) ──────────────────────────────

    /// Empty query with file filter should be allowed (not rejected as "Empty query").
    #[tokio::test]
    async fn test_search_empty_query_with_file_filter() {
        let nodes = vec![
            make_node("parse", NodeKind::Function, "src/parser.rs"),
            make_node("connect", NodeKind::Function, "src/db.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            file: Some("parser".into()),
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;
        assert!(
            !result.contains("Empty query"),
            "File filter should bypass empty query guard"
        );
        assert!(
            result.contains("parse"),
            "Should find symbols in the filtered file"
        );
    }

    /// Empty query with synthetic filter should be allowed.
    #[tokio::test]
    async fn test_search_empty_query_with_synthetic_filter() {
        let mut synth = make_node("MAGIC", NodeKind::Const, "src/lib.rs");
        synth.metadata.insert("synthetic".into(), "true".into());
        let real = make_node("real_fn", NodeKind::Function, "src/lib.rs");
        let gs = make_graph_state(vec![synth, real]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            synthetic: Some(false),
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;
        assert!(
            !result.contains("Empty query"),
            "Synthetic filter should bypass empty query guard"
        );
        assert!(
            result.contains("real_fn"),
            "Should include non-synthetic symbol when synthetic=false"
        );
        assert!(
            !result.contains("MAGIC"),
            "Should exclude synthetic symbol when synthetic=false"
        );
    }

    // ── resolve_entry_points_by_name tests (#290) ─────────────────────

    /// Name matching finds a struct by exact name.
    #[test]
    fn test_resolve_entry_points_by_name_exact_match() {
        let nodes = vec![
            make_node("SearchParams", NodeKind::Struct, "src/service.rs"),
            make_node("search_handler", NodeKind::Function, "src/handler.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            kind: Some("struct".into()),
            ..Default::default()
        };

        let results = resolve_entry_points_by_name("SearchParams", 10, &params, &ctx);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "SearchParams");
    }

    /// Name matching returns empty for unrelated query.
    #[test]
    fn test_resolve_entry_points_by_name_no_match() {
        let nodes = vec![make_node("Config", NodeKind::Struct, "src/config.rs")];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        let results = resolve_entry_points_by_name("nonexistent", 10, &params, &ctx);
        assert!(results.is_empty());
    }

    /// Kind filter is applied during name matching.
    #[test]
    fn test_resolve_entry_points_by_name_kind_filter() {
        let nodes = vec![
            make_node("Config", NodeKind::Struct, "src/config.rs"),
            make_node("config_init", NodeKind::Function, "src/config.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            kind: Some("function".into()),
            ..Default::default()
        };

        let results = resolve_entry_points_by_name("config", 10, &params, &ctx);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id.name, "config_init");
    }

    /// Exact name matches sort before substring matches.
    #[test]
    fn test_resolve_entry_points_by_name_exact_first() {
        let nodes = vec![
            make_node("search_handler", NodeKind::Function, "src/handler.rs"),
            make_node("search", NodeKind::Function, "src/search.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        let results = resolve_entry_points_by_name("search", 10, &params, &ctx);
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].id.name, "search",
            "exact match should come first"
        );
    }

    /// Exact signature match sorts before substring-only matches with limit=1.
    #[test]
    fn test_resolve_entry_points_exact_signature_first() {
        let mut node_a = make_node("foo", NodeKind::Function, "src/a.rs");
        node_a.signature = "fn foo(config: &SearchParams)".to_string();
        let mut node_b = make_node("bar", NodeKind::Function, "src/b.rs");
        node_b.signature = "fn bar()".to_string();
        let nodes = vec![node_a, node_b];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        // Query matches node_a by signature ("fn foo(config: &SearchParams)")
        // but not node_b. With limit=1, node_a must survive.
        let results =
            resolve_entry_points_by_name("fn foo(config: &SearchParams)", 1, &params, &ctx);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].id.name, "foo",
            "exact signature match should be kept with limit=1"
        );
    }

    // ── parse_path_name_query tests ────────────────────────────────────

    #[test]
    fn test_parse_path_name_query_basic() {
        let result = parse_path_name_query("auth/handlers/validate");
        assert_eq!(result, Some(("auth/handlers", "validate")));
    }

    #[test]
    fn test_parse_path_name_query_single_slash() {
        let result = parse_path_name_query("src/validate");
        assert_eq!(result, Some(("src", "validate")));
    }

    #[test]
    fn test_parse_path_name_query_no_slash() {
        let result = parse_path_name_query("validate");
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_path_name_query_trailing_slash() {
        // Empty name part — should return None (degenerate)
        let result = parse_path_name_query("auth/handlers/");
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_path_name_query_leading_slash() {
        // Empty path part — should return None (degenerate)
        let result = parse_path_name_query("/validate");
        assert_eq!(result, None);
    }

    // ── Path/name split in resolve_entry_points_by_name ────────────────

    /// search("auth/handlers/validate") returns only `validate` in auth/handlers files.
    #[test]
    fn test_resolve_entry_points_path_name_basic() {
        let nodes = vec![
            make_node("validate", NodeKind::Function, "src/auth/handlers/mod.rs"),
            make_node("validate", NodeKind::Function, "src/billing/validate.rs"),
            make_node("parse", NodeKind::Function, "src/auth/handlers/parse.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        let results = resolve_entry_points_by_name("auth/handlers/validate", 10, &params, &ctx);
        assert_eq!(results.len(), 1, "Only auth/handlers validate should match");
        assert_eq!(results[0].id.name, "validate");
        assert!(
            results[0]
                .id
                .file
                .to_string_lossy()
                .contains("auth/handlers")
        );
    }

    /// Plain queries (no `/`) still work identically to today.
    #[test]
    fn test_resolve_entry_points_plain_query_unchanged() {
        let nodes = vec![
            make_node("validate", NodeKind::Function, "src/auth/handlers/mod.rs"),
            make_node("validate", NodeKind::Function, "src/billing/validate.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        let results = resolve_entry_points_by_name("validate", 10, &params, &ctx);
        assert_eq!(
            results.len(),
            2,
            "Plain query should return all matching nodes"
        );
    }

    /// Path/name query with no matches returns empty.
    #[test]
    fn test_resolve_entry_points_path_name_no_match() {
        let nodes = vec![make_node(
            "validate",
            NodeKind::Function,
            "src/auth/handlers/mod.rs",
        )];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        // Path doesn't match: billing/validate vs src/auth/handlers/mod.rs
        let results = resolve_entry_points_by_name("billing/validate", 10, &params, &ctx);
        assert!(results.is_empty(), "No match when path doesn't fit");
    }

    // ── Path/name split in flat_code_symbol_search ─────────────────────

    /// flat search with path/name query returns only nodes where both file and name match.
    #[tokio::test]
    async fn test_flat_search_path_name_basic() {
        let nodes = vec![
            make_node("validate", NodeKind::Function, "src/auth/handlers/mod.rs"),
            make_node("validate", NodeKind::Function, "src/billing/validate.rs"),
            make_node("parse", NodeKind::Function, "src/auth/handlers/parse.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("auth/handlers/validate".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "auth/handlers/validate",
            SearchMode::Hybrid,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;

        assert_eq!(results.len(), 1, "Only auth/handlers validate should match");
        assert_eq!(results[0].id.name, "validate");
        assert!(
            results[0]
                .id
                .file
                .to_string_lossy()
                .contains("auth/handlers")
        );
    }

    /// Plain queries (no `/`) remain unchanged in flat search.
    #[tokio::test]
    async fn test_flat_search_plain_query_unchanged() {
        let nodes = vec![
            make_node("validate", NodeKind::Function, "src/auth/handlers/mod.rs"),
            make_node("validate", NodeKind::Function, "src/billing/validate.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("validate".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "validate",
            SearchMode::Hybrid,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;

        assert_eq!(results.len(), 2, "Plain query returns all matches");
    }

    // ── Adversarial path/name tests ────────────────────────────────────

    /// Adversarial: "//foo" — double slash degenerate case.
    /// rfind gives slash_pos=1, path_part="/", name_part="foo". Path part "/" is
    /// non-empty, so it parses. But every file path contains "/" (Unix paths), so
    /// this would match all nodes named "foo". This is an edge case worth asserting.
    #[test]
    fn test_parse_path_name_query_double_slash() {
        // "//foo" → slash_pos=1, path_part="/", name_part="foo"
        // "/" is non-empty, so Some(("/", "foo")) — this is intentional: every file
        // matches "/" as a path fragment. Document this by asserting the parsed result.
        let result = parse_path_name_query("//foo");
        assert_eq!(result, Some(("/", "foo")));
    }

    /// Adversarial: path/name fallback when path part matches nothing.
    /// When path filter eliminates all candidates, result should be empty (no
    /// silent fallback to plain name matching in resolve_entry_points_by_name).
    #[test]
    fn test_resolve_entry_points_path_name_empty_on_path_mismatch() {
        let nodes = vec![
            make_node("validate", NodeKind::Function, "src/billing/validate.rs"),
            make_node("validate", NodeKind::Function, "src/payments/validate.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        // "auth/handlers/validate" → path="auth/handlers", name="validate"
        // Neither node is in auth/handlers → should return empty, not fall back to plain
        let results = resolve_entry_points_by_name("auth/handlers/validate", 10, &params, &ctx);
        assert!(
            results.is_empty(),
            "Must not fall back to plain name matching"
        );
    }

    /// Adversarial: path/name where name part partially matches many nodes.
    /// Verify path discriminates correctly.
    #[tokio::test]
    async fn test_flat_search_path_name_path_discriminates() {
        let nodes = vec![
            make_node("new", NodeKind::Function, "src/auth/handlers.rs"),
            make_node("new", NodeKind::Function, "src/billing/invoice.rs"),
            make_node("new", NodeKind::Function, "src/payments/gateway.rs"),
        ];
        let gs = make_graph_state(nodes);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams::default();

        let results = flat_code_symbol_search(
            "auth/new",
            SearchMode::Hybrid,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;

        assert_eq!(
            results.len(),
            1,
            "Path filter should discriminate to only auth node"
        );
        assert!(results[0].id.file.to_string_lossy().contains("auth"));
    }

    // ── Subsystem filter tests ──────────────────────────────────────────

    #[tokio::test]
    async fn test_flat_search_subsystem_filter() {
        let mut node_a = make_node("scan_files", NodeKind::Function, "src/scanner.rs");
        node_a.metadata.insert(
            crate::server::SUBSYSTEM_KEY.to_owned(),
            "scanner".to_string(),
        );
        let mut node_b = make_node("scan_config", NodeKind::Function, "src/config.rs");
        node_b.metadata.insert(
            crate::server::SUBSYSTEM_KEY.to_owned(),
            "config".to_string(),
        );
        let node_c = make_node("scan_other", NodeKind::Function, "src/other.rs");
        // node_c has no subsystem metadata

        let gs = make_graph_state(vec![node_a, node_b, node_c]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("scan".into()),
            subsystem: Some("scanner".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "scan",
            SearchMode::Hybrid,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;

        assert_eq!(results.len(), 1, "Only scanner-subsystem node should match");
        assert_eq!(results[0].id.name, "scan_files");
    }

    #[tokio::test]
    async fn test_flat_search_subsystem_filter_case_insensitive() {
        let mut node = make_node("handler", NodeKind::Function, "src/server.rs");
        node.metadata.insert(
            crate::server::SUBSYSTEM_KEY.to_owned(),
            "Server".to_string(),
        );

        let gs = make_graph_state(vec![node]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("handler".into()),
            subsystem: Some("server".into()),
            ..Default::default()
        };

        let results = flat_code_symbol_search(
            "handler",
            SearchMode::Hybrid,
            10,
            &params,
            &gs,
            &ctx,
            false,
            false,
        )
        .await;

        assert_eq!(
            results.len(),
            1,
            "Case-insensitive subsystem match should work"
        );
    }

    #[tokio::test]
    async fn test_flat_search_subsystem_allows_empty_query_browse() {
        let mut node = make_node("extract", NodeKind::Function, "src/extract.rs");
        node.metadata.insert(
            crate::server::SUBSYSTEM_KEY.to_owned(),
            "extractor".to_string(),
        );
        let gs = make_graph_state(vec![node]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            subsystem: Some("extractor".into()),
            ..Default::default()
        };

        // Empty query with subsystem filter should be allowed (not rejected)
        let result = search(&params, &ctx).await;
        assert!(
            !result.contains("Empty query"),
            "Subsystem filter should act as browse filter"
        );
    }

    #[test]
    fn test_subsystem_matches_exact() {
        assert!(super::subsystem_matches("extract", "extract"));
        assert!(super::subsystem_matches("Extract", "extract"));
        assert!(super::subsystem_matches("extract", "Extract"));
    }

    #[test]
    fn test_subsystem_matches_parent_prefix() {
        // Parent filter matches child sub-modules
        assert!(super::subsystem_matches("extract/Node", "extract"));
        assert!(super::subsystem_matches("extract/enrich", "extract"));
        assert!(super::subsystem_matches("Extract/Node", "extract"));
    }

    #[test]
    fn test_subsystem_matches_child_specific() {
        // Child-specific filter matches only that child
        assert!(super::subsystem_matches("extract/enrich", "extract/enrich"));
        assert!(!super::subsystem_matches("extract/Node", "extract/enrich"));
    }

    #[test]
    fn test_subsystem_matches_no_false_prefix() {
        // "extract" should NOT match "extraction" (not a `/`-separated prefix)
        assert!(!super::subsystem_matches("extraction", "extract"));
    }

    #[tokio::test]
    async fn test_flat_search_subsystem_parent_matches_children() {
        let mut node_a = make_node("enrich", NodeKind::Function, "src/extract/enrich.rs");
        node_a.metadata.insert(
            crate::server::SUBSYSTEM_KEY.to_owned(),
            "extract/enrich".to_string(),
        );
        let mut node_b = make_node("NodeId", NodeKind::Struct, "src/extract/mod.rs");
        node_b.metadata.insert(
            crate::server::SUBSYSTEM_KEY.to_owned(),
            "extract/Node".to_string(),
        );
        let mut node_c = make_node("embed_texts", NodeKind::Function, "src/embed.rs");
        node_c
            .metadata
            .insert(crate::server::SUBSYSTEM_KEY.to_owned(), "embed".to_string());
        let gs = make_graph_state(vec![node_a, node_b, node_c]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            subsystem: Some("extract".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        // Both extract/enrich and extract/Node should match, but not embed
        assert!(
            result.contains("enrich"),
            "Should include extract/enrich child"
        );
        assert!(
            result.contains("NodeId"),
            "Should include extract/Node child"
        );
        assert!(
            !result.contains("embed_texts"),
            "Should NOT include embed subsystem"
        );
    }

    // ── Cross-subsystem traversal tests ───────────────────────────────

    #[tokio::test]
    async fn test_traversal_target_subsystem_filters_neighbors() {
        use crate::graph::EdgeKind;

        // Create nodes in different subsystems
        let mut node_a = make_node("handler", NodeKind::Function, "src/server.rs");
        node_a.metadata.insert(
            crate::server::SUBSYSTEM_KEY.to_owned(),
            "server".to_string(),
        );
        let mut node_b = make_node("embed_text", NodeKind::Function, "src/embed.rs");
        node_b
            .metadata
            .insert(crate::server::SUBSYSTEM_KEY.to_owned(), "embed".to_string());
        let mut node_c = make_node("scan_file", NodeKind::Function, "src/scanner.rs");
        node_c.metadata.insert(
            crate::server::SUBSYSTEM_KEY.to_owned(),
            "scanner".to_string(),
        );

        // handler calls embed_text and scan_file
        let edge1 = make_edge(&node_a, &node_b, EdgeKind::Calls);
        let edge2 = make_edge(&node_a, &node_c, EdgeKind::Calls);

        let gs = make_graph_state_with_edges(
            vec![node_a.clone(), node_b.clone(), node_c.clone()],
            vec![edge1, edge2],
        );
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // Query: neighbors of handler, filtered to embed subsystem only
        let params = SearchParams {
            node: Some(node_a.stable_id()),
            mode: Some("neighbors".into()),
            target_subsystem: Some("embed".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        assert!(
            result.contains("embed_text"),
            "Should include embed neighbor"
        );
        assert!(
            !result.contains("scan_file"),
            "Should NOT include scanner neighbor"
        );
    }

    #[tokio::test]
    async fn test_traversal_target_subsystem_no_match_returns_empty() {
        use crate::graph::EdgeKind;

        let mut node_a = make_node("handler", NodeKind::Function, "src/server.rs");
        node_a.metadata.insert(
            crate::server::SUBSYSTEM_KEY.to_owned(),
            "server".to_string(),
        );
        let mut node_b = make_node("embed_text", NodeKind::Function, "src/embed.rs");
        node_b
            .metadata
            .insert(crate::server::SUBSYSTEM_KEY.to_owned(), "embed".to_string());

        let edge = make_edge(&node_a, &node_b, EdgeKind::Calls);
        let gs = make_graph_state_with_edges(vec![node_a.clone(), node_b.clone()], vec![edge]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // Query: neighbors of handler, filtered to nonexistent subsystem
        let params = SearchParams {
            node: Some(node_a.stable_id()),
            mode: Some("neighbors".into()),
            target_subsystem: Some("nonexistent".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        assert!(
            result.contains("No outgoing neighbors"),
            "Should report no neighbors when target_subsystem matches nothing"
        );
    }

    #[tokio::test]
    async fn test_traversal_subsystem_and_target_subsystem_combined() {
        use crate::graph::EdgeKind;

        // node_a (server) -> node_b (embed), node_c (scanner), node_d (server)
        let mut node_a = make_node("handler", NodeKind::Function, "src/server.rs");
        node_a.metadata.insert(
            crate::server::SUBSYSTEM_KEY.to_owned(),
            "server".to_string(),
        );
        let mut node_b = make_node("embed_text", NodeKind::Function, "src/embed.rs");
        node_b
            .metadata
            .insert(crate::server::SUBSYSTEM_KEY.to_owned(), "embed".to_string());
        let mut node_c = make_node("scan_file", NodeKind::Function, "src/scanner.rs");
        node_c.metadata.insert(
            crate::server::SUBSYSTEM_KEY.to_owned(),
            "scanner".to_string(),
        );
        let mut node_d = make_node("route", NodeKind::Function, "src/server/route.rs");
        node_d.metadata.insert(
            crate::server::SUBSYSTEM_KEY.to_owned(),
            "server".to_string(),
        );

        let edges = vec![
            make_edge(&node_a, &node_b, EdgeKind::Calls),
            make_edge(&node_a, &node_c, EdgeKind::Calls),
            make_edge(&node_a, &node_d, EdgeKind::Calls),
        ];

        let gs = make_graph_state_with_edges(
            vec![
                node_a.clone(),
                node_b.clone(),
                node_c.clone(),
                node_d.clone(),
            ],
            edges,
        );
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // Both subsystem (server) and target_subsystem (embed) set.
        // subsystem filters entry-point resolution (not relevant here since we use node ID).
        // target_subsystem filters the traversal results.
        let params = SearchParams {
            node: Some(node_a.stable_id()),
            mode: Some("neighbors".into()),
            target_subsystem: Some("embed".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        assert!(
            result.contains("embed_text"),
            "Should include embed neighbor"
        );
        assert!(
            !result.contains("scan_file"),
            "Should NOT include scanner neighbor"
        );
        assert!(
            !result.contains("route"),
            "Should NOT include server neighbor"
        );
    }

    #[tokio::test]
    async fn test_traversal_target_subsystem_hierarchical_match() {
        use crate::graph::EdgeKind;

        let mut node_a = make_node("handler", NodeKind::Function, "src/server.rs");
        node_a.metadata.insert(
            crate::server::SUBSYSTEM_KEY.to_owned(),
            "server".to_string(),
        );
        let mut node_b = make_node("enrich_node", NodeKind::Function, "src/extract/enrich.rs");
        node_b.metadata.insert(
            crate::server::SUBSYSTEM_KEY.to_owned(),
            "extract/enrich".to_string(),
        );
        let mut node_c = make_node("parse_node", NodeKind::Function, "src/extract/parse.rs");
        node_c.metadata.insert(
            crate::server::SUBSYSTEM_KEY.to_owned(),
            "extract/parse".to_string(),
        );

        let edges = vec![
            make_edge(&node_a, &node_b, EdgeKind::Calls),
            make_edge(&node_a, &node_c, EdgeKind::Calls),
        ];

        let gs = make_graph_state_with_edges(
            vec![node_a.clone(), node_b.clone(), node_c.clone()],
            edges,
        );
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // target_subsystem="extract" should match both extract/enrich and extract/parse
        let params = SearchParams {
            node: Some(node_a.stable_id()),
            mode: Some("neighbors".into()),
            target_subsystem: Some("extract".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        assert!(
            result.contains("enrich_node"),
            "Should include extract/enrich child"
        );
        assert!(
            result.contains("parse_node"),
            "Should include extract/parse child"
        );
    }

    #[test]
    fn test_format_impact_subsystem_breakdown_empty() {
        let groups = std::collections::BTreeMap::new();
        let gs = make_graph_state(vec![]);
        let node_index_map = gs.node_index_map();
        let result = format_impact_subsystem_breakdown(&groups, &gs, &node_index_map, None);
        assert!(
            result.is_empty(),
            "No subsystem data should produce empty string"
        );
    }

    #[test]
    fn test_format_impact_subsystem_breakdown_groups_correctly() {
        use crate::graph::EdgeKind;
        let mut node_a = make_node("fn_a", NodeKind::Function, "src/alpha.rs");
        node_a
            .metadata
            .insert(crate::server::SUBSYSTEM_KEY.to_owned(), "alpha".to_string());
        let mut node_b = make_node("fn_b", NodeKind::Function, "src/beta.rs");
        node_b
            .metadata
            .insert(crate::server::SUBSYSTEM_KEY.to_owned(), "beta".to_string());
        let mut node_c = make_node("fn_c", NodeKind::Function, "src/beta.rs");
        node_c
            .metadata
            .insert(crate::server::SUBSYSTEM_KEY.to_owned(), "beta".to_string());
        let gs = make_graph_state(vec![node_a.clone(), node_b.clone(), node_c.clone()]);
        let node_index_map = gs.node_index_map();

        let mut groups = std::collections::BTreeMap::new();
        groups.insert(
            EdgeKind::Calls,
            vec![node_a.stable_id(), node_b.stable_id(), node_c.stable_id()],
        );

        let result = format_impact_subsystem_breakdown(&groups, &gs, &node_index_map, None);
        assert!(result.contains("alpha"), "Should contain alpha subsystem");
        assert!(result.contains("beta"), "Should contain beta subsystem");
        assert!(result.contains("2 symbol(s)"), "Beta should have 2 symbols");
        assert!(result.contains("1 symbol(s)"), "Alpha should have 1 symbol");
    }

    #[test]
    fn test_count_affected_subsystems() {
        use crate::graph::EdgeKind;
        let mut node_a = make_node("fn_a", NodeKind::Function, "src/alpha.rs");
        node_a
            .metadata
            .insert(crate::server::SUBSYSTEM_KEY.to_owned(), "alpha".to_string());
        let mut node_b = make_node("fn_b", NodeKind::Function, "src/beta.rs");
        node_b
            .metadata
            .insert(crate::server::SUBSYSTEM_KEY.to_owned(), "beta".to_string());
        let mut node_c = make_node("fn_c", NodeKind::Function, "src/beta.rs");
        node_c
            .metadata
            .insert(crate::server::SUBSYSTEM_KEY.to_owned(), "beta".to_string());
        let gs = make_graph_state(vec![node_a.clone(), node_b.clone(), node_c.clone()]);
        let node_index_map = gs.node_index_map();

        let mut groups = std::collections::BTreeMap::new();
        groups.insert(
            EdgeKind::Calls,
            vec![node_a.stable_id(), node_b.stable_id(), node_c.stable_id()],
        );

        assert_eq!(count_affected_subsystems(&groups, &gs, &node_index_map), 2);
    }

    #[test]
    fn test_count_affected_subsystems_empty() {
        let groups = std::collections::BTreeMap::new();
        let gs = make_graph_state(vec![]);
        let node_index_map = gs.node_index_map();
        assert_eq!(count_affected_subsystems(&groups, &gs, &node_index_map), 0);
    }

    #[test]
    fn test_impact_summary_thresholds_are_reasonable() {
        // Node threshold: low enough to catch moderate-count-but-verbose-output cases.
        // The old threshold of 100 was too high — 80 non-compact nodes produced 157K chars.
        assert!(
            IMPACT_SUMMARY_NODE_THRESHOLD >= 10,
            "Node threshold too low"
        );
        assert!(
            IMPACT_SUMMARY_NODE_THRESHOLD <= 60,
            "Node threshold too high"
        );
        // Character threshold: safety net for when node count is below the node threshold
        // but the rendered output is still too large.
        assert!(
            IMPACT_SUMMARY_CHAR_THRESHOLD >= 20_000,
            "Char threshold too low"
        );
        assert!(
            IMPACT_SUMMARY_CHAR_THRESHOLD <= 100_000,
            "Char threshold too high"
        );
    }

    /// Adversarial: verify large impact results produce summary, not full listing.
    /// Creates 150 nodes (across 3 subsystems) all calling one root node,
    /// then runs search(mode="impact") and verifies the output is compact.
    #[tokio::test]
    async fn test_large_impact_produces_subsystem_summary() {
        use crate::graph::EdgeKind;

        let root_node = make_node("RootType", NodeKind::Struct, "src/root.rs");
        let mut all_nodes = vec![root_node.clone()];
        let mut all_edges = Vec::new();

        // Impact traversal follows incoming Calls/ReferencedBy edges.
        // "fn_0 calls RootType" = edge from fn_0 to RootType = incoming edge on RootType.
        let subsystems = ["alpha", "beta", "gamma"];
        for i in 0..150 {
            let sub = subsystems[i % 3];
            let file = format!("src/{}/mod.rs", sub);
            let mut node = make_node(&format!("fn_{}", i), NodeKind::Function, &file);
            node.metadata
                .insert(crate::server::SUBSYSTEM_KEY.to_owned(), sub.to_string());
            all_edges.push(make_edge(&node, &root_node, EdgeKind::Calls));
            all_nodes.push(node);
        }

        let gs = make_graph_state_with_edges(all_nodes, all_edges);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        let params = SearchParams {
            node: Some(root_node.stable_id()),
            mode: Some("impact".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;

        // Should contain subsystem summary, not individual node listings
        assert!(
            result.contains("subsystems affected"),
            "Should show subsystem count in heading, got: {}",
            &result[..result.len().min(500)]
        );
        assert!(result.contains("alpha"), "Should list alpha subsystem");
        assert!(result.contains("beta"), "Should list beta subsystem");
        assert!(result.contains("gamma"), "Should list gamma subsystem");
        assert!(
            result.contains("50 symbol(s)"),
            "Each subsystem should have 50 nodes"
        );

        // Should NOT contain full node listings (edge-kind grouped sections)
        assert!(
            !result.contains("#### Calls"),
            "Should NOT have edge-kind grouped sections in summary mode"
        );

        // Output should be compact -- well under 10K chars for 150 nodes
        assert!(
            result.len() < 5000,
            "Summary should be compact, got {} chars",
            result.len()
        );
    }

    /// Adversarial: verify small impact results still show full listing.
    #[tokio::test]
    async fn test_small_impact_preserves_full_listing() {
        use crate::graph::EdgeKind;

        let root_node = make_node("SmallRoot", NodeKind::Struct, "src/root.rs");
        let mut dep = make_node("one_dep", NodeKind::Function, "src/dep.rs");
        dep.metadata
            .insert(crate::server::SUBSYSTEM_KEY.to_owned(), "dep".to_string());
        let edge = make_edge(&dep, &root_node, EdgeKind::Calls);

        let gs = make_graph_state_with_edges(vec![root_node.clone(), dep.clone()], vec![edge]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        let params = SearchParams {
            node: Some(root_node.stable_id()),
            mode: Some("impact".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;

        // Should show full listing with edge-kind groups
        assert!(
            result.contains("Impact analysis for"),
            "Should use standard heading for small results, got: {}",
            &result[..result.len().min(500)]
        );
        assert!(result.contains("one_dep"), "Should list individual nodes");
        // Should also have subsystem breakdown appended
        assert!(
            result.contains("Affected subsystems"),
            "Should still have subsystem breakdown"
        );
    }

    /// Adversarial: moderate node count (below node threshold) but verbose output
    /// that exceeds the character threshold should still trigger the summary view.
    /// This is the exact bug from #345 round 2: ~80 nodes producing 157K chars.
    #[tokio::test]
    async fn test_moderate_count_but_verbose_output_triggers_char_threshold() {
        use crate::graph::EdgeKind;

        let root_node = make_node("VerboseRoot", NodeKind::Struct, "src/root.rs");
        let mut all_nodes = vec![root_node.clone()];
        let mut all_edges = Vec::new();

        // Create 25 nodes (below node threshold of 30) but each with a very
        // long signature that inflates the non-compact output beyond 40K chars.
        let subsystems = ["verbose_a", "verbose_b"];
        for i in 0..25 {
            let sub = subsystems[i % 2];
            let file = format!("src/{}/mod.rs", sub);
            let mut node = make_node(&format!("verbose_fn_{}", i), NodeKind::Function, &file);
            // A 2000-char signature makes each node ~2KB+ in non-compact mode
            node.signature = format!("fn verbose_fn_{}({})", i, "x: SomeLongType, ".repeat(100));
            node.metadata
                .insert(crate::server::SUBSYSTEM_KEY.to_owned(), sub.to_string());
            all_edges.push(make_edge(&node, &root_node, EdgeKind::Calls));
            all_nodes.push(node);
        }

        let gs = make_graph_state_with_edges(all_nodes, all_edges);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        let params = SearchParams {
            node: Some(root_node.stable_id()),
            mode: Some("impact".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;

        // The char threshold should kick in and produce a summary
        assert!(
            result.contains("subsystems affected") || result.contains("result summarized"),
            "Should trigger summary via char threshold, got: {}",
            &result[..result.len().min(500)]
        );
        // Should be compact
        assert!(
            result.len() < IMPACT_SUMMARY_CHAR_THRESHOLD,
            "Summary should be well under char threshold, got {} chars",
            result.len()
        );
    }

    /// Adversarial: verify large impact with NO subsystem metadata handles gracefully.
    #[tokio::test]
    async fn test_large_impact_no_subsystem_metadata() {
        use crate::graph::EdgeKind;

        let root_node = make_node("OrphanRoot", NodeKind::Struct, "src/root.rs");
        let mut all_nodes = vec![root_node.clone()];
        let mut all_edges = Vec::new();

        // 150 nodes with NO subsystem metadata
        for i in 0..150 {
            let node = make_node(
                &format!("orphan_{}", i),
                NodeKind::Function,
                "src/orphan.rs",
            );
            all_edges.push(make_edge(&node, &root_node, EdgeKind::Calls));
            all_nodes.push(node);
        }

        let gs = make_graph_state_with_edges(all_nodes, all_edges);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        let params = SearchParams {
            node: Some(root_node.stable_id()),
            mode: Some("impact".into()),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;

        // Should fall back to count-only summary
        assert!(
            result.contains("150 dependent(s)"),
            "Should show total count, got: {}",
            &result[..result.len().min(500)]
        );
        assert!(
            result.contains("result summarized"),
            "Should indicate summarized output"
        );
        assert!(
            result.contains("subsystem"),
            "Should hint to use subsystem filter"
        );
        // Should NOT crash or produce empty output
        assert!(result.len() > 50, "Should produce meaningful output");
    }

    // ── Depth-aware traversal tests ────────────────────────────────────

    #[tokio::test]
    async fn test_depth_traversal_two_levels() {
        use crate::graph::EdgeKind;

        // Chain: module -> member -> sub_member
        let module = make_node("my_module", NodeKind::Module, "src/module.rs");
        let member = make_node("my_struct", NodeKind::Struct, "src/module.rs");
        let sub_member = make_node("my_field", NodeKind::Function, "src/module.rs");

        let edges = vec![
            make_edge(&module, &member, EdgeKind::Defines),
            make_edge(&member, &sub_member, EdgeKind::Defines),
        ];
        let gs = make_graph_state_with_edges(
            vec![module.clone(), member.clone(), sub_member.clone()],
            edges,
        );
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // depth=2 should return both member and sub_member
        let params = SearchParams {
            node: Some(module.stable_id()),
            mode: Some("neighbors".into()),
            depth: Some(2),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        assert!(
            result.contains("my_struct"),
            "depth=2 should include direct member"
        );
        assert!(
            result.contains("my_field"),
            "depth=2 should include sub-member"
        );
        // Entry node name appears in the header ("Graph neighbors of my_module") but should NOT
        // appear as a neighbor result (i.e., not as a backreference to itself in the result list).
        // Check that my_struct and my_field are present (they are the actual results).
        // We do NOT assert my_module is absent from the full output since it's in the section header.
    }

    #[tokio::test]
    async fn test_depth_one_same_as_default() {
        use crate::graph::EdgeKind;

        // Chain: module -> member -> sub_member
        let module = make_node("mod_a", NodeKind::Module, "src/mod_a.rs");
        let member = make_node("fn_b", NodeKind::Function, "src/mod_a.rs");
        let sub_member = make_node("fn_c", NodeKind::Function, "src/mod_a.rs");

        let edges = vec![
            make_edge(&module, &member, EdgeKind::Defines),
            make_edge(&member, &sub_member, EdgeKind::Defines),
        ];
        let gs = make_graph_state_with_edges(
            vec![module.clone(), member.clone(), sub_member.clone()],
            edges,
        );
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // depth=1 should behave like default (no depth param)
        let params_depth1 = SearchParams {
            node: Some(module.stable_id()),
            mode: Some("neighbors".into()),
            depth: Some(1),
            ..Default::default()
        };
        let params_default = SearchParams {
            node: Some(module.stable_id()),
            mode: Some("neighbors".into()),
            ..Default::default()
        };
        let result_d1 = search(&params_depth1, &ctx).await;
        let result_default = search(&params_default, &ctx).await;

        // Both should contain fn_b but not fn_c (only 1 hop)
        assert!(
            result_d1.contains("fn_b"),
            "depth=1 should include direct member"
        );
        assert!(
            !result_d1.contains("fn_c"),
            "depth=1 should NOT include sub-member"
        );
        assert_eq!(
            result_d1, result_default,
            "depth=1 output should match default behavior"
        );
    }

    #[tokio::test]
    async fn test_depth_traversal_deduplicates_across_levels() {
        use crate::graph::EdgeKind;

        // Diamond: module -> a -> c, module -> b -> c
        // c should appear only once even though both a and b point to it
        let module = make_node("diamond_mod", NodeKind::Module, "src/diamond.rs");
        let node_a = make_node("branch_a", NodeKind::Function, "src/diamond.rs");
        let node_b = make_node("branch_b", NodeKind::Function, "src/diamond.rs");
        let node_c = make_node("shared_leaf", NodeKind::Function, "src/diamond.rs");

        let edges = vec![
            make_edge(&module, &node_a, EdgeKind::Defines),
            make_edge(&module, &node_b, EdgeKind::Defines),
            make_edge(&node_a, &node_c, EdgeKind::Defines),
            make_edge(&node_b, &node_c, EdgeKind::Defines),
        ];
        let gs = make_graph_state_with_edges(
            vec![
                module.clone(),
                node_a.clone(),
                node_b.clone(),
                node_c.clone(),
            ],
            edges,
        );
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        let params = SearchParams {
            node: Some(module.stable_id()),
            mode: Some("neighbors".into()),
            depth: Some(2),
            compact: true,
            ..Default::default()
        };
        let result = search(&params, &ctx).await;

        // shared_leaf should appear as exactly one result entry.
        // Count stable ID occurrences (e.g., "local:src/diamond.rs:shared_leaf:function")
        // to avoid false positives from name appearing multiple times on one result line.
        let stable_id_occurrences = result.matches(":shared_leaf:").count();
        assert_eq!(
            stable_id_occurrences,
            1,
            "shared_leaf stable ID should appear exactly once (dedup failed), got {} occurrences: {}",
            stable_id_occurrences,
            &result[..result.len().min(500)]
        );
        // branch_a and branch_b should both appear
        assert!(result.contains("branch_a"), "branch_a should be in results");
        assert!(result.contains("branch_b"), "branch_b should be in results");
    }

    #[tokio::test]
    async fn test_depth_batch_nodes_rejects_depth_greater_than_one() {
        use crate::graph::EdgeKind;

        // depth > 1 with nodes=[...] should return error message
        let node_a = make_node("fn_a", NodeKind::Function, "src/a.rs");
        let node_b = make_node("fn_b", NodeKind::Function, "src/b.rs");
        let edges = vec![make_edge(&node_a, &node_b, EdgeKind::Calls)];
        let gs = make_graph_state_with_edges(vec![node_a.clone(), node_b.clone()], edges);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        let params = SearchParams {
            nodes: Some(vec![node_a.stable_id()]),
            mode: Some("neighbors".into()),
            depth: Some(2),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        assert!(
            result.contains("depth > 1 is not supported"),
            "Should return error for nodes+depth>1: {}",
            result
        );
    }

    // ── Adversarial depth traversal tests ────────────────────────────

    #[tokio::test]
    async fn test_depth_cyclic_graph_does_not_loop() {
        use crate::graph::EdgeKind;

        // Cycle: A -> B -> A (back-edge)
        // depth=3 should not loop infinitely; visited set must break cycle.
        let node_a = make_node("cycle_a", NodeKind::Module, "src/cycle.rs");
        let node_b = make_node("cycle_b", NodeKind::Function, "src/cycle.rs");

        let edges = vec![
            make_edge(&node_a, &node_b, EdgeKind::Calls),
            make_edge(&node_b, &node_a, EdgeKind::Calls), // back-edge creating cycle
        ];
        let gs = make_graph_state_with_edges(vec![node_a.clone(), node_b.clone()], edges);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // depth=3 should terminate (visited set breaks cycle after level 1)
        let params = SearchParams {
            node: Some(node_a.stable_id()),
            mode: Some("neighbors".into()),
            depth: Some(3),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        // cycle_b should appear (level 1); cycle_a should NOT re-appear (it's in visited)
        assert!(
            result.contains("cycle_b"),
            "cycle_b should appear in results"
        );
        // Result should be finite and not crash
        assert!(
            result.len() < 100_000,
            "Output should be bounded even with cycles"
        );
    }

    #[tokio::test]
    async fn test_depth_with_non_neighbors_mode_uses_hops() {
        use crate::graph::EdgeKind;

        // depth should be silently ignored for impact mode (uses hops instead)
        let node_a = make_node("caller_fn", NodeKind::Function, "src/a.rs");
        let node_b = make_node("callee_fn", NodeKind::Function, "src/b.rs");
        let edges = vec![make_edge(&node_a, &node_b, EdgeKind::Calls)];
        let gs = make_graph_state_with_edges(vec![node_a.clone(), node_b.clone()], edges);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        // impact mode with depth=2 — depth should be ignored, hops controls behavior
        let params = SearchParams {
            node: Some(node_b.stable_id()),
            mode: Some("impact".into()),
            depth: Some(2), // Should be ignored for impact mode
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        // Should still find the impact (caller_fn) — just verifying no crash/silent error
        assert!(
            !result.is_empty(),
            "impact mode with depth param should still produce output"
        );
        assert!(
            result.contains("Impact analysis"),
            "should be impact analysis output"
        );
    }

    #[tokio::test]
    async fn test_depth_with_edge_type_filter_limits_each_level() {
        use crate::graph::EdgeKind;

        // node_mod -[Defines]-> fn_a -[Calls]-> fn_b
        // With edge_types=["defines"] and depth=2, fn_b should NOT appear
        // because the Calls edge at level 2 is filtered out.
        let node_mod = make_node("filtered_mod", NodeKind::Module, "src/filt.rs");
        let fn_a = make_node("filtered_fn_a", NodeKind::Function, "src/filt.rs");
        let fn_b = make_node("filtered_fn_b", NodeKind::Function, "src/filt.rs");

        let edges = vec![
            make_edge(&node_mod, &fn_a, EdgeKind::Defines),
            make_edge(&fn_a, &fn_b, EdgeKind::Calls), // not Defines
        ];
        let gs =
            make_graph_state_with_edges(vec![node_mod.clone(), fn_a.clone(), fn_b.clone()], edges);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);

        let params = SearchParams {
            node: Some(node_mod.stable_id()),
            mode: Some("neighbors".into()),
            depth: Some(2),
            edge_types: Some(vec!["defines".to_string()]),
            ..Default::default()
        };
        let result = search(&params, &ctx).await;
        // fn_a should appear (Defines edge at level 1)
        assert!(
            result.contains("filtered_fn_a"),
            "fn_a should appear (Defines edge at level 1)"
        );
        // fn_b should NOT appear (Calls edge at level 2 is filtered by edge_types=["defines"])
        assert!(
            !result.contains("filtered_fn_b"),
            "fn_b should NOT appear (Calls edge is filtered)"
        );
    }
}
