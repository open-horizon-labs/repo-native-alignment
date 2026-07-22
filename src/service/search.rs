//! Flat search, graph traversal, and batch node retrieval.
//!
//! The `search` function is the unified entry point that dispatches to
//! `search_flat`, `search_traversal`, or `search_batch` depending on the
//! parameters supplied by the caller.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Once;

use crate::embed::{
    EMBEDDING_MODEL_NAME, ExecutedSearchMode, NativeScoreKind, NativeScoreSource,
    ObservedSearchOutcome, ScoreAdjustment, ScoreNormalization, SearchFilters, SearchMode,
    SearchOutcome, SearchResult, SearchScoreProvenance, TestResultPolicy,
};
use crate::graph::index::GraphIndex;
use crate::graph::{Edge, EdgeKind, ExtractionSource, Node, NodeKind};
use crate::ranking;
use crate::server::handlers::parse_search_mode;
use crate::server::helpers::{
    EdgeEvidenceIndex, format_capability_readiness, format_freshness_full,
    format_indexed_edge_evidence_for_groups, format_neighbors_grouped_with_root,
    format_node_entry_with_root, strip_root_prefix,
};
use crate::server::state::{CapabilityReadinessState, GraphState, LspEnrichmentStatus};
use crate::server::store::parse_edge_kind;
use crate::server::{EnrichmentCapability, EnrichmentJobState, EnrichmentScope};

use super::{
    SearchContext, SearchParams, node_passes_root_filter, search_result_passes_root_filter,
};

pub(crate) mod fusion;
pub(crate) mod graph_delta;
pub(crate) mod model;
pub(crate) mod projection;
pub(crate) mod render;
pub(crate) mod source;
pub(crate) mod task_context;

use fusion::{
    ChannelInput, EvidenceChannel, FusedCandidate, FusionPolicy, RawCandidateScore, ScoreKind,
    fuse_ranked_channels,
};
use model::{
    BodyPolicy, CandidateAudit, CandidateDisposition, CapabilityState, CapabilityStatus,
    ContextRole as ProjectionRole, EvidenceProvenance, HydrationHandle, HydrationKind,
    OmissionCode, ProjectedRelationship, ProjectionBudget, ProjectionInput, ProjectionOmission,
    ProjectionRequest, RecordIdentity, RetrievalLane as ProjectionLane, SearchIntent,
    SearchProjection, SelectedRecord, SelectionChannel, SelectionEvidence, SelectionSummary,
    SourceSpan as ProjectionSourceSpan, SymbolSummary,
};
use task_context::{
    ContextRole as TaskRole, EvidenceCandidate as TaskEvidenceCandidate, ExactCandidate,
    ExactResolution, RetrievalLane as TaskLane, SelectionPolicy as TaskSelectionPolicy,
    SelectionReason as TaskSelectionReason, SourceAnchor, TaskFacet,
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
const STRICT_SEMANTIC_MODE: &str = "strict";
const MAX_PROJECTED_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_PROJECTED_OUTPUT_TOKENS: usize = 64 * 1024;
const MAX_PROJECTED_BODY_BYTES: usize = 64 * 1024;
const MAX_PROJECTED_TOTAL_BODY_BYTES: usize = 256 * 1024;
const MAX_CONTEXT_HOPS: u32 = 4;
const MAX_PROPOSAL_BYTES: usize = 256 * 1024;
const MAX_CONTEXT_LIST_VALUES: usize = 16;
const MAX_CONTEXT_ENTRY_NODES: usize = 32;
const EVIDENCE_CAPSULE_SCHEMA_VERSION: u8 = 1;
const MAX_EVIDENCE_CAPSULE_BYTES: usize = 64 * 1024;
const EVIDENCE_CAPSULE_PREFIX: &str = "evidence-cache-v1:";
const SEMANTIC_EVIDENCE_CAPSULE_PREFIX: &str = "semantic-evidence-cache-v1:";

const CONTEXT_ROLES: &[&str] = &[
    "editable_source",
    "definition_or_api_state",
    "test",
    "behavioral_analogue",
    "direct_dependency",
    "caller_or_impact",
    "proposal_delta",
];
const CONTEXT_FACETS: &[&str] = &["behavior", "api_or_state", "test", "analogue", "proposal"];

fn validate_named_values(
    field: &str,
    values: Option<&[String]>,
    allowed: &[&str],
) -> Result<(), String> {
    let Some(values) = values else {
        return Ok(());
    };
    if values.len() > MAX_CONTEXT_LIST_VALUES {
        return Err(format!(
            "{field} accepts at most {MAX_CONTEXT_LIST_VALUES} values"
        ));
    }
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if !allowed.contains(&value) {
            return Err(format!(
                "unknown {field} `{value}`; allowed values: {}",
                allowed.join(", ")
            ));
        }
    }
    Ok(())
}

fn validate_search_experience(params: &SearchParams) -> Result<(), String> {
    if let Some(projection) = params.projection.as_deref().map(str::trim)
        && !matches!(projection, "agent" | "evidence")
    {
        return Err(format!(
            "unknown projection `{projection}`; allowed values: agent, evidence"
        ));
    }
    if let Some(body_policy) = params.body_policy.as_deref().map(str::trim)
        && !matches!(
            body_policy,
            "complete" | "focused_span" | "signature_only" | "minified" | "none"
        )
    {
        return Err(format!(
            "unknown body_policy `{body_policy}`; allowed values: complete, focused_span, signature_only, minified, none"
        ));
    }
    if let Some(bytes) = params.max_output_bytes
        && !(1..=MAX_PROJECTED_OUTPUT_BYTES).contains(&bytes)
    {
        return Err(format!(
            "max_output_bytes must be between 1 and {MAX_PROJECTED_OUTPUT_BYTES}"
        ));
    }
    if let Some(tokens) = params.max_output_tokens
        && !(1..=MAX_PROJECTED_OUTPUT_TOKENS).contains(&tokens)
    {
        return Err(format!(
            "max_output_tokens must be between 1 and {MAX_PROJECTED_OUTPUT_TOKENS}"
        ));
    }
    if let Some(bytes) = params.max_body_bytes
        && !(1..=MAX_PROJECTED_BODY_BYTES).contains(&bytes)
    {
        return Err(format!(
            "max_body_bytes must be between 1 and {MAX_PROJECTED_BODY_BYTES}"
        ));
    }
    if let Some(bytes) = params.max_total_body_bytes
        && !(1..=MAX_PROJECTED_TOTAL_BODY_BYTES).contains(&bytes)
    {
        return Err(format!(
            "max_total_body_bytes must be between 1 and {MAX_PROJECTED_TOTAL_BODY_BYTES}"
        ));
    }

    validate_named_values(
        "context role",
        params.context_roles.as_deref(),
        CONTEXT_ROLES,
    )?;
    validate_named_values(
        "context facet",
        params.context_facets.as_deref(),
        CONTEXT_FACETS,
    )?;

    let context_mode = params.context_mode.as_deref().map(str::trim);
    if let Some(mode) = context_mode
        && !matches!(mode, "task" | "graph-delta-beta")
    {
        return Err(format!(
            "unknown context_mode `{mode}`; allowed values: task, graph-delta-beta"
        ));
    }
    if context_mode != Some("task")
        && (params.context_roles.is_some() || params.context_facets.is_some())
    {
        return Err("context_roles/context_facets require context_mode=task".to_string());
    }
    if context_mode.is_some() {
        if let Some(direction) = params.direction.as_deref().map(str::trim)
            && !matches!(direction, "incoming" | "outgoing" | "both")
        {
            return Err(format!(
                "unknown task-context direction `{direction}`; allowed values: incoming, outgoing, both"
            ));
        }
        if params.hops.unwrap_or(1) > MAX_CONTEXT_HOPS {
            return Err(format!(
                "task-context hops cannot exceed {MAX_CONTEXT_HOPS}"
            ));
        }
        if params.depth.unwrap_or(1) > MAX_CONTEXT_HOPS {
            return Err(format!(
                "task-context depth cannot exceed {MAX_CONTEXT_HOPS}"
            ));
        }
        if params
            .nodes
            .as_ref()
            .is_some_and(|nodes| nodes.len() > MAX_CONTEXT_ENTRY_NODES)
        {
            return Err(format!(
                "task context accepts at most {MAX_CONTEXT_ENTRY_NODES} entry nodes"
            ));
        }
        if let Some(edge_types) = params.edge_types.as_deref() {
            if edge_types.len() > MAX_CONTEXT_LIST_VALUES {
                return Err(format!(
                    "task context accepts at most {MAX_CONTEXT_LIST_VALUES} edge types"
                ));
            }
            for edge_type in edge_types {
                if parse_edge_kind(edge_type).is_none() {
                    return Err(format!(
                        "unknown task-context edge type `{edge_type}`; use a registered graph edge kind"
                    ));
                }
            }
        }
    }
    if context_mode == Some("task")
        && params
            .query
            .as_deref()
            .is_none_or(|query| query.trim().is_empty())
    {
        return Err("task context requires a non-empty query".to_string());
    }
    if context_mode == Some("graph-delta-beta")
        && params
            .proposal
            .as_deref()
            .is_none_or(|proposal| proposal.is_empty())
    {
        return Err("graph-delta-beta requires a proposal".to_string());
    }
    if params.proposal.is_some() && context_mode.is_none() {
        return Err("proposal requires context_mode=task or graph-delta-beta".to_string());
    }
    if params
        .context_roles
        .as_deref()
        .is_some_and(|roles| roles.iter().any(|role| role.trim() == "proposal_delta"))
        && params.proposal.is_none()
    {
        return Err("the proposal_delta task role requires a proposal".to_string());
    }
    if let Some(proposal) = params.proposal.as_deref() {
        if proposal.len() > MAX_PROPOSAL_BYTES {
            return Err(format!(
                "proposal exceeds the {MAX_PROPOSAL_BYTES}-byte hard limit"
            ));
        }
        if proposal.as_bytes().contains(&0) {
            return Err("proposal contains NUL bytes".to_string());
        }
    }
    if strict_semantic_requested(params)
        && (context_mode.is_some()
            || params.projection.is_some()
            || params.body_policy.is_some()
            || params.max_output_bytes.is_some()
            || params.max_output_tokens.is_some()
            || params.max_body_bytes.is_some()
            || params.max_total_body_bytes.is_some()
            || params.context_roles.is_some()
            || params.context_facets.is_some()
            || params.proposal.is_some())
    {
        return Err("strict semantic qualification forbids product context controls".to_string());
    }
    Ok(())
}

fn sealed_semantic_bundle() -> bool {
    cfg!(feature = "swebench-semantic-bundle")
        && option_env!("RNA_SEMANTIC_BUNDLE_BUILD") == Some("1")
}

const fn use_verified_reranker_loader(
    strict_semantic: bool,
    sealed_bundle: bool,
    asset_seeding: bool,
) -> bool {
    strict_semantic || (sealed_bundle && !asset_seeding)
}

fn semantic_asset_seeding() -> bool {
    std::env::var("RNA_SEMANTIC_ASSET_SEEDING").as_deref() == Ok("1")
}

fn strict_semantic_requested(params: &SearchParams) -> bool {
    // CI asset acquisition is deliberately non-qualifying. The embedding
    // layer refuses publication in this mode; service search must likewise
    // never emit the strict READY sentinel for its discarded seed state.
    if semantic_asset_seeding() {
        return false;
    }
    let explicit = params
        .search_mode
        .as_deref()
        .is_some_and(|mode| mode.trim().eq_ignore_ascii_case(STRICT_SEMANTIC_MODE));
    // The frozen #779 packet generator and the artifact's offline qualification
    // probe predate `search_mode=strict`, so eligible bare queries from a sealed
    // bundle must retain implicit strict behavior. Explicit product-context
    // controls opt into the separate non-strict projection pipeline.
    let sealed_bundle_default = sealed_semantic_bundle() && implicit_strict_request(params);
    explicit || sealed_bundle_default
}

fn implicit_strict_request(params: &SearchParams) -> bool {
    legacy_product_controls(params).is_none()
        && params.context_mode.is_none()
        && params.normalized_mode().is_none()
        && params
            .query
            .as_deref()
            .is_some_and(|query| !query.trim().is_empty())
        && params
            .node
            .as_deref()
            .is_none_or(|node| node.trim().is_empty())
        && params.nodes.as_ref().is_none_or(|nodes| nodes.is_empty())
}

fn strict_semantic_failure(reason: &str) -> String {
    format!(
        "Strict semantic qualification FAILED: `{reason}`. No lexical, graph, vector-only, CPU, or original-order fallback results were returned."
    )
}

fn candle_metal_fast_math_enabled() -> bool {
    std::env::var("CANDLE_METAL_ENABLE_FAST_MATH").as_deref() == Ok("1")
}

async fn strict_semantic_preflight(
    params: &SearchParams,
    ctx: &SearchContext<'_>,
) -> Result<(), &'static str> {
    if params.normalized_mode().is_some()
        || params
            .node
            .as_deref()
            .is_some_and(|node| !node.trim().is_empty())
        || params.nodes.as_ref().is_some_and(|nodes| !nodes.is_empty())
        || params.line.is_some()
        || params.end_line.is_some()
        || compiler_location(&params.file).is_some()
    {
        return Err("strict mode accepts only a flat query");
    }
    if params
        .query
        .as_deref()
        .is_none_or(|query| query.trim().is_empty())
    {
        return Err("strict mode requires a non-empty query");
    }
    if params.sort_by.is_some() {
        return Err("strict mode forbids a non-relevance sort override");
    }
    if params.search_mode.as_deref().is_some_and(|mode| {
        let mode = mode.trim();
        !mode.eq_ignore_ascii_case("hybrid") && !mode.eq_ignore_ascii_case(STRICT_SEMANTIC_MODE)
    }) {
        return Err("sealed bundle requires hybrid retrieval");
    }
    if !sealed_semantic_bundle() {
        return Err("binary is not the sealed CI SWE-bench semantic bundle");
    }
    if option_env!("RNA_METAL_KERNEL_PROFILE") != Some("release-fast-math")
        || !candle_metal_fast_math_enabled()
    {
        return Err("release Metal kernel optimization is not active");
    }
    let Some(index) = ctx.embed_index else {
        return Err("embedding index is not attached");
    };
    match index.has_table().await {
        Ok(true) => {}
        Ok(false) => return Err("embedding index is not ready"),
        Err(_) => return Err("embedding readiness validation failed"),
    }

    #[cfg(feature = "metal")]
    {
        crate::embed::require_metal_device().map_err(|_| "Metal device attestation failed")?;
        Ok(())
    }
    #[cfg(not(feature = "metal"))]
    {
        Err("Metal support is not compiled in")
    }
}

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
        let completed_repo_job = ctx
            .enrichment_jobs
            .iter()
            .filter(|job| {
                job.capability == EnrichmentCapability::CallReferences
                    && job.scope == EnrichmentScope::Repo
                    && job.state == EnrichmentJobState::Completed
                    && job.completed_at.is_some()
                    && job.failure.is_none()
            })
            .max_by_key(|job| (job.updated_at, job.revision));
        let completed_repo_order = completed_repo_job.map(|job| (job.updated_at, job.revision));
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
            let detail = degraded_job
                .lsp_evidence
                .as_ref()
                .and_then(|evidence| evidence.detail.as_deref())
                .or(degraded_job.failure.as_deref())
                .unwrap_or("call-reference enrichment finalized with degraded output");
            if let Some(evidence) = degraded_job.lsp_evidence.as_ref() {
                if degraded_job.scope == EnrichmentScope::Repo {
                    status.set_degraded_with_coverage(
                        degraded_job.counters.edge_count.unwrap_or(0),
                        persisted_lsp_edges,
                        detail,
                    );
                } else {
                    status.set_degraded_scoped(
                        degraded_job.counters.edge_count.unwrap_or(0),
                        persisted_lsp_edges,
                        evidence.scope.clone(),
                        detail,
                    );
                }
            } else {
                status.set_degraded(persisted_lsp_edges, detail);
            }
            Some(status)
        } else if let Some(completed_repo_job) = completed_repo_job {
            let status = LspEnrichmentStatus::default();
            if completed_repo_job
                .lsp_evidence
                .as_ref()
                .is_some_and(|evidence| {
                    evidence.readiness == crate::server::LspEvidenceReadiness::DefaultProfile
                })
            {
                status.set_complete_default_profile(
                    completed_repo_job.counters.edge_count.unwrap_or(0),
                    persisted_lsp_edges,
                    completed_repo_job
                        .lsp_evidence
                        .as_ref()
                        .and_then(|evidence| evidence.detail.clone())
                        .unwrap_or_else(|| "broad references were omitted".to_string()),
                );
            } else {
                status.set_complete(persisted_lsp_edges);
            }
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
                EnrichmentJobState::Failed
                    if job.lsp_evidence.as_ref().is_some_and(|evidence| {
                        evidence.readiness == crate::server::LspEvidenceReadiness::Unavailable
                    }) =>
                {
                    status.set_unavailable_with_detail(
                        job.lsp_evidence
                            .as_ref()
                            .and_then(|evidence| evidence.detail.as_deref())
                            .or(job.failure.as_deref())
                            .unwrap_or("call-reference evidence unavailable"),
                    )
                }
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
        "{}{}{}{}",
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
        format_lsp_completeness(ctx.repo_root, &gs.nodes, &gs.edges),
        format_enrichment_jobs(ctx),
    )
}

