//! Deterministic, bounded primitives for assembling task-oriented context.
//!
//! This module deliberately stops at typed selection. Adapters in the shared
//! service layer are responsible for turning selected evidence into source
//! spans and projecting it for CLI or MCP rendering.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

pub(crate) const MAX_TASK_BYTES: usize = 64 * 1024;
pub(crate) const MAX_TASK_LINES: usize = 1_024;
pub(crate) const MAX_EXACT_REFERENCES: usize = 128;
pub(crate) const MAX_SELECTION_CANDIDATES: usize = 256;
pub(crate) const HARD_MAX_SELECTION_STATES: usize = 4_096;
pub(crate) const MAX_RENDERED_BUDGET: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum TaskFacet {
    Behavior,
    ApiOrState,
    Test,
    Analogue,
    Proposal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ContextRole {
    EditableSource,
    DefinitionOrApiState,
    Test,
    BehavioralAnalogue,
    DirectDependency,
    CallerOrImpact,
    ProposalDelta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum RetrievalLane {
    ExactReference,
    EditableSource,
    DefinitionOrState,
    Tests,
    Analogues,
    Dependencies,
    GraphImpact,
    ProposalDelta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ReferenceOrigin {
    Prose,
    CodeFence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ExactReferenceKind {
    RepositoryPath,
    CompilerLocation,
    QualifiedName,
    TestName,
    CodeIdentifier,
    BacktickedIdentifier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactReference {
    pub(crate) raw: String,
    pub(crate) normalized: String,
    pub(crate) kind: ExactReferenceKind,
    pub(crate) origin: ReferenceOrigin,
    pub(crate) byte_start: usize,
    pub(crate) byte_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedTask {
    pub(crate) facets: BTreeSet<TaskFacet>,
    /// Explicit references in prose. These are eligible for exact-first
    /// resolution and pinning.
    pub(crate) exact_references: Vec<ExactReference>,
    /// Reference-shaped tokens inside a fenced proposal. These are kept
    /// separate so a proposal cannot silently become current-source truth.
    pub(crate) proposal_references: Vec<ExactReference>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TaskContextError {
    TaskTooLarge {
        actual: usize,
        limit: usize,
    },
    TooManyLines {
        actual: usize,
        limit: usize,
    },
    TooManyReferences {
        limit: usize,
    },
    TooManyCandidates {
        actual: usize,
        limit: usize,
    },
    SelectionStateLimitExceeded {
        actual: usize,
        limit: usize,
    },
    InvalidSelectionPolicy(&'static str),
    InvalidCandidate {
        evidence_id: String,
        reason: &'static str,
    },
    ConflictingDuplicate {
        evidence_id: String,
    },
}

impl fmt::Display for TaskContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TaskTooLarge { actual, limit } => {
                write!(formatter, "task is {actual} bytes; limit is {limit}")
            }
            Self::TooManyLines { actual, limit } => {
                write!(formatter, "task is {actual} lines; limit is {limit}")
            }
            Self::TooManyReferences { limit } => {
                write!(
                    formatter,
                    "task contains more than {limit} exact references"
                )
            }
            Self::TooManyCandidates { actual, limit } => {
                write!(formatter, "received {actual} candidates; limit is {limit}")
            }
            Self::SelectionStateLimitExceeded { actual, limit } => write!(
                formatter,
                "task-context selection needs {actual} Pareto states; limit is {limit}"
            ),
            Self::InvalidSelectionPolicy(reason) => {
                write!(formatter, "invalid task-context selection policy: {reason}")
            }
            Self::InvalidCandidate {
                evidence_id,
                reason,
            } => write!(formatter, "invalid candidate {evidence_id:?}: {reason}"),
            Self::ConflictingDuplicate { evidence_id } => write!(
                formatter,
                "candidate {evidence_id:?} was supplied with conflicting metadata"
            ),
        }
    }
}

impl Error for TaskContextError {}

pub(crate) fn parse_task(task: &str) -> Result<ParsedTask, TaskContextError> {
    if task.len() > MAX_TASK_BYTES {
        return Err(TaskContextError::TaskTooLarge {
            actual: task.len(),
            limit: MAX_TASK_BYTES,
        });
    }

    let line_count = if task.is_empty() {
        0
    } else {
        task.as_bytes()
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
            + 1
    };
    if line_count > MAX_TASK_LINES {
        return Err(TaskContextError::TooManyLines {
            actual: line_count,
            limit: MAX_TASK_LINES,
        });
    }

    let mut references = Vec::new();
    let mut prose = String::new();
    let mut byte_offset = 0;
    let mut fence: Option<(u8, usize)> = None;

    for line_with_ending in task.split_inclusive('\n') {
        let line = line_with_ending
            .strip_suffix('\n')
            .unwrap_or(line_with_ending)
            .strip_suffix('\r')
            .unwrap_or_else(|| {
                line_with_ending
                    .strip_suffix('\n')
                    .unwrap_or(line_with_ending)
            });
        let trimmed = line.trim_start();
        let fence_marker = fence_run(trimmed);

        match (fence, fence_marker) {
            (Some((expected, minimum)), Some((actual, length)))
                if expected == actual && length >= minimum =>
            {
                fence = None;
            }
            (None, Some(marker)) => {
                fence = Some(marker);
            }
            (Some(_), _) => extract_line_references(
                line,
                byte_offset,
                ReferenceOrigin::CodeFence,
                &mut references,
            )?,
            (None, _) => {
                prose.push_str(line);
                prose.push('\n');
                extract_line_references(
                    line,
                    byte_offset,
                    ReferenceOrigin::Prose,
                    &mut references,
                )?;
            }
        }

        byte_offset += line_with_ending.len();
    }

    references.sort_by(|left, right| {
        left.byte_start
            .cmp(&right.byte_start)
            .then_with(|| left.byte_end.cmp(&right.byte_end))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.normalized.cmp(&right.normalized))
    });

    let mut seen = BTreeSet::new();
    references.retain(|reference| {
        seen.insert((
            reference.origin,
            reference.kind,
            reference.normalized.clone(),
        ))
    });

    let (proposal_references, exact_references): (Vec<_>, Vec<_>) = references
        .into_iter()
        .partition(|reference| reference.origin == ReferenceOrigin::CodeFence);

    let mut facets = infer_facets(&prose);
    if !proposal_references.is_empty() || fence.is_some() {
        facets.insert(TaskFacet::Proposal);
    }

    Ok(ParsedTask {
        facets,
        exact_references,
        proposal_references,
    })
}

fn fence_run(trimmed: &str) -> Option<(u8, usize)> {
    let marker = *trimmed.as_bytes().first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let length = trimmed
        .as_bytes()
        .iter()
        .take_while(|byte| **byte == marker)
        .count();
    (length >= 3).then_some((marker, length))
}

fn extract_line_references(
    line: &str,
    byte_offset: usize,
    origin: ReferenceOrigin,
    output: &mut Vec<ExactReference>,
) -> Result<(), TaskContextError> {
    let mut inline_ranges = Vec::new();

    if origin == ReferenceOrigin::Prose {
        let bytes = line.as_bytes();
        let mut cursor = 0;
        while cursor < bytes.len() {
            let Some(open_relative) = bytes[cursor..].iter().position(|byte| *byte == b'`') else {
                break;
            };
            let open = cursor + open_relative;
            let content_start = open + 1;
            let Some(close_relative) = bytes[content_start..].iter().position(|byte| *byte == b'`')
            else {
                break;
            };
            let close = content_start + close_relative;
            let raw = &line[content_start..close];
            let leading = raw.len() - raw.trim_start().len();
            let trimmed = raw.trim();
            if !trimmed.is_empty()
                && let Some((kind, normalized)) = classify_reference(trimmed, true)
            {
                push_reference(
                    output,
                    ExactReference {
                        raw: trimmed.to_owned(),
                        normalized,
                        kind,
                        origin,
                        byte_start: byte_offset + content_start + leading,
                        byte_end: byte_offset + content_start + leading + trimmed.len(),
                    },
                )?;
            }
            inline_ranges.push((open, close + 1));
            cursor = close + 1;
        }
    }

    let mut token_start = None;
    for (index, character) in line
        .char_indices()
        .chain(std::iter::once((line.len(), ' ')))
    {
        if index < line.len() && is_reference_token_character(character) {
            token_start.get_or_insert(index);
            continue;
        }

        let Some(start) = token_start.take() else {
            continue;
        };
        if inline_ranges
            .iter()
            .any(|(range_start, range_end)| start >= *range_start && start < *range_end)
        {
            continue;
        }
        let raw = &line[start..index];
        if let Some((kind, normalized)) = classify_reference(raw, false) {
            push_reference(
                output,
                ExactReference {
                    raw: raw.to_owned(),
                    normalized,
                    kind,
                    origin,
                    byte_start: byte_offset + start,
                    byte_end: byte_offset + index,
                },
            )?;
        }
    }

    Ok(())
}

fn push_reference(
    output: &mut Vec<ExactReference>,
    reference: ExactReference,
) -> Result<(), TaskContextError> {
    if output.len() >= MAX_EXACT_REFERENCES {
        return Err(TaskContextError::TooManyReferences {
            limit: MAX_EXACT_REFERENCES,
        });
    }
    output.push(reference);
    Ok(())
}

fn is_reference_token_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(character, '_' | '-' | '.' | '/' | ':' | '@' | '#')
}

fn classify_reference(raw: &str, backticked: bool) -> Option<(ExactReferenceKind, String)> {
    let token = raw.trim_matches(|character: char| {
        matches!(
            character,
            ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\''
        )
    });
    if token.is_empty() || token.contains("://") {
        return None;
    }

    if let Some(location) = normalize_compiler_location(token) {
        return Some((ExactReferenceKind::CompilerLocation, location));
    }
    if is_repository_path(token) {
        return Some((
            ExactReferenceKind::RepositoryPath,
            token.strip_prefix("./").unwrap_or(token).to_owned(),
        ));
    }
    if is_qualified_name(token) {
        return Some((ExactReferenceKind::QualifiedName, token.to_owned()));
    }
    if is_test_name(token) {
        return Some((ExactReferenceKind::TestName, token.to_owned()));
    }
    if is_identifier(token) && token.contains('_') {
        return Some((ExactReferenceKind::CodeIdentifier, token.to_owned()));
    }
    if backticked && is_identifier(token) {
        return Some((ExactReferenceKind::BacktickedIdentifier, token.to_owned()));
    }
    None
}

fn normalize_compiler_location(token: &str) -> Option<String> {
    let parts: Vec<_> = token.split(':').collect();
    if parts.len() < 2
        || !parts
            .last()?
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return None;
    }

    let path_end = if parts.len() >= 3
        && parts[parts.len() - 2]
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        parts.len() - 2
    } else {
        parts.len() - 1
    };
    let path = parts[..path_end].join(":");
    if !is_repository_path(&path) {
        return None;
    }
    let normalized_path = path.strip_prefix("./").unwrap_or(&path);
    Some(format!(
        "{}:{}",
        normalized_path,
        parts[path_end..].join(":")
    ))
}

fn is_repository_path(token: &str) -> bool {
    let candidate = token.strip_prefix("./").unwrap_or(token);
    if candidate.is_empty()
        || candidate.starts_with('/')
        || candidate.ends_with('/')
        || candidate.split('/').any(|segment| segment == "..")
        || !candidate.contains('/')
    {
        return false;
    }
    let last = candidate.rsplit('/').next().unwrap_or_default();
    last.contains('.')
        || candidate.starts_with("src/")
        || candidate.starts_with("tests/")
        || candidate.starts_with("crates/")
        || candidate.starts_with(".oh/")
}

fn is_identifier(token: &str) -> bool {
    let mut characters = token.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn is_qualified_name(token: &str) -> bool {
    token.contains("::")
        && token
            .split("::")
            .all(|component| !component.is_empty() && is_identifier(component))
}

fn is_test_name(token: &str) -> bool {
    is_identifier(token)
        && (token.starts_with("test_")
            || token.ends_with("_test")
            || token.starts_with("should_")
            || token.starts_with("it_"))
}

fn infer_facets(prose: &str) -> BTreeSet<TaskFacet> {
    let normalized = prose.to_ascii_lowercase();
    let mut facets = BTreeSet::new();
    if contains_any(
        &normalized,
        &[
            "behavior",
            "behaviour",
            "implement",
            "change",
            "fix",
            "support",
        ],
    ) {
        facets.insert(TaskFacet::Behavior);
    }
    if contains_any(
        &normalized,
        &[" api", "schema", "state", "contract", "interface", "field"],
    ) {
        facets.insert(TaskFacet::ApiOrState);
    }
    if contains_any(&normalized, &["test", "assert", "verify", "regression"]) {
        facets.insert(TaskFacet::Test);
    }
    if contains_any(
        &normalized,
        &[
            "analogue",
            "analog",
            "precedent",
            "similar",
            "existing pattern",
        ],
    ) {
        facets.insert(TaskFacet::Analogue);
    }
    if contains_any(&normalized, &["proposal", "patch", "diff", "sketch"]) {
        facets.insert(TaskFacet::Proposal);
    }
    facets
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactCandidate {
    pub(crate) evidence_id: String,
    pub(crate) display: String,
    pub(crate) match_keys: BTreeSet<String>,
    pub(crate) source_file: String,
    pub(crate) source_line: Option<u32>,
}

impl ExactCandidate {
    pub(crate) fn canonical_match_keys(&self) -> BTreeSet<String> {
        let mut keys = self
            .match_keys
            .iter()
            .map(|key| normalize_match_key(key))
            .collect::<BTreeSet<_>>();
        keys.insert(normalize_match_key(&self.display));
        keys.insert(normalize_match_key(&self.source_file));
        if let Some(line) = self.source_line {
            keys.insert(normalize_match_key(&format!("{}:{line}", self.source_file)));
        }
        keys
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExactResolution {
    Hit(ExactCandidate),
    Ambiguous(Vec<ExactCandidate>),
    Miss,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedExactReference {
    pub(crate) reference: ExactReference,
    pub(crate) resolution: ExactResolution,
}

pub(crate) fn resolve_exact_references(
    references: &[ExactReference],
    candidates: &[ExactCandidate],
) -> Vec<ResolvedExactReference> {
    let mut canonical_candidates = candidates.to_vec();
    canonical_candidates.sort_by(|left, right| {
        left.evidence_id
            .cmp(&right.evidence_id)
            .then_with(|| left.display.cmp(&right.display))
            .then_with(|| left.source_file.cmp(&right.source_file))
            .then_with(|| left.source_line.cmp(&right.source_line))
    });
    canonical_candidates.dedup_by(|left, right| left.evidence_id == right.evidence_id);

    references
        .iter()
        .cloned()
        .map(|reference| {
            let key = normalize_match_key(&reference.normalized);
            let matches = canonical_candidates
                .iter()
                .filter(|candidate| candidate.canonical_match_keys().contains(&key))
                .cloned()
                .collect::<Vec<_>>();
            let resolution = match matches.as_slice() {
                [] => ExactResolution::Miss,
                [candidate] => ExactResolution::Hit(candidate.clone()),
                _ => ExactResolution::Ambiguous(matches),
            };
            ResolvedExactReference {
                reference,
                resolution,
            }
        })
        .collect()
}

fn normalize_match_key(key: &str) -> String {
    key.trim()
        .trim_matches('`')
        .strip_prefix("./")
        .unwrap_or_else(|| key.trim().trim_matches('`'))
        .to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SourceAnchor {
    pub(crate) path: String,
    pub(crate) start_line: u32,
    pub(crate) end_line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvidenceCandidate {
    pub(crate) evidence_id: String,
    pub(crate) roles: BTreeSet<ContextRole>,
    pub(crate) lanes: BTreeSet<RetrievalLane>,
    pub(crate) facets: BTreeSet<TaskFacet>,
    pub(crate) rendered_cost: usize,
    /// Present only when this record is an unambiguous exact-reference hit.
    pub(crate) exact_reference: Option<String>,
    pub(crate) source: SourceAnchor,
    /// Rank inside the candidate's retrieval channel. Lower is better.
    pub(crate) channel_rank: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectionPolicy {
    pub(crate) rendered_budget: usize,
    pub(crate) per_record_limit: usize,
    pub(crate) candidate_limit: usize,
    pub(crate) per_file_limit: usize,
    /// Maximum non-dominated states retained by the exact selector. The
    /// selector rejects the request instead of truncating this frontier.
    pub(crate) state_limit: usize,
    pub(crate) required_roles: BTreeSet<ContextRole>,
}

impl Default for SelectionPolicy {
    fn default() -> Self {
        Self {
            rendered_budget: 32 * 1024,
            per_record_limit: 16 * 1024,
            candidate_limit: MAX_SELECTION_CANDIDATES,
            per_file_limit: 4,
            state_limit: 1_024,
            required_roles: BTreeSet::from([
                ContextRole::EditableSource,
                ContextRole::DefinitionOrApiState,
                ContextRole::Test,
                ContextRole::BehavioralAnalogue,
            ]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SelectionReason {
    ExactReference {
        reference: String,
    },
    CoveragePerCost {
        newly_covered_roles: BTreeSet<ContextRole>,
        newly_covered_lanes: BTreeSet<RetrievalLane>,
        newly_covered_facets: BTreeSet<TaskFacet>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectedEvidence {
    pub(crate) evidence_id: String,
    pub(crate) reason: SelectionReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OmissionReason {
    ExactRecordTooLarge,
    ExactBudgetExhausted,
    RecordTooLarge,
    BudgetExhausted,
    PerFileLimit,
    NoMarginalCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OmittedEvidence {
    pub(crate) evidence_id: String,
    pub(crate) reason: OmissionReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextSelection {
    pub(crate) selected: Vec<SelectedEvidence>,
    pub(crate) omitted: Vec<OmittedEvidence>,
    pub(crate) rendered_cost: usize,
    pub(crate) covered_roles: BTreeSet<ContextRole>,
    pub(crate) missing_roles: BTreeSet<ContextRole>,
    pub(crate) covered_lanes: BTreeSet<RetrievalLane>,
    pub(crate) covered_facets: BTreeSet<TaskFacet>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CoverageGain {
    roles: BTreeSet<ContextRole>,
    lanes: BTreeSet<RetrievalLane>,
    facets: BTreeSet<TaskFacet>,
}

pub(crate) fn select_context(
    candidates: Vec<EvidenceCandidate>,
    policy: &SelectionPolicy,
) -> Result<ContextSelection, TaskContextError> {
    validate_policy(policy)?;
    if candidates.len() > policy.candidate_limit {
        return Err(TaskContextError::TooManyCandidates {
            actual: candidates.len(),
            limit: policy.candidate_limit,
        });
    }

    let candidates = canonicalize_candidates(candidates)?;
    let mut selected = Vec::new();
    let mut omitted = Vec::new();
    let mut selected_ids = BTreeSet::new();
    let mut omitted_ids = BTreeSet::new();
    let mut covered_roles = BTreeSet::new();
    let mut covered_lanes = BTreeSet::new();
    let mut covered_facets = BTreeSet::new();
    let mut file_counts = BTreeMap::<String, usize>::new();
    let mut rendered_cost = 0usize;

    let mut exact = candidates
        .values()
        .filter(|candidate| candidate.exact_reference.is_some())
        .collect::<Vec<_>>();
    exact.sort_by(|left, right| {
        left.exact_reference
            .cmp(&right.exact_reference)
            .then_with(|| left.evidence_id.cmp(&right.evidence_id))
    });

    for candidate in exact {
        let reason = if candidate.rendered_cost > policy.per_record_limit {
            Some(OmissionReason::ExactRecordTooLarge)
        } else if rendered_cost + candidate.rendered_cost > policy.rendered_budget {
            Some(OmissionReason::ExactBudgetExhausted)
        } else {
            None
        };
        if let Some(reason) = reason {
            omitted_ids.insert(candidate.evidence_id.clone());
            omitted.push(OmittedEvidence {
                evidence_id: candidate.evidence_id.clone(),
                reason,
            });
            continue;
        }

        rendered_cost += candidate.rendered_cost;
        selected_ids.insert(candidate.evidence_id.clone());
        *file_counts
            .entry(candidate.source.path.clone())
            .or_default() += 1;
        covered_roles.extend(candidate.roles.iter().copied());
        covered_lanes.extend(candidate.lanes.iter().copied());
        covered_facets.extend(candidate.facets.iter().copied());
        selected.push(SelectedEvidence {
            evidence_id: candidate.evidence_id.clone(),
            reason: SelectionReason::ExactReference {
                reference: candidate.exact_reference.clone().unwrap_or_default(),
            },
        });
    }

    let eligible = candidates
        .values()
        .filter(|candidate| {
            candidate.exact_reference.is_none()
                && candidate.rendered_cost <= policy.per_record_limit
        })
        .collect::<Vec<_>>();
    let mut frontier = vec![SelectionState {
        selected_ids: Vec::new(),
        rendered_cost,
        role_mask: role_mask(&covered_roles, &policy.required_roles),
        lane_mask: lane_mask(&covered_lanes),
        facet_mask: facet_mask(&covered_facets),
        file_counts: file_counts.clone(),
        channel_rank_sum: 0,
    }];
    let initially_relevant_files = eligible
        .iter()
        .map(|candidate| candidate.source.path.clone())
        .collect::<BTreeSet<_>>();
    frontier[0]
        .file_counts
        .retain(|path, _| initially_relevant_files.contains(path));

    // This is an exact, bounded dynamic program rather than a greedy pass.
    // States are the Pareto frontier of coverage, cost, rank, and future
    // per-file capacity. If that frontier cannot be represented within the
    // caller's cap, selection fails closed instead of silently becoming
    // approximate or input-order dependent.
    for (index, candidate) in eligible.iter().enumerate() {
        let mut expanded = Vec::with_capacity(frontier.len().saturating_mul(2));
        for state in &frontier {
            expanded.push(state.clone());
            if state.rendered_cost + candidate.rendered_cost > policy.rendered_budget
                || state
                    .file_counts
                    .get(&candidate.source.path)
                    .copied()
                    .unwrap_or(0)
                    >= policy.per_file_limit
            {
                continue;
            }

            let candidate_role_mask = role_mask(&candidate.roles, &policy.required_roles);
            let candidate_lane_mask = lane_mask(&candidate.lanes);
            let candidate_facet_mask = facet_mask(&candidate.facets);
            if candidate_role_mask & !state.role_mask == 0
                && candidate_lane_mask & !state.lane_mask == 0
                && candidate_facet_mask & !state.facet_mask == 0
            {
                continue;
            }

            let mut added = state.clone();
            added.rendered_cost += candidate.rendered_cost;
            added.role_mask |= candidate_role_mask;
            added.lane_mask |= candidate_lane_mask;
            added.facet_mask |= candidate_facet_mask;
            *added
                .file_counts
                .entry(candidate.source.path.clone())
                .or_default() += 1;
            added.channel_rank_sum += u64::from(candidate.channel_rank);
            added.selected_ids.push(candidate.evidence_id.clone());
            expanded.push(added);
        }

        // Counts for files with no remaining candidates cannot constrain a
        // future decision. Forgetting them is exact and prevents irrelevant
        // file identities from inflating the Pareto frontier.
        let active_files = eligible[index + 1..]
            .iter()
            .map(|candidate| candidate.source.path.clone())
            .collect::<BTreeSet<_>>();
        for state in &mut expanded {
            state
                .file_counts
                .retain(|path, _| active_files.contains(path));
        }
        frontier = pareto_frontier(expanded);
        if frontier.len() > policy.state_limit {
            return Err(TaskContextError::SelectionStateLimitExceeded {
                actual: frontier.len(),
                limit: policy.state_limit,
            });
        }
    }

    let best = frontier
        .into_iter()
        .max_by(selection_state_cmp)
        .expect("selection frontier always contains the empty state");
    for evidence_id in best.selected_ids {
        let candidate = candidates
            .get(&evidence_id)
            .expect("selected evidence came from the canonical candidate map");
        let gain = coverage_gain(
            candidate,
            policy,
            &covered_roles,
            &covered_lanes,
            &covered_facets,
        );
        rendered_cost += candidate.rendered_cost;
        selected_ids.insert(candidate.evidence_id.clone());
        *file_counts
            .entry(candidate.source.path.clone())
            .or_default() += 1;
        covered_roles.extend(candidate.roles.iter().copied());
        covered_lanes.extend(candidate.lanes.iter().copied());
        covered_facets.extend(candidate.facets.iter().copied());
        selected.push(SelectedEvidence {
            evidence_id: candidate.evidence_id.clone(),
            reason: SelectionReason::CoveragePerCost {
                newly_covered_roles: gain.roles,
                newly_covered_lanes: gain.lanes,
                newly_covered_facets: gain.facets,
            },
        });
    }

    for candidate in candidates.values() {
        if selected_ids.contains(&candidate.evidence_id)
            || omitted_ids.contains(&candidate.evidence_id)
        {
            continue;
        }
        let reason = if candidate.rendered_cost > policy.per_record_limit {
            OmissionReason::RecordTooLarge
        } else if rendered_cost + candidate.rendered_cost > policy.rendered_budget {
            OmissionReason::BudgetExhausted
        } else if candidate.exact_reference.is_none()
            && file_counts
                .get(&candidate.source.path)
                .copied()
                .unwrap_or(0)
                >= policy.per_file_limit
        {
            OmissionReason::PerFileLimit
        } else {
            OmissionReason::NoMarginalCoverage
        };
        omitted.push(OmittedEvidence {
            evidence_id: candidate.evidence_id.clone(),
            reason,
        });
    }
    omitted.sort_by(|left, right| left.evidence_id.cmp(&right.evidence_id));

    let missing_roles = policy
        .required_roles
        .difference(&covered_roles)
        .copied()
        .collect();
    Ok(ContextSelection {
        selected,
        omitted,
        rendered_cost,
        covered_roles,
        missing_roles,
        covered_lanes,
        covered_facets,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectionState {
    selected_ids: Vec<String>,
    rendered_cost: usize,
    role_mask: u16,
    lane_mask: u16,
    facet_mask: u8,
    file_counts: BTreeMap<String, usize>,
    channel_rank_sum: u64,
}

fn role_bit(role: ContextRole) -> u16 {
    1 << match role {
        ContextRole::EditableSource => 0,
        ContextRole::DefinitionOrApiState => 1,
        ContextRole::Test => 2,
        ContextRole::BehavioralAnalogue => 3,
        ContextRole::DirectDependency => 4,
        ContextRole::CallerOrImpact => 5,
        ContextRole::ProposalDelta => 6,
    }
}

fn lane_bit(lane: RetrievalLane) -> u16 {
    1 << match lane {
        RetrievalLane::ExactReference => 0,
        RetrievalLane::EditableSource => 1,
        RetrievalLane::DefinitionOrState => 2,
        RetrievalLane::Tests => 3,
        RetrievalLane::Analogues => 4,
        RetrievalLane::Dependencies => 5,
        RetrievalLane::GraphImpact => 6,
        RetrievalLane::ProposalDelta => 7,
    }
}

fn facet_bit(facet: TaskFacet) -> u8 {
    1 << match facet {
        TaskFacet::Behavior => 0,
        TaskFacet::ApiOrState => 1,
        TaskFacet::Test => 2,
        TaskFacet::Analogue => 3,
        TaskFacet::Proposal => 4,
    }
}

fn role_mask(roles: &BTreeSet<ContextRole>, required_roles: &BTreeSet<ContextRole>) -> u16 {
    roles
        .intersection(required_roles)
        .fold(0, |mask, role| mask | role_bit(*role))
}

fn lane_mask(lanes: &BTreeSet<RetrievalLane>) -> u16 {
    lanes.iter().fold(0, |mask, lane| mask | lane_bit(*lane))
}

fn facet_mask(facets: &BTreeSet<TaskFacet>) -> u8 {
    facets
        .iter()
        .fold(0, |mask, facet| mask | facet_bit(*facet))
}

fn selection_state_value(state: &SelectionState) -> u32 {
    state.role_mask.count_ones() * 64
        + state.facet_mask.count_ones() * 4
        + state.lane_mask.count_ones() * 2
}

fn selection_state_cmp(left: &SelectionState, right: &SelectionState) -> Ordering {
    selection_state_value(left)
        .cmp(&selection_state_value(right))
        .then_with(|| right.rendered_cost.cmp(&left.rendered_cost))
        .then_with(|| right.channel_rank_sum.cmp(&left.channel_rank_sum))
        .then_with(|| right.selected_ids.cmp(&left.selected_ids))
}

fn file_capacity_no_worse(left: &BTreeMap<String, usize>, right: &BTreeMap<String, usize>) -> bool {
    left.iter()
        .all(|(path, count)| *count <= right.get(path).copied().unwrap_or(0))
}

fn state_dominates(left: &SelectionState, right: &SelectionState) -> bool {
    let coverage_no_worse = left.role_mask | right.role_mask == left.role_mask
        && left.lane_mask | right.lane_mask == left.lane_mask
        && left.facet_mask | right.facet_mask == left.facet_mask;
    let resources_no_worse = left.rendered_cost <= right.rendered_cost
        && left.channel_rank_sum <= right.channel_rank_sum
        && file_capacity_no_worse(&left.file_counts, &right.file_counts);
    if !coverage_no_worse || !resources_no_worse {
        return false;
    }

    left.role_mask != right.role_mask
        || left.lane_mask != right.lane_mask
        || left.facet_mask != right.facet_mask
        || left.rendered_cost != right.rendered_cost
        || left.channel_rank_sum != right.channel_rank_sum
        || left.file_counts != right.file_counts
        || left.selected_ids <= right.selected_ids
}

fn pareto_frontier(mut states: Vec<SelectionState>) -> Vec<SelectionState> {
    states.sort_by(|left, right| selection_state_cmp(right, left));
    let mut frontier = Vec::<SelectionState>::new();
    for candidate in states {
        if frontier
            .iter()
            .any(|existing| state_dominates(existing, &candidate))
        {
            continue;
        }
        frontier.retain(|existing| !state_dominates(&candidate, existing));
        frontier.push(candidate);
    }
    frontier.sort_by(|left, right| selection_state_cmp(right, left));
    frontier
}

fn validate_policy(policy: &SelectionPolicy) -> Result<(), TaskContextError> {
    if policy.rendered_budget == 0 || policy.rendered_budget > MAX_RENDERED_BUDGET {
        return Err(TaskContextError::InvalidSelectionPolicy(
            "rendered_budget must be within the hard service limit",
        ));
    }
    if policy.per_record_limit == 0 || policy.per_record_limit > policy.rendered_budget {
        return Err(TaskContextError::InvalidSelectionPolicy(
            "per_record_limit must be non-zero and no larger than rendered_budget",
        ));
    }
    if policy.candidate_limit == 0 || policy.candidate_limit > MAX_SELECTION_CANDIDATES {
        return Err(TaskContextError::InvalidSelectionPolicy(
            "candidate_limit must be within the hard service limit",
        ));
    }
    if policy.per_file_limit == 0 || policy.per_file_limit > policy.candidate_limit {
        return Err(TaskContextError::InvalidSelectionPolicy(
            "per_file_limit must be non-zero and no larger than candidate_limit",
        ));
    }
    if policy.state_limit == 0 || policy.state_limit > HARD_MAX_SELECTION_STATES {
        return Err(TaskContextError::InvalidSelectionPolicy(
            "state_limit must be within the hard service limit",
        ));
    }
    Ok(())
}

fn canonicalize_candidates(
    candidates: Vec<EvidenceCandidate>,
) -> Result<BTreeMap<String, EvidenceCandidate>, TaskContextError> {
    let mut canonical = BTreeMap::new();
    for candidate in candidates {
        if candidate.evidence_id.trim().is_empty() {
            return Err(TaskContextError::InvalidCandidate {
                evidence_id: candidate.evidence_id,
                reason: "evidence_id must not be empty",
            });
        }
        if candidate.rendered_cost == 0 {
            return Err(TaskContextError::InvalidCandidate {
                evidence_id: candidate.evidence_id,
                reason: "rendered_cost must be non-zero",
            });
        }
        if candidate.source.path.trim().is_empty()
            || candidate.source.start_line == 0
            || candidate.source.end_line < candidate.source.start_line
        {
            return Err(TaskContextError::InvalidCandidate {
                evidence_id: candidate.evidence_id,
                reason: "source anchor must name a path and a valid one-based line range",
            });
        }
        match canonical.get(&candidate.evidence_id) {
            Some(existing) if existing != &candidate => {
                return Err(TaskContextError::ConflictingDuplicate {
                    evidence_id: candidate.evidence_id,
                });
            }
            Some(_) => {}
            None => {
                canonical.insert(candidate.evidence_id.clone(), candidate);
            }
        }
    }
    Ok(canonical)
}

fn coverage_gain(
    candidate: &EvidenceCandidate,
    policy: &SelectionPolicy,
    covered_roles: &BTreeSet<ContextRole>,
    covered_lanes: &BTreeSet<RetrievalLane>,
    covered_facets: &BTreeSet<TaskFacet>,
) -> CoverageGain {
    CoverageGain {
        roles: candidate
            .roles
            .intersection(&policy.required_roles)
            .filter(|role| !covered_roles.contains(role))
            .copied()
            .collect(),
        lanes: candidate.lanes.difference(covered_lanes).copied().collect(),
        facets: candidate
            .facets
            .difference(covered_facets)
            .copied()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set<T: Ord>(values: impl IntoIterator<Item = T>) -> BTreeSet<T> {
        values.into_iter().collect()
    }

    fn candidate(
        id: &str,
        cost: usize,
        roles: impl IntoIterator<Item = ContextRole>,
        lanes: impl IntoIterator<Item = RetrievalLane>,
    ) -> EvidenceCandidate {
        EvidenceCandidate {
            evidence_id: id.to_owned(),
            roles: set(roles),
            lanes: set(lanes),
            facets: set([TaskFacet::Behavior]),
            rendered_cost: cost,
            exact_reference: None,
            source: SourceAnchor {
                path: format!("src/{id}.rs"),
                start_line: 1,
                end_line: 3,
            },
            channel_rank: 0,
        }
    }

    #[test]
    fn parser_keeps_proposal_tokens_out_of_exact_pins() {
        let parsed = parse_task(
            "Fix `service::search` in src/service/search.rs:42 and add a regression test.\n\
             ```rust\n\
             fn proposed_helper() { service::future(); }\n\
             ```",
        )
        .unwrap();

        assert!(
            parsed
                .exact_references
                .iter()
                .any(|reference| reference.normalized == "service::search")
        );
        assert!(
            parsed
                .exact_references
                .iter()
                .any(|reference| reference.normalized == "src/service/search.rs:42")
        );
        assert!(
            parsed
                .proposal_references
                .iter()
                .any(|reference| reference.normalized == "proposed_helper")
        );
        assert!(parsed.facets.contains(&TaskFacet::Test));
        assert!(parsed.facets.contains(&TaskFacet::Proposal));
    }

    #[test]
    fn exact_resolution_distinguishes_hit_ambiguity_and_miss() {
        let references = ["only_one", "shared_name", "missing_name"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| ExactReference {
                raw: name.to_owned(),
                normalized: name.to_owned(),
                kind: ExactReferenceKind::CodeIdentifier,
                origin: ReferenceOrigin::Prose,
                byte_start: index,
                byte_end: index + name.len(),
            })
            .collect::<Vec<_>>();
        let candidates = [
            ("a", "only_one"),
            ("b", "shared_name"),
            ("c", "shared_name"),
        ]
        .into_iter()
        .map(|(id, key)| ExactCandidate {
            evidence_id: id.to_owned(),
            display: key.to_owned(),
            match_keys: BTreeSet::new(),
            source_file: format!("src/{id}.rs"),
            source_line: Some(1),
        })
        .collect::<Vec<_>>();

        let resolved = resolve_exact_references(&references, &candidates);
        assert!(matches!(resolved[0].resolution, ExactResolution::Hit(_)));
        assert!(matches!(
            resolved[1].resolution,
            ExactResolution::Ambiguous(ref matches) if matches.len() == 2
        ));
        assert_eq!(resolved[2].resolution, ExactResolution::Miss);
    }

    #[test]
    fn selection_is_invariant_to_candidate_order() {
        let policy = SelectionPolicy {
            rendered_budget: 16,
            per_record_limit: 16,
            candidate_limit: 16,
            per_file_limit: 2,
            state_limit: 128,
            required_roles: set([
                ContextRole::EditableSource,
                ContextRole::Test,
                ContextRole::BehavioralAnalogue,
            ]),
        };
        let candidates = vec![
            candidate(
                "multi",
                8,
                [ContextRole::EditableSource, ContextRole::Test],
                [RetrievalLane::EditableSource, RetrievalLane::Tests],
            ),
            candidate(
                "analogue",
                4,
                [ContextRole::BehavioralAnalogue],
                [RetrievalLane::Analogues],
            ),
            candidate("decoy", 15, [], [RetrievalLane::Dependencies]),
        ];
        let forward = select_context(candidates.clone(), &policy).unwrap();
        let reverse = select_context(candidates.into_iter().rev().collect(), &policy).unwrap();

        assert_eq!(forward, reverse);
        assert!(forward.missing_roles.is_empty());
        assert_eq!(
            forward
                .selected
                .iter()
                .map(|evidence| evidence.evidence_id.as_str())
                .collect::<Vec<_>>(),
            vec!["analogue", "multi"]
        );
    }

    #[test]
    fn exact_hits_are_pinned_before_coverage_candidates() {
        let policy = SelectionPolicy {
            rendered_budget: 10,
            per_record_limit: 10,
            candidate_limit: 8,
            per_file_limit: 1,
            state_limit: 128,
            required_roles: set([ContextRole::EditableSource]),
        };
        let mut exact = candidate("exact", 7, [], [RetrievalLane::ExactReference]);
        exact.exact_reference = Some("src/exact.rs".to_owned());
        let high_coverage = candidate(
            "coverage",
            5,
            [ContextRole::EditableSource],
            [RetrievalLane::EditableSource],
        );

        let selection = select_context(vec![high_coverage, exact], &policy).unwrap();
        assert_eq!(selection.selected[0].evidence_id, "exact");
        assert_eq!(selection.rendered_cost, 7);
        assert!(
            selection
                .omitted
                .iter()
                .any(|omission| omission.evidence_id == "coverage"
                    && omission.reason == OmissionReason::BudgetExhausted)
        );
    }

    #[test]
    fn oversized_records_are_visible_omissions() {
        let policy = SelectionPolicy {
            rendered_budget: 10,
            per_record_limit: 5,
            candidate_limit: 8,
            per_file_limit: 2,
            state_limit: 128,
            required_roles: set([ContextRole::Test]),
        };
        let selection = select_context(
            vec![candidate(
                "giant",
                6,
                [ContextRole::Test],
                [RetrievalLane::Tests],
            )],
            &policy,
        )
        .unwrap();

        assert!(selection.selected.is_empty());
        assert_eq!(selection.omitted[0].reason, OmissionReason::RecordTooLarge);
        assert_eq!(selection.missing_roles, set([ContextRole::Test]));
    }

    #[test]
    fn pareto_selector_finds_maximum_role_coverage_that_greedy_misses() {
        let policy = SelectionPolicy {
            rendered_budget: 6,
            per_record_limit: 6,
            candidate_limit: 8,
            per_file_limit: 2,
            state_limit: 128,
            required_roles: set([
                ContextRole::EditableSource,
                ContextRole::DefinitionOrApiState,
                ContextRole::Test,
                ContextRole::BehavioralAnalogue,
            ]),
        };
        let candidates = vec![
            candidate(
                "broad",
                4,
                [
                    ContextRole::EditableSource,
                    ContextRole::DefinitionOrApiState,
                    ContextRole::Test,
                ],
                [RetrievalLane::EditableSource],
            ),
            candidate(
                "left",
                3,
                [
                    ContextRole::EditableSource,
                    ContextRole::DefinitionOrApiState,
                ],
                [RetrievalLane::EditableSource],
            ),
            candidate(
                "right",
                3,
                [ContextRole::Test, ContextRole::BehavioralAnalogue],
                [RetrievalLane::Analogues],
            ),
        ];

        let selection = select_context(candidates, &policy).unwrap();
        assert_eq!(
            selection
                .selected
                .iter()
                .map(|evidence| evidence.evidence_id.as_str())
                .collect::<Vec<_>>(),
            ["left", "right"]
        );
        assert!(selection.missing_roles.is_empty());
    }

    #[test]
    fn selector_fails_closed_when_pareto_frontier_exceeds_cap() {
        let policy = SelectionPolicy {
            rendered_budget: 8,
            per_record_limit: 8,
            candidate_limit: 8,
            per_file_limit: 2,
            state_limit: 1,
            required_roles: set([ContextRole::Test]),
        };
        let result = select_context(
            vec![candidate(
                "test",
                1,
                [ContextRole::Test],
                [RetrievalLane::Tests],
            )],
            &policy,
        );

        assert_eq!(
            result,
            Err(TaskContextError::SelectionStateLimitExceeded {
                actual: 2,
                limit: 1,
            })
        );
    }

    #[test]
    fn graph_only_evidence_can_satisfy_an_impact_role() {
        let policy = SelectionPolicy {
            rendered_budget: 8,
            per_record_limit: 8,
            candidate_limit: 8,
            per_file_limit: 2,
            state_limit: 32,
            required_roles: set([ContextRole::CallerOrImpact]),
        };
        let selection = select_context(
            vec![candidate(
                "graph-impact",
                2,
                [ContextRole::CallerOrImpact],
                [RetrievalLane::GraphImpact],
            )],
            &policy,
        )
        .unwrap();

        assert_eq!(selection.selected[0].evidence_id, "graph-impact");
        assert_eq!(selection.covered_lanes, set([RetrievalLane::GraphImpact]));
        assert!(selection.missing_roles.is_empty());
    }

    #[test]
    fn one_source_span_can_cover_multiple_roles_without_duplication() {
        let policy = SelectionPolicy {
            rendered_budget: 8,
            per_record_limit: 8,
            candidate_limit: 8,
            per_file_limit: 2,
            state_limit: 32,
            required_roles: set([ContextRole::EditableSource, ContextRole::Test]),
        };
        let selection = select_context(
            vec![candidate(
                "multi-role",
                2,
                [ContextRole::EditableSource, ContextRole::Test],
                [RetrievalLane::EditableSource, RetrievalLane::Tests],
            )],
            &policy,
        )
        .unwrap();

        assert_eq!(selection.selected.len(), 1);
        assert!(matches!(
            &selection.selected[0].reason,
            SelectionReason::CoveragePerCost {
                newly_covered_roles,
                ..
            } if newly_covered_roles
                == &set([ContextRole::EditableSource, ContextRole::Test])
        ));
    }
}