fn format_lsp_completeness(
    repo_root: &Path,
    nodes: &[crate::graph::Node],
    edges: &[crate::graph::Edge],
) -> String {
    match crate::lsp_completeness::load_readiness_check_with_graph(
        repo_root,
        crate::business_context::BusinessContextMode::Disabled,
        nodes,
        edges,
    ) {
        Ok(check) => {
            let blocked_paths = check
                .report
                .violations
                .iter()
                .filter_map(|violation| violation.path.as_deref())
                .collect::<std::collections::BTreeSet<_>>();
            let covered = if check.compatibility_violations.is_empty() {
                check
                .report
                .files
                .iter()
                .filter(|file| {
                    file.role.is_included() && !blocked_paths.contains(file.path.as_str())
                })
                .count()
            } else {
                0
            };
            format!(
                "\n- **benchmark per-file LSP completeness**: {} — {}/{} included files covered; {} violation(s); digest={}",
                if check.ready { "ready" } else { "partial/degraded" },
                covered,
                check.report.summary.included_files,
                check.report.violations.len() + check.compatibility_violations.len(),
                check.report.digest,
            )
        }
        Err(_) => "\n- **benchmark per-file LSP completeness**: unavailable — no persisted report; run a full LSP scan".to_string(),
    }
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
        let evidence = job
            .lsp_evidence
            .as_ref()
            .map(|evidence| {
                let validations = if evidence.validations.is_empty() {
                    String::new()
                } else {
                    let summaries = evidence
                        .validations
                        .iter()
                        .map(|validation| {
                            format!(
                                "{}/{}: {}",
                                validation.language,
                                validation.server_name,
                                validation.summary()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(" validation=[{summaries}]")
                };
                format!(
                    " evidence={} declared_nodes={} requests={}/{} elapsed_ms={}/{} circuit_open={}{}",
                    evidence.readiness.as_str(),
                    evidence.declared_node_count,
                    evidence.scheduled_requests,
                    evidence
                        .max_requests
                        .map_or_else(|| "n/a".to_string(), |value| value.to_string()),
                    evidence.elapsed_ms,
                    evidence
                        .max_duration_ms
                        .map_or_else(|| "n/a".to_string(), |value| value.to_string()),
                    evidence.circuit_open,
                    validations,
                )
            })
            .unwrap_or_default();
        lines.push(format!(
            "- `{}` {} {} scope={} phase={} updated={}{}{}",
            job.job_id,
            job.capability.as_str(),
            state,
            job.scope.stable_key(),
            phase,
            job.updated_at,
            evidence,
            failure
        ));
    }
    lines.join("\n")
}

/// Unified search entry point. Returns formatted markdown.
pub async fn search(params: &SearchParams, ctx: &SearchContext<'_>) -> String {
    if let Err(reason) = validate_search_experience(params) {
        return format!("Invalid search context: {reason}.");
    }
    let strict_semantic = strict_semantic_requested(params);
    if strict_semantic && let Err(reason) = strict_semantic_preflight(params, ctx).await {
        return strict_semantic_failure(reason);
    }

    // The frozen #779 path deliberately bypasses every new product policy and
    // keeps the established selection, ordering, and renderer byte-for-byte.
    if strict_semantic {
        return legacy_search_dispatch(params, ctx).await;
    }

    let normalized_params = normalize_product_context_controls(params);
    let params = &normalized_params;

    if let Some(node) = params.node.as_deref()
        && node.starts_with("rna-hydrate-v1:")
    {
        if params.normalized_mode().is_some()
            || params.nodes.as_ref().is_some_and(|nodes| !nodes.is_empty())
            || params.target_subsystem.is_some()
        {
            return "Invalid search context: hydration cannot be combined with legacy nodes/traversal/target_subsystem dispatch."
                .to_string();
        }
        return hydrate_from_handle(node, params, ctx).await;
    }

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

    let legacy_dispatch = params.context_mode.is_none()
        && (params.normalized_mode().is_some()
            || params
                .node
                .as_deref()
                .is_some_and(|node| !node.trim().is_empty())
            || params.nodes.as_ref().is_some_and(|nodes| !nodes.is_empty())
            || params.target_subsystem.is_some());
    if legacy_dispatch && let Some(controls) = legacy_product_controls(params) {
        return format!(
            "Invalid search context: product controls ({controls}) cannot be combined with legacy node/nodes/traversal/target_subsystem dispatch. Remove those controls or use flat/task context search."
        );
    }

    // Traversal and batch rendering retain their established contracts until
    // typed projection adapters exist. In particular, never reinterpret a
    // node/mode request as a flat product search.
    if legacy_dispatch {
        return legacy_search_dispatch(params, ctx).await;
    }

    projected_search(params, ctx).await
}

/// Return the stable names of product-only controls that legacy dispatch
/// cannot honor. Legacy requests with none of these controls retain their
/// established renderer and ordering; silently ignoring an explicit control
/// would otherwise make the response claim a contract it did not execute.
fn legacy_product_controls(params: &SearchParams) -> Option<String> {
    let controls = [
        params.projection.is_some().then_some("projection"),
        params.body_policy.is_some().then_some("body_policy"),
        params
            .max_output_bytes
            .is_some()
            .then_some("max_output_bytes"),
        params
            .max_output_tokens
            .is_some()
            .then_some("max_output_tokens"),
        params.max_body_bytes.is_some().then_some("max_body_bytes"),
        params
            .max_total_body_bytes
            .is_some()
            .then_some("max_total_body_bytes"),
        params.context_roles.is_some().then_some("context_roles"),
        params.context_facets.is_some().then_some("context_facets"),
        params.proposal.is_some().then_some("proposal"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    (!controls.is_empty()).then(|| controls.join(", "))
}

fn normalize_product_context_controls(params: &SearchParams) -> SearchParams {
    let mut normalized = params.clone();
    let normalize_scalar = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let normalize_list = |value: &Option<Vec<String>>| {
        let values = value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        (!values.is_empty()).then_some(values)
    };
    normalized.projection = normalize_scalar(&params.projection);
    normalized.body_policy = normalize_scalar(&params.body_policy);
    normalized.context_mode = normalize_scalar(&params.context_mode);
    normalized.context_roles = normalize_list(&params.context_roles);
    normalized.context_facets = normalize_list(&params.context_facets);
    normalized.edge_types = normalize_list(&params.edge_types);
    normalized
}

async fn legacy_search_dispatch(params: &SearchParams, ctx: &SearchContext<'_>) -> String {
    let query = nonempty_query_preserving_bytes(params.query.as_deref());
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

fn projection_request(params: &SearchParams, intent: SearchIntent) -> ProjectionRequest {
    let projection = match params.projection.as_deref() {
        Some("evidence") => SearchProjection::Evidence,
        _ => SearchProjection::Agent,
    };
    let body_policy = match params.body_policy.as_deref() {
        Some("complete") => BodyPolicy::Complete,
        Some("focused_span") => BodyPolicy::FocusedSpan,
        Some("signature_only") => BodyPolicy::SignatureOnly,
        Some("minified") => BodyPolicy::Minified,
        Some("none") => BodyPolicy::NoBody,
        _ if params.minify_body => BodyPolicy::Minified,
        _ if params.include_body => BodyPolicy::Complete,
        _ => BodyPolicy::SignatureOnly,
    };
    ProjectionRequest {
        intent,
        projection,
        body_policy,
        budget: ProjectionBudget {
            max_rendered_bytes: params.max_output_bytes,
            max_estimated_tokens: params.max_output_tokens,
            per_record_body_bytes: params.max_body_bytes,
            // The final renderer still enforces the complete output bound. This
            // early body bound prevents source alone from consuming all of it.
            total_body_bytes: params.max_total_body_bytes.or(params.max_output_bytes),
        },
    }
}

fn projection_source_reader(
    params: &SearchParams,
    repo_root: &Path,
) -> Result<source::SourceReader, String> {
    let mut all_roots = params.clone();
    all_roots.root = Some("all".to_string());
    source::SourceReader::new(
        source_roots(&all_roots, repo_root),
        source::SourceReadLimits::default(),
    )
    .map_err(|error| error.to_string())
}

async fn hydrate_from_handle(
    encoded: &str,
    params: &SearchParams,
    ctx: &SearchContext<'_>,
) -> String {
    let handle = match HydrationHandle::decode(encoded) {
        Ok(handle) => handle,
        Err(error) => return format!("Invalid hydration handle: {error}."),
    };
    match handle.kind {
        HydrationKind::Source => {
            let Some(page) = handle.source.clone() else {
                return "Invalid hydration handle: source target is missing.".to_string();
            };
            if let Some(span_id) = handle.record_id.strip_prefix("span:") {
                let authority = match ProjectionSourceSpan::from_stable_id(span_id) {
                    Ok(authority) => authority,
                    Err(error) => {
                        return format!("Hydration span authority is invalid: {error}.");
                    }
                };
                if !authority.contains(&page) {
                    return "Hydration page is outside its bound source authority.".to_string();
                }
                let reader = match projection_source_reader(params, ctx.repo_root) {
                    Ok(reader) => reader,
                    Err(error) => {
                        return format!("Hydration source projection unavailable: {error}.");
                    }
                };
                if let Err(error) = reader.read(&page) {
                    return format!("Hydration source validation failed: {error}.");
                }
                let selected =
                    selected_for_source_span_hydration(&handle.record_id, authority, page);
                let mut request = projection_request(params, SearchIntent::Hydrate);
                request.projection = SearchProjection::Agent;
                request.body_policy = BodyPolicy::FocusedSpan;
                return render_projected_input(
                    request,
                    ProjectionInput {
                        records: vec![selected],
                        ..Default::default()
                    },
                    params,
                    ctx,
                );
            }
            let node = ctx
                .graph_state
                .nodes
                .iter()
                .find(|node| node.stable_id() == handle.record_id);
            let Some(node) = node else {
                return "Hydration source target is no longer present in the graph.".to_string();
            };
            // If the record still exists, bind the handle to its authoritative
            // identity. A changed path/range fails closed instead of redirecting.
            let Some(authority) = node_source_span(node) else {
                return "Hydration source target has no current authoritative span.".to_string();
            };
            if !authority.contains(&page) {
                return "Hydration handle no longer matches the indexed record source.".to_string();
            }
            let selected = selected_for_hydration(node, &handle.record_id, authority, page);
            let mut request = projection_request(params, SearchIntent::Hydrate);
            request.projection = SearchProjection::Agent;
            request.body_policy = BodyPolicy::FocusedSpan;
            render_projected_input(
                request,
                ProjectionInput {
                    records: vec![selected],
                    ..Default::default()
                },
                params,
                ctx,
            )
        }
        HydrationKind::Evidence => {
            if handle
                .record_id
                .starts_with(SEMANTIC_EVIDENCE_CAPSULE_PREFIX)
            {
                return hydrate_semantic_evidence_capsule(&handle, params, ctx);
            }
            let (digest, node_id) = match parse_evidence_capsule_reference(&handle.record_id) {
                Ok(reference) => reference,
                Err(error) => return format!("Evidence hydration rejected: {error}."),
            };
            let directory = match evidence_capsule_directory(ctx.repo_root, false) {
                Ok(directory) => directory,
                Err(error) => return format!("Evidence hydration rejected: {error}."),
            };
            let path = directory.join(format!("{digest}.json"));
            let bytes = match read_evidence_capsule_bytes(&path) {
                Ok(bytes) => bytes,
                Err(error) => return format!("Evidence hydration rejected: {error}."),
            };
            if blake3::hash(&bytes).to_hex().as_str() != digest {
                return "Evidence hydration rejected: capsule digest mismatch.".to_string();
            }
            let capsule: EvidenceCapsuleV1 = match serde_json::from_slice(&bytes) {
                Ok(capsule) => capsule,
                Err(error) => {
                    return format!("Evidence hydration rejected: invalid capsule: {error}.");
                }
            };
            if capsule.schema_version != EVIDENCE_CAPSULE_SCHEMA_VERSION
                || capsule.record_id != node_id
                || capsule.query_digest.len() != 64
                || !capsule
                    .query_digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return "Evidence hydration rejected: capsule binding mismatch.".to_string();
            }
            let canonical = match serde_json::to_vec(&capsule) {
                Ok(canonical) => canonical,
                Err(error) => {
                    return format!(
                        "Evidence hydration rejected: capsule canonicalization failed: {error}."
                    );
                }
            };
            if canonical != bytes {
                return "Evidence hydration rejected: capsule is not canonical.".to_string();
            }
            let Some(node) = find_node(ctx.graph_state, node_id) else {
                return "Evidence hydration rejected: selected node is no longer present."
                    .to_string();
            };
            let current_hash = node_projection_digest(node);
            if capsule.current_node_content_hash != current_hash
                || capsule.evidence.content_hash.as_deref() != Some(current_hash.as_str())
            {
                return "Evidence hydration rejected: selected node content changed.".to_string();
            }
            let selected = SelectedRecord {
                selection_rank: capsule.selection_rank,
                identity: RecordIdentity {
                    node_id: node_id.to_string(),
                    source: node_source_span(node),
                },
                symbol: symbol_summary(node),
                selection: capsule.selection,
                evidence: capsule.evidence,
                evidence_hydration: Some(handle),
                focused_span: None,
            };
            let mut request = projection_request(params, SearchIntent::Hydrate);
            request.projection = SearchProjection::Evidence;
            request.body_policy = BodyPolicy::SignatureOnly;
            render_projected_input(
                request,
                ProjectionInput {
                    records: vec![selected],
                    ..Default::default()
                },
                params,
                ctx,
            )
        }
    }
}

fn hydrate_semantic_evidence_capsule(
    handle: &HydrationHandle,
    params: &SearchParams,
    ctx: &SearchContext<'_>,
) -> String {
    let (digest, record_id) = match parse_semantic_evidence_capsule_reference(&handle.record_id) {
        Ok(reference) => reference,
        Err(error) => return format!("Semantic evidence hydration rejected: {error}."),
    };
    let directory = match evidence_capsule_directory(ctx.repo_root, false) {
        Ok(directory) => directory,
        Err(error) => return format!("Semantic evidence hydration rejected: {error}."),
    };
    let bytes = match read_evidence_capsule_bytes(&directory.join(format!("{digest}.json"))) {
        Ok(bytes) => bytes,
        Err(error) => return format!("Semantic evidence hydration rejected: {error}."),
    };
    if blake3::hash(&bytes).to_hex().as_str() != digest {
        return "Semantic evidence hydration rejected: capsule digest mismatch.".into();
    }
    let capsule: SemanticEvidenceCapsuleV1 = match serde_json::from_slice(&bytes) {
        Ok(capsule) => capsule,
        Err(error) => {
            return format!("Semantic evidence hydration rejected: invalid capsule: {error}.");
        }
    };
    let body_hash = blake3::hash(capsule.body.as_bytes()).to_hex().to_string();
    if capsule.schema_version != EVIDENCE_CAPSULE_SCHEMA_VERSION
        || capsule.record_id != record_id
        || capsule.evidence.content_hash.as_deref() != Some(body_hash.as_str())
    {
        return "Semantic evidence hydration rejected: capsule binding mismatch.".into();
    }
    let canonical = match serde_json::to_vec(&capsule) {
        Ok(canonical) => canonical,
        Err(error) => {
            return format!(
                "Semantic evidence hydration rejected: canonicalization failed: {error}."
            );
        }
    };
    if canonical != bytes {
        return "Semantic evidence hydration rejected: capsule is not canonical.".into();
    }
    let mut symbol = capsule.symbol;
    symbol.signature = capsule.body;
    let mut selection = capsule.selection;
    selection.reason = format!(
        "{}; hydrated full content-addressed semantic row body",
        selection.reason
    );
    let selected = SelectedRecord {
        selection_rank: 0,
        identity: RecordIdentity {
            node_id: capsule.record_id,
            source: None,
        },
        symbol,
        selection,
        evidence: capsule.evidence,
        evidence_hydration: Some(handle.clone()),
        focused_span: None,
    };
    let mut request = projection_request(params, SearchIntent::Hydrate);
    request.projection = SearchProjection::Evidence;
    request.body_policy = BodyPolicy::SignatureOnly;
    render_projected_input(
        request,
        ProjectionInput {
            records: vec![selected],
            ..Default::default()
        },
        params,
        ctx,
    )
}

fn selected_for_source_span_hydration(
    record_id: &str,
    authority: ProjectionSourceSpan,
    page: ProjectionSourceSpan,
) -> SelectedRecord {
    let display = format!("{}:{}-{}", page.path, page.start_line, page.end_line);
    SelectedRecord {
        selection_rank: 0,
        identity: RecordIdentity {
            node_id: record_id.to_string(),
            source: Some(authority),
        },
        symbol: SymbolSummary {
            name: display.clone(),
            kind: "source_span".into(),
            language: "text".into(),
            signature: display,
            extraction_source: None,
            declared_metadata: BTreeMap::new(),
        },
        selection: SelectionSummary {
            channel: SelectionChannel::Exact,
            reason: "checksum-bound coalesced source hydration".into(),
            role: Some(ProjectionRole::EditableSource),
            lane: Some(ProjectionLane::ExactReference),
        },
        evidence: SelectionEvidence::default(),
        evidence_hydration: None,
        focused_span: Some(page),
    }
}

fn selected_for_hydration(
    node: &Node,
    record_id: &str,
    authority: ProjectionSourceSpan,
    page: ProjectionSourceSpan,
) -> SelectedRecord {
    let symbol = symbol_summary(node);
    SelectedRecord {
        selection_rank: 0,
        identity: RecordIdentity {
            node_id: record_id.to_string(),
            source: Some(authority),
        },
        symbol,
        selection: SelectionSummary {
            channel: SelectionChannel::Exact,
            reason: "verified source hydration handle".to_string(),
            role: Some(ProjectionRole::EditableSource),
            lane: Some(ProjectionLane::ExactReference),
        },
        evidence: SelectionEvidence::default(),
        evidence_hydration: None,
        focused_span: Some(page),
    }
}

async fn projected_search(params: &SearchParams, ctx: &SearchContext<'_>) -> String {
    let edge_index = ProjectedEdgeIndex::new(ctx.graph_state);
    if params.context_mode.as_deref() == Some("graph-delta-beta") {
        return projected_graph_delta(params, ctx, &edge_index).await;
    }
    let intent = if params.context_mode.as_deref() == Some("task") {
        SearchIntent::Implement
    } else {
        SearchIntent::Discover
    };
    let request = projection_request(params, intent);
    let (mut records, mut relationships, mut capabilities, mut omissions, mut candidate_audit) =
        if params.context_mode.as_deref() == Some("task") {
            match task_records(params, ctx, &edge_index).await {
                Ok(output) => (
                    output.records,
                    output.relationships,
                    output.capabilities,
                    output.omissions,
                    output.candidate_audit,
                ),
                Err(error) => return format!("Task context selection failed: {error}."),
            }
        } else {
            let (fused, capabilities, product_score_audit) =
                match projected_fused_candidates(params, ctx, &edge_index).await {
                    Ok(result) => result,
                    Err(error) => return format!("Search projection failed: {error}."),
                };
            let selected_limit = params.limit.unwrap_or(10);
            let records = fused
                .iter()
                .take(selected_limit)
                .enumerate()
                .filter_map(|(rank, fused)| {
                    find_node(ctx.graph_state, &fused.stable_id).map(|node| {
                        let mut selected = selected_from_fused(
                            node,
                            fused,
                            rank,
                            SelectionPlacement::default(),
                            params.query.as_deref(),
                            ctx.repo_root,
                        );
                        append_selected_product_score_audit(
                            &mut selected,
                            product_score_audit.get(&fused.stable_id).map(Vec::as_slice),
                            params.query.as_deref().unwrap_or_default(),
                            ctx.repo_root,
                        );
                        selected
                    })
                })
                .collect::<Vec<_>>();
            let selected_ids = records
                .iter()
                .map(|record| record.identity.node_id.as_str())
                .collect::<BTreeSet<_>>();
            let candidate_audit = fused
                .iter()
                .enumerate()
                .filter_map(|(rank, fused)| {
                    let node = find_node(ctx.graph_state, &fused.stable_id)?;
                    let selected = selected_ids.contains(fused.stable_id.as_str());
                    let mut audit = candidate_audit_from_fused(
                        node,
                        fused,
                        rank,
                        selected,
                        if selected {
                            "selected within requested result limit"
                        } else {
                            "omitted after bounded candidate fusion by requested result limit"
                        },
                    );
                    append_product_score_audit(
                        &mut audit.evidence,
                        product_score_audit.get(&fused.stable_id).map(Vec::as_slice),
                    );
                    Some(audit)
                })
                .collect();
            (
                records,
                Vec::new(),
                capabilities,
                Vec::new(),
                candidate_audit,
            )
        };
    if params.context_mode.is_none() {
        let (mut non_code, non_code_capabilities, mut non_code_audit) =
            projected_non_code_records(params, ctx, records.len()).await;
        records.append(&mut non_code);
        capabilities.extend(non_code_capabilities);
        candidate_audit.append(&mut non_code_audit);
    }
    records.retain(|record| {
        let hydratable = record.identity.source.is_some() || record.evidence_hydration.is_some();
        if !hydratable {
            omissions.push(ProjectionOmission {
                record_id: Some(record.identity.node_id.clone()),
                source: None,
                code: OmissionCode::MissingSource,
                detail: "selected semantic-only record has no independently verifiable source or evidence hydration handle".into(),
            });
        }
        hydratable
    });
    capabilities.push(evidence_capsule_capability(&records));
    let mut seen_records = BTreeSet::new();
    records.retain(|record| {
        seen_records.insert((record.identity.node_id.clone(), record.selection.role))
    });
    records.sort_by(|left, right| {
        left.selection_rank
            .cmp(&right.selection_rank)
            .then_with(|| left.identity.node_id.cmp(&right.identity.node_id))
            .then_with(|| left.selection.role.cmp(&right.selection.role))
    });
    capabilities.extend(default_capabilities(ctx, request.projection).await);
    capabilities = merge_capabilities(capabilities);
    relationships.extend(projected_relationships(&edge_index, &records));
    relationships.sort();
    relationships.dedup();
    candidate_audit.sort_by(|left, right| {
        left.candidate_rank
            .cmp(&right.candidate_rank)
            .then_with(|| left.identity.node_id.cmp(&right.identity.node_id))
    });
    render_projected_input(
        request,
        ProjectionInput {
            records,
            candidate_audit,
            relationships,
            omissions,
            capabilities,
        },
        params,
        ctx,
    )
}

async fn projected_non_code_records(
    params: &SearchParams,
    ctx: &SearchContext<'_>,
    rank_offset: usize,
) -> (
    Vec<SelectedRecord>,
    Vec<CapabilityStatus>,
    Vec<CandidateAudit>,
) {
    let query = nonempty_query_preserving_bytes(params.query.as_deref()).unwrap_or("");
    if !params.include_markdown && !params.include_artifacts {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let limit = params.limit.unwrap_or(10).min(100);
    let mut capabilities = Vec::new();
    let mut orders = BTreeMap::<EvidenceChannel, Vec<String>>::new();
    let mut synthetic = Vec::<(SearchResult, EvidenceChannel, usize)>::new();
    let mut product_score_audit = BTreeMap::<String, Vec<ProductScoreAudit>>::new();
    let (mut live_markdown, mut live_markdown_audit) =
        projected_live_markdown_records(params, ctx, query, rank_offset);
    let live_markdown_sources = live_markdown
        .iter()
        .filter_map(|record| {
            record
                .identity
                .source
                .as_ref()
                .map(ProjectionSourceSpan::stable_id)
        })
        .collect::<BTreeSet<_>>();

    let mut lexical = ctx
        .graph_state
        .nodes
        .iter()
        .filter(|node| node_delivery_class(node) != NodeDeliveryClass::Code)
        .filter(|node| projected_node_passes(node, params, ctx))
        .filter_map(|node| {
            let score = if query.is_empty() {
                1.0
            } else {
                non_code_lexical_score(node, query)
            };
            (score > 0.0).then_some((node.stable_id(), score))
        })
        .collect::<Vec<_>>();
    lexical.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    lexical.truncate(limit.saturating_mul(3).min(100));
    if !lexical.is_empty() {
        orders.insert(
            EvidenceChannel::ExactLexical,
            lexical.into_iter().map(|(id, _)| id).collect(),
        );
    }

    if let Some(embed_index) = ctx.embed_index {
        let search_mode = parse_search_mode(params.search_mode.as_deref());
        let filters = SearchFilters {
            subsystem: params.subsystem.clone(),
            file: params.file.clone(),
            language: params.language.clone(),
            min_complexity: params.min_complexity,
        };
        let over_fetch = limit.saturating_mul(5).clamp(20, 100);
        let mut requests = Vec::new();
        if params.include_markdown {
            requests.push((
                "markdown_search",
                Some(vec!["code:markdown_section".to_string()]),
                NodeDeliveryClass::Markdown,
            ));
        }
        if params.include_artifacts {
            requests.push((
                "artifact_search",
                params.artifact_types.clone(),
                NodeDeliveryClass::Artifact,
            ));
        }
        for (capability, artifact_types, class) in requests {
            let scorer = {
                let embed_index = embed_index.clone();
                let query = query.to_owned();
                let filters = filters.clone();
                async move {
                    embed_index
                        .search_with_filters_observed(
                            &query,
                            artifact_types.as_deref(),
                            over_fetch,
                            search_mode,
                            &filters,
                            TestResultPolicy::Neutral,
                        )
                        .await
                }
            };
            match isolate_embedding_scorer(scorer, search_mode).await {
                Ok(ObservedSearchOutcome {
                    outcome: SearchOutcome::Results(results),
                    executed_mode,
                    score_provenance,
                }) => {
                    let channel = executed_mode
                        .map(evidence_channel_for_executed_mode)
                        .unwrap_or(EvidenceChannel::Vector);
                    merge_product_score_audit(
                        &mut product_score_audit,
                        aligned_product_score_audit(&results, score_provenance, channel),
                    );
                    let mut graph_order = Vec::new();
                    for result in results {
                        let result_class = embedding_result_delivery_class(&result);
                        if result_class != class
                            || !search_result_passes_root_filter(
                                &result,
                                &ctx.root_filter,
                                &ctx.non_code_slugs,
                            )
                        {
                            continue;
                        }
                        if let Some(node) = find_node(ctx.graph_state, &result.id) {
                            if projected_node_passes(node, params, ctx)
                                && node_delivery_class(node) == class
                            {
                                graph_order.push(node.stable_id());
                            }
                        } else if class == NodeDeliveryClass::Artifact {
                            let rank = synthetic.len() + 1;
                            synthetic.push((result, channel, rank));
                        }
                    }
                    if !graph_order.is_empty() {
                        orders.entry(channel).or_default().extend(graph_order);
                    }
                    capabilities.push(CapabilityStatus {
                        capability: capability.into(),
                        state: CapabilityState::Ready,
                        detail: format!(
                            "embedding query executed as {}",
                            executed_mode.map_or("unknown", |mode| match mode {
                                ExecutedSearchMode::Keyword => "keyword",
                                ExecutedSearchMode::Semantic => "semantic",
                                ExecutedSearchMode::HybridRrf => "hybrid_rrf",
                            })
                        ),
                    });
                }
                Ok(ObservedSearchOutcome {
                    outcome: SearchOutcome::NotReady,
                    ..
                }) => capabilities.push(CapabilityStatus {
                    capability: capability.into(),
                    state: CapabilityState::Unavailable,
                    detail: "embedding table is not ready; bounded source lexical evidence used"
                        .into(),
                }),
                Err(diagnostic) => capabilities.push(CapabilityStatus {
                    capability: capability.into(),
                    state: CapabilityState::Degraded,
                    detail: diagnostic.render(),
                }),
            }
        }
    }

    for order in orders.values_mut() {
        let mut seen = BTreeSet::new();
        order.retain(|id| seen.insert(id.clone()));
        order.truncate(limit.saturating_mul(3).min(100));
    }
    let channels = orders
        .into_iter()
        .filter(|(_, order)| !order.is_empty())
        .map(|(channel, order)| {
            ChannelInput::new(
                channel,
                ScoreKind::WithinChannelRank,
                order
                    .into_iter()
                    .enumerate()
                    .map(|(rank, id)| RawCandidateScore::new(id, (rank + 1) as f64))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let fused = fuse_ranked_channels(FusionPolicy::ordinary_search(), &channels)
        .unwrap_or_else(|_| Vec::new());
    let mut superseded_markdown = BTreeSet::new();
    let mut records = fused
        .iter()
        .take(limit)
        .enumerate()
        .filter_map(|(rank, fused)| {
            let node = find_node(ctx.graph_state, &fused.stable_id)?;
            if node_delivery_class(node) == NodeDeliveryClass::Markdown
                && node_source_span(node)
                    .as_ref()
                    .is_some_and(|source| live_markdown_sources.contains(&source.stable_id()))
            {
                superseded_markdown.insert(fused.stable_id.clone());
                return None;
            }
            let mut selected = selected_from_fused(
                node,
                fused,
                rank_offset + rank,
                SelectionPlacement::default(),
                Some(query),
                ctx.repo_root,
            );
            append_selected_product_score_audit(
                &mut selected,
                product_score_audit.get(&fused.stable_id).map(Vec::as_slice),
                query,
                ctx.repo_root,
            );
            Some(selected)
        })
        .collect::<Vec<_>>();
    for record in &mut live_markdown {
        record.selection_rank = rank_offset + records.len();
        records.push(record.clone());
    }
    let mut candidate_audit = Vec::new();
    let mut synthetic_seen = BTreeSet::new();
    for (result, channel, channel_rank) in synthetic {
        let audit_rank = rank_offset + channel_rank.saturating_sub(1);
        if records.len() >= limit.saturating_mul(2)
            || !synthetic_seen.insert((result.kind.clone(), result.id.clone()))
        {
            let mut audit = candidate_audit_from_embedding_result(
                &result,
                channel,
                audit_rank,
                false,
                "omitted after bounded non-code candidate selection limit or duplicate identity",
            );
            append_product_score_audit(
                &mut audit.evidence,
                product_score_audit.get(&result.id).map(Vec::as_slice),
            );
            candidate_audit.push(audit);
            continue;
        }
        let mut audit_seed = candidate_audit_from_embedding_result(
            &result,
            channel,
            audit_rank,
            true,
            "selected from bounded semantic artifact candidates",
        );
        append_product_score_audit(
            &mut audit_seed.evidence,
            product_score_audit.get(&result.id).map(Vec::as_slice),
        );
        let result_id = result.id.clone();
        match selected_from_embedding_result(
            result,
            channel,
            channel_rank,
            rank_offset + records.len(),
            ctx.repo_root,
            product_score_audit.get(&result_id).map(Vec::as_slice),
        ) {
            Ok(record) => {
                records.push(record);
                candidate_audit.push(audit_seed);
            }
            Err(error) => {
                candidate_audit.push(CandidateAudit {
                    disposition: CandidateDisposition::Omitted,
                    reason: format!(
                        "omitted because semantic evidence could not be sealed: {error}"
                    ),
                    ..audit_seed
                });
                capabilities.push(CapabilityStatus {
                    capability: "semantic_artifact_hydration".into(),
                    state: CapabilityState::Degraded,
                    detail: format!("semantic-only artifact was omitted: {error}"),
                });
            }
        }
    }
    let selected_ids = records
        .iter()
        .map(|record| record.identity.node_id.as_str())
        .collect::<BTreeSet<_>>();
    candidate_audit.extend(fused.iter().enumerate().filter_map(|(rank, candidate)| {
        let node = find_node(ctx.graph_state, &candidate.stable_id)?;
        let selected = selected_ids.contains(candidate.stable_id.as_str());
        let superseded = superseded_markdown.contains(candidate.stable_id.as_str());
        let mut audit = candidate_audit_from_fused(
            node,
            candidate,
            rank_offset + rank,
            selected,
            if selected {
                "selected from bounded non-code fused candidates"
            } else if superseded {
                "omitted because current source-ranked Markdown supersedes the indexed duplicate"
            } else {
                "omitted by the non-code result limit after bounded fusion"
            },
        );
        append_product_score_audit(
            &mut audit.evidence,
            product_score_audit
                .get(&candidate.stable_id)
                .map(Vec::as_slice),
        );
        Some(audit)
    }));
    candidate_audit.append(&mut live_markdown_audit);
    (records, capabilities, candidate_audit)
}

fn projected_live_markdown_records(
    params: &SearchParams,
    ctx: &SearchContext<'_>,
    query: &str,
    rank_offset: usize,
) -> (Vec<SelectedRecord>, Vec<CandidateAudit>) {
    if !params.include_markdown {
        return (Vec::new(), Vec::new());
    }
    let Some(chunks) = admitted_live_markdown_chunks(ctx) else {
        return (Vec::new(), Vec::new());
    };
    let chunks = chunks
        .into_iter()
        .filter(|chunk| live_markdown_chunk_passes(chunk, params))
        .collect::<Vec<_>>();
    let limit = params.limit.unwrap_or(10).min(100);
    let considered_limit = limit.saturating_mul(3).min(100);
    let roots = source_roots(params, ctx.repo_root);
    let scored = crate::markdown::search_chunks_ranked(&chunks, query);
    let mut records = Vec::new();
    let mut audit = Vec::new();
    for (rank, scored) in scored.into_iter().take(considered_limit).enumerate() {
        let Some(source) = markdown_chunk_source_span(scored.chunk, &roots) else {
            continue;
        };
        let selected = records.len() < limit;
        let record_id = source.stable_id();
        let candidate_rank = rank_offset + rank + 1;
        let signature = {
            let section = scored.chunk.section_path();
            if section.is_empty() {
                scored.chunk.file_path.to_string_lossy().into_owned()
            } else {
                section
            }
        };
        let language = match scored
            .chunk
            .file_path
            .extension()
            .and_then(|extension| extension.to_str())
        {
            Some(extension) if extension.eq_ignore_ascii_case("rst") => "rst",
            _ => "markdown",
        };
        let evidence = SelectionEvidence {
            raw_scores: BTreeMap::from([(
                "markdown.source_ranked_score".to_string(),
                scored.score.to_string(),
            )]),
            content_hash: Some(
                blake3::hash(scored.chunk.content.as_bytes())
                    .to_hex()
                    .to_string(),
            ),
            candidate_rank: Some(candidate_rank),
            provenance: vec![EvidenceProvenance {
                source: "live_markdown".into(),
                detail: "current root-confined source ranked by heading/body lexical evidence"
                    .into(),
            }],
            diagnostics: BTreeMap::new(),
        };
        audit.push(CandidateAudit {
            candidate_rank,
            identity: RecordIdentity {
                node_id: record_id.clone(),
                source: Some(source.clone()),
            },
            disposition: if selected {
                CandidateDisposition::Selected
            } else {
                CandidateDisposition::Omitted
            },
            reason: if selected {
                "selected from current source-ranked Markdown".into()
            } else {
                "omitted by the bounded live Markdown result limit".into()
            },
            evidence: evidence.clone(),
        });
        if selected {
            records.push(SelectedRecord {
                selection_rank: rank_offset + records.len(),
                identity: RecordIdentity {
                    node_id: record_id,
                    source: Some(source),
                },
                symbol: SymbolSummary {
                    name: if scored.chunk.heading_text.is_empty() {
                        scored.chunk.file_path.to_string_lossy().into_owned()
                    } else {
                        scored.chunk.heading_text.clone()
                    },
                    kind: "markdown_section".into(),
                    language: language.into(),
                    signature,
                    extraction_source: None,
                    declared_metadata: BTreeMap::new(),
                },
                selection: SelectionSummary {
                    channel: SelectionChannel::Markdown,
                    reason: format!(
                        "current source-ranked Markdown at within-channel rank {}",
                        rank + 1
                    ),
                    role: Some(ProjectionRole::DefinitionOrApiState),
                    lane: Some(ProjectionLane::DefinitionOrState),
                },
                evidence,
                evidence_hydration: None,
                focused_span: None,
            });
        }
    }
    (records, audit)
}

fn live_markdown_chunk_passes(chunk: &crate::types::MarkdownChunk, params: &SearchParams) -> bool {
    let language = match chunk
        .file_path
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some(extension) if extension.eq_ignore_ascii_case("rst") => "rst",
        _ => "markdown",
    };
    params
        .kind
        .as_ref()
        .is_none_or(|kind| kind.eq_ignore_ascii_case("markdown_section"))
        && params
            .language
            .as_ref()
            .is_none_or(|expected| language.eq_ignore_ascii_case(expected))
        && params.file.as_ref().is_none_or(|file| {
            chunk
                .file_path
                .to_string_lossy()
                .replace('\\', "/")
                .contains(file)
        })
        && params.min_complexity.is_none()
        && params.synthetic.is_none()
        && params.subsystem.is_none()
}

fn markdown_chunk_source_span(
    chunk: &crate::types::MarkdownChunk,
    roots: &[(String, PathBuf)],
) -> Option<ProjectionSourceSpan> {
    let (root, _, relative) = roots
        .iter()
        .filter_map(|(root, path)| {
            chunk
                .file_path
                .strip_prefix(path)
                .ok()
                .map(|relative| (root, path, relative))
        })
        .max_by_key(|(_, path, _)| path.components().count())?;
    let content = fs::read(&chunk.file_path).ok()?;
    let end_offset = chunk.byte_offset.checked_add(chunk.byte_len)?;
    if end_offset > content.len() {
        return None;
    }
    let start_line = content[..chunk.byte_offset]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        .checked_add(1)?;
    let end_line = content[..end_offset]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        .checked_add(1)?;
    let span = ProjectionSourceSpan {
        root: root.clone(),
        path: relative.to_string_lossy().replace('\\', "/"),
        start_line: u32::try_from(start_line).ok()?,
        end_line: u32::try_from(end_line).ok()?,
    };
    span.is_valid().then_some(span)
}

fn candidate_audit_from_embedding_result(
    result: &SearchResult,
    channel: EvidenceChannel,
    rank: usize,
    selected: bool,
    reason: &str,
) -> CandidateAudit {
    CandidateAudit {
        candidate_rank: rank + 1,
        identity: RecordIdentity {
            node_id: format!("artifact:{}:{}", result.kind, result.id),
            source: None,
        },
        disposition: if selected {
            CandidateDisposition::Selected
        } else {
            CandidateDisposition::Omitted
        },
        reason: reason.into(),
        evidence: SelectionEvidence {
            raw_scores: BTreeMap::from([(
                format!("{}.adjusted_product_score", channel.label()),
                result.score.to_string(),
            )]),
            content_hash: Some(blake3::hash(result.body.as_bytes()).to_hex().to_string()),
            candidate_rank: Some(rank + 1),
            provenance: vec![EvidenceProvenance {
                source: channel.label().into(),
                detail: "bounded semantic-only non-code candidate".into(),
            }],
            diagnostics: BTreeMap::new(),
        },
    }
}

fn non_code_lexical_score(node: &Node, query: &str) -> f64 {
    let base = lexical_score(node, query);
    if base > 1.0 {
        return base;
    }
    let body = node.body.to_ascii_lowercase();
    let terms = query_terms(query);
    let matches = terms.iter().filter(|term| body.contains(*term)).count();
    if matches == 0 {
        0.0
    } else {
        20.0 + matches as f64
    }
}

fn embedding_result_delivery_class(result: &SearchResult) -> NodeDeliveryClass {
    if result.kind == "code:markdown_section" {
        NodeDeliveryClass::Markdown
    } else if result.kind.starts_with("code:") {
        NodeDeliveryClass::Code
    } else {
        NodeDeliveryClass::Artifact
    }
}

fn selected_from_embedding_result(
    result: SearchResult,
    channel: EvidenceChannel,
    channel_rank: usize,
    selection_rank: usize,
    repo_root: &Path,
    score_audit: Option<&[ProductScoreAudit]>,
) -> Result<SelectedRecord, String> {
    let node_id = format!("artifact:{}:{}", result.kind, result.id);
    let signature = result.body.chars().take(2_048).collect::<String>();
    let mut raw_scores = BTreeMap::new();
    raw_scores.insert(
        format!("{}.adjusted_product_score", channel.label()),
        result.score.to_string(),
    );
    let mut selected = SelectedRecord {
        selection_rank,
        identity: RecordIdentity {
            node_id,
            source: None,
        },
        symbol: SymbolSummary {
            name: result.title,
            kind: result.kind,
            language: "text".into(),
            signature,
            extraction_source: None,
            declared_metadata: BTreeMap::new(),
        },
        selection: SelectionSummary {
            channel: SelectionChannel::Artifact,
            reason: format!(
                "semantic artifact observation via {} at within-channel rank {channel_rank}",
                channel.label()
            ),
            role: Some(ProjectionRole::DefinitionOrApiState),
            lane: Some(ProjectionLane::DefinitionOrState),
        },
        evidence: SelectionEvidence {
            raw_scores,
            content_hash: Some(blake3::hash(result.body.as_bytes()).to_hex().to_string()),
            candidate_rank: Some(channel_rank),
            provenance: vec![EvidenceProvenance {
                source: channel.label().into(),
                detail: "semantic-only artifact row; no graph identity was fabricated".into(),
            }],
            diagnostics: BTreeMap::new(),
        },
        evidence_hydration: None,
        focused_span: None,
    };
    append_product_score_audit(&mut selected.evidence, score_audit);
    selected.evidence_hydration = Some(persist_semantic_evidence_capsule(
        repo_root,
        &selected,
        &result.body,
    )?);
    Ok(selected)
}

fn evidence_capsule_capability(records: &[SelectedRecord]) -> CapabilityStatus {
    let available = records
        .iter()
        .filter(|record| record.evidence_hydration.is_some())
        .count();
    let (state, detail) = if records.is_empty() {
        (
            CapabilityState::Unavailable,
            "no selected records required evidence capsules".to_string(),
        )
    } else if available == records.len() {
        (
            CapabilityState::Ready,
            format!(
                "{available}/{} selected records have checksum-bound repo-native evidence capsules",
                records.len()
            ),
        )
    } else {
        (
            CapabilityState::Degraded,
            format!(
                "{available}/{} selected records have checksum-bound repo-native evidence capsules",
                records.len()
            ),
        )
    };
    CapabilityStatus {
        capability: "evidence_hydration".into(),
        state,
        detail,
    }
}

const TASK_LANE_CANDIDATE_LIMIT: usize = 12;
const TASK_GRAPH_CANDIDATE_LIMIT: usize = 64;

#[derive(Default, Clone)]
struct TaskAdapterOutput {
    records: Vec<SelectedRecord>,
    relationships: Vec<ProjectedRelationship>,
    capabilities: Vec<CapabilityStatus>,
    omissions: Vec<ProjectionOmission>,
    candidate_audit: Vec<CandidateAudit>,
}

struct TaskAssembly {
    fused: FusedCandidate,
    roles: BTreeSet<TaskRole>,
    lanes: BTreeSet<TaskLane>,
    facets: BTreeSet<TaskFacet>,
    exact_reference: Option<String>,
    channel_rank: u32,
    reason: String,
}

async fn task_records(
    params: &SearchParams,
    ctx: &SearchContext<'_>,
    edge_index: &ProjectedEdgeIndex<'_>,
) -> Result<TaskAdapterOutput, String> {
    let task = task_context::parse_task(params.query.as_deref().unwrap_or_default())
        .map_err(|error| error.to_string())?;
    let candidate_nodes: BTreeMap<_, _> = ctx
        .graph_state
        .nodes
        .iter()
        .filter(|node| projected_node_passes(node, params, ctx))
        .map(|node| (node.stable_id(), node))
        .collect();
    let exact_candidates = candidate_nodes
        .iter()
        .map(|(evidence_id, node)| ExactCandidate {
            evidence_id: evidence_id.clone(),
            display: node.id.name.clone(),
            match_keys: BTreeSet::from([
                node.id.name.clone(),
                node.signature.clone(),
                node.stable_id(),
            ]),
            source_file: node.id.file.to_string_lossy().replace('\\', "/"),
            source_line: u32::try_from(node.line_start).ok(),
        })
        .collect::<Vec<_>>();

    let mut output = TaskAdapterOutput::default();
    let mut assemblies = BTreeMap::<String, TaskAssembly>::new();
    let mut product_score_audit = BTreeMap::<String, Vec<ProductScoreAudit>>::new();
    let resolutions =
        task_context::resolve_exact_references(&task.exact_references, &exact_candidates);
    let mut exact_hits = 0usize;
    for resolution in resolutions {
        match resolution.resolution {
            ExactResolution::Hit(candidate) => {
                exact_hits += 1;
                let Some(node) = candidate_nodes.get(&candidate.evidence_id).copied() else {
                    output.omissions.push(ProjectionOmission {
                        record_id: Some(candidate.evidence_id),
                        source: None,
                        code: OmissionCode::MissingSource,
                        detail: format!(
                            "exact reference {:?} resolved outside the filtered task graph",
                            resolution.reference.raw
                        ),
                    });
                    continue;
                };
                let roles = exact_task_roles(node);
                let fused = single_channel_fused(
                    &candidate.evidence_id,
                    EvidenceChannel::ExactLexical,
                    ScoreKind::ExactMatchTier,
                );
                for role in roles {
                    merge_task_assembly(
                        &mut assemblies,
                        fused.clone(),
                        role,
                        TaskLane::ExactReference,
                        task_facet_for_role(role),
                        Some(resolution.reference.raw.clone()),
                        0,
                        format!(
                            "exact reference {:?} resolved as eligible role {role:?} across the full filtered graph",
                            resolution.reference.raw
                        ),
                    );
                }
            }
            ExactResolution::Ambiguous(candidates) => output.omissions.push(ProjectionOmission {
                record_id: None,
                source: None,
                code: OmissionCode::MissingSource,
                detail: format!(
                    "exact reference {:?} is ambiguous across {} graph records: {}",
                    resolution.reference.raw,
                    candidates.len(),
                    candidates
                        .iter()
                        .map(|candidate| candidate.evidence_id.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            }),
            ExactResolution::Miss => output.omissions.push(ProjectionOmission {
                record_id: None,
                source: None,
                code: OmissionCode::MissingSource,
                detail: format!(
                    "exact reference {:?} did not resolve in the full filtered graph",
                    resolution.reference.raw
                ),
            }),
        }
    }
    output.capabilities.push(CapabilityStatus {
        capability: "task_exact_reference_resolution".into(),
        state: if exact_hits == task.exact_references.len() {
            CapabilityState::Ready
        } else {
            CapabilityState::Degraded
        },
        detail: format!(
            "resolved {exact_hits}/{} prose exact references against {} root-filtered graph records before ranked retrieval",
            task.exact_references.len(),
            exact_candidates.len()
        ),
    });

    let requested_facets = requested_task_facets(params, &task.facets);
    let lane_specs = [
        (
            TaskFacet::Behavior,
            TaskRole::EditableSource,
            TaskLane::EditableSource,
            "behavior implementation editable source",
        ),
        (
            TaskFacet::ApiOrState,
            TaskRole::DefinitionOrApiState,
            TaskLane::DefinitionOrState,
            "API contract state definition",
        ),
        (
            TaskFacet::Test,
            TaskRole::Test,
            TaskLane::Tests,
            "tests regression assertions",
        ),
        (
            TaskFacet::Analogue,
            TaskRole::BehavioralAnalogue,
            TaskLane::Analogues,
            "existing behavioral analogue precedent",
        ),
    ];
    let base_query = params.query.as_deref().unwrap_or_default();
    for (facet, role, lane, qualifier) in lane_specs {
        if !requested_facets.contains(&facet) {
            continue;
        }
        let mut lane_params = params.clone();
        lane_params.query = Some(format!("{base_query}\n{qualifier}"));
        lane_params.limit = Some(TASK_LANE_CANDIDATE_LIMIT);
        lane_params.node = None;
        lane_params.nodes = None;
        lane_params.mode = None;
        let (fused, capabilities, lane_score_audit) =
            projected_fused_candidates(&lane_params, ctx, edge_index).await?;
        merge_product_score_audit(&mut product_score_audit, lane_score_audit);
        let observed = fused.len().min(TASK_LANE_CANDIDATE_LIMIT);
        let mut eligible = 0usize;
        let mut rejected = 0usize;
        for (rank, candidate) in fused
            .into_iter()
            .take(TASK_LANE_CANDIDATE_LIMIT)
            .enumerate()
        {
            let Some(node) = candidate_nodes.get(&candidate.stable_id).copied() else {
                rejected += 1;
                continue;
            };
            match task_lane_candidate_evidence(node, role, &assemblies, ctx.graph_state, edge_index)
            {
                Ok(role_evidence) => {
                    eligible += 1;
                    merge_task_assembly(
                        &mut assemblies,
                        candidate,
                        role,
                        lane,
                        facet,
                        None,
                        u32::try_from(rank + 1).unwrap_or(u32::MAX),
                        format!(
                            "independent {lane:?} retrieval lane rank {}; {role_evidence}",
                            rank + 1
                        ),
                    );
                }
                Err(reason) => {
                    rejected += 1;
                    let audit_rank = output.candidate_audit.len();
                    let mut audit = candidate_audit_from_fused(
                        node,
                        &candidate,
                        audit_rank,
                        false,
                        &format!("ineligible for independent {lane:?} lane: {reason}"),
                    );
                    append_product_score_audit(
                        &mut audit.evidence,
                        product_score_audit
                            .get(&candidate.stable_id)
                            .map(Vec::as_slice),
                    );
                    output.candidate_audit.push(audit);
                }
            }
        }
        let degraded = capabilities
            .iter()
            .any(|capability| capability.state != CapabilityState::Ready);
        output.capabilities.extend(capabilities);
        output.capabilities.push(CapabilityStatus {
            capability: format!("task_lane_{}", projection_lane_for_task(lane)),
            state: if eligible == 0 || degraded {
                CapabilityState::Degraded
            } else {
                CapabilityState::Ready
            },
            detail: format!(
                "independently queried {observed} bounded candidates; retained {eligible} role-eligible source-grounded candidates; rejected {rejected}"
            ),
        });
        if eligible == 0 {
            output.omissions.push(ProjectionOmission {
                record_id: None,
                source: None,
                code: OmissionCode::MissingSource,
                detail: format!(
                    "independent {lane:?} task lane had no role-eligible source-grounded evidence"
                ),
            });
        }
    }

    if requested_facets.contains(&TaskFacet::Proposal) {
        let proposal_query = params.proposal.as_deref().unwrap_or(base_query);
        let mut proposal_params = params.clone();
        proposal_params.query = Some(proposal_query.to_string());
        proposal_params.limit = Some(TASK_LANE_CANDIDATE_LIMIT);
        proposal_params.node = None;
        proposal_params.nodes = None;
        proposal_params.mode = None;
        let (fused, capabilities, proposal_score_audit) =
            projected_fused_candidates(&proposal_params, ctx, edge_index).await?;
        merge_product_score_audit(&mut product_score_audit, proposal_score_audit);
        let observed = fused.len().min(TASK_LANE_CANDIDATE_LIMIT);
        for (rank, candidate) in fused
            .into_iter()
            .take(TASK_LANE_CANDIDATE_LIMIT)
            .enumerate()
        {
            merge_task_assembly(
                &mut assemblies,
                candidate,
                TaskRole::ProposalDelta,
                TaskLane::ProposalDelta,
                TaskFacet::Proposal,
                None,
                u32::try_from(rank + 1).unwrap_or(u32::MAX),
                format!("independent proposal retrieval lane rank {}", rank + 1),
            );
        }
        output.capabilities.extend(capabilities);
        output.capabilities.push(CapabilityStatus {
            capability: "task_lane_proposal_delta".into(),
            state: if observed == 0 {
                CapabilityState::Degraded
            } else {
                CapabilityState::Ready
            },
            detail: format!(
                "independently queried proposal evidence and retained {observed} bounded candidates"
            ),
        });
        match live_graph_delta_card(params, ctx, edge_index) {
            Ok(card) => {
                for (rank, impact) in card.impacted_loci.iter().enumerate() {
                    let Ok(node) = graph_delta_grounding_node(&impact.grounding, ctx) else {
                        continue;
                    };
                    merge_task_assembly(
                        &mut assemblies,
                        single_channel_fused(
                            &node.stable_id(),
                            EvidenceChannel::Graph,
                            ScoreKind::GraphHeuristic,
                        ),
                        TaskRole::ProposalDelta,
                        TaskLane::ProposalDelta,
                        TaskFacet::Proposal,
                        None,
                        u32::try_from(rank + 1).unwrap_or(u32::MAX),
                        format!(
                            "reused live graph-delta affected locus {} ({:?})",
                            impact.label, impact.kind
                        ),
                    );
                }
                for changed in &card.changed_edges {
                    output.relationships.push(ProjectedRelationship {
                        from: changed.edge.key.from.clone(),
                        kind: format!("graph_delta_{:?}_{}", changed.change, changed.edge.key.kind)
                            .to_ascii_lowercase(),
                        to: changed.edge.key.to.clone(),
                        reason: "task proposal_delta reused the live graph-delta adapter".into(),
                    });
                }
                output
                    .capabilities
                    .extend(card.capabilities.iter().map(|report| CapabilityStatus {
                        capability: format!(
                            "task_proposal_{}",
                            graph_delta_capability_name(report.capability)
                        ),
                        state: match report.state {
                            graph_delta::CapabilityState::Ready => CapabilityState::Ready,
                            graph_delta::CapabilityState::Degraded => CapabilityState::Degraded,
                            graph_delta::CapabilityState::Unavailable => {
                                CapabilityState::Unavailable
                            }
                        },
                        detail: report.detail.clone(),
                    }));
                output
                    .omissions
                    .extend(card.omissions.iter().map(|omission| {
                        ProjectionOmission {
                            record_id: omission.hydration_key.clone(),
                            source: omission
                                .grounding
                                .as_ref()
                                .and_then(graph_delta_projection_span),
                            code: OmissionCode::MissingSource,
                            detail: format!(
                                "task proposal graph-delta {:?}: {}",
                                omission.code, omission.detail
                            ),
                        }
                    }));
            }
            Err(error) => {
                output.capabilities.push(CapabilityStatus {
                    capability: "task_proposal_graph_delta".into(),
                    state: CapabilityState::Degraded,
                    detail: error.clone(),
                });
                output.omissions.push(ProjectionOmission {
                    record_id: None,
                    source: None,
                    code: OmissionCode::MissingSource,
                    detail: format!(
                        "task proposal could not enter live graph-delta adapter: {error}"
                    ),
                });
            }
        }
    }

    expand_task_graph(
        params,
        ctx,
        edge_index,
        &mut assemblies,
        &mut output.relationships,
    );

    if assemblies.len() > task_context::MAX_SELECTION_CANDIDATES {
        return Err(format!(
            "task adapter produced {} candidates; limit is {}",
            assemblies.len(),
            task_context::MAX_SELECTION_CANDIDATES
        ));
    }
    let reader = projection_source_reader(params, ctx.repo_root)?;
    let mut bundles = BTreeMap::<String, Vec<SelectedRecord>>::new();
    let mut typed = BTreeMap::<String, TaskEvidenceCandidate>::new();
    for (id, assembly) in &assemblies {
        let Some(node) = candidate_nodes.get(id).copied() else {
            continue;
        };
        let Some(source) = node_source_span(node) else {
            output.omissions.push(ProjectionOmission {
                record_id: Some(id.clone()),
                source: None,
                code: OmissionCode::MissingSource,
                detail: "task candidate has no valid current source anchor and was omitted before selection"
                    .into(),
            });
            continue;
        };
        let mut records = Vec::new();
        for role in &assembly.roles {
            let projected_role = projection_role_for_task(*role);
            let mut selected = selected_from_fused(
                node,
                &assembly.fused,
                0,
                SelectionPlacement {
                    role: Some(projected_role),
                    lane: Some(lane_for_role(projected_role)),
                    reason: Some(assembly.reason.clone()),
                },
                params.query.as_deref(),
                ctx.repo_root,
            );
            append_selected_product_score_audit(
                &mut selected,
                product_score_audit.get(id).map(Vec::as_slice),
                params.query.as_deref().unwrap_or_default(),
                ctx.repo_root,
            );
            records.push(selected);
        }
        typed.insert(
            id.clone(),
            TaskEvidenceCandidate {
                evidence_id: id.clone(),
                roles: assembly.roles.clone(),
                lanes: assembly.lanes.clone(),
                facets: assembly.facets.clone(),
                // Replaced below with the exact canonical singleton bundle
                // cost once fixed task-only sections are available.
                rendered_cost: 1,
                exact_reference: assembly.exact_reference.clone(),
                source: SourceAnchor {
                    path: source.path,
                    start_line: source.start_line,
                    end_line: source.end_line,
                },
                channel_rank: assembly.channel_rank,
            },
        );
        bundles.insert(id.clone(), records);
    }

    let mut policy = TaskSelectionPolicy::default();
    // Admission uses one deterministic byte currency. A token-only request is
    // conservatively projected at four bytes per estimated token; when callers
    // supply both bounds, the tighter one wins. The final renderer still
    // validates the exact independently measured byte and token totals.
    policy.rendered_budget = task_admission_budget(params, policy.rendered_budget);
    policy.per_record_limit = policy.rendered_budget;
    policy.candidate_limit = typed.len().clamp(1, task_context::MAX_SELECTION_CANDIDATES);
    policy.per_file_limit = policy.per_file_limit.min(policy.candidate_limit).max(1);
    if let Some(required) = params.context_roles.as_ref() {
        policy.required_roles = required
            .iter()
            .filter_map(|role| task_role_from_str(role))
            .collect();
    }
    let request = projection_request(params, SearchIntent::Implement);
    let default_task_capabilities = default_capabilities(ctx, request.projection).await;
    let base_output = output;

    // A record-level cap is evaluated in the same currency as selection: the
    // canonical final task response with that identity selected and every
    // fixed task-only section present.
    let candidate_ids = typed.keys().cloned().collect::<Vec<_>>();
    for id in candidate_ids {
        let singleton = [id.clone()];
        let cost = rendered_task_bundle_cost(
            params,
            &reader,
            &singleton,
            &bundles,
            &typed,
            &assemblies,
            &candidate_nodes,
            &product_score_audit,
            &base_output,
            &policy.required_roles,
            &default_task_capabilities,
            edge_index,
        )?;
        if let Some(candidate) = typed.get_mut(&id) {
            candidate.rendered_cost = cost.max(1);
        }
    }

    let selection = task_context::select_context_with_cost_and_interactions(
        typed.values().cloned().collect(),
        &policy,
        |selected_ids| {
            rendered_task_bundle_cost(
                params,
                &reader,
                selected_ids,
                &bundles,
                &typed,
                &assemblies,
                &candidate_nodes,
                &product_score_audit,
                &base_output,
                &policy.required_roles,
                &default_task_capabilities,
                edge_index,
            )
            .map_err(|reason| task_context::TaskContextError::BundleCost { reason })
        },
        |selected_ids, remaining_ids| {
            task_future_interaction_signature(selected_ids, remaining_ids, edge_index)
        },
    )
    .map_err(|error| error.to_string())?;
    let selected_ids = selection
        .selected
        .iter()
        .map(|selected| selected.evidence_id.clone())
        .collect::<Vec<_>>();
    let output = materialize_task_output(
        &selected_ids,
        &bundles,
        &typed,
        &assemblies,
        &candidate_nodes,
        &product_score_audit,
        &base_output,
        &policy.required_roles,
    );
    Ok(output)
}

fn task_admission_budget(params: &SearchParams, default_budget: usize) -> usize {
    let token_budget_bytes = params
        .max_output_tokens
        .map(|tokens| tokens.saturating_mul(4));
    match (params.max_output_bytes, token_budget_bytes) {
        (Some(bytes), Some(token_bytes)) => bytes.min(token_bytes),
        (Some(bytes), None) => bytes,
        (None, Some(token_bytes)) => token_bytes,
        (None, None) => default_budget,
    }
    .min(task_context::MAX_RENDERED_BUDGET)
}

fn task_lane_candidate_evidence(
    node: &Node,
    role: TaskRole,
    assemblies: &BTreeMap<String, TaskAssembly>,
    graph: &GraphState,
    edge_index: &ProjectedEdgeIndex<'_>,
) -> Result<String, String> {
    let span = node_source_span(node)
        .ok_or_else(|| "candidate has no valid current source span".to_string())?;
    let source_evidence = format!("{}:{}-{}", span.path, span.start_line, span.end_line);
    match role {
        TaskRole::EditableSource => {
            if default_role(node) == ProjectionRole::Test {
                return Err("test source cannot satisfy the editable behavior lane".into());
            }
            if !matches!(
                &node.id.kind,
                NodeKind::Function | NodeKind::Impl | NodeKind::Macro | NodeKind::ApiEndpoint
            ) {
                return Err(format!(
                    "node kind {} is not executable behavior evidence",
                    node.id.kind
                ));
            }
            Ok(format!(
                "role eligibility: executable production kind={} source={source_evidence}",
                node.id.kind
            ))
        }
        TaskRole::DefinitionOrApiState => {
            if !matches!(
                &node.id.kind,
                NodeKind::Struct
                    | NodeKind::Trait
                    | NodeKind::Enum
                    | NodeKind::TypeAlias
                    | NodeKind::Const
                    | NodeKind::Field
                    | NodeKind::EnumVariant
                    | NodeKind::ProtoMessage
                    | NodeKind::SqlTable
                    | NodeKind::ApiEndpoint
            ) {
                return Err(format!(
                    "node kind {} is not typed API/state evidence",
                    node.id.kind
                ));
            }
            Ok(format!(
                "role eligibility: typed API/state kind={} source={source_evidence}",
                node.id.kind
            ))
        }
        TaskRole::Test => {
            if default_role(node) != ProjectionRole::Test {
                return Err("production source cannot satisfy the test lane".into());
            }
            Ok(format!(
                "role eligibility: test path/metadata source={source_evidence}"
            ))
        }
        TaskRole::BehavioralAnalogue => {
            if default_role(node) == ProjectionRole::Test
                || !matches!(
                    &node.id.kind,
                    NodeKind::Function | NodeKind::Impl | NodeKind::Macro | NodeKind::ApiEndpoint
                )
            {
                return Err(format!(
                    "node kind {} is not production behavioral evidence",
                    node.id.kind
                ));
            }
            let candidate_id = node.stable_id();
            let mut anchors = assemblies
                .iter()
                .filter(|(_, assembly)| assembly.roles.contains(&TaskRole::EditableSource))
                .filter(|(id, _)| id.as_str() != candidate_id.as_str())
                .collect::<Vec<_>>();
            if anchors
                .iter()
                .any(|(_, assembly)| assembly.exact_reference.is_some())
            {
                anchors.retain(|(_, assembly)| assembly.exact_reference.is_some());
            }
            let candidate_edges = edge_index
                .outgoing(&candidate_id)
                .iter()
                .map(|edge| (edge.kind.to_string(), edge.to.to_stable_id()))
                .collect::<BTreeSet<_>>();
            let mut corroborated = anchors
                .into_iter()
                .filter_map(|(anchor_id, _)| {
                    let anchor = find_node(graph, anchor_id)?;
                    if anchor.id.kind != node.id.kind {
                        return None;
                    }
                    let anchor_edges = edge_index
                        .outgoing(anchor_id)
                        .iter()
                        .map(|edge| (edge.kind.to_string(), edge.to.to_stable_id()))
                        .collect::<BTreeSet<_>>();
                    let shared = candidate_edges
                        .intersection(&anchor_edges)
                        .cloned()
                        .collect::<Vec<_>>();
                    (!shared.is_empty()).then_some((anchor_id.clone(), shared))
                })
                .collect::<Vec<_>>();
            corroborated.sort_by(|left, right| {
                right
                    .1
                    .len()
                    .cmp(&left.1.len())
                    .then_with(|| left.0.cmp(&right.0))
            });
            let Some((anchor_id, shared)) = corroborated.first() else {
                return Err(
                    "same-kind retrieval lacks a shared typed graph target with an editable anchor"
                        .into(),
                );
            };
            Ok(format!(
                "role eligibility: source={source_evidence} anchor={anchor_id} shared_typed_targets={}",
                shared
                    .iter()
                    .map(|(kind, target)| format!("{kind}:{target}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ))
        }
        TaskRole::DirectDependency | TaskRole::CallerOrImpact | TaskRole::ProposalDelta => Ok(
            format!("role eligibility: typed adapter source={source_evidence}"),
        ),
    }
}

fn requested_task_facets(
    params: &SearchParams,
    inferred: &BTreeSet<TaskFacet>,
) -> BTreeSet<TaskFacet> {
    if let Some(values) = params.context_facets.as_deref() {
        return values
            .iter()
            .filter_map(|value| match value.as_str() {
                "behavior" => Some(TaskFacet::Behavior),
                "api_or_state" => Some(TaskFacet::ApiOrState),
                "test" => Some(TaskFacet::Test),
                "analogue" => Some(TaskFacet::Analogue),
                "proposal" => Some(TaskFacet::Proposal),
                _ => None,
            })
            .collect();
    }
    let mut facets = BTreeSet::from([
        TaskFacet::Behavior,
        TaskFacet::ApiOrState,
        TaskFacet::Test,
        TaskFacet::Analogue,
    ]);
    facets.extend(inferred.iter().copied());
    if params.proposal.is_some() {
        facets.insert(TaskFacet::Proposal);
    }
    facets
}

fn projection_lane_for_task(lane: TaskLane) -> &'static str {
    match lane {
        TaskLane::ExactReference => "exact_reference",
        TaskLane::EditableSource => "editable_source",
        TaskLane::DefinitionOrState => "definition_or_state",
        TaskLane::Tests => "tests",
        TaskLane::Analogues => "analogues",
        TaskLane::Dependencies => "dependencies",
        TaskLane::GraphImpact => "graph_impact",
        TaskLane::ProposalDelta => "proposal_delta",
    }
}

fn single_channel_fused(
    id: &str,
    channel: EvidenceChannel,
    score_kind: ScoreKind,
) -> FusedCandidate {
    fuse_ranked_channels(
        FusionPolicy::task_search(),
        &[ChannelInput::new(
            channel,
            score_kind,
            vec![RawCandidateScore::new(id.to_string(), 1.0)],
        )],
    )
    .expect("single finite task candidate satisfies task fusion")
    .remove(0)
}

#[allow(clippy::too_many_arguments)]
fn merge_task_assembly(
    assemblies: &mut BTreeMap<String, TaskAssembly>,
    fused: FusedCandidate,
    role: TaskRole,
    lane: TaskLane,
    facet: TaskFacet,
    exact_reference: Option<String>,
    channel_rank: u32,
    reason: String,
) {
    let id = fused.stable_id.clone();
    let entry = assemblies.entry(id).or_insert_with(|| TaskAssembly {
        fused,
        roles: BTreeSet::new(),
        lanes: BTreeSet::new(),
        facets: BTreeSet::new(),
        exact_reference: exact_reference.clone(),
        channel_rank,
        reason: reason.clone(),
    });
    entry.roles.insert(role);
    entry.lanes.insert(lane);
    entry.facets.insert(facet);
    if entry.exact_reference.is_none() {
        entry.exact_reference = exact_reference;
    }
    entry.channel_rank = entry.channel_rank.min(channel_rank);
    if !entry.reason.contains(&reason) {
        entry.reason.push_str("; ");
        entry.reason.push_str(&reason);
    }
}

fn expand_task_graph(
    params: &SearchParams,
    ctx: &SearchContext<'_>,
    edge_index: &ProjectedEdgeIndex<'_>,
    assemblies: &mut BTreeMap<String, TaskAssembly>,
    relationships: &mut Vec<ProjectedRelationship>,
) {
    let allowed = params.edge_types.as_ref().map(|labels| {
        labels
            .iter()
            .filter_map(|label| parse_edge_kind(label))
            .collect::<BTreeSet<_>>()
    });
    let hops = params
        .hops
        .or(params.depth)
        .unwrap_or(1)
        .min(MAX_CONTEXT_HOPS);
    let incoming = params.direction.as_deref() != Some("outgoing");
    let outgoing = params.direction.as_deref() != Some("incoming");
    let mut queue = assemblies
        .keys()
        .cloned()
        .map(|id| (id, 0u32))
        .collect::<VecDeque<_>>();
    let mut visited = assemblies.keys().cloned().collect::<BTreeSet<_>>();
    let mut added = 0usize;
    while let Some((current, depth)) = queue.pop_front() {
        if depth >= hops || added >= TASK_GRAPH_CANDIDATE_LIMIT {
            continue;
        }
        let mut adjacent = Vec::new();
        for edge in edge_index.outgoing(&current) {
            if allowed
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(&edge.kind))
            {
                continue;
            }
            let to = edge.to.to_stable_id();
            if outgoing {
                adjacent.push((to.clone(), true, edge));
            }
        }
        for edge in edge_index.incoming(&current) {
            if allowed
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(&edge.kind))
            {
                continue;
            }
            if incoming {
                adjacent.push((edge.from.to_stable_id(), false, edge));
            }
        }
        adjacent.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.2.kind.cmp(&right.2.kind))
        });
        for (neighbor_id, is_outgoing, edge) in adjacent {
            if added >= TASK_GRAPH_CANDIDATE_LIMIT {
                break;
            }
            let Some(node) = find_node(ctx.graph_state, &neighbor_id) else {
                continue;
            };
            if !projected_node_passes(node, params, ctx) {
                continue;
            }
            let role = if default_role(node) == ProjectionRole::Test
                || matches!(&edge.kind, EdgeKind::TestedBy) && !is_outgoing
            {
                TaskRole::Test
            } else if is_outgoing {
                match &edge.kind {
                    EdgeKind::Implements | EdgeKind::Defines | EdgeKind::HasField => {
                        TaskRole::DefinitionOrApiState
                    }
                    _ => TaskRole::DirectDependency,
                }
            } else {
                TaskRole::CallerOrImpact
            };
            let lane = task_lane_for_role(role);
            let facet = task_facet_for_role(role);
            let fused =
                single_channel_fused(&neighbor_id, EvidenceChannel::Graph, ScoreKind::GraphHops);
            merge_task_assembly(
                assemblies,
                fused,
                role,
                lane,
                facet,
                None,
                depth.saturating_add(1),
                format!(
                    "typed graph {} {} at hop {}",
                    if is_outgoing { "outgoing" } else { "incoming" },
                    edge.kind,
                    depth + 1
                ),
            );
            relationships.push(ProjectedRelationship {
                from: edge.from.to_stable_id(),
                kind: edge.kind.to_string(),
                to: edge.to.to_stable_id(),
                reason: format!(
                    "task graph expansion direction={} hop={} source={} confidence={:?}",
                    if is_outgoing { "outgoing" } else { "incoming" },
                    depth + 1,
                    edge.source,
                    edge.confidence
                ),
            });
            if visited.insert(neighbor_id.clone()) {
                added += 1;
                queue.push_back((neighbor_id, depth + 1));
            }
        }
    }
    relationships.sort();
    relationships.dedup_by(|left, right| {
        left.from == right.from && left.kind == right.kind && left.to == right.to
    });
}

#[allow(clippy::too_many_arguments)]
fn materialize_task_output(
    selected_ids: &[String],
    bundles: &BTreeMap<String, Vec<SelectedRecord>>,
    typed: &BTreeMap<String, TaskEvidenceCandidate>,
    assemblies: &BTreeMap<String, TaskAssembly>,
    candidate_nodes: &BTreeMap<String, &Node>,
    product_score_audit: &BTreeMap<String, Vec<ProductScoreAudit>>,
    base_output: &TaskAdapterOutput,
    required_roles: &BTreeSet<TaskRole>,
) -> TaskAdapterOutput {
    let mut output = base_output.clone();
    let selected_set = selected_ids.iter().cloned().collect::<BTreeSet<_>>();
    let mut covered_roles = BTreeSet::new();
    let mut covered_lanes = BTreeSet::new();
    let mut covered_facets = BTreeSet::new();

    for (rank, id) in selected_ids.iter().enumerate() {
        let Some(candidate) = typed.get(id) else {
            continue;
        };
        let reason = if let Some(reference) = candidate.exact_reference.clone() {
            TaskSelectionReason::ExactReference { reference }
        } else {
            TaskSelectionReason::CoveragePerCost {
                newly_covered_roles: candidate
                    .roles
                    .intersection(required_roles)
                    .filter(|role| !covered_roles.contains(*role))
                    .copied()
                    .collect(),
                newly_covered_lanes: candidate
                    .lanes
                    .difference(&covered_lanes)
                    .copied()
                    .collect(),
                newly_covered_facets: candidate
                    .facets
                    .difference(&covered_facets)
                    .copied()
                    .collect(),
            }
        };
        covered_roles.extend(candidate.roles.iter().copied());
        covered_lanes.extend(candidate.lanes.iter().copied());
        covered_facets.extend(candidate.facets.iter().copied());
        let reason = task_selection_reason(&reason);
        if let Some(records) = bundles.get(id) {
            for mut record in records.clone() {
                record.selection_rank = rank;
                record.selection.reason = format!("{}; {reason}", record.selection.reason);
                output.records.push(record);
            }
        }
    }

    for (rank, (id, assembly)) in assemblies.iter().enumerate() {
        let Some(node) = candidate_nodes.get(id).copied() else {
            continue;
        };
        let selected = selected_set.contains(id);
        let mut audit = candidate_audit_from_fused(
            node,
            &assembly.fused,
            rank,
            selected,
            if selected {
                "selected by exact-first maximum coverage under canonical final task-bundle cost"
            } else {
                "omitted by canonical final task-bundle coverage, diversity, or budget selection"
            },
        );
        append_product_score_audit(
            &mut audit.evidence,
            product_score_audit.get(id).map(Vec::as_slice),
        );
        output.candidate_audit.push(audit);
    }
    for id in typed.keys() {
        if selected_set.contains(id) {
            continue;
        }
        output.omissions.push(ProjectionOmission {
            record_id: Some(id.clone()),
            source: candidate_nodes
                .get(id)
                .copied()
                .and_then(node_source_span),
            code: OmissionCode::RenderBudget,
            detail: "task candidate was not selected by canonical final-bundle coverage, diversity, or budget optimization".into(),
        });
    }
    let missing_roles = required_roles
        .difference(&covered_roles)
        .copied()
        .collect::<BTreeSet<_>>();
    for role in &missing_roles {
        output.omissions.push(ProjectionOmission {
            record_id: None,
            source: None,
            code: OmissionCode::MissingSource,
            detail: format!("required task context role {role:?} is not covered"),
        });
    }
    output.capabilities.push(CapabilityStatus {
        capability: "task_context_selection".into(),
        state: if missing_roles.is_empty() {
            CapabilityState::Ready
        } else {
            CapabilityState::Degraded
        },
        // Do not put the measured cost in the response being measured: that
        // would make task admission self-referential.
        detail: format!(
            "selected {} of {} candidates using canonical final-bundle cost; missing_roles={missing_roles:?}",
            selected_ids.len(),
            typed.len()
        ),
    });
    output
}

#[allow(clippy::too_many_arguments)]
fn rendered_task_bundle_cost(
    params: &SearchParams,
    reader: &source::SourceReader,
    selected_ids: &[String],
    bundles: &BTreeMap<String, Vec<SelectedRecord>>,
    typed: &BTreeMap<String, TaskEvidenceCandidate>,
    assemblies: &BTreeMap<String, TaskAssembly>,
    candidate_nodes: &BTreeMap<String, &Node>,
    product_score_audit: &BTreeMap<String, Vec<ProductScoreAudit>>,
    base_output: &TaskAdapterOutput,
    required_roles: &BTreeSet<TaskRole>,
    default_task_capabilities: &[CapabilityStatus],
    edge_index: &ProjectedEdgeIndex<'_>,
) -> Result<usize, String> {
    let mut output = materialize_task_output(
        selected_ids,
        bundles,
        typed,
        assemblies,
        candidate_nodes,
        product_score_audit,
        base_output,
        required_roles,
    );
    let mut seen_records = BTreeSet::new();
    output.records.retain(|record| {
        seen_records.insert((record.identity.node_id.clone(), record.selection.role))
    });
    output.records.sort_by(|left, right| {
        left.selection_rank
            .cmp(&right.selection_rank)
            .then_with(|| left.identity.node_id.cmp(&right.identity.node_id))
            .then_with(|| left.selection.role.cmp(&right.selection.role))
    });
    output
        .capabilities
        .push(evidence_capsule_capability(&output.records));
    output
        .capabilities
        .extend_from_slice(default_task_capabilities);
    output.capabilities = merge_capabilities(output.capabilities);
    output
        .relationships
        .extend(projected_relationships(edge_index, &output.records));
    output.relationships.sort();
    output.relationships.dedup();
    output.candidate_audit.sort_by(|left, right| {
        left.candidate_rank
            .cmp(&right.candidate_rank)
            .then_with(|| left.identity.node_id.cmp(&right.identity.node_id))
    });

    let mut request = projection_request(params, SearchIntent::Implement);
    request.budget.max_rendered_bytes = None;
    request.budget.max_estimated_tokens = None;
    let plan = projection::plan_projection(
        request,
        ProjectionInput {
            records: output.records,
            candidate_audit: output.candidate_audit,
            relationships: output.relationships,
            omissions: output.omissions,
            capabilities: output.capabilities,
        },
        reader,
    );
    render::render_projection(&plan)
        .map(|response| response.accounting.total.utf8_bytes.max(1))
        .map_err(|error| format!("task candidate cost projection failed: {error}"))
}

fn candidate_audit_from_fused(
    node: &Node,
    fused: &FusedCandidate,
    rank: usize,
    selected: bool,
    reason: &str,
) -> CandidateAudit {
    let mut evidence = SelectionEvidence {
        candidate_rank: Some(rank + 1),
        content_hash: Some(node_projection_digest(node)),
        ..Default::default()
    };
    for channel in &fused.channels {
        evidence.raw_scores.insert(
            channel.channel.label().to_string(),
            channel.raw_score.to_string(),
        );
        evidence.provenance.push(EvidenceProvenance {
            source: channel.channel.label().to_string(),
            detail: format!(
                "kind={} rank={} depth={} contribution={}",
                channel.score_kind.label(),
                channel.rank,
                channel.depth,
                channel.contribution
            ),
        });
    }
    evidence
        .diagnostics
        .insert("final_score".into(), fused.final_score.to_string());
    CandidateAudit {
        candidate_rank: rank + 1,
        identity: RecordIdentity {
            node_id: node.stable_id(),
            source: node_source_span(node),
        },
        disposition: if selected {
            CandidateDisposition::Selected
        } else {
            CandidateDisposition::Omitted
        },
        reason: reason.to_string(),
        evidence,
    }
}

fn projection_role_for_task(role: TaskRole) -> ProjectionRole {
    match role {
        TaskRole::EditableSource => ProjectionRole::EditableSource,
        TaskRole::DefinitionOrApiState => ProjectionRole::DefinitionOrApiState,
        TaskRole::Test => ProjectionRole::Test,
        TaskRole::BehavioralAnalogue => ProjectionRole::BehavioralAnalogue,
        TaskRole::DirectDependency => ProjectionRole::DirectDependency,
        TaskRole::CallerOrImpact => ProjectionRole::CallerOrImpact,
        TaskRole::ProposalDelta => ProjectionRole::ProposalDelta,
    }
}

/// Derive the roles an exact graph record can truthfully satisfy. Exact-first
/// pinning controls priority only; it must not relabel tests or API/state
/// declarations as editable implementation source.
fn exact_task_roles(node: &Node) -> BTreeSet<TaskRole> {
    if default_role(node) == ProjectionRole::Test {
        return BTreeSet::from([TaskRole::Test]);
    }

    let mut roles = BTreeSet::new();
    if matches!(
        &node.id.kind,
        NodeKind::Function | NodeKind::Impl | NodeKind::Macro | NodeKind::ApiEndpoint
    ) {
        roles.insert(TaskRole::EditableSource);
    }
    if matches!(
        &node.id.kind,
        NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Enum
            | NodeKind::TypeAlias
            | NodeKind::Const
            | NodeKind::Field
            | NodeKind::EnumVariant
            | NodeKind::ProtoMessage
            | NodeKind::SqlTable
            | NodeKind::ApiEndpoint
    ) {
        roles.insert(TaskRole::DefinitionOrApiState);
    }
    if roles.is_empty() {
        roles.insert(TaskRole::DirectDependency);
    }
    roles
}

fn task_lane_for_role(role: TaskRole) -> TaskLane {
    match role {
        TaskRole::EditableSource => TaskLane::EditableSource,
        TaskRole::DefinitionOrApiState => TaskLane::DefinitionOrState,
        TaskRole::Test => TaskLane::Tests,
        TaskRole::BehavioralAnalogue => TaskLane::Analogues,
        TaskRole::DirectDependency => TaskLane::Dependencies,
        TaskRole::CallerOrImpact => TaskLane::GraphImpact,
        TaskRole::ProposalDelta => TaskLane::ProposalDelta,
    }
}

fn task_facet_for_role(role: TaskRole) -> TaskFacet {
    match role {
        TaskRole::DefinitionOrApiState => TaskFacet::ApiOrState,
        TaskRole::Test => TaskFacet::Test,
        TaskRole::BehavioralAnalogue => TaskFacet::Analogue,
        TaskRole::ProposalDelta => TaskFacet::Proposal,
        TaskRole::EditableSource | TaskRole::DirectDependency | TaskRole::CallerOrImpact => {
            TaskFacet::Behavior
        }
    }
}

fn task_role_from_str(value: &str) -> Option<TaskRole> {
    match value {
        "editable_source" => Some(TaskRole::EditableSource),
        "definition_or_api_state" => Some(TaskRole::DefinitionOrApiState),
        "test" => Some(TaskRole::Test),
        "behavioral_analogue" => Some(TaskRole::BehavioralAnalogue),
        "direct_dependency" => Some(TaskRole::DirectDependency),
        "caller_or_impact" => Some(TaskRole::CallerOrImpact),
        "proposal_delta" => Some(TaskRole::ProposalDelta),
        _ => None,
    }
}

fn task_selection_reason(reason: &TaskSelectionReason) -> String {
    match reason {
        TaskSelectionReason::ExactReference { reference } => {
            format!("exact task reference {reference:?}")
        }
        TaskSelectionReason::CoveragePerCost {
            newly_covered_roles,
            newly_covered_lanes,
            newly_covered_facets,
        } => format!(
            "task coverage per cost: roles={newly_covered_roles:?} lanes={newly_covered_lanes:?} facets={newly_covered_facets:?}"
        ),
    }
}

async fn projected_graph_delta(
    params: &SearchParams,
    ctx: &SearchContext<'_>,
    edge_index: &ProjectedEdgeIndex<'_>,
) -> String {
    let card = match live_graph_delta_card(params, ctx, edge_index) {
        Ok(card) => card,
        Err(error) => return format!("Graph-delta analysis failed: {error}."),
    };
    let (records, relationships, capabilities, omissions, candidate_audit) =
        graph_delta_projection(&card, params, ctx);
    render_projected_input(
        projection_request(params, SearchIntent::Review),
        ProjectionInput {
            records,
            candidate_audit,
            relationships,
            omissions,
            capabilities,
        },
        params,
        ctx,
    )
}

fn live_graph_delta_card(
    params: &SearchParams,
    ctx: &SearchContext<'_>,
    edge_index: &ProjectedEdgeIndex<'_>,
) -> Result<graph_delta::GraphDeltaCard, String> {
    let proposal = params.proposal.clone().unwrap_or_default();
    let root = graph_delta_request_root(params, ctx)?;
    let proposal_input = if proposal.trim_start().starts_with('{') {
        graph_delta::ProposalInput::StructuredJson(proposal)
    } else {
        graph_delta::ProposalInput::UnifiedDiff(proposal)
    };
    let limits = graph_delta::GraphDeltaLimits::default();
    let mut overlay = graph_delta::parse_beta_proposal(
        graph_delta::BetaGraphDeltaRequest {
            beta: true,
            root,
            proposal: proposal_input,
        },
        &limits,
    )
    .map_err(|error| format!("proposal rejected: {error}"))?;
    let source_reader = projection_source_reader(params, ctx.repo_root)?;
    enrich_live_graph_delta(&mut overlay, ctx, edge_index, &source_reader);
    let mut endpoint_pairs = overlay
        .edge_additions
        .iter()
        .map(|edge| graph_delta::EndpointPair {
            from: edge.key.from.clone(),
            to: edge.key.to.clone(),
        })
        .chain(
            overlay
                .edge_removals
                .iter()
                .map(|edge| graph_delta::EndpointPair {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                }),
        )
        .collect::<Vec<_>>();
    endpoint_pairs.sort();
    endpoint_pairs.dedup();
    endpoint_pairs.truncate(limits.endpoint_pairs);
    graph_delta::analyze_graph_delta(
        &graph_delta_snapshot(ctx.graph_state),
        &overlay,
        &endpoint_pairs,
        &limits,
    )
    .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SourceBehaviorProfile {
    features: BTreeMap<graph_delta::BehavioralDeltaKind, BTreeMap<String, u32>>,
}

impl SourceBehaviorProfile {
    fn record(&mut self, kind: graph_delta::BehavioralDeltaKind, feature: impl Into<String>) {
        *self
            .features
            .entry(kind)
            .or_default()
            .entry(feature.into())
            .or_default() += 1;
    }

    fn remove(&mut self, kind: graph_delta::BehavioralDeltaKind, feature: &str) {
        let Some(features) = self.features.get_mut(&kind) else {
            return;
        };
        let Some(count) = features.get_mut(feature) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            features.remove(feature);
        }
    }

    fn values(&self, kind: graph_delta::BehavioralDeltaKind) -> BTreeSet<String> {
        self.features
            .get(&kind)
            .into_iter()
            .flat_map(|features| features.iter())
            .map(|(feature, count)| format!("{feature}:{count}"))
            .collect()
    }

    fn shared(&self, other: &Self) -> BTreeSet<String> {
        behavior_profile_kinds()
            .into_iter()
            .flat_map(|kind| {
                let left = self.features.get(&kind).cloned().unwrap_or_default();
                let right = other.features.get(&kind).cloned().unwrap_or_default();
                left.keys()
                    .filter(|feature| right.contains_key(*feature))
                    .map(|feature| format!("{}={feature}", behavior_kind_name(kind)))
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

fn behavior_profile_kinds() -> [graph_delta::BehavioralDeltaKind; 6] {
    [
        graph_delta::BehavioralDeltaKind::BypassedCall,
        graph_delta::BehavioralDeltaKind::BranchBehavior,
        graph_delta::BehavioralDeltaKind::Reconciliation,
        graph_delta::BehavioralDeltaKind::Representation,
        graph_delta::BehavioralDeltaKind::ErrorPath,
        graph_delta::BehavioralDeltaKind::StatePropagation,
    ]
}

fn behavior_kind_name(kind: graph_delta::BehavioralDeltaKind) -> &'static str {
    match kind {
        graph_delta::BehavioralDeltaKind::BypassedCall => "helper_calls",
        graph_delta::BehavioralDeltaKind::BranchBehavior => "branches",
        graph_delta::BehavioralDeltaKind::Reconciliation => "reconciliation",
        graph_delta::BehavioralDeltaKind::Representation => "representation",
        graph_delta::BehavioralDeltaKind::ErrorPath => "error_paths",
        graph_delta::BehavioralDeltaKind::StatePropagation => "state_propagation",
        graph_delta::BehavioralDeltaKind::Other => "other",
    }
}

fn source_behavior_profile(
    node: &Node,
    reader: &source::SourceReader,
    edge_index: &ProjectedEdgeIndex<'_>,
) -> Result<SourceBehaviorProfile, String> {
    let span =
        node_source_span(node).ok_or_else(|| "node has no current source span".to_string())?;
    let text = reader.read(&span).map_err(|error| error.to_string())?.text;
    let mut profile = SourceBehaviorProfile::default();
    for edge in edge_index.outgoing(&node.stable_id()) {
        match &edge.kind {
            EdgeKind::Calls => profile.record(
                graph_delta::BehavioralDeltaKind::BypassedCall,
                edge.to.to_stable_id(),
            ),
            EdgeKind::HasField | EdgeKind::References | EdgeKind::ReferencedBy => profile.record(
                graph_delta::BehavioralDeltaKind::StatePropagation,
                edge.to.name.clone(),
            ),
            _ => {}
        }
    }
    for line in text.lines() {
        record_text_behavior_features(&mut profile, line, graph_delta::ChangedLineKind::Added);
    }
    Ok(profile)
}

fn behavior_words(text: &str) -> BTreeSet<String> {
    text.split(|character: char| !(character == '_' || character.is_ascii_alphanumeric()))
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn record_text_behavior_features(
    profile: &mut SourceBehaviorProfile,
    text: &str,
    change: graph_delta::ChangedLineKind,
) {
    let words = behavior_words(text);
    for (kind, markers) in [
        (
            graph_delta::BehavioralDeltaKind::BranchBehavior,
            &["if", "else", "elif", "match", "switch", "case", "guard"][..],
        ),
        (
            graph_delta::BehavioralDeltaKind::Reconciliation,
            &[
                "dedupe",
                "deduplicate",
                "merge",
                "reconcile",
                "sync",
                "synchronize",
            ][..],
        ),
        (
            graph_delta::BehavioralDeltaKind::Representation,
            &[
                "encode",
                "decode",
                "serialize",
                "deserialize",
                "format",
                "render",
            ][..],
        ),
        (
            graph_delta::BehavioralDeltaKind::ErrorPath,
            &[
                "error", "err", "raise", "throw", "catch", "except", "panic", "bail",
            ][..],
        ),
        (
            graph_delta::BehavioralDeltaKind::StatePropagation,
            &["state", "status", "cache", "context", "metadata"][..],
        ),
    ] {
        for marker in markers {
            if words.contains(*marker) {
                match change {
                    graph_delta::ChangedLineKind::Added => profile.record(kind, *marker),
                    graph_delta::ChangedLineKind::Removed => profile.remove(kind, marker),
                }
            }
        }
    }
}

fn apply_proposal_behavior(
    profile: &mut SourceBehaviorProfile,
    lines: &[graph_delta::ChangedLineFact],
    relationships: &[graph_delta::ChangedRelationshipFact],
) {
    for line in lines {
        record_text_behavior_features(profile, &line.text, line.kind);
        for fact in relationships.iter().filter(|fact| {
            fact.grounding.path == line.grounding.path
                && fact.grounding.proposal_line == line.grounding.proposal_line
        }) {
            let Some(kind) = (match fact.kind {
                // Helper calls use only uniquely corroborated overlay edges so
                // their feature identity is the exact stable target ID.
                graph_delta::InferredRelationshipKind::Call => None,
                graph_delta::InferredRelationshipKind::Reference
                | graph_delta::InferredRelationshipKind::Registration
                | graph_delta::InferredRelationshipKind::AttributeOrStateReference => {
                    Some(graph_delta::BehavioralDeltaKind::StatePropagation)
                }
            }) else {
                continue;
            };
            match fact.change {
                graph_delta::ChangedLineKind::Added => profile.record(kind, fact.target.clone()),
                graph_delta::ChangedLineKind::Removed => profile.remove(kind, &fact.target),
            }
        }
    }
}

fn source_backed_behavioral_contrasts(
    proposed: &SourceBehaviorProfile,
    analogue: &SourceBehaviorProfile,
    proposal_loci: &BTreeMap<graph_delta::BehavioralDeltaKind, graph_delta::EvidenceGrounding>,
    current_locus: &graph_delta::EvidenceGrounding,
    analogue_locus: &graph_delta::EvidenceGrounding,
) -> Vec<graph_delta::BehavioralDelta> {
    behavior_profile_kinds()
        .into_iter()
        .filter_map(|kind| {
            let proposed = proposed.values(kind);
            let existing = analogue.values(kind);
            if proposed == existing {
                return None;
            }
            let proposal_only = proposed.difference(&existing).cloned().collect::<Vec<_>>();
            let analogue_only = existing.difference(&proposed).cloned().collect::<Vec<_>>();
            Some(graph_delta::BehavioralDelta {
                kind,
                label: format!(
                    "{} source-backed contrast: proposal_only=[{}]; analogue_only=[{}]",
                    behavior_kind_name(kind),
                    proposal_only.join(","),
                    analogue_only.join(",")
                ),
                changed_locus: proposal_loci.get(&kind).unwrap_or(current_locus).clone(),
                analogue_locus: Some(analogue_locus.clone()),
            })
        })
        .collect()
}

fn proposal_behavior_loci(
    lines: &[graph_delta::ChangedLineFact],
    relationships: &[graph_delta::ChangedRelationshipFact],
) -> BTreeMap<graph_delta::BehavioralDeltaKind, graph_delta::EvidenceGrounding> {
    let mut loci = BTreeMap::new();
    for line in lines {
        let grounding = graph_delta::EvidenceGrounding::Proposal(line.grounding.clone());
        let mut line_profile = SourceBehaviorProfile::default();
        record_text_behavior_features(
            &mut line_profile,
            &line.text,
            graph_delta::ChangedLineKind::Added,
        );
        for kind in line_profile.features.keys() {
            loci.entry(*kind).or_insert_with(|| grounding.clone());
        }
        for fact in relationships.iter().filter(|fact| {
            fact.grounding.path == line.grounding.path
                && fact.grounding.proposal_line == line.grounding.proposal_line
        }) {
            let kind = match fact.kind {
                graph_delta::InferredRelationshipKind::Call => {
                    graph_delta::BehavioralDeltaKind::BypassedCall
                }
                graph_delta::InferredRelationshipKind::Reference
                | graph_delta::InferredRelationshipKind::Registration
                | graph_delta::InferredRelationshipKind::AttributeOrStateReference => {
                    graph_delta::BehavioralDeltaKind::StatePropagation
                }
            };
            loci.entry(kind).or_insert_with(|| grounding.clone());
        }
    }
    loci
}

fn enrich_live_graph_delta(
    overlay: &mut graph_delta::EphemeralOverlay,
    ctx: &SearchContext<'_>,
    edge_index: &ProjectedEdgeIndex<'_>,
    source_reader: &source::SourceReader,
) {
    let mut grounded = BTreeMap::<(String, u32), String>::new();
    let mut changed_nodes = BTreeSet::new();
    let mut misses = Vec::new();
    for file in &overlay.changed_files {
        for hunk in &file.hunks {
            for line in &hunk.changed_lines {
                match pre_edit_node_for_changed_line(ctx, file, hunk, line) {
                    Ok(node) => {
                        let id = node.stable_id();
                        grounded.insert(
                            (file.path.clone(), line.grounding.proposal_line),
                            id.clone(),
                        );
                        changed_nodes.insert(id);
                    }
                    Err(reason) => misses.push((line.grounding.clone(), reason)),
                }
            }
        }
    }

    for id in &changed_nodes {
        if let Some(node) = find_node(ctx.graph_state, id)
            && let Some(grounding) = graph_delta_node_grounding(node)
        {
            overlay.impacted.push(graph_delta::ImpactEvidence {
                label: node.id.name.clone(),
                kind: graph_delta_impact_kind(node, false),
                grounding,
            });
        }
    }

    let relationship_facts = overlay.relationships.len();
    let mut inferred_relationships = 0usize;
    for fact in &overlay.relationships {
        let Some(source_id) =
            grounded.get(&(fact.grounding.path.clone(), fact.grounding.proposal_line))
        else {
            misses.push((
                fact.grounding.clone(),
                "relationship fact has no uniquely grounded changed source line",
            ));
            continue;
        };
        match corroborate_changed_relationship(fact, source_id, ctx, edge_index) {
            Ok(edge) => {
                inferred_relationships += 1;
                match fact.change {
                    graph_delta::ChangedLineKind::Added => {
                        if !edge_index.contains(&edge.key.from, &edge.key.to, &edge.key.kind) {
                            overlay.edge_additions.push(edge);
                        }
                    }
                    graph_delta::ChangedLineKind::Removed => {
                        overlay.edge_removals.push(edge.key);
                    }
                }
            }
            Err(reason) => misses.push((fact.grounding.clone(), reason)),
        }
    }

    let mut queue = changed_nodes
        .iter()
        .cloned()
        .map(|id| (id, 0u32))
        .collect::<VecDeque<_>>();
    let mut visited = changed_nodes.clone();
    while let Some((current, depth)) = queue.pop_front() {
        if depth >= 2 || visited.len() >= TASK_GRAPH_CANDIDATE_LIMIT {
            continue;
        }
        let adjacent = edge_index
            .outgoing(&current)
            .iter()
            .map(|edge| (edge.to.to_stable_id(), false))
            .chain(
                edge_index
                    .incoming(&current)
                    .iter()
                    .map(|edge| (edge.from.to_stable_id(), true)),
            )
            .collect::<Vec<_>>();
        for (neighbor, caller) in adjacent {
            if !visited.insert(neighbor.clone()) {
                continue;
            }
            let Some(node) = find_node(ctx.graph_state, &neighbor) else {
                continue;
            };
            let kind = graph_delta_impact_kind(node, caller);
            if let Some(grounding) = graph_delta_node_grounding(node) {
                overlay.impacted.push(graph_delta::ImpactEvidence {
                    label: node.id.name.clone(),
                    kind,
                    grounding,
                });
            }
            queue.push_back((neighbor, depth + 1));
        }
    }

    let mut changed_lines_by_node = BTreeMap::<String, Vec<graph_delta::ChangedLineFact>>::new();
    for file in &overlay.changed_files {
        for hunk in &file.hunks {
            for line in &hunk.changed_lines {
                if let Some(id) =
                    grounded.get(&(line.grounding.path.clone(), line.grounding.proposal_line))
                {
                    changed_lines_by_node
                        .entry(id.clone())
                        .or_default()
                        .push(line.clone());
                }
            }
        }
    }
    for lines in changed_lines_by_node.values_mut() {
        lines.sort_by_key(|line| line.grounding.proposal_line);
        lines.dedup();
    }

    let mut analogue_count = 0usize;
    for changed_id in &changed_nodes {
        let Some(changed) = find_node(ctx.graph_state, changed_id) else {
            continue;
        };
        let Some(changed_grounding) = graph_delta_node_grounding(changed) else {
            continue;
        };
        let Ok(mut proposed_profile) = source_behavior_profile(changed, source_reader, edge_index)
        else {
            continue;
        };
        let changed_lines = changed_lines_by_node
            .get(changed_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        apply_proposal_behavior(&mut proposed_profile, changed_lines, &overlay.relationships);
        for removed in overlay
            .edge_removals
            .iter()
            .filter(|edge| edge.from == *changed_id)
        {
            let kind = if removed.kind == EdgeKind::Calls.to_string() {
                graph_delta::BehavioralDeltaKind::BypassedCall
            } else {
                graph_delta::BehavioralDeltaKind::StatePropagation
            };
            proposed_profile.remove(kind, &removed.to);
        }
        for added in overlay
            .edge_additions
            .iter()
            .filter(|edge| edge.key.from == *changed_id)
        {
            let kind = if added.key.kind == EdgeKind::Calls.to_string() {
                graph_delta::BehavioralDeltaKind::BypassedCall
            } else {
                graph_delta::BehavioralDeltaKind::StatePropagation
            };
            proposed_profile.record(kind, added.key.to.clone());
        }
        let proposal_loci = proposal_behavior_loci(changed_lines, &overlay.relationships);
        let mut candidates = ctx
            .graph_state
            .nodes
            .iter()
            .filter(|candidate| {
                candidate.stable_id() != *changed_id && candidate.id.kind == changed.id.kind
            })
            .filter_map(|candidate| {
                let profile = source_behavior_profile(candidate, source_reader, edge_index).ok()?;
                let shared = proposed_profile.shared(&profile);
                (!shared.is_empty()).then_some((
                    std::cmp::Reverse(shared.len()),
                    candidate.stable_id(),
                    candidate,
                    profile,
                    shared,
                ))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        for (std::cmp::Reverse(_), _, analogue, analogue_profile, shared) in
            candidates.into_iter().take(2)
        {
            if analogue_count >= 8 {
                break;
            }
            let Some(analogue_grounding) = graph_delta_node_grounding(analogue) else {
                continue;
            };
            analogue_count += 1;
            overlay.analogues.push(graph_delta::BehavioralAnalogue {
                label: format!("{} analogue {}", changed.id.name, analogue.id.name),
                changed_locus: graph_delta::ImpactEvidence {
                    label: changed.id.name.clone(),
                    kind: graph_delta::ImpactKind::EditableLocus,
                    grounding: changed_grounding.clone(),
                },
                analogue_locus: graph_delta::ImpactEvidence {
                    label: analogue.id.name.clone(),
                    kind: graph_delta::ImpactKind::BehavioralAnalogue,
                    grounding: analogue_grounding.clone(),
                },
                similarity_basis: format!(
                    "source-backed proposal/analogue behavior shared: {}",
                    shared.into_iter().collect::<Vec<_>>().join(",")
                ),
            });
            overlay
                .behavioral_deltas
                .extend(source_backed_behavioral_contrasts(
                    &proposed_profile,
                    &analogue_profile,
                    &proposal_loci,
                    &changed_grounding,
                    &analogue_grounding,
                ));
        }
    }

    overlay.omissions.retain(|omission| {
        omission.code != graph_delta::GraphDeltaOmissionCode::LiveGraphInferenceDeferred
    });
    for (grounding, reason) in misses {
        overlay.omissions.push(graph_delta::GraphDeltaOmission {
            code: graph_delta::GraphDeltaOmissionCode::CapabilityDegraded,
            detail: format!("live graph inference skipped changed line: {reason}"),
            hydration_key: Some(
                graph_delta::EvidenceGrounding::Proposal(grounding.clone()).stable_hydration_key(),
            ),
            grounding: Some(graph_delta::EvidenceGrounding::Proposal(grounding)),
        });
    }
    set_graph_delta_capability(
        &mut overlay.capabilities,
        graph_delta::GraphDeltaCapability::LiveGraphInference,
        if grounded.is_empty() || inferred_relationships < relationship_facts {
            graph_delta::CapabilityState::Degraded
        } else {
            graph_delta::CapabilityState::Ready
        },
        format!(
            "uniquely grounded {}/{} changed lines and inferred {inferred_relationships}/{relationship_facts} canonical corroborated relationship facts",
            grounded.len(),
            overlay
                .changed_files
                .iter()
                .flat_map(|file| &file.hunks)
                .map(|hunk| hunk.changed_lines.len())
                .sum::<usize>()
        ),
    );
    set_graph_delta_capability(
        &mut overlay.capabilities,
        graph_delta::GraphDeltaCapability::ImpactTraversal,
        if changed_nodes.is_empty() {
            graph_delta::CapabilityState::Degraded
        } else {
            graph_delta::CapabilityState::Ready
        },
        format!(
            "bounded typed impact traversal retained {} grounded loci",
            overlay.impacted.len()
        ),
    );
    set_graph_delta_capability(
        &mut overlay.capabilities,
        graph_delta::GraphDeltaCapability::BehavioralAnalogueDiscovery,
        if analogue_count == 0 {
            graph_delta::CapabilityState::Degraded
        } else {
            graph_delta::CapabilityState::Ready
        },
        format!(
            "retained {analogue_count} source-backed behavioral analogues with explicit shared/missing feature contrasts"
        ),
    );
    set_graph_delta_capability(
        &mut overlay.capabilities,
        graph_delta::GraphDeltaCapability::RouteAnalysis,
        if overlay.edge_additions.is_empty() && overlay.edge_removals.is_empty() {
            graph_delta::CapabilityState::Unavailable
        } else {
            graph_delta::CapabilityState::Ready
        },
        format!(
            "derived {} typed edge additions and {} typed edge removals as route endpoints",
            overlay.edge_additions.len(),
            overlay.edge_removals.len()
        ),
    );
}

/// Resolve a changed proposal line only against coordinates that exist in the
/// immutable pre-edit graph. Removed lines have an exact old coordinate.
/// Added lines instead use the narrowest current node enclosing the hunk's
/// old range; hunks without pre-edit context remain proposal-only.
fn pre_edit_node_for_changed_line<'a>(
    ctx: &'a SearchContext<'_>,
    file: &graph_delta::ChangedFileFact,
    hunk: &graph_delta::ChangedHunkFact,
    line: &graph_delta::ChangedLineFact,
) -> Result<&'a Node, &'static str> {
    if let Some(old_line) = line.grounding.old_line {
        return unique_node_at_line(ctx, &file.path, old_line);
    }
    if line.kind != graph_delta::ChangedLineKind::Added || hunk.old_count == 0 {
        return Err("changed line has no pre-edit coordinate and remains proposal-only");
    }
    if let Some(old_line) = paired_replacement_old_line(hunk, line) {
        return unique_node_at_line(ctx, &file.path, old_line);
    }
    let new_line = line
        .grounding
        .new_line
        .ok_or("added line has no new hunk coordinate")?;
    let new_offset = new_line
        .checked_sub(hunk.new_start)
        .ok_or("added line precedes its hunk")?;
    let prior_added = hunk
        .changed_lines
        .iter()
        .filter(|changed| {
            changed.kind == graph_delta::ChangedLineKind::Added
                && changed.grounding.proposal_line < line.grounding.proposal_line
        })
        .count() as u32;
    let prior_removed = hunk
        .changed_lines
        .iter()
        .filter(|changed| {
            changed.kind == graph_delta::ChangedLineKind::Removed
                && changed.grounding.proposal_line < line.grounding.proposal_line
        })
        .count() as u32;
    let old_gap = hunk
        .old_start
        .checked_add(new_offset)
        .and_then(|value| value.checked_sub(prior_added))
        .and_then(|value| value.checked_add(prior_removed))
        .ok_or("added line cannot map to a bounded old-side insertion gap")?;
    let old_end_exclusive = hunk
        .old_start
        .checked_add(hunk.old_count)
        .ok_or("hunk old range overflows")?;
    let before = old_gap
        .checked_sub(1)
        .filter(|value| *value >= hunk.old_start);
    let after = (old_gap < old_end_exclusive).then_some(old_gap);
    let (Some(before), Some(after)) = (before, after) else {
        return Err("added line lacks old-side context on both sides and remains proposal-only");
    };
    let before_node = unique_node_at_line(ctx, &file.path, before)?;
    let after_node = unique_node_at_line(ctx, &file.path, after)?;
    if before_node.stable_id() != after_node.stable_id() {
        return Err("added line crosses pre-edit symbol boundaries and remains proposal-only");
    }
    Ok(before_node)
}

fn paired_replacement_old_line(
    hunk: &graph_delta::ChangedHunkFact,
    line: &graph_delta::ChangedLineFact,
) -> Option<u32> {
    let line_index = hunk.changed_lines.iter().position(|candidate| {
        candidate.kind == line.kind
            && candidate.grounding.proposal_line == line.grounding.proposal_line
    })?;
    let mut added_start = line_index;
    while added_start > 0 {
        let previous = &hunk.changed_lines[added_start - 1];
        let current = &hunk.changed_lines[added_start];
        if previous.kind != graph_delta::ChangedLineKind::Added
            || previous.grounding.proposal_line + 1 != current.grounding.proposal_line
        {
            break;
        }
        added_start -= 1;
    }
    if added_start == 0 {
        return None;
    }
    let removed_end = added_start;
    let last_removed = &hunk.changed_lines[removed_end - 1];
    if last_removed.kind != graph_delta::ChangedLineKind::Removed
        || last_removed.grounding.proposal_line + 1
            != hunk.changed_lines[added_start].grounding.proposal_line
    {
        return None;
    }
    let mut removed_start = removed_end - 1;
    while removed_start > 0 {
        let previous = &hunk.changed_lines[removed_start - 1];
        let current = &hunk.changed_lines[removed_start];
        if previous.kind != graph_delta::ChangedLineKind::Removed
            || previous.grounding.proposal_line + 1 != current.grounding.proposal_line
        {
            break;
        }
        removed_start -= 1;
    }
    let added_offset = line_index.checked_sub(added_start)?;
    hunk.changed_lines
        .get(removed_start + added_offset)
        .filter(|_| removed_start + added_offset < removed_end)
        .and_then(|removed| removed.grounding.old_line)
}

fn unique_node_at_line<'a>(
    ctx: &'a SearchContext<'_>,
    path: &str,
    line: u32,
) -> Result<&'a Node, &'static str> {
    let mut candidates = ctx
        .graph_state
        .nodes
        .iter()
        .filter(|node| node.id.file.to_string_lossy().replace('\\', "/") == path)
        // Persisted document-symbol nodes prove the LSP response survived
        // reopen; they intentionally duplicate the source symbol and must not
        // compete with that symbol as proposal grounding.
        .filter(|node| {
            !matches!(
                &node.id.kind,
                NodeKind::Other(kind) if kind == "lsp_document_symbol"
            )
        })
        .filter(|node| {
            u32::try_from(node.line_start).is_ok_and(|start| start <= line)
                && u32::try_from(node.line_end).is_ok_and(|end| end >= line)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.line_end
            .saturating_sub(left.line_start)
            .cmp(&right.line_end.saturating_sub(right.line_start))
            .then_with(|| {
                let left_synthetic = left
                    .metadata
                    .get("synthetic")
                    .is_some_and(|value| value == "true");
                let right_synthetic = right
                    .metadata
                    .get("synthetic")
                    .is_some_and(|value| value == "true");
                left_synthetic.cmp(&right_synthetic)
            })
            .then_with(|| left.stable_id().cmp(&right.stable_id()))
    });
    let Some(best) = candidates.first().copied() else {
        return Err("no current graph node contains the changed line");
    };
    let best_width = best.line_end.saturating_sub(best.line_start);
    let best_synthetic = best
        .metadata
        .get("synthetic")
        .is_some_and(|value| value == "true");
    if candidates.iter().skip(1).any(|candidate| {
        candidate.line_end.saturating_sub(candidate.line_start) == best_width
            && candidate
                .metadata
                .get("synthetic")
                .is_some_and(|value| value == "true")
                == best_synthetic
    }) {
        return Err("multiple equally specific current graph nodes contain the changed line");
    }
    Ok(best)
}

fn corroborate_changed_relationship(
    fact: &graph_delta::ChangedRelationshipFact,
    source_id: &str,
    ctx: &SearchContext<'_>,
    edge_index: &ProjectedEdgeIndex<'_>,
) -> Result<graph_delta::WeightedEdge, &'static str> {
    let mut matches = ctx
        .graph_state
        .nodes
        .iter()
        .filter(|node| node.stable_id() != source_id && node.id.name == fact.target)
        .collect::<Vec<_>>();
    matches.sort_by_key(|node| node.stable_id());
    matches.dedup_by_key(|node| node.stable_id());
    if matches.len() > 1
        && let Some(qualifier) = fact.qualifier.as_deref()
    {
        let qualified = matches
            .iter()
            .copied()
            .filter(|node| changed_relationship_qualifier_matches(node, qualifier))
            .collect::<Vec<_>>();
        if !qualified.is_empty() {
            matches = qualified;
        }
    }
    let [target] = matches.as_slice() else {
        return if matches.is_empty() {
            Err("canonical relationship fact has no corroborated current endpoint")
        } else {
            Err("canonical relationship fact has multiple corroborated current endpoints")
        };
    };
    let key = graph_delta::EdgeKey {
        from: source_id.to_string(),
        to: target.stable_id(),
        kind: fact.kind.edge_kind().to_string(),
    };
    if fact.change == graph_delta::ChangedLineKind::Removed
        && !edge_index.contains(&key.from, &key.to, &key.kind)
    {
        return Err("removed relationship is not corroborated by a persisted current edge");
    }
    Ok(graph_delta::WeightedEdge {
        key,
        cost: 1,
        priority: 0,
        registration_order: fact.grounding.proposal_line,
        grounding: graph_delta::EvidenceGrounding::Proposal(fact.grounding.clone()),
    })
}

fn changed_relationship_qualifier_matches(node: &Node, qualifier: &str) -> bool {
    node.stable_id().contains(qualifier)
        || node.id.file.to_string_lossy().contains(qualifier)
        || node.signature.contains(qualifier)
}

fn graph_delta_node_grounding(node: &Node) -> Option<graph_delta::EvidenceGrounding> {
    node_source_span(node).map(|span| {
        graph_delta::EvidenceGrounding::CurrentSource(graph_delta::SourceSpan {
            root: span.root,
            path: span.path,
            start_line: span.start_line,
            end_line: span.end_line,
        })
    })
}

fn graph_delta_impact_kind(node: &Node, caller: bool) -> graph_delta::ImpactKind {
    if default_role(node) == ProjectionRole::Test {
        graph_delta::ImpactKind::Test
    } else if caller {
        graph_delta::ImpactKind::Caller
    } else if matches!(
        &node.id.kind,
        NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Enum
            | NodeKind::TypeAlias
            | NodeKind::Field
            | NodeKind::Const
            | NodeKind::ApiEndpoint
    ) {
        graph_delta::ImpactKind::StateOrApi
    } else {
        graph_delta::ImpactKind::EditableLocus
    }
}

fn set_graph_delta_capability(
    capabilities: &mut Vec<graph_delta::CapabilityReport>,
    capability: graph_delta::GraphDeltaCapability,
    state: graph_delta::CapabilityState,
    detail: String,
) {
    capabilities.retain(|report| report.capability != capability);
    capabilities.push(graph_delta::CapabilityReport {
        capability,
        state,
        detail,
    });
}

#[allow(clippy::type_complexity)]
fn graph_delta_projection(
    card: &graph_delta::GraphDeltaCard,
    params: &SearchParams,
    ctx: &SearchContext<'_>,
) -> (
    Vec<SelectedRecord>,
    Vec<ProjectedRelationship>,
    Vec<CapabilityStatus>,
    Vec<ProjectionOmission>,
    Vec<CandidateAudit>,
) {
    let mut by_role = BTreeMap::<(String, ProjectionRole), SelectedRecord>::new();
    let mut relationships = Vec::new();
    let mut omissions = card
        .omissions
        .iter()
        .map(|omission| ProjectionOmission {
            record_id: omission.hydration_key.clone(),
            source: omission
                .grounding
                .as_ref()
                .and_then(graph_delta_projection_span),
            code: OmissionCode::MissingSource,
            detail: format!("graph-delta {:?}: {}", omission.code, omission.detail),
        })
        .collect::<Vec<_>>();

    // Proposal lines are first-class, source-grounded evidence even when an
    // added line has no pre-edit coordinate or current graph node. Keep the
    // conservative live-graph inference degradation, but do not erase the
    // proposal itself from the graph-delta card.
    for file in &card.changed_files {
        for hunk in &file.hunks {
            for line in &hunk.changed_lines {
                let grounding = graph_delta::EvidenceGrounding::Proposal(line.grounding.clone());
                let id = grounding.stable_hydration_key();
                let change = format!("{:?}", line.kind).to_ascii_lowercase();
                by_role
                    .entry((id.clone(), ProjectionRole::ProposalDelta))
                    .or_insert_with(|| SelectedRecord {
                        selection_rank: 0,
                        identity: RecordIdentity {
                            node_id: id.clone(),
                            source: None,
                        },
                        symbol: SymbolSummary {
                            name: format!("{}:{}", file.path, line.grounding.proposal_line),
                            kind: format!("proposal_{change}_line"),
                            language: "diff".into(),
                            signature: line.text.clone(),
                            extraction_source: None,
                            declared_metadata: BTreeMap::new(),
                        },
                        selection: SelectionSummary {
                            channel: SelectionChannel::Graph,
                            reason: format!("explicit {change} proposal line grounded by {id}"),
                            role: Some(ProjectionRole::ProposalDelta),
                            lane: Some(ProjectionLane::ProposalDelta),
                        },
                        evidence: SelectionEvidence {
                            content_hash: Some(
                                blake3::hash(line.text.as_bytes()).to_hex().to_string(),
                            ),
                            provenance: vec![EvidenceProvenance {
                                source: "proposal".into(),
                                detail: format!(
                                    "{} line {} from validated bounded unified diff",
                                    file.path, line.grounding.proposal_line
                                ),
                            }],
                            ..Default::default()
                        },
                        evidence_hydration: None,
                        focused_span: None,
                    });
            }
        }
    }

    for impact in &card.impacted_loci {
        insert_graph_delta_impact(
            impact,
            graph_delta_role(impact.kind),
            params,
            ctx,
            &mut by_role,
            &mut omissions,
        );
    }
    for impact in &card.impacted_tests {
        insert_graph_delta_impact(
            impact,
            ProjectionRole::Test,
            params,
            ctx,
            &mut by_role,
            &mut omissions,
        );
    }
    for impact in &card.impacted_state_or_api {
        insert_graph_delta_impact(
            impact,
            ProjectionRole::DefinitionOrApiState,
            params,
            ctx,
            &mut by_role,
            &mut omissions,
        );
    }
    for bypassed in &card.bypassed_loci {
        if let Some(node) = find_node(ctx.graph_state, &bypassed.id)
            && node_source_span(node).is_some()
        {
            insert_graph_delta_node(
                node,
                ProjectionRole::CallerOrImpact,
                "route comparison found this current locus bypassed after the proposal",
                params,
                ctx,
                &mut by_role,
            );
        } else {
            omissions.push(ProjectionOmission {
                record_id: Some(bypassed.id.clone()),
                source: None,
                code: OmissionCode::MissingSource,
                detail: "graph-delta bypassed locus has no hydratable current source".into(),
            });
        }
    }
    for analogue in &card.behavioral_analogues {
        insert_graph_delta_impact(
            &analogue.changed_locus,
            ProjectionRole::ProposalDelta,
            params,
            ctx,
            &mut by_role,
            &mut omissions,
        );
        insert_graph_delta_impact(
            &analogue.analogue_locus,
            ProjectionRole::BehavioralAnalogue,
            params,
            ctx,
            &mut by_role,
            &mut omissions,
        );
        relationships.push(ProjectedRelationship {
            from: analogue.changed_locus.grounding.stable_hydration_key(),
            kind: "behavioral_analogue".into(),
            to: analogue.analogue_locus.grounding.stable_hydration_key(),
            reason: format!("{}; {}", analogue.label, analogue.similarity_basis),
        });
    }
    for item in &card.affected_locus_checklist {
        let impact = graph_delta::ImpactEvidence {
            label: item.label.clone(),
            kind: item
                .kinds
                .iter()
                .next()
                .copied()
                .unwrap_or(graph_delta::ImpactKind::EditableLocus),
            grounding: item.grounding.clone(),
        };
        insert_graph_delta_impact(
            &impact,
            graph_delta_role(impact.kind),
            params,
            ctx,
            &mut by_role,
            &mut omissions,
        );
    }
    for changed in &card.changed_edges {
        relationships.push(ProjectedRelationship {
            from: changed.edge.key.from.clone(),
            kind: format!("graph_delta_{:?}_{}", changed.change, changed.edge.key.kind)
                .to_ascii_lowercase(),
            to: changed.edge.key.to.clone(),
            reason: format!(
                "typed proposed edge {:?}; cost={} priority={} grounding={}",
                changed.change,
                changed.edge.cost,
                changed.edge.priority,
                changed.edge.grounding.stable_hydration_key()
            ),
        });
    }
    for route in &card.routes {
        relationships.push(ProjectedRelationship {
            from: route.endpoints.from.clone(),
            kind: "graph_delta_route".into(),
            to: route.endpoints.to.clone(),
            reason: format!(
                "change={:?} before_paths={} after_paths={}",
                route.change,
                route.before.alternatives.len(),
                route.after.alternatives.len()
            ),
        });
    }
    for delta in &card.behavioral_deltas {
        relationships.push(ProjectedRelationship {
            from: delta.changed_locus.stable_hydration_key(),
            kind: format!("behavioral_delta_{:?}", delta.kind).to_ascii_lowercase(),
            to: delta
                .analogue_locus
                .as_ref()
                .map(graph_delta::EvidenceGrounding::stable_hydration_key)
                .unwrap_or_else(|| delta.changed_locus.stable_hydration_key()),
            reason: delta.label.clone(),
        });
    }

    let mut candidates = by_role.into_values().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        graph_delta_record_priority(left)
            .cmp(&graph_delta_record_priority(right))
            .then_with(|| left.identity.node_id.cmp(&right.identity.node_id))
            .then_with(|| left.selection.role.cmp(&right.selection.role))
    });
    let record_limit = params.limit.unwrap_or(20).min(100);
    let mut selected_keys = BTreeSet::<(String, Option<ProjectionRole>)>::new();
    let mut selected_order = Vec::<(String, Option<ProjectionRole>)>::new();
    let mut covered_roles = BTreeSet::new();
    for candidate in &candidates {
        let Some(role) = candidate.selection.role else {
            continue;
        };
        if selected_order.len() >= record_limit {
            break;
        }
        if covered_roles.insert(role) {
            let key = (candidate.identity.node_id.clone(), Some(role));
            selected_keys.insert(key.clone());
            selected_order.push(key);
        }
    }
    for candidate in &candidates {
        if selected_order.len() >= record_limit {
            break;
        }
        let key = (candidate.identity.node_id.clone(), candidate.selection.role);
        if selected_keys.insert(key.clone()) {
            selected_order.push(key);
        }
    }
    let mut records = selected_order
        .iter()
        .filter_map(|key| {
            candidates.iter().find(|candidate| {
                candidate.identity.node_id.as_str() == key.0.as_str()
                    && candidate.selection.role == key.1
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    for (rank, record) in records.iter_mut().enumerate() {
        record.selection_rank = rank;
    }
    let mut candidate_audit = candidates
        .iter()
        .enumerate()
        .map(|(rank, candidate)| {
            let selected = selected_keys.contains(&(
                candidate.identity.node_id.clone(),
                candidate.selection.role,
            ));
            CandidateAudit {
                candidate_rank: rank + 1,
                identity: candidate.identity.clone(),
                disposition: if selected {
                    CandidateDisposition::Selected
                } else {
                    CandidateDisposition::Omitted
                },
                reason: if selected {
                    "selected by deterministic graph-delta diagnostic-role coverage and priority"
                        .into()
                } else {
                    format!(
                        "omitted after deterministic diagnostic-role selection reached record limit {record_limit}"
                    )
                },
                evidence: candidate.evidence.clone(),
            }
        })
        .collect::<Vec<_>>();
    for candidate in &candidates {
        if selected_keys.contains(&(candidate.identity.node_id.clone(), candidate.selection.role)) {
            continue;
        }
        omissions.push(ProjectionOmission {
            record_id: Some(candidate.identity.node_id.clone()),
            source: candidate.identity.source.clone(),
            code: OmissionCode::RenderBudget,
            detail: format!(
                "graph-delta {:?} locus was not rendered after deterministic diagnostic-role selection reached record limit {record_limit}; evidence remains in candidate audit",
                candidate.selection.role
            ),
        });
    }
    let mut audited_ids = candidate_audit
        .iter()
        .map(|audit| audit.identity.node_id.clone())
        .collect::<BTreeSet<_>>();
    let mut unhydrated = omissions
        .iter()
        .filter_map(|omission| {
            let id = omission.record_id.clone()?;
            (!audited_ids.contains(&id)).then_some((id, omission))
        })
        .collect::<Vec<_>>();
    unhydrated.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.detail.cmp(&right.1.detail))
    });
    for (id, omission) in unhydrated {
        if !audited_ids.insert(id.clone()) {
            continue;
        }
        candidate_audit.push(CandidateAudit {
            candidate_rank: candidate_audit.len() + 1,
            identity: RecordIdentity {
                node_id: id,
                source: omission.source.clone(),
            },
            disposition: CandidateDisposition::Omitted,
            reason: format!(
                "graph-delta candidate could not be rendered: {}",
                omission.detail
            ),
            evidence: SelectionEvidence::default(),
        });
    }
    let projection_omitted = candidates.len().saturating_sub(records.len());
    let audited_omitted = candidate_audit
        .iter()
        .filter(|audit| audit.disposition == CandidateDisposition::Omitted)
        .count();
    let mut capabilities = card
        .capabilities
        .iter()
        .map(|report| CapabilityStatus {
            capability: graph_delta_capability_name(report.capability).into(),
            state: match report.state {
                graph_delta::CapabilityState::Ready => CapabilityState::Ready,
                graph_delta::CapabilityState::Degraded => CapabilityState::Degraded,
                graph_delta::CapabilityState::Unavailable => CapabilityState::Unavailable,
            },
            detail: report.detail.clone(),
        })
        .collect::<Vec<_>>();
    capabilities.extend([
        CapabilityStatus {
            capability: "graph_delta_card_coverage".into(),
            state: if audited_omitted == 0 {
                CapabilityState::Ready
            } else {
                CapabilityState::Degraded
            },
            detail: format!(
                "beta={}; files={}; routes={}; changed_edges={}; impacted={}; tests={}; state_or_api={}; bypassed={}; behavioral_deltas={}; analogues={}; checklist={}; adapter_omissions={}; projection_candidates={}; rendered={}; projection_omitted={}; audited_omitted={}",
                card.beta,
                card.changed_files.len(),
                card.routes.len(),
                card.changed_edges.len(),
                card.impacted_loci.len(),
                card.impacted_tests.len(),
                card.impacted_state_or_api.len(),
                card.bypassed_loci.len(),
                card.behavioral_deltas.len(),
                card.behavioral_analogues.len(),
                card.affected_locus_checklist.len(),
                card.omissions.len(),
                candidates.len(),
                records.len(),
                projection_omitted,
                audited_omitted
            ),
        },
        CapabilityStatus {
            capability: "graph_delta_changed_files".into(),
            state: CapabilityState::Ready,
            detail: card
                .changed_files
                .iter()
                .map(|file| format!("{}:{} hunks", file.path, file.hunks.len()))
                .collect::<Vec<_>>()
                .join(", "),
        },
        CapabilityStatus {
            capability: "graph_delta_affected_locus_checklist".into(),
            state: if card.affected_locus_checklist.is_empty() || audited_omitted > 0 {
                CapabilityState::Degraded
            } else {
                CapabilityState::Ready
            },
            detail: card
                .affected_locus_checklist
                .iter()
                .map(|item| format!("{}={}", item.stable_id, item.label))
                .collect::<Vec<_>>()
                .join(", "),
        },
        CapabilityStatus {
            capability: "proposal_overlay_persistence".into(),
            state: CapabilityState::Ready,
            detail: "analysis projected an immutable GraphState snapshot; no live node, edge, source, index, or worktree state was mutated".into(),
        },
    ]);
    relationships.sort();
    relationships.dedup();
    (
        records,
        relationships,
        capabilities,
        omissions,
        candidate_audit,
    )
}

fn graph_delta_record_priority(record: &SelectedRecord) -> u8 {
    match record.selection.role {
        Some(ProjectionRole::ProposalDelta) => 0,
        Some(ProjectionRole::Test) => 1,
        Some(ProjectionRole::DefinitionOrApiState) => 2,
        Some(ProjectionRole::BehavioralAnalogue) => 3,
        Some(ProjectionRole::CallerOrImpact) => 4,
        Some(ProjectionRole::EditableSource) => 5,
        Some(ProjectionRole::DirectDependency) => 6,
        None => 7,
    }
}

fn graph_delta_projection_span(
    grounding: &graph_delta::EvidenceGrounding,
) -> Option<ProjectionSourceSpan> {
    match grounding {
        graph_delta::EvidenceGrounding::CurrentSource(span) => Some(ProjectionSourceSpan {
            root: span.root.clone(),
            path: span.path.clone(),
            start_line: span.start_line,
            end_line: span.end_line,
        }),
        graph_delta::EvidenceGrounding::Proposal(_) => None,
    }
}

fn graph_delta_grounding_node<'a>(
    grounding: &graph_delta::EvidenceGrounding,
    ctx: &'a SearchContext<'_>,
) -> Result<&'a Node, &'static str> {
    match grounding {
        graph_delta::EvidenceGrounding::CurrentSource(span) => {
            unique_node_at_line(ctx, &span.path, span.start_line)
        }
        graph_delta::EvidenceGrounding::Proposal(line) => {
            let current_line = line.old_line.ok_or(
                "added proposal line has no exact pre-edit coordinate; it remains proposal-only",
            )?;
            unique_node_at_line(ctx, &line.path, current_line)
        }
    }
}

fn insert_graph_delta_impact(
    impact: &graph_delta::ImpactEvidence,
    role: ProjectionRole,
    params: &SearchParams,
    ctx: &SearchContext<'_>,
    by_role: &mut BTreeMap<(String, ProjectionRole), SelectedRecord>,
    omissions: &mut Vec<ProjectionOmission>,
) {
    match graph_delta_grounding_node(&impact.grounding, ctx) {
        Ok(node) => insert_graph_delta_node(
            node,
            role,
            &format!(
                "graph-delta {:?} {} grounded by {}",
                impact.kind,
                impact.label,
                impact.grounding.stable_hydration_key()
            ),
            params,
            ctx,
            by_role,
        ),
        Err(reason) => omissions.push(ProjectionOmission {
            record_id: Some(impact.grounding.stable_hydration_key()),
            source: graph_delta_projection_span(&impact.grounding),
            code: OmissionCode::MissingSource,
            detail: format!(
                "graph-delta impact {:?} could not hydrate: {reason}",
                impact.label
            ),
        }),
    }
}

fn insert_graph_delta_node(
    node: &Node,
    role: ProjectionRole,
    reason: &str,
    params: &SearchParams,
    ctx: &SearchContext<'_>,
    by_role: &mut BTreeMap<(String, ProjectionRole), SelectedRecord>,
) {
    let id = node.stable_id();
    let mut selected =
        selected_from_exact_node(node, 0, reason, params.query.as_deref(), ctx.repo_root);
    selected.selection.role = Some(role);
    selected.selection.lane = Some(lane_for_role(role));
    by_role.entry((id, role)).or_insert(selected);
}

fn graph_delta_role(kind: graph_delta::ImpactKind) -> ProjectionRole {
    match kind {
        graph_delta::ImpactKind::EditableLocus => ProjectionRole::ProposalDelta,
        graph_delta::ImpactKind::Test => ProjectionRole::Test,
        graph_delta::ImpactKind::StateOrApi => ProjectionRole::DefinitionOrApiState,
        graph_delta::ImpactKind::Caller => ProjectionRole::CallerOrImpact,
        graph_delta::ImpactKind::BehavioralAnalogue => ProjectionRole::BehavioralAnalogue,
    }
}

fn graph_delta_capability_name(capability: graph_delta::GraphDeltaCapability) -> &'static str {
    match capability {
        graph_delta::GraphDeltaCapability::ProposalParsing => "graph_delta_proposal_parsing",
        graph_delta::GraphDeltaCapability::LiveGraphInference => "graph_delta_live_graph_inference",
        graph_delta::GraphDeltaCapability::RouteAnalysis => "graph_delta_route_analysis",
        graph_delta::GraphDeltaCapability::ImpactTraversal => "graph_delta_impact_traversal",
        graph_delta::GraphDeltaCapability::BehavioralAnalogueDiscovery => {
            "graph_delta_behavioral_analogue_discovery"
        }
    }
}

fn graph_delta_request_root(
    params: &SearchParams,
    ctx: &SearchContext<'_>,
) -> Result<String, String> {
    if let Some(root) = params
        .root
        .as_deref()
        .map(str::trim)
        .filter(|root| !root.is_empty() && *root != "all")
    {
        return Ok(root.to_owned());
    }
    if let Some(root) = ctx
        .root_filter
        .as_deref()
        .map(str::trim)
        .filter(|root| !root.is_empty() && *root != "all")
    {
        return Ok(root.to_owned());
    }
    let roots = ctx
        .graph_state
        .nodes
        .iter()
        .map(|node| node.id.root.as_str())
        .filter(|root| !root.is_empty())
        .collect::<BTreeSet<_>>();
    match roots.len() {
        1 => Ok((*roots.first().expect("one root exists")).to_owned()),
        0 => Err("an explicit root is required because the live graph has no root identity".into()),
        _ => Err("an explicit root is required for a multi-root graph-delta request".into()),
    }
}

fn graph_delta_snapshot(graph: &GraphState) -> graph_delta::GraphSnapshot {
    let mut nodes: Vec<_> = graph
        .nodes
        .iter()
        .filter_map(|node| {
            let span = node_source_span(node)?;
            safe_graph_delta_path(&span.path).then_some(graph_delta::GraphNode {
                id: node.stable_id(),
                kind: graph_delta_impact_kind(node, false),
                grounding: graph_delta::EvidenceGrounding::CurrentSource(graph_delta::SourceSpan {
                    root: span.root,
                    path: span.path,
                    start_line: span.start_line,
                    end_line: span.end_line,
                }),
            })
        })
        .collect();
    nodes.sort();
    nodes.dedup_by(|left, right| left.id == right.id);
    let grounding_by_id = nodes
        .iter()
        .map(|node| (node.id.clone(), node.grounding.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut edges = graph
        .edges
        .iter()
        .filter_map(|edge| {
            let from = edge.from.to_stable_id();
            let to = edge.to.to_stable_id();
            let grounding = grounding_by_id.get(&from)?.clone();
            grounding_by_id.get(&to)?;
            Some(graph_delta::WeightedEdge {
                key: graph_delta::EdgeKey {
                    from,
                    to,
                    kind: edge.kind.to_string(),
                },
                cost: 1,
                priority: 0,
                registration_order: 0,
                grounding,
            })
        })
        .collect::<Vec<_>>();
    edges.sort_by(|left, right| left.key.cmp(&right.key));
    for (index, edge) in edges.iter_mut().enumerate() {
        edge.registration_order = u32::try_from(index).unwrap_or(u32::MAX);
    }
    graph_delta::GraphSnapshot { nodes, edges }
}

fn safe_graph_delta_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn render_projected_input(
    request: ProjectionRequest,
    input: ProjectionInput,
    params: &SearchParams,
    ctx: &SearchContext<'_>,
) -> String {
    let reader = match projection_source_reader(params, ctx.repo_root) {
        Ok(reader) => reader,
        Err(error) => return format!("Search source projection unavailable: {error}."),
    };
    let plan = projection::plan_projection(request, input, &reader);
    render::render_projection(&plan)
        .map(|response| response.text)
        .unwrap_or_else(|error| format!("Search render failed: {error}."))
}

async fn projected_fused_candidates(
    params: &SearchParams,
    ctx: &SearchContext<'_>,
    edge_index: &ProjectedEdgeIndex<'_>,
) -> Result<
    (
        Vec<FusedCandidate>,
        Vec<CapabilityStatus>,
        BTreeMap<String, Vec<ProductScoreAudit>>,
    ),
    String,
> {
    let query = nonempty_query_preserving_bytes(params.query.as_deref()).unwrap_or("");
    let has_filters = params.kind.is_some()
        || params.language.is_some()
        || params.file.is_some()
        || params.synthetic.is_some()
        || params.min_complexity.is_some()
        || params.subsystem.is_some()
        || params.sort_by.is_some();
    let has_identity = params
        .node
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || params.nodes.as_ref().is_some_and(|nodes| !nodes.is_empty());
    if query.is_empty() && !has_filters && !has_identity {
        return Err("Empty query; provide a query, node, nodes, or browse filter".to_string());
    }

    let candidate_limit = if params.context_mode.as_deref() == Some("task") {
        params.limit.unwrap_or(40).clamp(20, 100)
    } else {
        params.limit.unwrap_or(10).clamp(20, 100)
    };
    let mut final_orders = BTreeMap::<EvidenceChannel, Vec<String>>::new();
    let mut final_scores = BTreeMap::<EvidenceChannel, (ScoreKind, Vec<RawCandidateScore>)>::new();
    let mut product_score_audit = BTreeMap::<String, Vec<ProductScoreAudit>>::new();
    let mut capabilities = Vec::new();
    let mut nodes = if has_identity {
        resolve_projected_entry_nodes(params, ctx)
    } else {
        let search_mode = parse_search_mode(params.search_mode.as_deref());
        let result = flat_code_symbol_search_with_diagnostics(
            query,
            search_mode,
            candidate_limit,
            params,
            ctx.graph_state,
            ctx,
            params.sort_by.as_deref() == Some("complexity"),
            params.sort_by.as_deref() == Some("importance"),
        )
        .await;
        if let Some(reason) = result.strict_failure {
            return Err(reason.to_string());
        }
        if let Some(diagnostic) = result.scorer_diagnostic {
            capabilities.push(CapabilityStatus {
                capability: "semantic_search".into(),
                state: CapabilityState::Degraded,
                detail: diagnostic.render(),
            });
        }
        final_scores = result.score_evidence;
        product_score_audit = result.product_score_audit;
        final_orders = result.order_evidence;
        let mut matches = result.matches;
        // Fuse the union of bounded per-channel observations. The legacy flat
        // list is still retained for legacy rendering, but it must not evict a
        // candidate from another channel before calibrated fusion sees it.
        let mut observed_ids = BTreeSet::new();
        for order in final_orders.values() {
            observed_ids.extend(order.iter().cloned());
        }
        for id in observed_ids {
            if let Some(node) = find_node(ctx.graph_state, &id) {
                matches.push(node);
            }
        }
        matches
    };

    let mut graph_ids = BTreeSet::new();
    if has_identity && params.normalized_mode().is_some() {
        let seeds: Vec<_> = nodes.iter().map(|node| node.stable_id()).collect();
        for seed in seeds {
            for id in projected_traversal_ids(params, ctx.graph_state, &seed) {
                graph_ids.insert(id.clone());
                if let Some(node) = find_node(ctx.graph_state, &id) {
                    nodes.push(node);
                }
            }
        }
    }
    let mut unique = BTreeMap::new();
    for node in nodes
        .into_iter()
        .filter(|node| projected_node_passes(node, params, ctx))
    {
        unique.entry(node.stable_id()).or_insert(node);
    }
    let nodes: Vec<_> = unique.values().copied().collect();
    if nodes.is_empty() {
        return Ok((Vec::new(), capabilities, product_score_audit));
    }

    let mut channels = Vec::new();
    if !final_orders.contains_key(&EvidenceChannel::ExactLexical) {
        channels.push(ChannelInput::new(
            EvidenceChannel::ExactLexical,
            ScoreKind::LexicalHeuristic,
            nodes
                .iter()
                .map(|node| RawCandidateScore::new(node.stable_id(), lexical_score(node, query)))
                .collect(),
        ));
    }
    let scored_channels = final_scores.keys().copied().collect::<BTreeSet<_>>();
    for (channel, (score_kind, scores)) in final_scores {
        let scores = scores
            .into_iter()
            .filter(|score| unique.contains_key(&score.stable_id))
            .collect::<Vec<_>>();
        if scores.is_empty() {
            continue;
        }
        channels.push(ChannelInput::new(channel, score_kind, scores));
    }
    for (channel, order) in final_orders {
        if scored_channels.contains(&channel) {
            continue;
        }
        if order.is_empty() {
            continue;
        }
        channels.push(ChannelInput::new(
            channel,
            ScoreKind::WithinChannelRank,
            order
                .into_iter()
                .enumerate()
                .filter(|(_, id)| unique.contains_key(id))
                .map(|(rank, id)| RawCandidateScore::new(id, (rank + 1) as f64))
                .collect(),
        ));
    }
    if !channels
        .iter()
        .any(|channel| channel.channel == EvidenceChannel::Structural)
        && graph_ids.is_empty()
    {
        channels.push(ChannelInput::new(
            EvidenceChannel::Structural,
            ScoreKind::EdgeDegree,
            nodes
                .iter()
                .map(|node| {
                    RawCandidateScore::new(
                        node.stable_id(),
                        edge_index.degree(&node.stable_id()) as f64,
                    )
                })
                .collect(),
        ));
    }
    if !graph_ids.is_empty() {
        channels.push(ChannelInput::new(
            EvidenceChannel::Graph,
            ScoreKind::GraphHeuristic,
            graph_ids
                .into_iter()
                .filter_map(|id| {
                    find_node(ctx.graph_state, &id).map(|_node| {
                        RawCandidateScore::new(id.clone(), edge_index.degree(&id) as f64)
                    })
                })
                .collect(),
        ));
    }
    let policy = if params.context_mode.as_deref() == Some("task") {
        FusionPolicy::task_search()
    } else {
        FusionPolicy::ordinary_search()
    };
    fuse_ranked_channels(policy, &channels)
        .map(|fused| (fused, capabilities, product_score_audit))
        .map_err(|error| error.to_string())
}

fn resolve_projected_entry_nodes<'a>(
    params: &SearchParams,
    ctx: &'a SearchContext<'_>,
) -> Vec<&'a Node> {
    let requested = params
        .nodes
        .as_ref()
        .map(|values| values.iter().map(String::as_str).collect::<Vec<_>>())
        .unwrap_or_else(|| params.node.as_deref().into_iter().collect());
    let mut matches = Vec::new();
    for value in requested {
        let value = value.trim();
        let mut found: Vec<_> = ctx
            .graph_state
            .nodes
            .iter()
            .filter(|node| node.stable_id() == value || node.id.name == value)
            .collect();
        found.sort_by_key(|node| node.stable_id());
        matches.extend(found);
    }
    matches
}

fn projected_traversal_ids(params: &SearchParams, graph: &GraphState, seed: &str) -> Vec<String> {
    let hops = params
        .hops
        .or(params.depth)
        .unwrap_or(1)
        .min(MAX_CONTEXT_HOPS) as usize;
    match params.normalized_mode() {
        Some("impact") => graph.index.impact(seed, hops, None),
        Some("reachable") => graph.index.reachable(seed, hops, None),
        _ => {
            let mut ids = graph
                .index
                .neighbors(seed, None, petgraph::Direction::Outgoing);
            ids.extend(
                graph
                    .index
                    .neighbors(seed, None, petgraph::Direction::Incoming),
            );
            ids.sort();
            ids.dedup();
            ids
        }
    }
}

fn projected_node_passes(node: &Node, params: &SearchParams, ctx: &SearchContext<'_>) -> bool {
    let included = match node_delivery_class(node) {
        NodeDeliveryClass::Code => true,
        NodeDeliveryClass::Markdown => params.include_markdown,
        NodeDeliveryClass::Artifact => {
            params.include_artifacts
                && params.artifact_types.as_ref().is_none_or(|types| {
                    let kind = artifact_kind(node);
                    types.is_empty() || types.iter().any(|expected| expected == kind)
                })
        }
    };
    included
        && node_passes_root_filter(&node.id.root, &ctx.root_filter, &ctx.non_code_slugs)
        && params
            .kind
            .as_ref()
            .is_none_or(|kind| node.id.kind.to_string().eq_ignore_ascii_case(kind))
        && params
            .language
            .as_ref()
            .is_none_or(|language| node.language.eq_ignore_ascii_case(language))
        && params
            .file
            .as_ref()
            .is_none_or(|file| node.id.file.to_string_lossy().contains(file))
        && params.min_complexity.is_none_or(|minimum| {
            node.metadata
                .get("cyclomatic")
                .and_then(|value| value.parse::<u32>().ok())
                .is_some_and(|value| value >= minimum)
        })
        && params.synthetic.is_none_or(|expected| {
            node.metadata
                .get("synthetic")
                .is_some_and(|value| value == "true")
                == expected
        })
        && params.subsystem.as_ref().is_none_or(|expected| {
            node.metadata
                .get(crate::server::SUBSYSTEM_KEY)
                .is_some_and(|actual| subsystem_matches(actual, expected))
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeDeliveryClass {
    Code,
    Markdown,
    Artifact,
}

fn node_delivery_class(node: &Node) -> NodeDeliveryClass {
    if node.metadata.contains_key("oh_kind") || node.id.kind == NodeKind::PrMerge {
        NodeDeliveryClass::Artifact
    } else if node
        .metadata
        .get("local_knowledge")
        .is_some_and(|value| value == "true")
    {
        // A frontmatter-declared domain entity is a persisted graph record,
        // not a generic Markdown search chunk. Preserve exact graph lookup
        // even when callers disable live Markdown expansion.
        NodeDeliveryClass::Code
    } else if node.id.kind == NodeKind::MarkdownSection
        || matches!(
            node.language.to_ascii_lowercase().as_str(),
            "markdown" | "rst"
        )
    {
        NodeDeliveryClass::Markdown
    } else {
        NodeDeliveryClass::Code
    }
}

fn artifact_kind(node: &Node) -> &str {
    node.metadata
        .get("oh_kind")
        .map(String::as_str)
        .unwrap_or_else(|| match &node.id.kind {
            NodeKind::PrMerge => "merge",
            _ => "artifact",
        })
}

fn lexical_score(node: &Node, query: &str) -> f64 {
    if query.is_empty() {
        return 1.0;
    }
    let query = query.to_lowercase();
    let name = node.id.name.to_lowercase();
    if name == query {
        100.0
    } else if name.contains(&query) {
        80.0
    } else if node.signature.to_lowercase().contains(&query) {
        60.0
    } else if node
        .id
        .file
        .to_string_lossy()
        .to_lowercase()
        .contains(&query)
    {
        40.0
    } else {
        1.0
    }
}

struct ProjectedEdgeIndex<'a> {
    outgoing: HashMap<String, Vec<&'a Edge>>,
    incoming: HashMap<String, Vec<&'a Edge>>,
    degrees: HashMap<String, usize>,
}

impl<'a> ProjectedEdgeIndex<'a> {
    fn new(graph: &'a GraphState) -> Self {
        let mut outgoing = HashMap::<String, Vec<&Edge>>::new();
        let mut incoming = HashMap::<String, Vec<&Edge>>::new();
        let mut degrees = HashMap::<String, usize>::new();
        for edge in &graph.edges {
            let from = edge.from.to_stable_id();
            let to = edge.to.to_stable_id();
            outgoing.entry(from.clone()).or_default().push(edge);
            incoming.entry(to.clone()).or_default().push(edge);
            *degrees.entry(from.clone()).or_default() += 1;
            if to != from {
                *degrees.entry(to).or_default() += 1;
            }
        }
        Self {
            outgoing,
            incoming,
            degrees,
        }
    }

    fn outgoing(&self, id: &str) -> &[&'a Edge] {
        self.outgoing.get(id).map(Vec::as_slice).unwrap_or_default()
    }

    fn incoming(&self, id: &str) -> &[&'a Edge] {
        self.incoming.get(id).map(Vec::as_slice).unwrap_or_default()
    }

    fn degree(&self, id: &str) -> usize {
        self.degrees.get(id).copied().unwrap_or_default()
    }

    fn contains(&self, from: &str, to: &str, kind: &str) -> bool {
        self.outgoing(from)
            .iter()
            .any(|edge| edge.to.to_stable_id() == to && edge.kind.to_string() == kind)
    }
}

fn find_node<'a>(graph: &'a GraphState, id: &str) -> Option<&'a Node> {
    graph.node_by_stable_id(id, graph.node_index_map())
}

fn symbol_summary(node: &Node) -> SymbolSummary {
    let local_knowledge = node
        .metadata
        .get("local_knowledge")
        .is_some_and(|value| value == "true");
    SymbolSummary {
        name: node.id.name.clone(),
        kind: node.id.kind.to_string(),
        language: node.language.clone(),
        signature: node.signature.clone(),
        extraction_source: local_knowledge.then(|| node.source.to_string()),
        declared_metadata: if local_knowledge {
            node.metadata
                .iter()
                .filter_map(|(key, value)| {
                    key.strip_prefix("rna.metadata.")
                        .map(|name| (name.to_string(), value.clone()))
                })
                .collect()
        } else {
            BTreeMap::new()
        },
    }
}

fn node_projection_digest(node: &Node) -> String {
    let canonical = serde_json::to_vec(&(
        node.stable_id(),
        node.id.name.as_str(),
        node.id.kind.to_string(),
        node.language.as_str(),
        node.signature.as_str(),
        node.body.as_str(),
        node_source_span(node),
    ))
    .expect("node projection fields are JSON serializable");
    blake3::hash(&canonical).to_hex().to_string()
}

fn node_source_span(node: &Node) -> Option<ProjectionSourceSpan> {
    let start_line = u32::try_from(node.line_start).ok()?;
    let end_line = u32::try_from(node.line_end).ok()?;
    let span = ProjectionSourceSpan {
        root: node.id.root.clone(),
        path: node.id.file.to_string_lossy().replace('\\', "/"),
        start_line,
        end_line,
    };
    span.is_valid().then_some(span)
}

fn default_role(node: &Node) -> ProjectionRole {
    let path = node.id.file.to_string_lossy().to_ascii_lowercase();
    if path.contains("test")
        || node
            .metadata
            .get("is_test")
            .is_some_and(|value| value == "true")
    {
        ProjectionRole::Test
    } else if matches!(
        &node.id.kind,
        NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Enum
            | NodeKind::TypeAlias
            | NodeKind::Const
            | NodeKind::ApiEndpoint
            | NodeKind::SqlTable
            | NodeKind::ProtoMessage
    ) {
        ProjectionRole::DefinitionOrApiState
    } else {
        ProjectionRole::EditableSource
    }
}

fn lane_for_role(role: ProjectionRole) -> ProjectionLane {
    match role {
        ProjectionRole::EditableSource => ProjectionLane::EditableSource,
        ProjectionRole::DefinitionOrApiState => ProjectionLane::DefinitionOrState,
        ProjectionRole::Test => ProjectionLane::Tests,
        ProjectionRole::BehavioralAnalogue => ProjectionLane::Analogues,
        ProjectionRole::DirectDependency => ProjectionLane::Dependencies,
        ProjectionRole::CallerOrImpact => ProjectionLane::GraphImpact,
        ProjectionRole::ProposalDelta => ProjectionLane::ProposalDelta,
    }
}

fn channel_for_evidence(channel: EvidenceChannel) -> SelectionChannel {
    match channel {
        EvidenceChannel::ExactLexical => SelectionChannel::Exact,
        EvidenceChannel::FullText
        | EvidenceChannel::Vector
        | EvidenceChannel::HybridRrf
        | EvidenceChannel::Rerank => SelectionChannel::Semantic,
        EvidenceChannel::Graph => SelectionChannel::Graph,
        EvidenceChannel::Structural => SelectionChannel::Lexical,
    }
}

fn evidence_channel_for_executed_mode(mode: ExecutedSearchMode) -> EvidenceChannel {
    match mode {
        ExecutedSearchMode::Keyword => EvidenceChannel::FullText,
        ExecutedSearchMode::Semantic => EvidenceChannel::Vector,
        ExecutedSearchMode::HybridRrf => EvidenceChannel::HybridRrf,
    }
}

fn product_test_policy(params: &SearchParams, query: &str) -> TestResultPolicy {
    let explicit_role = params
        .context_roles
        .as_deref()
        .is_some_and(|roles| roles.iter().any(|role| role == "test"));
    let explicit_facet = params
        .context_facets
        .as_deref()
        .is_some_and(|facets| facets.iter().any(|facet| facet == "test"));
    let query_requests_tests = query
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .any(|term| {
            matches!(
                term.to_ascii_lowercase().as_str(),
                "test" | "tests" | "testing" | "spec" | "specs" | "regression" | "fixture"
            )
        });
    if explicit_role || explicit_facet || query_requests_tests {
        TestResultPolicy::Neutral
    } else {
        TestResultPolicy::Demote
    }
}

const FOCUSED_SOURCE_LINES: u32 = 40;
const FOCUSED_SOURCE_CONTEXT_BEFORE: u32 = 12;

fn query_focused_span(
    node: &Node,
    source: &ProjectionSourceSpan,
    query: Option<&str>,
) -> (Option<ProjectionSourceSpan>, Option<String>) {
    let line_count = source
        .end_line
        .saturating_sub(source.start_line)
        .saturating_add(1);
    if line_count <= FOCUSED_SOURCE_LINES {
        return (None, None);
    }

    let query = query.unwrap_or_default().to_ascii_lowercase();
    let node_name = node.id.name.to_ascii_lowercase();
    let mut terms = BTreeMap::<String, u32>::new();
    if node_name.len() >= 3 && query.contains(&node_name) {
        terms.insert(node_name.clone(), 1);
    }
    for term in query.split(|character: char| !character.is_alphanumeric() && character != '_') {
        if term.len() >= 3
            && !matches!(
                term,
                "and"
                    | "the"
                    | "for"
                    | "with"
                    | "from"
                    | "this"
                    | "that"
                    | "into"
                    | "when"
                    | "where"
                    | "should"
                    | "must"
            )
        {
            let behavior_weight = if matches!(
                term,
                "behavior"
                    | "behaviour"
                    | "error"
                    | "failure"
                    | "state"
                    | "contract"
                    | "regression"
                    | "validate"
                    | "persist"
                    | "discard"
            ) {
                8
            } else if term == node_name {
                1
            } else {
                4
            };
            terms
                .entry(term.to_owned())
                .and_modify(|weight| *weight = (*weight).max(behavior_weight))
                .or_insert(behavior_weight);
        }
    }

    let body_lines = node
        .body
        .lines()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let matched = body_lines
        .iter()
        .enumerate()
        .filter_map(|(line_number, line)| {
            let matched_terms = terms
                .iter()
                .filter(|(term, _)| line.contains(term.as_str()))
                .collect::<Vec<_>>();
            let score = matched_terms
                .iter()
                .map(|(_, weight)| **weight)
                .sum::<u32>();
            (score > 0).then(|| {
                (
                    score,
                    std::cmp::Reverse(line_number),
                    matched_terms
                        .iter()
                        .map(|(term, _)| term.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                )
            })
        })
        .max()
        .map(|(score, std::cmp::Reverse(line), terms)| (line, score, terms));
    let (match_offset, grounding) = matched.map_or_else(
        || {
            (
                0usize,
                "fallback:first_window:no_query_term_matched_authoritative_body".to_string(),
            )
        },
        |(line, score, terms)| {
            (
                line,
                format!(
                    "query_terms={terms:?}; score={score}; body_line={}",
                    line + 1
                ),
            )
        },
    );
    let matched_line = source
        .start_line
        .saturating_add(u32::try_from(match_offset).unwrap_or(u32::MAX))
        .min(source.end_line);
    let mut start_line = matched_line
        .saturating_sub(FOCUSED_SOURCE_CONTEXT_BEFORE)
        .max(source.start_line);
    let mut end_line = start_line
        .saturating_add(FOCUSED_SOURCE_LINES - 1)
        .min(source.end_line);
    if end_line.saturating_sub(start_line).saturating_add(1) < FOCUSED_SOURCE_LINES {
        start_line = end_line
            .saturating_sub(FOCUSED_SOURCE_LINES - 1)
            .max(source.start_line);
        end_line = start_line
            .saturating_add(FOCUSED_SOURCE_LINES - 1)
            .min(source.end_line);
    }
    (
        Some(ProjectionSourceSpan {
            start_line,
            end_line,
            ..source.clone()
        }),
        Some(grounding),
    )
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceCapsuleV1 {
    schema_version: u8,
    record_id: String,
    query_digest: String,
    selection_rank: usize,
    selection: SelectionSummary,
    evidence: SelectionEvidence,
    current_node_content_hash: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticEvidenceCapsuleV1 {
    schema_version: u8,
    record_id: String,
    symbol: SymbolSummary,
    selection: SelectionSummary,
    evidence: SelectionEvidence,
    body: String,
}

fn persist_semantic_evidence_capsule(
    repo_root: &Path,
    selected: &SelectedRecord,
    body: &str,
) -> Result<HydrationHandle, String> {
    let content_hash = blake3::hash(body.as_bytes()).to_hex().to_string();
    if selected.evidence.content_hash.as_deref() != Some(content_hash.as_str()) {
        return Err("semantic row body does not match its selection evidence hash".into());
    }
    let capsule = SemanticEvidenceCapsuleV1 {
        schema_version: EVIDENCE_CAPSULE_SCHEMA_VERSION,
        record_id: selected.identity.node_id.clone(),
        symbol: selected.symbol.clone(),
        selection: selected.selection.clone(),
        evidence: selected.evidence.clone(),
        body: body.to_string(),
    };
    let bytes = serde_json::to_vec(&capsule)
        .map_err(|error| format!("semantic evidence capsule serialization failed: {error}"))?;
    if bytes.len() > MAX_EVIDENCE_CAPSULE_BYTES {
        return Err(format!(
            "semantic evidence capsule is {} bytes; limit is {MAX_EVIDENCE_CAPSULE_BYTES}",
            bytes.len()
        ));
    }
    let digest = blake3::hash(&bytes).to_hex().to_string();
    let directory = evidence_capsule_directory(repo_root, true)?;
    let destination = directory.join(format!("{digest}.json"));
    if destination.exists() {
        let existing = read_evidence_capsule_bytes(&destination)?;
        if existing != bytes {
            return Err("content-addressed semantic evidence capsule collision".into());
        }
    } else {
        write_evidence_capsule_atomic(&directory, &destination, &digest, &bytes)?;
    }
    Ok(HydrationHandle::evidence(format!(
        "{SEMANTIC_EVIDENCE_CAPSULE_PREFIX}{digest}:{}",
        selected.identity.node_id
    )))
}

fn persist_evidence_capsule(
    repo_root: &Path,
    query: &str,
    selected: &SelectedRecord,
) -> Result<HydrationHandle, String> {
    let current_node_content_hash = selected
        .evidence
        .content_hash
        .clone()
        .ok_or_else(|| "selected record has no authoritative content hash".to_string())?;
    let capsule = EvidenceCapsuleV1 {
        schema_version: EVIDENCE_CAPSULE_SCHEMA_VERSION,
        record_id: selected.identity.node_id.clone(),
        query_digest: blake3::hash(query.as_bytes()).to_hex().to_string(),
        selection_rank: selected.selection_rank,
        selection: selected.selection.clone(),
        evidence: selected.evidence.clone(),
        current_node_content_hash,
    };
    let bytes = serde_json::to_vec(&capsule)
        .map_err(|error| format!("evidence capsule serialization failed: {error}"))?;
    if bytes.len() > MAX_EVIDENCE_CAPSULE_BYTES {
        return Err(format!(
            "evidence capsule is {} bytes; limit is {MAX_EVIDENCE_CAPSULE_BYTES}",
            bytes.len()
        ));
    }
    let digest = blake3::hash(&bytes).to_hex().to_string();
    let directory = evidence_capsule_directory(repo_root, true)?;
    let destination = directory.join(format!("{digest}.json"));
    if destination.exists() {
        let existing = read_evidence_capsule_bytes(&destination)?;
        if existing != bytes {
            return Err("content-addressed evidence capsule collision".into());
        }
    } else {
        write_evidence_capsule_atomic(&directory, &destination, &digest, &bytes)?;
    }
    Ok(HydrationHandle::evidence(format!(
        "{EVIDENCE_CAPSULE_PREFIX}{digest}:{}",
        selected.identity.node_id
    )))
}

fn evidence_capsule_directory(repo_root: &Path, create: bool) -> Result<PathBuf, String> {
    let mut current = repo_root.to_path_buf();
    for component in [".oh", ".cache", "search_evidence", "v1"] {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "evidence cache component is a symlink: {}",
                    current.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!(
                    "evidence cache component is not a directory: {}",
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                if let Err(create_error) = fs::create_dir(&current)
                    && create_error.kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(format!(
                        "failed to create evidence cache {}: {create_error}",
                        current.display()
                    ));
                }
                let metadata = fs::symlink_metadata(&current).map_err(|metadata_error| {
                    format!(
                        "failed to verify evidence cache {}: {metadata_error}",
                        current.display()
                    )
                })?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(format!(
                        "evidence cache component is not a real directory: {}",
                        current.display()
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err("evidence cache is unavailable".into());
            }
            Err(error) => {
                return Err(format!(
                    "failed to inspect evidence cache {}: {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(current)
}

fn write_evidence_capsule_atomic(
    directory: &Path,
    destination: &Path,
    digest: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let mut temporary = None;
    for attempt in 0..16u8 {
        let path = directory.join(format!(".{digest}.tmp-{}-{attempt}", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(bytes)
                    .and_then(|()| file.sync_all())
                    .map_err(|error| format!("failed to seal evidence capsule: {error}"))?;
                temporary = Some(path);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!("failed to create evidence capsule: {error}"));
            }
        }
    }
    let temporary = temporary.ok_or_else(|| "evidence capsule temp slots exhausted".to_string())?;
    match fs::hard_link(&temporary, destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = read_evidence_capsule_bytes(destination)?;
            if existing != bytes {
                let _ = fs::remove_file(&temporary);
                return Err("content-addressed evidence capsule collision".into());
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(format!("failed to publish evidence capsule: {error}"));
        }
    }
    fs::remove_file(&temporary)
        .map_err(|error| format!("failed to remove evidence capsule temp file: {error}"))?;
    Ok(())
}

fn read_evidence_capsule_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect evidence capsule: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("evidence capsule is not a regular file".into());
    }
    if metadata.len() > MAX_EVIDENCE_CAPSULE_BYTES as u64 {
        return Err(format!(
            "evidence capsule exceeds {MAX_EVIDENCE_CAPSULE_BYTES} bytes"
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(path)
        .and_then(|mut file| {
            std::io::Read::by_ref(&mut file)
                .take(MAX_EVIDENCE_CAPSULE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| format!("failed to read evidence capsule: {error}"))?;
    if bytes.len() > MAX_EVIDENCE_CAPSULE_BYTES {
        return Err(format!(
            "evidence capsule exceeds {MAX_EVIDENCE_CAPSULE_BYTES} bytes"
        ));
    }
    Ok(bytes)
}

fn parse_evidence_capsule_reference(record_id: &str) -> Result<(&str, &str), String> {
    let reference = record_id
        .strip_prefix(EVIDENCE_CAPSULE_PREFIX)
        .ok_or_else(|| "unsupported evidence hydration reference".to_string())?;
    let (digest, node_id) = reference
        .split_once(':')
        .ok_or_else(|| "invalid evidence hydration reference".to_string())?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || node_id.is_empty()
    {
        return Err("invalid evidence hydration reference".into());
    }
    Ok((digest, node_id))
}

fn parse_semantic_evidence_capsule_reference(record_id: &str) -> Result<(&str, &str), String> {
    let reference = record_id
        .strip_prefix(SEMANTIC_EVIDENCE_CAPSULE_PREFIX)
        .ok_or_else(|| "unsupported semantic evidence hydration reference".to_string())?;
    let (digest, semantic_id) = reference
        .split_once(':')
        .ok_or_else(|| "invalid semantic evidence hydration reference".to_string())?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || semantic_id.is_empty()
    {
        return Err("invalid semantic evidence hydration reference".into());
    }
    Ok((digest, semantic_id))
}

#[derive(Default)]
struct SelectionPlacement {
    role: Option<ProjectionRole>,
    lane: Option<ProjectionLane>,
    reason: Option<String>,
}

fn selected_from_fused(
    node: &Node,
    fused: &FusedCandidate,
    rank: usize,
    placement: SelectionPlacement,
    query: Option<&str>,
    repo_root: &Path,
) -> SelectedRecord {
    let SelectionPlacement { role, lane, reason } = placement;
    let source = node_source_span(node);
    let (focused_span, focus_grounding) = source
        .as_ref()
        .map(|span| query_focused_span(node, span, query))
        .unwrap_or((None, None));
    let mut evidence = SelectionEvidence {
        candidate_rank: Some(rank + 1),
        content_hash: Some(node_projection_digest(node)),
        ..Default::default()
    };
    for channel in &fused.channels {
        evidence.raw_scores.insert(
            channel.channel.label().to_string(),
            channel.raw_score.to_string(),
        );
        evidence.provenance.push(EvidenceProvenance {
            source: channel.channel.label().to_string(),
            detail: format!(
                "kind={} rank={} depth={} normalized_rank_micros={} weight={} contribution={}",
                channel.score_kind.label(),
                channel.rank,
                channel.depth,
                channel.normalized_rank_micros,
                channel.weight,
                channel.contribution
            ),
        });
    }
    let tie_vector = fused
        .tie_break
        .channel_ranks
        .iter()
        .map(|entry| {
            format!(
                "{}={}",
                entry.channel.label(),
                entry
                    .rank
                    .map_or_else(|| "none".to_string(), |rank| rank.to_string())
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let fusion_reason = fused.final_reason.summary();
    evidence.diagnostics.insert(
        "fusion_policy".into(),
        fused.final_reason.policy.label().into(),
    );
    if let Some(focus_grounding) = focus_grounding {
        evidence
            .diagnostics
            .insert("focus_grounding".into(), focus_grounding);
    }
    evidence
        .diagnostics
        .insert("final_score".into(), fused.final_score.to_string());
    evidence
        .diagnostics
        .insert("final_reason".into(), fusion_reason.clone());
    evidence.diagnostics.insert(
        "tie_break".into(),
        format!(
            "channel_ranks=[{tie_vector}] stable_id_utf8={:?}",
            fused.tie_break.stable_id_utf8
        ),
    );
    let role = role.unwrap_or_else(|| default_role(node));
    let lane = lane.unwrap_or_else(|| lane_for_role(role));
    let selection_channel = match node_delivery_class(node) {
        NodeDeliveryClass::Markdown => SelectionChannel::Markdown,
        NodeDeliveryClass::Artifact => SelectionChannel::Artifact,
        NodeDeliveryClass::Code => channel_for_evidence(fused.final_reason.leading_channel),
    };
    let reason = reason.map_or(fusion_reason.clone(), |task_reason| {
        format!("{fusion_reason}; {task_reason}")
    });
    let mut selected = SelectedRecord {
        selection_rank: rank,
        identity: RecordIdentity {
            node_id: node.stable_id(),
            source,
        },
        symbol: symbol_summary(node),
        selection: SelectionSummary {
            channel: selection_channel,
            reason,
            role: Some(role),
            lane: Some(lane),
        },
        evidence,
        evidence_hydration: None,
        focused_span,
    };
    match persist_evidence_capsule(repo_root, query.unwrap_or_default(), &selected) {
        Ok(handle) => selected.evidence_hydration = Some(handle),
        Err(error) => {
            selected
                .evidence
                .diagnostics
                .insert("evidence_hydration".into(), format!("unavailable: {error}"));
        }
    }
    selected
}

fn selected_from_exact_node(
    node: &Node,
    rank: usize,
    reason: &str,
    query: Option<&str>,
    repo_root: &Path,
) -> SelectedRecord {
    let fused = fuse_ranked_channels(
        FusionPolicy::ordinary_search(),
        &[ChannelInput::new(
            EvidenceChannel::ExactLexical,
            ScoreKind::ExactMatchTier,
            vec![RawCandidateScore::new(node.stable_id(), 1.0)],
        )],
    )
    .expect("a single finite exact candidate satisfies ordinary fusion")
    .remove(0);
    selected_from_fused(
        node,
        &fused,
        rank,
        SelectionPlacement {
            role: None,
            lane: Some(ProjectionLane::ExactReference),
            reason: Some(reason.to_string()),
        },
        query,
        repo_root,
    )
}

async fn default_capabilities(
    ctx: &SearchContext<'_>,
    projection: SearchProjection,
) -> Vec<CapabilityStatus> {
    let semantic_attached = ctx.embed_index.is_some();
    let semantic_probe = match ctx.embed_index {
        Some(index) => index.has_table().await,
        None => Ok(false),
    };
    let semantic_available = semantic_probe.as_ref().copied().unwrap_or(false);
    let semantic = if let Some(status) = ctx.embed_status {
        let readiness = status.capability_readiness(semantic_attached, semantic_available);
        CapabilityStatus {
            capability: "semantic_search".into(),
            state: projection_capability_state(readiness.state),
            detail: readiness.detail,
        }
    } else {
        match semantic_probe {
            Ok(true) => CapabilityStatus {
                capability: "semantic_search".into(),
                state: CapabilityState::Ready,
                detail: "embedding table was probed and is queryable".into(),
            },
            Ok(false) if semantic_attached => CapabilityStatus {
                capability: "semantic_search".into(),
                state: CapabilityState::Degraded,
                detail: "embedding index is attached but its table is not queryable".into(),
            },
            Ok(false) => CapabilityStatus {
                capability: "semantic_search".into(),
                state: CapabilityState::Unavailable,
                detail: "embedding index is not attached; lexical/graph evidence used".into(),
            },
            Err(error) => CapabilityStatus {
                capability: "semantic_search".into(),
                state: CapabilityState::Degraded,
                detail: format!("embedding readiness probe failed: {error}"),
            },
        }
    };
    let lsp = ctx.lsp_status.map_or_else(
        || CapabilityStatus {
            capability: "lsp_call_references".into(),
            state: CapabilityState::Unavailable,
            detail: "LSP status is not attached".into(),
        },
        |status| {
            let readiness = status.call_reference_readiness();
            CapabilityStatus {
                capability: "lsp_call_references".into(),
                state: projection_capability_state(readiness.state),
                detail: readiness.detail,
            }
        },
    );
    let mut capabilities = vec![semantic, lsp.clone()];
    if projection == SearchProjection::Evidence {
        capabilities.push(CapabilityStatus {
            capability: "readiness_diagnostics".into(),
            state: lsp.state,
            detail: format_verbose_readiness(
                ctx.graph_state,
                ctx,
                semantic_attached,
                semantic_available,
            ),
        });
    }
    capabilities
}

fn projection_capability_state(state: CapabilityReadinessState) -> CapabilityState {
    match state {
        CapabilityReadinessState::Ready => CapabilityState::Ready,
        CapabilityReadinessState::Partial
        | CapabilityReadinessState::Running
        | CapabilityReadinessState::Stale => CapabilityState::Degraded,
        CapabilityReadinessState::Failed
        | CapabilityReadinessState::Unavailable
        | CapabilityReadinessState::NotNeeded => CapabilityState::Unavailable,
    }
}

fn merge_capabilities(capabilities: Vec<CapabilityStatus>) -> Vec<CapabilityStatus> {
    let severity = |state| match state {
        CapabilityState::Ready => 0,
        CapabilityState::Degraded => 1,
        CapabilityState::Unavailable => 2,
    };
    let mut merged = BTreeMap::<String, CapabilityStatus>::new();
    for capability in capabilities {
        match merged.entry(capability.capability.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(capability);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let current = entry.get_mut();
                if severity(capability.state) > severity(current.state) {
                    current.state = capability.state;
                }
                if !current.detail.contains(&capability.detail) {
                    current.detail.push_str("; ");
                    current.detail.push_str(&capability.detail);
                }
            }
        }
    }
    merged.into_values().collect()
}

fn projected_relationships(
    edge_index: &ProjectedEdgeIndex<'_>,
    records: &[SelectedRecord],
) -> Vec<ProjectedRelationship> {
    let ids: BTreeSet<_> = records
        .iter()
        .map(|record| record.identity.node_id.as_str())
        .collect();
    let mut relationships = Vec::new();
    for id in &ids {
        relationships.extend(edge_index.outgoing(id).iter().filter_map(|edge| {
            let to = edge.to.to_stable_id();
            ids.contains(to.as_str()).then(|| ProjectedRelationship {
                from: edge.from.to_stable_id(),
                kind: edge.kind.to_string(),
                to,
                reason: format!("{} {:?}", edge.source, edge.confidence),
            })
        }));
    }
    relationships.sort();
    relationships.dedup();
    relationships
}

fn task_future_interaction_signature(
    selected_ids: &[String],
    remaining_ids: &[String],
    edge_index: &ProjectedEdgeIndex<'_>,
) -> task_context::FutureInteractionSignature {
    let selected = selected_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    remaining_ids
        .iter()
        .map(|remaining_id| {
            let mut relationships = BTreeSet::new();
            for selected_id in &selected {
                for edge in edge_index.outgoing(selected_id) {
                    let to = edge.to.to_stable_id();
                    if to == *remaining_id {
                        relationships.insert((
                            edge.from.to_stable_id(),
                            edge.kind.to_string(),
                            to,
                            format!("{} {:?}", edge.source, edge.confidence),
                        ));
                    }
                }
            }
            for edge in edge_index.outgoing(remaining_id) {
                let to = edge.to.to_stable_id();
                if selected.contains(to.as_str()) {
                    relationships.insert((
                        edge.from.to_stable_id(),
                        edge.kind.to_string(),
                        to,
                        format!("{} {:?}", edge.source, edge.confidence),
                    ));
                }
            }
            (remaining_id.clone(), relationships.into_iter().collect())
        })
        .collect()
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

async fn poll_scorer_with_content_safe_panic_hook<F, T>(scorer: F) -> anyhow::Result<T>
where
    F: std::future::Future<Output = anyhow::Result<T>>,
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

async fn isolate_embedding_scorer<F, T>(
    scorer: F,
    mode: SearchMode,
) -> Result<T, EmbeddingScorerDiagnostic>
where
    F: std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
    T: Send + 'static,
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
    /// Per-channel candidate order after supplements and optional reranking.
    order_evidence: BTreeMap<EvidenceChannel, Vec<String>>,
    /// Native scorer values that remain available at this seam. Channels not
    /// present here truthfully fall back to within-channel order evidence.
    score_evidence: BTreeMap<EvidenceChannel, (ScoreKind, Vec<RawCandidateScore>)>,
    /// Backend-native score provenance is audit-only. Product fusion continues
    /// to consume the final adjusted result order, never these incomparable
    /// native values.
    product_score_audit: BTreeMap<String, Vec<ProductScoreAudit>>,
    scorer_diagnostic: Option<EmbeddingScorerDiagnostic>,
    strict_failure: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq)]
struct ProductScoreAudit {
    channel: EvidenceChannel,
    provenance: SearchScoreProvenance,
    adjusted_product_score: f32,
}

fn aligned_product_score_audit(
    results: &[SearchResult],
    provenance: Vec<SearchScoreProvenance>,
    channel: EvidenceChannel,
) -> BTreeMap<String, Vec<ProductScoreAudit>> {
    if results.len() != provenance.len()
        || results
            .iter()
            .zip(&provenance)
            .any(|(result, score)| result.id != score.result_id)
    {
        tracing::warn!(
            result_count = results.len(),
            provenance_count = provenance.len(),
            "Embedding score audit was not aligned with returned results; native audit omitted"
        );
        return BTreeMap::new();
    }
    results
        .iter()
        .zip(provenance)
        .map(|(result, provenance)| {
            (
                result.id.clone(),
                vec![ProductScoreAudit {
                    channel,
                    provenance,
                    adjusted_product_score: result.score,
                }],
            )
        })
        .collect()
}

fn merge_product_score_audit(
    target: &mut BTreeMap<String, Vec<ProductScoreAudit>>,
    source: BTreeMap<String, Vec<ProductScoreAudit>>,
) {
    for (id, audits) in source {
        let entry = target.entry(id).or_default();
        for audit in audits {
            if !entry.contains(&audit) {
                entry.push(audit);
            }
        }
    }
}

fn append_product_score_audit(
    evidence: &mut SelectionEvidence,
    audits: Option<&[ProductScoreAudit]>,
) {
    let Some(audits) = audits else {
        return;
    };
    for (index, audit) in audits.iter().enumerate() {
        let prefix = format!("score_audit.{}.{}", audit.channel.label(), index + 1);
        evidence.raw_scores.insert(
            format!("{prefix}.native_value"),
            audit.provenance.native_value.to_string(),
        );
        evidence.diagnostics.extend([
            (
                format!("{prefix}.native_kind"),
                native_score_kind_label(audit.provenance.native_kind).into(),
            ),
            (
                format!("{prefix}.native_source"),
                native_score_source_label(audit.provenance.native_source).into(),
            ),
            (
                format!("{prefix}.normalization"),
                score_normalization_label(audit.provenance.normalization).into(),
            ),
            (
                format!("{prefix}.normalized_score"),
                audit.provenance.normalized_score.to_string(),
            ),
            (
                format!("{prefix}.adjustment"),
                score_adjustment_label(audit.provenance.adjustment).into(),
            ),
            (
                format!("{prefix}.adjusted_product_score"),
                audit.adjusted_product_score.to_string(),
            ),
        ]);
        evidence.provenance.push(EvidenceProvenance {
            source: format!("{}_native_score", audit.channel.label()),
            detail: format!(
                "kind={} source={} normalization={} adjustment={}; native audit did not replace adjusted product order",
                native_score_kind_label(audit.provenance.native_kind),
                native_score_source_label(audit.provenance.native_source),
                score_normalization_label(audit.provenance.normalization),
                score_adjustment_label(audit.provenance.adjustment)
            ),
        });
    }
}

fn append_selected_product_score_audit(
    selected: &mut SelectedRecord,
    audits: Option<&[ProductScoreAudit]>,
    query: &str,
    repo_root: &Path,
) {
    if audits.is_none_or(<[ProductScoreAudit]>::is_empty) {
        return;
    }
    append_product_score_audit(&mut selected.evidence, audits);
    match persist_evidence_capsule(repo_root, query, selected) {
        Ok(handle) => selected.evidence_hydration = Some(handle),
        Err(error) => {
            selected
                .evidence
                .diagnostics
                .insert("evidence_hydration".into(), format!("unavailable: {error}"));
            selected.evidence_hydration = None;
        }
    }
}

const fn native_score_kind_label(kind: NativeScoreKind) -> &'static str {
    match kind {
        NativeScoreKind::Bm25 => "bm25",
        NativeScoreKind::CosineDistance => "cosine_distance",
        NativeScoreKind::HybridRrfRelevance => "hybrid_rrf_relevance",
    }
}

const fn native_score_source_label(source: NativeScoreSource) -> &'static str {
    match source {
        NativeScoreSource::Backend => "backend",
        NativeScoreSource::DeterministicFallback => "deterministic_fallback",
    }
}

const fn score_normalization_label(normalization: ScoreNormalization) -> &'static str {
    match normalization {
        ScoreNormalization::NonNegativeSaturation => "non_negative_saturation",
        ScoreNormalization::OneMinusDistanceFloorZero => "one_minus_distance_floor_zero",
    }
}

const fn score_adjustment_label(adjustment: ScoreAdjustment) -> &'static str {
    match adjustment {
        ScoreAdjustment::None => "none",
        ScoreAdjustment::TestPathDemotion70Percent => "test_path_demotion_70_percent",
    }
}

fn admitted_live_markdown_chunks(
    ctx: &SearchContext<'_>,
) -> Option<Vec<crate::types::MarkdownChunk>> {
    let paths = crate::walk::walk_repo_files(ctx.repo_root, &["md", "rst"]).ok()?;
    let chunks: Vec<_> = paths
        .into_iter()
        .flat_map(|path| match crate::markdown::parse_markdown_file(&path) {
            Ok(chunks) => chunks,
            Err(error) => {
                tracing::warn!("Failed to parse {}: {}", path.display(), error);
                Vec::new()
            }
        })
        .filter(|chunk| {
            let repository_path = chunk
                .file_path
                .strip_prefix(ctx.repo_root)
                .unwrap_or(&chunk.file_path);
            ctx.business_context.admit_repository_file(repository_path)
        })
        .collect();
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
            .find(|root| root.slug.eq_ignore_ascii_case(slug))
            .map(|root| root.path);
        if let Some(root_path) = root_path {
            chunks
                .into_iter()
                .filter(|chunk| chunk.file_path.starts_with(&root_path))
                .collect()
        } else {
            Vec::new()
        }
    } else {
        chunks
    };
    Some(filtered_chunks)
}

fn markdown_search_section(
    params: &SearchParams,
    query_str: &str,
    ctx: &SearchContext<'_>,
    limit: usize,
) -> Option<String> {
    if !params.include_markdown || query_str.is_empty() {
        return None;
    }

    let filtered_chunks = admitted_live_markdown_chunks(ctx)?;
    let scored = crate::markdown::search_chunks_ranked(&filtered_chunks, query_str);
    if scored.is_empty() {
        return None;
    }

    let markdown = scored
        .iter()
        .take(limit)
        .map(|scored| {
            format!(
                "- (score: {:.2}) {}",
                scored.score,
                scored.chunk.to_markdown()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    Some(format!(
        "### Markdown ({} result(s))\n\n{}",
        scored.len().min(limit),
        markdown
    ))
}

fn append_flat_search_tail_sections(
    sections: &mut Vec<String>,
    strict_semantic: bool,
    params: &SearchParams,
    query_str: &str,
    ctx: &SearchContext<'_>,
    limit: usize,
) {
    if strict_semantic {
        sections.push(
            "### Strict semantic qualification\n\n\
             `status=READY embeddings=true retrieval=hybrid rerank=true metal=true fallback=false`"
                .to_string(),
        );
    }

    if let Some(markdown) = markdown_search_section(params, query_str, ctx, limit) {
        sections.push(markdown);
    }
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

    let strict_semantic = strict_semantic_requested(params);
    let search_mode = if strict_semantic {
        SearchMode::Hybrid
    } else {
        parse_search_mode(params.search_mode.as_deref())
    };
    let limit = params.limit.unwrap_or(10);
    let mut sections: Vec<String> = Vec::new();
    let graph_state = ctx.graph_state;

    // Try embedding-ranked code symbol search first; fall back to name/signature matching.
    let FlatCodeSymbolSearch {
        matches,
        mut scorer_diagnostic,
        strict_failure,
        ..
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
    if let Some(reason) = strict_failure {
        return strict_semantic_failure(reason);
    }

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

    // Artifacts remain ordinary-search-only because their embedding scorer does
    // not participate in strict qualification's one-to-one code rerank contract.
    // Repository Markdown/RST is source-ranked independently and appended later
    // without changing the qualified code candidate set or its order.
    if !strict_semantic
        && params.include_artifacts
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

    append_flat_search_tail_sections(
        &mut sections,
        strict_semantic,
        params,
        query_str,
        ctx,
        limit,
    );

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
    let strict_semantic = strict_semantic_requested(params);
    let query_lower = query_str.to_lowercase();
    let complexity_search = params.min_complexity.is_some() || sort_by_complexity;

    // Detect path/name split query (e.g. "auth/handlers/validate" → path="auth/handlers", name="validate").
    // When present, embed search uses only the name part; name-matching filters by both.
    // Strict semantic qualification binds the model input to the caller's exact
    // query bytes. The interactive path/name shorthand is useful for ordinary
    // search, but must never rewrite a sealed benchmark query.
    let path_name = parse_path_name_query_for_search(query_str, strict_semantic);
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
        if node_delivery_class(n) != NodeDeliveryClass::Code {
            return false;
        }
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
    let rerank_over_fetch = if strict_semantic || params.rerank {
        limit.max(20)
    } else {
        limit
    };

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
    // Preserve every independently observed channel order. A candidate may be
    // present in lexical, semantic, graph, and rerank lanes simultaneously;
    // provenance must be a union, never a single mutable "origin" label.
    let mut order_evidence = BTreeMap::<EvidenceChannel, Vec<String>>::new();
    let mut score_evidence =
        BTreeMap::<EvidenceChannel, (ScoreKind, Vec<RawCandidateScore>)>::new();
    let mut product_score_audit = BTreeMap::<String, Vec<ProductScoreAudit>>::new();

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
                Ok(ObservedSearchOutcome {
                    outcome: SearchOutcome::NotReady,
                    executed_mode: None,
                    score_provenance: Vec::new(),
                })
            };
            match isolate_embedding_scorer(scorer, search_mode).await {
                Err(diagnostic) => {
                    if strict_semantic {
                        return FlatCodeSymbolSearch {
                            matches: Vec::new(),
                            order_evidence: BTreeMap::new(),
                            score_evidence: BTreeMap::new(),
                            product_score_audit: BTreeMap::new(),
                            scorer_diagnostic: None,
                            strict_failure: Some("embedding scorer panicked"),
                        };
                    }
                    scorer_diagnostic = Some(diagnostic);
                }
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
                let test_policy = product_test_policy(params, query_str);
                async move {
                    if strict_semantic {
                        let outcome = embed_idx
                            .search_with_filters_strict(
                                &embed_query,
                                None,
                                over_fetch,
                                SearchMode::Hybrid,
                                &embed_filters,
                            )
                            .await?;
                        Ok(ObservedSearchOutcome {
                            outcome,
                            executed_mode: Some(ExecutedSearchMode::HybridRrf),
                            score_provenance: Vec::new(),
                        })
                    } else {
                        embed_idx
                            .search_with_filters_observed(
                                &embed_query,
                                None,
                                over_fetch,
                                search_mode,
                                &embed_filters,
                                test_policy,
                            )
                            .await
                    }
                }
            };
            match isolate_embedding_scorer(scorer, search_mode).await {
                Ok(ObservedSearchOutcome {
                    outcome: SearchOutcome::Results(results),
                    executed_mode,
                    score_provenance,
                }) => {
                    used_embed = true;
                    let channel = executed_mode
                        .map(evidence_channel_for_executed_mode)
                        .unwrap_or(EvidenceChannel::Vector);
                    if !strict_semantic {
                        merge_product_score_audit(
                            &mut product_score_audit,
                            aligned_product_score_audit(&results, score_provenance, channel),
                        );
                    }
                    // Keep only code results, resolve to graph nodes via HashMap (O(1)), apply filters.
                    // node_passes_filters already handles the path/name split check.
                    let found: Vec<_> = results
                        .iter()
                        .filter(|r| r.kind.starts_with("code:"))
                        .filter_map(|result| {
                            graph_state.node_by_stable_id(&result.id, node_index_map)
                        })
                        .filter(|node| node_passes_filters(node))
                        .take(rerank_over_fetch)
                        .collect();
                    order_evidence
                        .insert(channel, found.iter().map(|node| node.stable_id()).collect());
                    found
                }
                // Embedding index not ready -- ordinary search falls through to
                // name/signature matching; strict qualification stops here.
                Ok(ObservedSearchOutcome {
                    outcome: SearchOutcome::NotReady,
                    ..
                }) => {
                    if strict_semantic {
                        return FlatCodeSymbolSearch {
                            matches: Vec::new(),
                            order_evidence: BTreeMap::new(),
                            score_evidence: BTreeMap::new(),
                            product_score_audit: BTreeMap::new(),
                            scorer_diagnostic: None,
                            strict_failure: Some("embedding index is not ready"),
                        };
                    }
                    Vec::new()
                }
                Err(diagnostic) => {
                    if strict_semantic {
                        return FlatCodeSymbolSearch {
                            matches: Vec::new(),
                            order_evidence: BTreeMap::new(),
                            score_evidence: BTreeMap::new(),
                            product_score_audit: BTreeMap::new(),
                            scorer_diagnostic: None,
                            strict_failure: Some("strict hybrid embedding search failed"),
                        };
                    }
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
        if strict_semantic {
            return FlatCodeSymbolSearch {
                matches: Vec::new(),
                order_evidence: BTreeMap::new(),
                score_evidence: BTreeMap::new(),
                product_score_audit: BTreeMap::new(),
                scorer_diagnostic: None,
                strict_failure: Some("embedding search produced no admissible code results"),
            };
        }
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
    } else if !strict_semantic && !query_lower.is_empty() {
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
        order_evidence.insert(
            EvidenceChannel::Structural,
            matches.iter().map(|node| node.stable_id()).collect(),
        );
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
        order_evidence.insert(
            EvidenceChannel::Structural,
            matches.iter().map(|node| node.stable_id()).collect(),
        );
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
    if strict_semantic && matches.is_empty() {
        return FlatCodeSymbolSearch {
            matches: Vec::new(),
            order_evidence: BTreeMap::new(),
            score_evidence: BTreeMap::new(),
            product_score_audit: BTreeMap::new(),
            scorer_diagnostic: None,
            strict_failure: Some("strict embedding search produced no admissible code results"),
        };
    }
    let should_rerank = if strict_semantic {
        !matches.is_empty()
    } else {
        params.rerank && matches.len() > 1
    };
    if should_rerank && use_relevance_sort && !query_str.is_empty() {
        use crate::rerank::{RerankCandidate, rerank_results, rerank_results_strict};

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
        let verified_reranker = use_verified_reranker_loader(
            strict_semantic,
            sealed_semantic_bundle(),
            semantic_asset_seeding(),
        );
        let rerank_result = tokio::task::spawn_blocking(move || {
            if verified_reranker {
                rerank_results_strict(&query_owned, &candidates)
            } else {
                rerank_results(&query_owned, &candidates)
            }
        })
        .await;

        match rerank_result {
            Ok(Ok(reranked)) => {
                let original_matches = matches.clone();
                score_evidence.insert(
                    EvidenceChannel::Rerank,
                    (
                        ScoreKind::CrossEncoderScore,
                        reranked
                            .iter()
                            .filter_map(|result| {
                                original_matches.get(result.original_index).map(|node| {
                                    RawCandidateScore::new(
                                        node.stable_id(),
                                        f64::from(result.score),
                                    )
                                })
                            })
                            .collect(),
                    ),
                );
                matches = reranked
                    .iter()
                    .filter_map(|r| original_matches.get(r.original_index).copied())
                    .collect();
                order_evidence.insert(
                    EvidenceChannel::Rerank,
                    matches.iter().map(|node| node.stable_id()).collect(),
                );
                tracing::debug!(
                    "Reranked {} candidates for query \"{}\"",
                    reranked.len(),
                    query_str
                );
            }
            Ok(Err(e)) => {
                if strict_semantic {
                    return FlatCodeSymbolSearch {
                        matches: Vec::new(),
                        order_evidence: BTreeMap::new(),
                        score_evidence: BTreeMap::new(),
                        product_score_audit: BTreeMap::new(),
                        scorer_diagnostic: None,
                        strict_failure: Some("strict cross-encoder reranking failed"),
                    };
                }
                tracing::warn!(
                    "Cross-encoder reranking failed, using original order: {}",
                    e
                );
                // Fall through with original ordering -- reranking is best-effort.
            }
            Err(e) => {
                if strict_semantic {
                    return FlatCodeSymbolSearch {
                        matches: Vec::new(),
                        order_evidence: BTreeMap::new(),
                        score_evidence: BTreeMap::new(),
                        product_score_audit: BTreeMap::new(),
                        scorer_diagnostic: None,
                        strict_failure: Some("strict reranking task panicked"),
                    };
                }
                tracing::warn!("Reranking task panicked, using original order: {}", e);
            }
        }
    }

    // Independently observe bounded lexical order even when semantic search
    // already found the same candidates. This records contributors without
    // evicting a semantic tail merely to make a lexical supplement visible.
    if !query_lower.is_empty() && !sort_by_complexity && !sort_by_importance {
        let mut lexical: Vec<&Node> = graph_state
            .nodes
            .iter()
            .filter(|node| {
                (path_name.is_some() || node_matches_text_query(node, &query_lower, &text_terms))
                    && node_passes_filters(node)
            })
            .collect();
        let sort_key = name_filter_lower.as_deref().unwrap_or(&query_lower);
        sort_symbol_text_matches(&mut lexical, sort_key, &text_terms, &graph_state.index);
        lexical.truncate(rerank_over_fetch.min(100));
        if !lexical.is_empty() {
            order_evidence.insert(
                EvidenceChannel::ExactLexical,
                lexical.into_iter().map(|node| node.stable_id()).collect(),
            );
        }
    }
    matches.truncate(limit);
    FlatCodeSymbolSearch {
        matches,
        order_evidence,
        score_evidence,
        product_score_audit,
        scorer_diagnostic,
        strict_failure: None,
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

            let mut md = format_neighbors_grouped_with_root(
                &gs.nodes,
                &merged_groups,
                &gs.index,
                params.compact,
                strip,
                params.include_body,
                params.minify_body,
            );
            let evidence_index = EdgeEvidenceIndex::new(&gs.edges);
            for origin in &valid_entry_ids {
                let evidence_direction = match mode {
                    "impact" | "tests_for" => "incoming",
                    "reachable" => "outgoing",
                    _ => params.direction.as_deref().unwrap_or("outgoing"),
                };
                md.push_str(&format_indexed_edge_evidence_for_groups(
                    &evidence_index,
                    origin,
                    &merged_groups,
                    evidence_direction,
                ));
            }

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
        let evidence_index = EdgeEvidenceIndex::new(&gs.edges);
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
                        let mut md = format_neighbors_grouped_with_root(
                            &gs.nodes,
                            &groups,
                            &gs.index,
                            params.compact,
                            strip,
                            params.include_body,
                            params.minify_body,
                        );
                        md.push_str(&format_indexed_edge_evidence_for_groups(
                            &evidence_index,
                            &resolved_nid,
                            &groups,
                            match mode {
                                "impact" | "tests_for" => "incoming",
                                "reachable" => "outgoing",
                                _ => params.direction.as_deref().unwrap_or("outgoing"),
                            },
                        ));
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

fn parse_path_name_query_for_search(query: &str, strict_semantic: bool) -> Option<(&str, &str)> {
    if strict_semantic {
        None
    } else {
        parse_path_name_query(query)
    }
}

fn nonempty_query_preserving_bytes(query: Option<&str>) -> Option<&str> {
    query.filter(|value| !value.trim().is_empty())
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
        if std::ptr::eq(*a, *b) {
            return std::cmp::Ordering::Equal;
        }
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

        // `sort_symbol_matches` intentionally returns Equal for several legacy
        // ties. Probe both input orders to distinguish a real preference from
        // stable-sort retention, then close every true tie by stable ID. This
        // keeps the comparator antisymmetric and byte-stable across reopen.
        let mut forward = [*a, *b];
        ranking::sort_symbol_matches(&mut forward, query_lower, index);
        let mut reverse = [*b, *a];
        ranking::sort_symbol_matches(&mut reverse, query_lower, index);
        match (std::ptr::eq(forward[0], *a), std::ptr::eq(reverse[0], *a)) {
            (true, true) => std::cmp::Ordering::Less,
            (false, false) => std::cmp::Ordering::Greater,
            _ => a.stable_id().as_bytes().cmp(b.stable_id().as_bytes()),
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
    use tempfile::TempDir;

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
            evidence: Vec::new(),
        }
    }

    fn make_search_context<'a>(
        graph_state: &'a GraphState,
        repo_root: &'a Path,
    ) -> SearchContext<'a> {
        static BUSINESS_CONTEXT: std::sync::LazyLock<
            crate::business_context::BusinessContextAdmission,
        > = std::sync::LazyLock::new(crate::business_context::BusinessContextAdmission::default);
        SearchContext {
            graph_state,
            embed_index: None,
            repo_root,
            lsp_status: None,
            embed_status: None,
            root_filter: None,
            non_code_slugs: HashSet::new(),
            enrichment_jobs: Vec::new(),
            business_context: &BUSINESS_CONTEXT,
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

        assert!(result.contains("# RNA search context"));
        assert!(result.contains("projection: agent"));
        assert!(result.contains("auth_handler"));
        assert!(!result.contains("Unknown mode"));
    }

    #[tokio::test]
    async fn default_compact_evidence_projection_matrix_is_byte_stable() {
        let repository = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repository.path().join("src")).unwrap();
        std::fs::write(
            repository.path().join("src/matrix.rs"),
            "fn matrix_target() {}\n",
        )
        .unwrap();
        let mut node = make_node("matrix_target", NodeKind::Function, "src/matrix.rs");
        node.line_start = 1;
        node.line_end = 1;
        let graph = make_graph_state(vec![node]);
        let ctx = make_search_context(&graph, repository.path());
        let base = SearchParams {
            query: Some("matrix_target".into()),
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };
        let action = search(&base, &ctx).await;
        let compact = search(
            &SearchParams {
                compact: true,
                ..base.clone()
            },
            &ctx,
        )
        .await;
        let evidence = search(
            &SearchParams {
                projection: Some("evidence".into()),
                ..base.clone()
            },
            &ctx,
        )
        .await;

        assert_eq!(action, compact, "compact is the concise agent projection");
        assert_eq!(action, search(&base, &ctx).await);
        assert!(action.contains("- projection: agent"));
        assert!(!action.contains("## Candidate audit"));
        assert!(!action.contains("evidence.content_hash"));
        assert!(evidence.contains("- projection: evidence"));
        assert!(evidence.contains("## Candidate audit"));
        assert!(evidence.contains("evidence.content_hash"));
        for rendered in [&action, &compact, &evidence] {
            assert!(rendered.contains("## Render accounting"));
            assert!(rendered.contains("deterministic estimate; not provider usage"));
        }
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
            projection: Some("evidence".into()),
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
            result.contains("global dead-code prerequisites**: partial/degraded"),
            "got: {}",
            result
        );
        assert!(result.contains("default query profile"), "got: {}", result);
    }

    #[tokio::test]
    async fn test_verbose_readiness_reports_default_profile_as_partial_not_full() {
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
                crate::server::EnrichmentTrigger::ForegroundScan,
                None,
            )
            .expect("begin default-profile job")
        {
            crate::server::JobStart::Started(job) => job.job_id,
            crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
        };
        ledger.mark_completed(&repo_root, &job_id, 2, 1);
        ledger.record_lsp_evidence(
            &repo_root,
            &job_id,
            crate::server::LspEvidenceCoverage {
                readiness: crate::server::LspEvidenceReadiness::DefaultProfile,
                scope: "repo".to_string(),
                declared_node_count: 0,
                max_requests: None,
                max_duration_ms: None,
                scheduled_requests: 0,
                elapsed_ms: 12,
                circuit_open: false,
                detail: Some("broad references were omitted".to_string()),
                validations: vec![
                    crate::extract::scan_stats::LspValidationEvidence::processed(
                        "json",
                        "vscode-json-languageserver",
                        "textDocument/documentSymbol",
                        0,
                    ),
                ],
            },
        );
        let mut ctx = make_search_context(&gs, &repo_root);
        ctx.enrichment_jobs = ledger.recent_jobs(&repo_root, 5);
        let params = SearchParams {
            query: Some("caller".into()),
            verbose: true,
            projection: Some("evidence".into()),
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;

        assert!(result.contains("LSP call/reference coverage**: partial/degraded"));
        assert!(result.contains("default query profile"));
        assert!(result.contains("broad references were omitted"));
        assert!(result.contains("evidence=default_profile"));
        assert!(result.contains(
            "validation=[json/vscode-json-languageserver: processed via textDocument/documentSymbol (0 symbols)]"
        ));
        assert!(!result.contains("LSP call/reference coverage**: ready"));
    }

    #[tokio::test]
    async fn test_verbose_mcp_readiness_renders_all_persisted_evidence_classes() {
        let node = make_node("caller", NodeKind::Function, "src/caller.rs");
        let gs = make_graph_state(vec![node]);
        let tmp = tempfile::tempdir().expect("temp repo");
        let repo_root = tmp.path().to_path_buf();
        let ledger = crate::server::EnrichmentJobLedger::default();
        let cases = [
            (
                crate::server::EnrichmentScope::Repo,
                crate::server::LspEvidenceReadiness::Full,
                false,
            ),
            (
                crate::server::EnrichmentScope::ChangedFiles,
                crate::server::LspEvidenceReadiness::Scoped,
                false,
            ),
            (
                crate::server::EnrichmentScope::TargetSymbols(vec!["caller".to_string()]),
                crate::server::LspEvidenceReadiness::Partial,
                true,
            ),
            (
                crate::server::EnrichmentScope::TaskRelevant {
                    files: vec!["src/caller.rs".to_string()],
                    symbols: Vec::new(),
                },
                crate::server::LspEvidenceReadiness::Unavailable,
                false,
            ),
        ];
        for (scope, readiness, circuit_open) in cases {
            let job_id = match ledger
                .begin_job(
                    &repo_root,
                    crate::server::EnrichmentCapability::CallReferences,
                    scope.clone(),
                    crate::server::EnrichmentTrigger::Explicit,
                    None,
                )
                .expect("begin evidence job")
            {
                crate::server::JobStart::Started(job) => job.job_id,
                crate::server::JobStart::Joined { existing_job_id } => existing_job_id,
            };
            if readiness == crate::server::LspEvidenceReadiness::Unavailable {
                ledger.mark_failed(&repo_root, &job_id, "server unavailable");
            } else if readiness == crate::server::LspEvidenceReadiness::Partial {
                ledger.mark_degraded(&repo_root, &job_id, 1, 1, "budget exhausted");
            } else {
                ledger.mark_completed(&repo_root, &job_id, 1, 1);
            }
            ledger.record_lsp_evidence(
                &repo_root,
                &job_id,
                crate::server::LspEvidenceCoverage {
                    readiness,
                    scope: scope.stable_key(),
                    declared_node_count: 1,
                    max_requests: Some(1),
                    max_duration_ms: Some(100),
                    scheduled_requests: usize::from(
                        readiness != crate::server::LspEvidenceReadiness::Unavailable,
                    ),
                    elapsed_ms: 10,
                    circuit_open,
                    detail: None,
                    validations: Vec::new(),
                },
            );
        }
        let mut ctx = make_search_context(&gs, &repo_root);
        ctx.enrichment_jobs = ledger.recent_jobs(&repo_root, 10);
        let params = SearchParams {
            query: Some("caller".into()),
            verbose: true,
            projection: Some("evidence".into()),
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;
        for evidence in ["full", "scoped", "partial", "unavailable"] {
            assert!(
                result.contains(&format!("evidence={evidence}")),
                "missing {evidence}: {result}"
            );
        }
        assert!(result.contains("requests=1/1"));
        assert!(result.contains("circuit_open=true"));
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
            projection: Some("evidence".into()),
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
            projection: Some("evidence".into()),
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
            projection: Some("evidence".into()),
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
            projection: Some("evidence".into()),
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
            projection: Some("evidence".into()),
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
        assert!(result.contains("for repo-wide"), "got: {result}");
        assert!(
            !result.contains("explicit scoped/degraded context"),
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
                projection: Some("evidence".into()),
                include_artifacts: false,
                include_markdown: false,
                ..Default::default()
            },
            &ctx,
        )
        .await;

        assert!(
            result.contains("LSP call/reference coverage**: partial/degraded"),
            "got: {result}"
        );
        assert!(result.contains("default query profile"), "got: {result}");
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
                projection: Some("evidence".into()),
                include_artifacts: false,
                include_markdown: false,
                ..Default::default()
            },
            &ctx,
        )
        .await;

        assert!(
            result.contains(
                "0 persisted call/reference edges available for the default query profile"
            ),
            "got: {result}"
        );
        assert!(
            result.contains("broad references were omitted"),
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
            projection: Some("evidence".into()),
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
            result.contains("global dead-code prerequisites**: partial/degraded"),
            "got: {}",
            result
        );
        assert!(result.contains("default query profile"), "got: {}", result);
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
            projection: Some("evidence".into()),
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
            result.contains("global dead-code prerequisites**: partial/degraded"),
            "got: {}",
            result
        );
        assert!(result.contains("default query profile"), "got: {}", result);
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
            projection: Some("evidence".into()),
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
            projection: Some("evidence".into()),
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
                Err::<ObservedSearchOutcome, anyhow::Error>(anyhow::anyhow!(
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
        let strict = SearchParams {
            search_mode: Some("  STRICT  ".to_string()),
            ..Default::default()
        };
        assert!(strict_semantic_requested(&strict));
    }

    #[test]
    fn sealed_implicit_strict_preserves_qualification_and_excludes_product_controls() {
        let frozen = SearchParams {
            query: Some("registered frozen query".into()),
            search_mode: Some("hybrid".into()),
            rerank: true,
            limit: Some(20),
            include_artifacts: false,
            include_markdown: true,
            compact: true,
            ..Default::default()
        };
        assert!(implicit_strict_request(&frozen));

        let offline_probe = SearchParams {
            query: Some("function returns value".into()),
            limit: Some(10),
            compact: true,
            ..Default::default()
        };
        assert!(implicit_strict_request(&offline_probe));

        for product in [
            SearchParams {
                projection: Some("agent".into()),
                ..frozen.clone()
            },
            SearchParams {
                context_mode: Some("task".into()),
                ..frozen.clone()
            },
            SearchParams {
                body_policy: Some("signature_only".into()),
                ..frozen.clone()
            },
        ] {
            assert!(
                !implicit_strict_request(&product),
                "product controls must never enter the frozen implicit strict path"
            );
        }
    }

    #[tokio::test]
    async fn strict_semantic_search_rejects_an_unsealed_binary_without_fallback() {
        let gs = make_graph_state(vec![make_node(
            "auth_handler",
            NodeKind::Function,
            "src/auth.rs",
        )]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("auth".into()),
            search_mode: Some("strict".into()),
            rerank: false,
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;
        assert!(result.contains("Strict semantic qualification FAILED"));
        assert!(result.contains("binary is not the sealed CI SWE-bench semantic bundle"));
        assert!(!result.contains("auth_handler"));
        assert!(result.contains("No lexical, graph, vector-only, CPU"));
    }

    #[tokio::test]
    async fn strict_semantic_search_accepts_only_flat_code_queries() {
        let gs = make_graph_state(vec![make_node("target", NodeKind::Function, "src/lib.rs")]);
        let repo_root = PathBuf::from("/tmp/test");
        let ctx = make_search_context(&gs, &repo_root);
        let params = SearchParams {
            query: Some("target".into()),
            mode: Some("neighbors".into()),
            search_mode: Some("strict".into()),
            rerank: true,
            include_artifacts: false,
            include_markdown: false,
            ..Default::default()
        };

        let result = search(&params, &ctx).await;
        assert!(result.contains("accepts only a flat query"));
        assert!(!result.contains("target"));
    }

    #[test]
    fn strict_semantic_renderer_keeps_twenty_code_results_and_repository_docs() {
        let repo = TempDir::new().expect("create repository fixture");
        std::fs::write(
            repo.path().join("README.md"),
            "# Context packet evidence\n\nREADME context packet evidence for agent discovery.\n",
        )
        .expect("write README fixture");
        std::fs::create_dir_all(repo.path().join("docs")).expect("create nested docs fixture");
        std::fs::write(
            repo.path().join("docs/configuration.rst"),
            "Context packet evidence\n=======================\n\nNested RST context packet evidence for configuration.\n",
        )
        .expect("write RST fixture");
        for index in 0..20 {
            std::fs::write(
                repo.path().join(format!("docs/filler_{index:02}.md")),
                format!(
                    "# Supporting note {index:02}\n\ncontext packet evidence filler {index:02}.\n"
                ),
            )
            .expect("write Markdown limit fixture");
        }

        let nodes = (0..20)
            .map(|index| {
                make_node(
                    &format!("context_packet_{index:02}"),
                    NodeKind::Function,
                    &format!("src/context_{index:02}.rs"),
                )
            })
            .collect();
        let graph_state = make_graph_state(nodes);
        let ctx = make_search_context(&graph_state, repo.path());
        let params = SearchParams {
            query: Some("context packet evidence".into()),
            search_mode: Some("strict".into()),
            rerank: true,
            limit: Some(20),
            include_artifacts: false,
            include_markdown: true,
            ..Default::default()
        };
        assert!(strict_semantic_requested(&params));
        assert_eq!(
            parse_search_mode(params.search_mode.as_deref()),
            SearchMode::Hybrid
        );

        let strict_docs = markdown_search_section(&params, "context packet evidence", &ctx, 20)
            .expect("strict rendering must include repository documentation");
        let mut ordinary_params = params.clone();
        ordinary_params.search_mode = Some("hybrid".into());
        ordinary_params.rerank = false;
        let ordinary_docs =
            markdown_search_section(&ordinary_params, "context packet evidence", &ctx, 20)
                .expect("ordinary rendering must include repository documentation");
        assert_eq!(strict_docs, ordinary_docs);
        assert_eq!(
            strict_docs,
            markdown_search_section(&params, "context packet evidence", &ctx, 20)
                .expect("repeated rendering must be deterministic")
        );
        assert!(strict_docs.starts_with("### Markdown (20 result(s))"));
        assert_eq!(strict_docs.matches("- (score:").count(), 20);
        assert!(strict_docs.contains("README context packet evidence for agent discovery."));
        assert!(strict_docs.contains("Nested RST context packet evidence for configuration."));

        let mut markdown_disabled = params.clone();
        markdown_disabled.include_markdown = false;
        assert!(
            markdown_search_section(&markdown_disabled, "context packet evidence", &ctx, 20)
                .is_none()
        );
        assert!(markdown_search_section(&params, "", &ctx, 20).is_none());

        let code = graph_state
            .nodes
            .iter()
            .map(|node| {
                format_node_entry_with_root(
                    node,
                    &graph_state.index,
                    params.compact,
                    None,
                    false,
                    false,
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut sections = vec![format!("### Code symbols (20 result(s))\n\n{code}")];
        append_flat_search_tail_sections(
            &mut sections,
            true,
            &params,
            "context packet evidence",
            &ctx,
            20,
        );
        let output = format!(
            "## Search: \"context packet evidence\"\n\n{}",
            sections.join("\n\n")
        );

        assert!(output.contains("### Code symbols (20 result(s))"));
        assert_eq!(output.matches("**function**").count(), 20);
        assert!(output.contains(
            "status=READY embeddings=true retrieval=hybrid rerank=true metal=true fallback=false"
        ));
        assert!(output.contains("### Markdown (20 result(s))"));
        assert!(output.contains("README context packet evidence for agent discovery."));
        assert!(output.contains("Nested RST context packet evidence for configuration."));
        assert!(output.contains(&repo.path().join("README.md").display().to_string()));
        assert!(
            output.contains(
                &repo
                    .path()
                    .join("docs/configuration.rst")
                    .display()
                    .to_string()
            )
        );

        let code_position = output.find("### Code symbols").expect("code section");
        let ready_position = output
            .find("### Strict semantic qualification")
            .expect("strict READY section");
        let docs_position = output.find("### Markdown").expect("Markdown section");
        assert!(code_position < ready_position && ready_position < docs_position);
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
            result.contains("## Results"),
            "Should have projected results"
        );
        assert!(
            !result.contains("evidence.raw_scores"),
            "default agent projection must hide scorer internals"
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
        let business_context = crate::business_context::BusinessContextAdmission::default();
        let ctx = SearchContext {
            graph_state: &gs,
            embed_index: None,
            repo_root: &repo_root,
            lsp_status: None,
            embed_status: None,
            root_filter: Some("my-project".into()),
            non_code_slugs: HashSet::new(),
            enrichment_jobs: Vec::new(),
            business_context: &business_context,
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
    fn nonempty_query_preserves_exact_bytes() {
        let query = "\n  Diagnose Src/naïve_path.py EXACTLY.  \t";
        assert_eq!(nonempty_query_preserving_bytes(Some(query)), Some(query));
        assert_eq!(nonempty_query_preserving_bytes(Some(" \n\t ")), None);
    }

    #[test]
    fn strict_semantic_query_bypasses_path_name_rewrite() {
        let query = "\n  Diagnose Src/naïve_path.py WITHOUT rewriting.  \t";
        let path_name = parse_path_name_query_for_search(query, true);
        let embed_query = path_name.map(|(_, name)| name).unwrap_or(query);

        assert!(parse_path_name_query(query).is_some());
        assert_eq!(
            parse_path_name_query_for_search(query, false),
            parse_path_name_query(query)
        );
        assert_eq!(embed_query.as_bytes(), query.as_bytes());
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

    #[test]
    fn persisted_per_file_lsp_completeness_is_mcp_visible() {
        use crate::lsp_completeness::{
            AdvertisedCapability, FileCoverageRecord, FileRole, FileTerminalStatus,
            LspCompletenessReport, PersistedResults, RequestAttempt, RequestOutcome,
            ServerIdentity,
        };

        let repo = tempfile::tempdir().unwrap();
        let identity = crate::lsp_completeness::current_report_identity(
            repo.path(),
            crate::business_context::BusinessContextMode::Disabled,
        )
        .unwrap();
        let report = LspCompletenessReport::new(
            identity,
            vec![FileCoverageRecord {
                path: "src/app.py".to_string(),
                role: FileRole::Source,
                language: Some("python".to_string()),
                expected_server: Some(ServerIdentity {
                    name: "pyrefly".to_string(),
                    version: Some("1.1.0".to_string()),
                    executable_digest: Some("blake3:fixture".to_string()),
                }),
                advertised_capabilities: vec![AdvertisedCapability {
                    name: "textDocument/references".to_string(),
                    supported: true,
                }],
                requests_attempted: vec![RequestAttempt {
                    method: "textDocument/references".to_string(),
                    outcome: RequestOutcome::Completed,
                    result_count: Some(0),
                    duration_ms: Some(1),
                    detail: None,
                }],
                expected_results: Default::default(),
                expected_result_ids: Default::default(),
                persisted_results: PersistedResults::default(),
                terminal_status: FileTerminalStatus::Processed { result_count: 0 },
                exclusion: None,
            }],
        );
        crate::lsp_completeness::persist_report(repo.path(), &report).unwrap();
        let rendered = format_lsp_completeness(repo.path(), &[], &[]);
        assert!(rendered.contains("benchmark per-file LSP completeness**: partial/degraded"));
        assert!(rendered.contains(&report.digest));
        assert!(rendered.contains("0/1 included files covered"));
    }

    #[test]
    fn search_experience_validation_rejects_unknown_and_unbounded_controls() {
        let unknown_projection = SearchParams {
            projection: Some("raw".to_string()),
            ..Default::default()
        };
        assert_eq!(
            validate_search_experience(&unknown_projection).unwrap_err(),
            "unknown projection `raw`; allowed values: agent, evidence"
        );

        let unknown_role = SearchParams {
            context_mode: Some("task".to_string()),
            query: Some("change the renderer".to_string()),
            context_roles: Some(vec!["everything".to_string()]),
            ..Default::default()
        };
        assert!(
            validate_search_experience(&unknown_role)
                .unwrap_err()
                .contains("unknown context role `everything`")
        );

        let too_many_hops = SearchParams {
            context_mode: Some("task".to_string()),
            query: Some("change the renderer".to_string()),
            hops: Some(MAX_CONTEXT_HOPS + 1),
            ..Default::default()
        };
        assert_eq!(
            validate_search_experience(&too_many_hops).unwrap_err(),
            "task-context hops cannot exceed 4"
        );
    }

    #[tokio::test]
    async fn legacy_dispatch_rejects_product_controls_but_preserves_default_bytes() {
        let caller = make_node("legacy_caller", NodeKind::Function, "src/caller.rs");
        let callee = make_node("legacy_callee", NodeKind::Function, "src/callee.rs");
        let graph = make_graph_state_with_edges(
            vec![caller.clone(), callee.clone()],
            vec![make_edge(&caller, &callee, EdgeKind::Calls)],
        );
        let repository = PathBuf::from("/tmp/legacy-product-control-fixture");
        let ctx = make_search_context(&graph, &repository);
        let legacy = SearchParams {
            node: Some(caller.stable_id()),
            mode: Some("neighbors".into()),
            compact: true,
            ..Default::default()
        };
        assert_eq!(
            search(&legacy, &ctx).await,
            legacy_search_dispatch(&legacy, &ctx).await,
            "default legacy dispatch must remain byte/order compatible"
        );

        let controlled = [
            SearchParams {
                projection: Some("evidence".into()),
                ..legacy.clone()
            },
            SearchParams {
                body_policy: Some("focused_span".into()),
                ..legacy.clone()
            },
            SearchParams {
                max_output_bytes: Some(4096),
                ..legacy.clone()
            },
            SearchParams {
                max_output_tokens: Some(1024),
                ..legacy.clone()
            },
            SearchParams {
                max_body_bytes: Some(512),
                ..legacy.clone()
            },
            SearchParams {
                max_total_body_bytes: Some(2048),
                ..legacy.clone()
            },
        ];
        for params in controlled {
            let response = search(&params, &ctx).await;
            assert!(
                response.contains("product controls")
                    && response.contains("legacy node/nodes/traversal/target_subsystem dispatch"),
                "explicit product control must fail closed: {response}"
            );
        }

        for params in [
            SearchParams {
                nodes: Some(vec![caller.stable_id()]),
                projection: Some("agent".into()),
                ..Default::default()
            },
            SearchParams {
                query: Some("legacy".into()),
                target_subsystem: Some("service".into()),
                projection: Some("agent".into()),
                ..Default::default()
            },
        ] {
            assert!(search(&params, &ctx).await.contains("product controls"));
        }
    }

    #[test]
    fn task_admission_uses_token_only_and_tighter_combined_budget() {
        assert_eq!(
            task_admission_budget(
                &SearchParams {
                    max_output_tokens: Some(250),
                    ..Default::default()
                },
                9_999,
            ),
            1_000
        );
        assert_eq!(
            task_admission_budget(
                &SearchParams {
                    max_output_bytes: Some(700),
                    max_output_tokens: Some(250),
                    ..Default::default()
                },
                9_999,
            ),
            700
        );
        assert_eq!(
            task_admission_budget(
                &SearchParams {
                    max_output_bytes: Some(2_000),
                    max_output_tokens: Some(250),
                    ..Default::default()
                },
                9_999,
            ),
            1_000
        );
    }

    #[test]
    fn graph_delta_beta_requires_bounded_non_binary_proposal() {
        let missing = SearchParams {
            context_mode: Some("graph-delta-beta".to_string()),
            ..Default::default()
        };
        assert_eq!(
            validate_search_experience(&missing).unwrap_err(),
            "graph-delta-beta requires a proposal"
        );

        let binary = SearchParams {
            context_mode: Some("graph-delta-beta".to_string()),
            proposal: Some("diff\0payload".to_string()),
            ..Default::default()
        };
        assert_eq!(
            validate_search_experience(&binary).unwrap_err(),
            "proposal contains NUL bytes"
        );

        let valid = SearchParams {
            context_mode: Some("graph-delta-beta".to_string()),
            proposal: Some("--- a/src/lib.rs\n+++ b/src/lib.rs\n".to_string()),
            ..Default::default()
        };
        assert!(validate_search_experience(&valid).is_ok());
    }

    #[test]
    fn focused_span_scores_all_lines_and_prefers_behavioral_terms() {
        let mut node = make_node(
            "projected_search",
            NodeKind::Function,
            "src/service/search.rs",
        );
        node.line_start = 1;
        node.line_end = 80;
        node.body = (1..=80)
            .map(|line| match line {
                2 => "projected_search signature mention".to_string(),
                67 => "regression behavior preserves discarded evidence".to_string(),
                _ => format!("ordinary line {line}"),
            })
            .collect::<Vec<_>>()
            .join("\n");
        let source = node_source_span(&node).unwrap();
        let (focused, grounding) = query_focused_span(
            &node,
            &source,
            Some("projected_search regression behavior discarded evidence"),
        );
        let focused = focused.expect("long source should receive a focused window");
        assert!(
            focused.start_line > 40,
            "behavioral sentinel must beat the name-only line"
        );
        assert!(focused.start_line <= 67 && focused.end_line >= 67);
        assert!(grounding.unwrap().contains("regression"));
    }

    #[test]
    fn product_context_normalization_is_shared_and_preserves_query_and_proposal_bytes() {
        let params = SearchParams {
            query: Some("  exact query bytes  ".into()),
            proposal: Some("  exact proposal bytes  ".into()),
            projection: Some(" evidence ".into()),
            body_policy: Some(" focused_span ".into()),
            context_mode: Some(" task ".into()),
            context_roles: Some(vec![" test ".into(), "".into()]),
            context_facets: Some(vec![" behavior ".into(), "   ".into()]),
            edge_types: Some(vec![" calls ".into()]),
            ..Default::default()
        };
        let normalized = normalize_product_context_controls(&params);
        assert_eq!(normalized.query, params.query);
        assert_eq!(normalized.proposal, params.proposal);
        assert_eq!(normalized.projection.as_deref(), Some("evidence"));
        assert_eq!(normalized.body_policy.as_deref(), Some("focused_span"));
        assert_eq!(normalized.context_mode.as_deref(), Some("task"));
        assert_eq!(normalized.context_roles, Some(vec!["test".into()]));
        assert_eq!(normalized.context_facets, Some(vec!["behavior".into()]));
        assert_eq!(normalized.edge_types, Some(vec!["calls".into()]));
    }

    #[tokio::test]
    async fn semantic_only_artifact_capsule_hydrates_full_body_without_graph_identity() {
        let repository = tempfile::tempdir().unwrap();
        let body = "full semantic row body with a terminal sentinel";
        let selected = SelectedRecord {
            selection_rank: 0,
            identity: RecordIdentity {
                node_id: "artifact:metis:fixture".into(),
                source: None,
            },
            symbol: SymbolSummary {
                name: "fixture".into(),
                kind: "metis".into(),
                language: "text".into(),
                signature: "truncated".into(),
                extraction_source: None,
                declared_metadata: BTreeMap::new(),
            },
            selection: SelectionSummary {
                channel: SelectionChannel::Artifact,
                reason: "semantic fixture".into(),
                role: Some(ProjectionRole::DefinitionOrApiState),
                lane: Some(ProjectionLane::DefinitionOrState),
            },
            evidence: SelectionEvidence {
                content_hash: Some(blake3::hash(body.as_bytes()).to_hex().to_string()),
                ..Default::default()
            },
            evidence_hydration: None,
            focused_span: None,
        };
        let handle = persist_semantic_evidence_capsule(repository.path(), &selected, body).unwrap();
        let graph = make_graph_state(Vec::new());
        let ctx = make_search_context(&graph, repository.path());
        let response = search(
            &SearchParams {
                node: Some(handle.encode()),
                projection: Some("evidence".into()),
                ..Default::default()
            },
            &ctx,
        )
        .await;
        assert!(response.contains("terminal sentinel"));
        assert!(response.contains("content-addressed semantic row body"));
    }

    #[tokio::test]
    async fn task_exact_reference_is_resolved_before_ranked_lane_truncation() {
        let repository = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repository.path().join("src")).unwrap();
        std::fs::write(
            repository.path().join("src/lib.rs"),
            "fn target_symbol() {}\n",
        )
        .unwrap();
        let mut nodes = (0..30)
            .map(|index| {
                let mut node = make_node(
                    &format!("unrelated_{index}"),
                    NodeKind::Function,
                    "src/lib.rs",
                );
                node.line_start = 1;
                node.line_end = 1;
                node
            })
            .collect::<Vec<_>>();
        let mut target = make_node("target_symbol", NodeKind::Function, "src/lib.rs");
        target.line_start = 1;
        target.line_end = 1;
        let target_id = target.stable_id();
        nodes.push(target);
        let graph = make_graph_state(nodes);
        let ctx = make_search_context(&graph, repository.path());
        let edge_index = ProjectedEdgeIndex::new(&graph);
        let output = task_records(
            &SearchParams {
                query: Some("Fix `target_symbol` behavior and add a regression test".into()),
                context_mode: Some("task".into()),
                include_artifacts: false,
                include_markdown: false,
                ..Default::default()
            },
            &ctx,
            &edge_index,
        )
        .await
        .unwrap();
        assert!(
            output
                .records
                .iter()
                .any(|record| record.identity.node_id == target_id)
        );
        assert!(output.capabilities.iter().any(|capability| {
            capability.capability == "task_exact_reference_resolution"
                && capability.state == CapabilityState::Ready
        }));
    }

    #[tokio::test]
    async fn task_evidence_omits_source_less_graph_candidate_without_failing_valid_records() {
        let repository = tempfile::tempdir().unwrap();
        std::fs::write(
            repository.path().join("lib.rs"),
            "fn hello() {}\nfn test_hello() {}\n",
        )
        .unwrap();
        let mut hello = make_node("hello", NodeKind::Function, "lib.rs");
        hello.line_start = 1;
        hello.line_end = 1;
        let mut test = make_node("test_hello", NodeKind::Function, "lib.rs");
        test.line_start = 2;
        test.line_end = 2;
        test.metadata.insert("is_test".into(), "true".into());
        let source_less_module = make_node("lib", NodeKind::Module, "lib.rs");
        let source_less_id = source_less_module.stable_id();
        let graph = make_graph_state_with_edges(
            vec![hello.clone(), test.clone(), source_less_module],
            vec![make_edge(
                &hello,
                &make_node("lib", NodeKind::Module, "lib.rs"),
                EdgeKind::DependsOn,
            )],
        );
        let ctx = make_search_context(&graph, repository.path());
        let edge_index = ProjectedEdgeIndex::new(&graph);
        let output = task_records(
            &SearchParams {
                query: Some("Fix `hello` and update `test_hello`".into()),
                context_mode: Some("task".into()),
                context_roles: Some(vec!["editable_source".into()]),
                context_facets: Some(Vec::new()),
                hops: Some(1),
                include_artifacts: false,
                include_markdown: false,
                ..Default::default()
            },
            &ctx,
            &edge_index,
        )
        .await
        .expect("a source-less graph neighbor must not fail the valid task response");

        assert!(
            output
                .records
                .iter()
                .any(|record| record.identity.node_id == hello.stable_id())
        );
        assert!(
            output
                .records
                .iter()
                .any(|record| record.identity.node_id == test.stable_id())
        );
        assert!(output.omissions.iter().any(|omission| {
            omission.record_id.as_deref() == Some(source_less_id.as_str())
                && omission.detail.contains("no valid current source anchor")
        }));
    }

    #[test]
    fn task_test_lane_rejects_production_code_without_test_evidence() {
        let mut production = make_node("production_handler", NodeKind::Function, "src/lib.rs");
        production.line_start = 1;
        production.line_end = 1;
        let mut regression = make_node(
            "production_handler_regression",
            NodeKind::Function,
            "tests/regression.rs",
        );
        regression.line_start = 1;
        regression.line_end = 1;
        let graph = make_graph_state(vec![production.clone(), regression.clone()]);
        let edge_index = ProjectedEdgeIndex::new(&graph);
        let assemblies = BTreeMap::new();

        let rejection = task_lane_candidate_evidence(
            &production,
            TaskRole::Test,
            &assemblies,
            &graph,
            &edge_index,
        )
        .unwrap_err();
        assert!(rejection.contains("production source cannot satisfy the test lane"));
        assert!(
            task_lane_candidate_evidence(
                &regression,
                TaskRole::Test,
                &assemblies,
                &graph,
                &edge_index,
            )
            .unwrap()
            .contains("test path/metadata")
        );
    }

    #[test]
    fn native_score_audit_is_separate_from_adjusted_product_order() {
        let mut evidence = SelectionEvidence::default();
        append_product_score_audit(
            &mut evidence,
            Some(&[ProductScoreAudit {
                channel: EvidenceChannel::FullText,
                provenance: SearchScoreProvenance {
                    result_id: "candidate".into(),
                    native_kind: NativeScoreKind::Bm25,
                    native_value: 12.0,
                    native_source: NativeScoreSource::Backend,
                    normalization: ScoreNormalization::NonNegativeSaturation,
                    normalized_score: 12.0 / 13.0,
                    adjustment: ScoreAdjustment::TestPathDemotion70Percent,
                },
                adjusted_product_score: (12.0 / 13.0) * 0.7,
            }]),
        );
        assert_eq!(
            evidence
                .raw_scores
                .get("score_audit.fts.1.native_value")
                .map(String::as_str),
            Some("12")
        );
        assert_eq!(
            evidence
                .diagnostics
                .get("score_audit.fts.1.adjustment")
                .map(String::as_str),
            Some("test_path_demotion_70_percent")
        );
        assert!(
            evidence
                .diagnostics
                .contains_key("score_audit.fts.1.adjusted_product_score")
        );
        assert!(!evidence.raw_scores.contains_key("bm25_score"));
    }

    #[test]
    fn task_analogue_lane_rejects_unrelated_same_kind_code() {
        let mut anchor = make_node("changed_behavior", NodeKind::Function, "src/changed.rs");
        anchor.line_start = 1;
        anchor.line_end = 1;
        let mut candidate = make_node("unrelated_behavior", NodeKind::Function, "src/other.rs");
        candidate.line_start = 1;
        candidate.line_end = 1;
        let mut helper = make_node("shared_helper", NodeKind::Function, "src/helper.rs");
        helper.line_start = 1;
        helper.line_end = 1;
        let mut assemblies = BTreeMap::new();
        merge_task_assembly(
            &mut assemblies,
            single_channel_fused(
                &anchor.stable_id(),
                EvidenceChannel::ExactLexical,
                ScoreKind::ExactMatchTier,
            ),
            TaskRole::EditableSource,
            TaskLane::ExactReference,
            TaskFacet::Behavior,
            Some("changed_behavior".into()),
            0,
            "exact anchor".into(),
        );
        let unrelated_graph =
            make_graph_state(vec![anchor.clone(), candidate.clone(), helper.clone()]);
        let unrelated_edge_index = ProjectedEdgeIndex::new(&unrelated_graph);
        let rejection = task_lane_candidate_evidence(
            &candidate,
            TaskRole::BehavioralAnalogue,
            &assemblies,
            &unrelated_graph,
            &unrelated_edge_index,
        )
        .unwrap_err();
        assert!(rejection.contains("lacks a shared typed graph target"));

        let corroborated_graph = make_graph_state_with_edges(
            vec![anchor.clone(), candidate.clone(), helper.clone()],
            vec![
                make_edge(&anchor, &helper, EdgeKind::Calls),
                make_edge(&candidate, &helper, EdgeKind::Calls),
            ],
        );
        let corroborated_edge_index = ProjectedEdgeIndex::new(&corroborated_graph);
        assert!(
            task_lane_candidate_evidence(
                &candidate,
                TaskRole::BehavioralAnalogue,
                &assemblies,
                &corroborated_graph,
                &corroborated_edge_index,
            )
            .unwrap()
            .contains("shared_typed_targets=calls:")
        );
    }

    #[tokio::test]
    async fn two_rna_self_tasks_deliver_exact_source_and_test_roles() {
        let repository = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repository.path().join("src/service")).unwrap();
        let source = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/service/search.rs"),
        )
        .unwrap();
        std::fs::write(repository.path().join("src/service/search.rs"), &source).unwrap();
        let line_of = |needle: &str| {
            u32::try_from(
                source
                    .lines()
                    .position(|line| line.contains(needle))
                    .expect("RNA self fixture symbol exists")
                    + 1,
            )
            .unwrap()
        };
        let node_at = |name: &str, needle: &str| {
            let mut node = make_node(name, NodeKind::Function, "src/service/search.rs");
            let line = line_of(needle);
            node.line_start = line as usize;
            node.line_end = line as usize;
            node.body = source.lines().nth(line as usize - 1).unwrap().to_string();
            node
        };
        let projected = node_at("projected_search", "async fn projected_search(");
        let delta = node_at("projected_graph_delta", "async fn projected_graph_delta(");
        let mut task_test = node_at(
            "task_exact_reference_is_resolved_before_ranked_lane_truncation",
            "async fn task_exact_reference_is_resolved_before_ranked_lane_truncation(",
        );
        task_test.metadata.insert("is_test".into(), "true".into());
        let mut delta_test = node_at(
            "live_graph_delta_infers_corroborated_edges_without_mutating_graph",
            "fn live_graph_delta_infers_corroborated_edges_without_mutating_graph(",
        );
        delta_test.metadata.insert("is_test".into(), "true".into());
        let mut nodes = vec![
            projected.clone(),
            delta.clone(),
            task_test.clone(),
            delta_test.clone(),
        ];
        for (name, needle) in [
            ("projection_request", "fn projection_request("),
            ("projection_source_reader", "fn projection_source_reader("),
            (
                "projected_non_code_records",
                "async fn projected_non_code_records(",
            ),
            (
                "projected_live_markdown_records",
                "fn projected_live_markdown_records(",
            ),
            (
                "evidence_capsule_capability",
                "fn evidence_capsule_capability(",
            ),
            ("graph_delta_projection", "fn graph_delta_projection("),
            (
                "graph_delta_projection_span",
                "fn graph_delta_projection_span(",
            ),
            ("render_projected_input", "fn render_projected_input("),
            (
                "projected_fused_candidates",
                "async fn projected_fused_candidates(",
            ),
            (
                "resolve_projected_entry_nodes",
                "fn resolve_projected_entry_nodes<'a>(",
            ),
            ("projected_traversal_ids", "fn projected_traversal_ids("),
            ("projected_node_passes", "fn projected_node_passes("),
            ("node_projection_digest", "fn node_projection_digest("),
        ] {
            nodes.push(node_at(name, needle));
        }
        // Put each real test two typed hops behind its editable source. Task
        // mode is allowed to satisfy its explicit test obligation through the
        // bounded graph expansion; ordinary flat fusion sees only the direct
        // helper and must spend top-k slots without receiving that obligation
        // for free.
        let projection_bridge = nodes
            .iter()
            .find(|node| node.id.name == "projection_request")
            .unwrap()
            .clone();
        let delta_bridge = nodes
            .iter()
            .find(|node| node.id.name == "graph_delta_projection")
            .unwrap()
            .clone();
        let graph = make_graph_state_with_edges(
            nodes,
            vec![
                make_edge(&task_test, &projection_bridge, EdgeKind::TestedBy),
                make_edge(&projection_bridge, &projected, EdgeKind::Calls),
                make_edge(&delta_test, &delta_bridge, EdgeKind::TestedBy),
                make_edge(&delta_bridge, &delta, EdgeKind::Calls),
            ],
        );
        let edge_index = ProjectedEdgeIndex::new(&graph);
        let ctx = make_search_context(&graph, repository.path());
        for (query, expected, expected_test) in [
            (
                "Change `projected_search` search projection source evidence behavior and verify `task_agent_projection_carries_stable_action_fields`",
                projected.stable_id(),
                task_test.stable_id(),
            ),
            (
                "Review `projected_graph_delta` search projection source evidence behavior and verify `live_graph_delta_infers_corroborated_edges_without_mutating_graph`",
                delta.stable_id(),
                delta_test.stable_id(),
            ),
        ] {
            let task_params = SearchParams {
                query: Some(query.into()),
                context_mode: Some("task".into()),
                context_roles: Some(vec![
                    "editable_source".into(),
                    "test".into(),
                    "direct_dependency".into(),
                    "caller_or_impact".into(),
                ]),
                context_facets: Some(vec!["behavior".into(), "test".into()]),
                hops: Some(2),
                limit: Some(16),
                include_artifacts: false,
                include_markdown: false,
                ..Default::default()
            };
            let output = task_records(&task_params, &ctx, &edge_index).await.unwrap();
            assert!(output.records.iter().any(|record| {
                record.identity.node_id == expected
                    && record.selection.role == Some(ProjectionRole::EditableSource)
            }));
            assert!(output.records.iter().any(|record| {
                record.identity.node_id == expected_test
                    && record.selection.role == Some(ProjectionRole::Test)
            }));

            let flat_params = SearchParams {
                query: task_params.query.clone(),
                // Give flat retrieval twice as many records as the role-aware
                // bundle. It still cannot express the required dependency and
                // impact obligations, while its extra records make the cost
                // comparison conservative rather than k-starved.
                limit: Some(32),
                include_artifacts: false,
                include_markdown: false,
                ..Default::default()
            };
            let (flat, _, _) = projected_fused_candidates(&flat_params, &ctx, &edge_index)
                .await
                .unwrap();
            let flat_roles = flat
                .iter()
                .take(flat_params.limit.unwrap())
                .filter_map(|candidate| find_node(&graph, &candidate.stable_id))
                .map(default_role)
                .collect::<BTreeSet<_>>();
            let flat_ids = flat
                .iter()
                .take(flat_params.limit.unwrap())
                .map(|candidate| candidate.stable_id.as_str())
                .collect::<BTreeSet<_>>();
            let task_roles = output
                .records
                .iter()
                .filter_map(|record| record.selection.role)
                .collect::<BTreeSet<_>>();
            let required = [
                ProjectionRole::EditableSource,
                ProjectionRole::Test,
                ProjectionRole::DirectDependency,
                ProjectionRole::CallerOrImpact,
            ];
            let task_coverage = required
                .iter()
                .filter(|role| task_roles.contains(role))
                .count();
            let flat_coverage = required
                .iter()
                .filter(|role| flat_roles.contains(role))
                .count();
            assert_eq!(task_coverage, required.len());
            assert!(
                task_coverage > flat_coverage,
                "task lanes must improve required-role coverage: task={task_roles:?} flat={flat_roles:?} flat_ids={flat_ids:?}"
            );

            let task_rendered = projected_search(&task_params, &ctx).await;
            let flat_rendered = projected_search(&flat_params, &ctx).await;
            assert!(
                task_rendered.len() < flat_rendered.len(),
                "task context must render below flat top-k bytes: task={} flat={}",
                task_rendered.len(),
                flat_rendered.len()
            );
        }
    }

    #[test]
    fn task_graph_expansion_honors_edge_types_and_hops_for_graph_only_nodes() {
        let mut a = make_node("a", NodeKind::Function, "src/lib.rs");
        let mut b = make_node("b", NodeKind::Function, "src/lib.rs");
        let mut c = make_node("c", NodeKind::Function, "src/lib.rs");
        for node in [&mut a, &mut b, &mut c] {
            node.line_start = 1;
            node.line_end = 1;
        }
        let graph = make_graph_state_with_edges(
            vec![a.clone(), b.clone(), c.clone()],
            vec![
                make_edge(&a, &b, EdgeKind::Calls),
                make_edge(&b, &c, EdgeKind::DependsOn),
            ],
        );
        let repository = PathBuf::from("/tmp/task-graph-fixture");
        let ctx = make_search_context(&graph, &repository);
        let edge_index = ProjectedEdgeIndex::new(&graph);
        let mut assemblies = BTreeMap::new();
        merge_task_assembly(
            &mut assemblies,
            single_channel_fused(
                &a.stable_id(),
                EvidenceChannel::ExactLexical,
                ScoreKind::ExactMatchTier,
            ),
            TaskRole::EditableSource,
            TaskLane::ExactReference,
            TaskFacet::Behavior,
            Some("a".into()),
            0,
            "seed".into(),
        );
        let mut relationships = Vec::new();
        expand_task_graph(
            &SearchParams {
                edge_types: Some(vec!["calls".into()]),
                hops: Some(2),
                ..Default::default()
            },
            &ctx,
            &edge_index,
            &mut assemblies,
            &mut relationships,
        );
        assert!(assemblies.contains_key(&b.stable_id()));
        assert!(!assemblies.contains_key(&c.stable_id()));
        assert_eq!(relationships.len(), 1);
    }

    #[test]
    fn live_graph_delta_infers_corroborated_edges_without_mutating_graph() {
        let mut caller = make_node("caller", NodeKind::Function, "src/lib.rs");
        caller.line_start = 1;
        caller.line_end = 3;
        let mut callee = make_node("callee", NodeKind::Function, "src/lib.rs");
        callee.line_start = 5;
        callee.line_end = 5;
        let mut replacement = make_node("replacement", NodeKind::Function, "src/lib.rs");
        replacement.line_start = 7;
        replacement.line_end = 7;
        let graph = make_graph_state_with_edges(
            vec![caller.clone(), callee.clone(), replacement.clone()],
            vec![make_edge(&caller, &callee, EdgeKind::Calls)],
        );
        let nodes_before = graph.nodes.iter().map(Node::stable_id).collect::<Vec<_>>();
        let edges_before = graph
            .edges
            .iter()
            .map(|edge| {
                (
                    edge.from.to_stable_id(),
                    edge.to.to_stable_id(),
                    edge.kind.clone(),
                )
            })
            .collect::<Vec<_>>();
        let repository = PathBuf::from("/tmp/graph-delta-fixture");
        let ctx = make_search_context(&graph, &repository);
        let edge_index = ProjectedEdgeIndex::new(&graph);
        let card = live_graph_delta_card(
            &SearchParams {
                context_mode: Some("graph-delta-beta".into()),
                root: Some("local".into()),
                proposal: Some(
                    "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n fn caller() {\n-    callee();\n+    replacement();\n }\n"
                        .into(),
                ),
                ..Default::default()
            },
            &ctx,
            &edge_index,
        )
        .unwrap();
        assert_eq!(card.changed_edges.len(), 2);
        assert_eq!(card.routes.len(), 2);
        assert!(
            card.impacted_loci
                .iter()
                .any(|impact| impact.label == "caller")
        );
        assert_eq!(
            nodes_before,
            graph.nodes.iter().map(Node::stable_id).collect::<Vec<_>>()
        );
        assert_eq!(
            edges_before,
            graph
                .edges
                .iter()
                .map(|edge| (
                    edge.from.to_stable_id(),
                    edge.to.to_stable_id(),
                    edge.kind.clone()
                ))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn rna_graph_delta_card_is_smaller_and_more_role_specific_than_raw_traversal() {
        let repository = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repository.path().join("src/service")).unwrap();
        let source = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/service/search.rs"),
        )
        .unwrap();
        std::fs::write(repository.path().join("src/service/search.rs"), &source).unwrap();
        let line_of = |needle: &str| {
            u32::try_from(
                source
                    .lines()
                    .position(|line| line.contains(needle))
                    .unwrap_or_else(|| panic!("real RNA source fixture is missing {needle:?}"))
                    + 1,
            )
            .unwrap()
        };
        let node_at = |name: &str, needle: &str| {
            let mut node = make_node(name, NodeKind::Function, "src/service/search.rs");
            let line = line_of(needle);
            node.line_start = line as usize;
            node.line_end = (line as usize + 39).min(source.lines().count());
            node.body = source
                .lines()
                .skip(line as usize - 1)
                .take(node.line_end - node.line_start + 1)
                .collect::<Vec<_>>()
                .join("\n");
            node
        };

        let mut search_node = make_node("search", NodeKind::Function, "src/service/search.rs");
        search_node.line_start = line_of("pub async fn search(") as usize;
        search_node.line_end = line_of("fn legacy_product_controls(") as usize - 1;
        search_node.body = "pub async fn search(/* current product dispatch */)".into();
        let legacy = node_at("legacy_search_dispatch", "async fn legacy_search_dispatch(");
        let projected = node_at("projected_search", "async fn projected_search(");
        let mut nodes = vec![search_node.clone(), legacy.clone(), projected.clone()];
        for (name, needle) in [
            ("validate_named_values", "fn validate_named_values("),
            (
                "validate_search_experience",
                "fn validate_search_experience(",
            ),
            ("strict_semantic_requested", "fn strict_semantic_requested("),
            ("projection_request", "fn projection_request("),
            ("projection_source_reader", "fn projection_source_reader("),
            ("hydrate_from_handle", "async fn hydrate_from_handle("),
            (
                "projected_non_code_records",
                "async fn projected_non_code_records(",
            ),
            (
                "projected_live_markdown_records",
                "fn projected_live_markdown_records(",
            ),
            (
                "evidence_capsule_capability",
                "fn evidence_capsule_capability(",
            ),
            ("task_records", "async fn task_records("),
            (
                "task_lane_candidate_evidence",
                "fn task_lane_candidate_evidence(",
            ),
            ("requested_task_facets", "fn requested_task_facets("),
            ("expand_task_graph", "fn expand_task_graph("),
            ("rendered_task_bundle_cost", "fn rendered_task_bundle_cost("),
            ("live_graph_delta_card", "fn live_graph_delta_card("),
            ("enrich_live_graph_delta", "fn enrich_live_graph_delta("),
            ("graph_delta_projection", "fn graph_delta_projection("),
            ("render_projected_input", "fn render_projected_input("),
            (
                "projected_fused_candidates",
                "async fn projected_fused_candidates(",
            ),
            ("projected_node_passes", "fn projected_node_passes("),
            ("node_projection_digest", "fn node_projection_digest("),
            ("default_role", "fn default_role("),
            ("projected_relationships", "fn projected_relationships("),
            ("compiler_location", "fn compiler_location("),
            ("source_roots", "fn source_roots("),
            ("collect_suffix_matches", "fn collect_suffix_matches("),
            ("source_span", "fn source_span("),
            ("format_lsp_completeness", "fn format_lsp_completeness("),
            ("legacy_search_dispatch", "async fn legacy_search_dispatch("),
            ("selected_for_hydration", "fn selected_for_hydration("),
            ("task_role_from_str", "fn task_role_from_str("),
            ("lexical_score", "fn lexical_score("),
            ("node_source_span", "fn node_source_span("),
            ("symbol_summary", "fn symbol_summary("),
            ("selected_from_exact_node", "fn selected_from_exact_node("),
            ("default_capabilities", "fn default_capabilities("),
            ("channel_for_evidence", "fn channel_for_evidence("),
            (
                "resolve_projected_entry_nodes",
                "fn resolve_projected_entry_nodes<'a>(",
            ),
        ] {
            nodes.push(node_at(name, needle));
        }
        let mut nearest_route_test = node_at(
            "live_graph_delta_infers_corroborated_edges_without_mutating_graph",
            "fn live_graph_delta_infers_corroborated_edges_without_mutating_graph(",
        );
        nearest_route_test
            .metadata
            .insert("is_test".into(), "true".into());
        nodes.push(nearest_route_test.clone());

        let mut edges = vec![make_edge(&search_node, &legacy, EdgeKind::Calls)];
        for node in nodes.iter().skip(3) {
            edges.push(make_edge(node, &search_node, EdgeKind::Calls));
        }
        edges.push(make_edge(
            &nearest_route_test,
            &search_node,
            EdgeKind::TestedBy,
        ));
        let graph = make_graph_state_with_edges(nodes, edges);
        let edge_index = ProjectedEdgeIndex::new(&graph);
        let ctx = make_search_context(&graph, repository.path());
        let changed_line = line_of("return legacy_search_dispatch(params, ctx).await;");
        let proposal = format!(
            "diff --git a/src/service/search.rs b/src/service/search.rs\n--- a/src/service/search.rs\n+++ b/src/service/search.rs\n@@ -{changed_line},1 +{changed_line},1 @@\n-        return legacy_search_dispatch(params, ctx).await;\n+        return projected_search(params, ctx).await;\n"
        );
        let card = projected_graph_delta(
            &SearchParams {
                context_mode: Some("graph-delta-beta".into()),
                root: Some("local".into()),
                proposal: Some(proposal),
                limit: Some(4),
                include_artifacts: false,
                include_markdown: false,
                ..Default::default()
            },
            &ctx,
            &edge_index,
        )
        .await;
        // The card covers all affected loci in one bounded response. The
        // context-equivalent legacy baseline therefore needs the neighbors and
        // impact view for every locus, not only the edited seed.
        let mut raw_bytes = 0usize;
        let mut seed_neighbors = String::new();
        let mut seed_impact = String::new();
        for node_id in graph.nodes.iter().map(Node::stable_id) {
            let neighbors = search(
                &SearchParams {
                    node: Some(node_id.clone()),
                    mode: Some("neighbors".into()),
                    limit: Some(50),
                    include_body: true,
                    ..Default::default()
                },
                &ctx,
            )
            .await;
            let impact = search(
                &SearchParams {
                    node: Some(node_id.clone()),
                    mode: Some("impact".into()),
                    hops: Some(1),
                    limit: Some(50),
                    include_body: true,
                    ..Default::default()
                },
                &ctx,
            )
            .await;
            if node_id == search_node.stable_id() {
                seed_neighbors = neighbors.clone();
                seed_impact = impact.clone();
            }
            raw_bytes = raw_bytes
                .saturating_add(neighbors.len())
                .saturating_add(impact.len());
        }

        assert!(
            card.len() < raw_bytes,
            "graph-delta card must be smaller than raw neighbors+impact: card={} raw={raw_bytes}",
            card.len()
        );
        assert!(card.contains("role: caller_or_impact"));
        assert!(
            card.contains("role: test"),
            "nearest route test missing: {card}"
        );
        assert!(card.contains("graph_delta_route"));
        assert!(!seed_neighbors.contains("role:"));
        assert!(!seed_impact.contains("role:"));
    }

    #[test]
    fn acceptance_remediation_task_direction_validation_fails_closed_and_accepts_both() {
        let mut params = SearchParams {
            query: Some("change behavior".into()),
            context_mode: Some("task".into()),
            direction: Some("sideways".into()),
            ..Default::default()
        };
        assert_eq!(
            validate_search_experience(&params),
            Err("unknown task-context direction `sideways`; allowed values: incoming, outgoing, both".into())
        );
        params.direction = Some("both".into());
        assert!(validate_search_experience(&params).is_ok());
    }

    #[test]
    fn acceptance_remediation_exact_task_roles_preserve_test_and_definition_truth() {
        let mut test = make_node("test_task_roles", NodeKind::Function, "tests/task.rs");
        test.metadata.insert("is_test".into(), "true".into());
        let definition = make_node("TaskState", NodeKind::Struct, "src/task.rs");

        assert_eq!(exact_task_roles(&test), BTreeSet::from([TaskRole::Test]));
        assert_eq!(
            task_facet_for_role(*exact_task_roles(&test).first().unwrap()),
            TaskFacet::Test
        );
        assert_eq!(
            exact_task_roles(&definition),
            BTreeSet::from([TaskRole::DefinitionOrApiState])
        );
        assert_eq!(
            task_facet_for_role(*exact_task_roles(&definition).first().unwrap()),
            TaskFacet::ApiOrState
        );
    }

    #[test]
    fn acceptance_remediation_added_line_uses_only_same_symbol_old_side_context() {
        let mut before = make_node("before", NodeKind::Function, "src/lib.rs");
        before.line_start = 1;
        before.line_end = 9;
        let mut after = make_node("after", NodeKind::Function, "src/lib.rs");
        after.line_start = 10;
        after.line_end = 20;
        let boundary_graph = make_graph_state(vec![before, after]);
        let repository = tempfile::tempdir().unwrap();
        let boundary_ctx = make_search_context(&boundary_graph, repository.path());
        let added = graph_delta::ChangedLineFact {
            kind: graph_delta::ChangedLineKind::Added,
            grounding: graph_delta::ProposalLine {
                root: "local".into(),
                path: "src/lib.rs".into(),
                proposal_line: 5,
                old_line: None,
                new_line: Some(10),
            },
            text: "inserted();".into(),
        };
        let hunk = graph_delta::ChangedHunkFact {
            proposal_header_line: 4,
            old_start: 9,
            old_count: 2,
            new_start: 9,
            new_count: 3,
            changed_lines: vec![added.clone()],
        };
        let file = graph_delta::ChangedFileFact {
            root: "local".into(),
            path: "src/lib.rs".into(),
            hunks: vec![hunk.clone()],
        };
        assert!(pre_edit_node_for_changed_line(&boundary_ctx, &file, &hunk, &added).is_err());

        let mut enclosing = make_node("enclosing", NodeKind::Function, "src/lib.rs");
        enclosing.line_start = 1;
        enclosing.line_end = 20;
        let enclosing_graph = make_graph_state(vec![enclosing]);
        let enclosing_ctx = make_search_context(&enclosing_graph, repository.path());
        assert_eq!(
            pre_edit_node_for_changed_line(&enclosing_ctx, &file, &hunk, &added)
                .unwrap()
                .id
                .name,
            "enclosing"
        );
        let proposal_grounding = graph_delta::EvidenceGrounding::Proposal(added.grounding);
        assert!(graph_delta_grounding_node(&proposal_grounding, &enclosing_ctx).is_err());
    }

    #[test]
    fn acceptance_delivery_changed_line_prefers_source_over_lsp_document_symbol_proof() {
        let mut source = make_node("hello", NodeKind::Function, "lib.rs");
        source.line_start = 2;
        source.line_end = 2;
        let mut literal = make_node("world", NodeKind::Const, "lib.rs");
        literal.line_start = 2;
        literal.line_end = 2;
        literal.metadata.insert("synthetic".into(), "true".into());
        let mut proof = make_node(
            "hello@proof",
            NodeKind::Other("lsp_document_symbol".into()),
            "lib.rs",
        );
        proof.line_start = 2;
        proof.line_end = 2;
        let graph = make_graph_state(vec![source.clone(), literal, proof]);
        let repository = tempfile::tempdir().unwrap();
        let ctx = make_search_context(&graph, repository.path());

        assert_eq!(
            unique_node_at_line(&ctx, "lib.rs", 2).unwrap().stable_id(),
            source.stable_id()
        );

        let mut ambiguous = make_node("also_hello", NodeKind::Function, "lib.rs");
        ambiguous.line_start = 2;
        ambiguous.line_end = 2;
        let ambiguous_graph = make_graph_state(vec![source, ambiguous]);
        let ambiguous_ctx = make_search_context(&ambiguous_graph, repository.path());
        assert_eq!(
            unique_node_at_line(&ambiguous_ctx, "lib.rs", 2).unwrap_err(),
            "multiple equally specific current graph nodes contain the changed line"
        );
    }

    #[test]
    fn acceptance_delivery_replacement_addition_uses_paired_removed_coordinate() {
        let mut source = make_node("hello", NodeKind::Function, "lib.rs");
        source.line_start = 2;
        source.line_end = 2;
        let graph = make_graph_state(vec![source.clone()]);
        let repository = tempfile::tempdir().unwrap();
        let ctx = make_search_context(&graph, repository.path());
        let removed = graph_delta::ChangedLineFact {
            kind: graph_delta::ChangedLineKind::Removed,
            grounding: graph_delta::ProposalLine {
                root: "local".into(),
                path: "lib.rs".into(),
                proposal_line: 5,
                old_line: Some(2),
                new_line: None,
            },
            text: "pub fn hello() { old(); }".into(),
        };
        let added = graph_delta::ChangedLineFact {
            kind: graph_delta::ChangedLineKind::Added,
            grounding: graph_delta::ProposalLine {
                root: "local".into(),
                path: "lib.rs".into(),
                proposal_line: 6,
                old_line: None,
                new_line: Some(2),
            },
            text: "pub fn hello() { new(); }".into(),
        };
        let hunk = graph_delta::ChangedHunkFact {
            proposal_header_line: 4,
            old_start: 2,
            old_count: 1,
            new_start: 2,
            new_count: 1,
            changed_lines: vec![removed, added.clone()],
        };
        let file = graph_delta::ChangedFileFact {
            root: "local".into(),
            path: "lib.rs".into(),
            hunks: vec![hunk.clone()],
        };

        assert_eq!(
            pre_edit_node_for_changed_line(&ctx, &file, &hunk, &added)
                .unwrap()
                .stable_id(),
            source.stable_id()
        );
    }

    #[tokio::test]
    async fn acceptance_delivery_local_knowledge_remains_exactly_searchable_without_markdown_expansion()
     {
        let mut quote = make_node(
            "quote.mcp-provenance",
            NodeKind::Other("quote".into()),
            "provenance.md",
        );
        quote.language = "markdown".into();
        quote.source = ExtractionSource::Markdown;
        quote
            .metadata
            .insert("local_knowledge".into(), "true".into());
        quote.metadata.insert("rna.kind".into(), "quote".into());
        quote
            .metadata
            .insert("rna.id".into(), "quote.mcp-provenance".into());
        quote
            .metadata
            .insert("rna.name".into(), "MCP provenance smoke fixture".into());
        quote
            .metadata
            .insert("rna.metadata.public_use".into(), "mcp_verified".into());
        let mut section = make_node("section", NodeKind::MarkdownSection, "provenance.md");
        section.language = "markdown".into();

        assert_eq!(node_delivery_class(&quote), NodeDeliveryClass::Code);
        assert_eq!(node_delivery_class(&section), NodeDeliveryClass::Markdown);

        let graph = make_graph_state(vec![quote]);
        let repository = tempfile::tempdir().unwrap();
        let ctx = make_search_context(&graph, repository.path());
        let output = search(
            &SearchParams {
                query: Some("quote.mcp-provenance".into()),
                compact: true,
                limit: Some(3),
                include_artifacts: false,
                include_markdown: false,
                ..Default::default()
            },
            &ctx,
        )
        .await;

        assert!(output.contains("quote.mcp-provenance"), "{output}");
        assert!(output.contains("src:markdown"), "{output}");
        assert!(output.contains("mcp_verified"), "{output}");
    }

    #[test]
    fn acceptance_delivery_graph_delta_keeps_proposal_only_lines_as_typed_records() {
        let card = graph_delta::GraphDeltaCard {
            beta: true,
            capabilities: Vec::new(),
            changed_files: vec![graph_delta::ChangedFileFact {
                root: "local".into(),
                path: "lib.rs".into(),
                hunks: vec![graph_delta::ChangedHunkFact {
                    proposal_header_line: 4,
                    old_start: 2,
                    old_count: 1,
                    new_start: 2,
                    new_count: 1,
                    changed_lines: vec![graph_delta::ChangedLineFact {
                        kind: graph_delta::ChangedLineKind::Added,
                        grounding: graph_delta::ProposalLine {
                            root: "local".into(),
                            path: "lib.rs".into(),
                            proposal_line: 6,
                            old_line: None,
                            new_line: Some(2),
                        },
                        text: "pub fn hello() { proposed(); }".into(),
                    }],
                }],
            }],
            routes: Vec::new(),
            changed_edges: Vec::new(),
            impacted_loci: Vec::new(),
            impacted_tests: Vec::new(),
            impacted_state_or_api: Vec::new(),
            bypassed_loci: Vec::new(),
            behavioral_deltas: Vec::new(),
            behavioral_analogues: Vec::new(),
            affected_locus_checklist: Vec::new(),
            omissions: Vec::new(),
        };
        let graph = make_graph_state(Vec::new());
        let repository = tempfile::tempdir().unwrap();
        let ctx = make_search_context(&graph, repository.path());

        let (records, _, _, _, _) = graph_delta_projection(
            &card,
            &SearchParams {
                limit: Some(4),
                ..Default::default()
            },
            &ctx,
        );

        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].selection.role,
            Some(ProjectionRole::ProposalDelta)
        );
        assert_eq!(
            records[0].selection.lane,
            Some(ProjectionLane::ProposalDelta)
        );
        assert_eq!(records[0].symbol.kind, "proposal_added_line");
        assert_eq!(
            records[0].symbol.signature,
            "pub fn hello() { proposed(); }"
        );
        assert!(records[0].identity.source.is_none());
        assert!(
            records[0]
                .identity
                .node_id
                .starts_with("graph-delta:v1:proposal:")
        );
    }

    #[test]
    fn acceptance_delivery_sealed_product_queries_use_verified_reranker_loader() {
        assert!(!use_verified_reranker_loader(false, false, false));
        assert!(use_verified_reranker_loader(true, false, false));
        assert!(use_verified_reranker_loader(false, true, false));
        assert!(use_verified_reranker_loader(true, true, false));
        assert!(!use_verified_reranker_loader(false, true, true));
    }

    #[test]
    fn acceptance_remediation_source_backed_behavior_contrasts_cover_all_typed_classes_and_loci() {
        let mut proposed = SourceBehaviorProfile::default();
        let mut analogue = SourceBehaviorProfile::default();
        for kind in behavior_profile_kinds() {
            proposed.record(kind, "shared");
            analogue.record(kind, "shared");
            proposed.record(kind, "proposal-only");
            analogue.record(kind, "analogue-only");
        }
        // Whole bodies are classified line-by-line: removing one occurrence
        // must not erase the same branch behavior retained on another line.
        record_text_behavior_features(
            &mut proposed,
            "if first",
            graph_delta::ChangedLineKind::Added,
        );
        record_text_behavior_features(
            &mut proposed,
            "if second",
            graph_delta::ChangedLineKind::Added,
        );
        record_text_behavior_features(
            &mut proposed,
            "if first",
            graph_delta::ChangedLineKind::Removed,
        );
        assert!(
            proposed
                .values(graph_delta::BehavioralDeltaKind::BranchBehavior)
                .contains("if:1")
        );

        let changed = graph_delta::EvidenceGrounding::Proposal(graph_delta::ProposalLine {
            root: "local".into(),
            path: "src/lib.rs".into(),
            proposal_line: 7,
            old_line: None,
            new_line: Some(12),
        });
        let source = graph_delta::EvidenceGrounding::CurrentSource(graph_delta::SourceSpan {
            root: "local".into(),
            path: "src/analogue.rs".into(),
            start_line: 3,
            end_line: 9,
        });
        let current = graph_delta::EvidenceGrounding::CurrentSource(graph_delta::SourceSpan {
            root: "local".into(),
            path: "src/lib.rs".into(),
            start_line: 10,
            end_line: 20,
        });
        let helper_changed = graph_delta::EvidenceGrounding::Proposal(graph_delta::ProposalLine {
            root: "local".into(),
            path: "src/lib.rs".into(),
            proposal_line: 8,
            old_line: Some(13),
            new_line: None,
        });
        let loci = BTreeMap::from([
            (graph_delta::BehavioralDeltaKind::BranchBehavior, changed),
            (
                graph_delta::BehavioralDeltaKind::BypassedCall,
                helper_changed,
            ),
        ]);
        let deltas =
            source_backed_behavioral_contrasts(&proposed, &analogue, &loci, &current, &source);
        assert_eq!(deltas.len(), 6);
        assert_eq!(
            deltas
                .iter()
                .map(|delta| delta.kind)
                .collect::<BTreeSet<_>>(),
            behavior_profile_kinds().into_iter().collect()
        );
        assert!(deltas.iter().all(|delta| {
            delta.kind != graph_delta::BehavioralDeltaKind::Other
                && matches!(
                    delta.analogue_locus,
                    Some(graph_delta::EvidenceGrounding::CurrentSource(_))
                )
                && delta.label.contains("proposal_only")
                && delta.label.contains("analogue_only")
        }));
        assert!(deltas.iter().any(|delta| {
            delta.kind == graph_delta::BehavioralDeltaKind::BranchBehavior
                && matches!(
                    delta.changed_locus,
                    graph_delta::EvidenceGrounding::Proposal(graph_delta::ProposalLine {
                        proposal_line: 7,
                        ..
                    })
                )
        }));
        assert!(deltas.iter().any(|delta| {
            delta.kind == graph_delta::BehavioralDeltaKind::BypassedCall
                && matches!(
                    delta.changed_locus,
                    graph_delta::EvidenceGrounding::Proposal(graph_delta::ProposalLine {
                        proposal_line: 8,
                        ..
                    })
                )
        }));
        assert!(
            deltas
                .iter()
                .filter(|delta| !matches!(
                    delta.kind,
                    graph_delta::BehavioralDeltaKind::BranchBehavior
                        | graph_delta::BehavioralDeltaKind::BypassedCall
                ))
                .all(|delta| matches!(
                    delta.changed_locus,
                    graph_delta::EvidenceGrounding::CurrentSource(_)
                ))
        );
        assert_eq!(
            deltas,
            source_backed_behavioral_contrasts(&proposed, &analogue, &loci, &current, &source)
        );
    }
}
