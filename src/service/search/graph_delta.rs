//! Opt-in, bounded graph-delta analysis over an ephemeral proposal overlay.
//!
//! The types here are intentionally independent of persistence. A service
//! adapter may project the repository graph into `GraphSnapshot`, but applying
//! a proposal never mutates that snapshot or the persisted RNA graph.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

pub(crate) const HARD_MAX_PROPOSAL_BYTES: usize = 256 * 1024;
pub(crate) const HARD_MAX_PROPOSAL_FILES: usize = 32;
pub(crate) const HARD_MAX_PROPOSAL_HUNKS: usize = 256;
pub(crate) const HARD_MAX_CHANGED_LINES: usize = 4_096;
pub(crate) const HARD_MAX_OVERLAY_EDGES: usize = 512;
pub(crate) const HARD_MAX_ENDPOINT_PAIRS: usize = 64;
pub(crate) const HARD_MAX_EQUAL_PATHS: usize = 16;
pub(crate) const HARD_MAX_VISITED_NODES: usize = 10_000;
pub(crate) const HARD_MAX_PATH_HOPS: usize = 128;
pub(crate) const HARD_MAX_EDGE_COST: u32 = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GraphDeltaError {
    BetaOptInRequired,
    InvalidLimits(&'static str),
    LimitExceeded {
        resource: &'static str,
        actual: usize,
        limit: usize,
    },
    UnsupportedProposal(&'static str),
    MalformedProposal {
        line: usize,
        reason: &'static str,
    },
    MalformedStructuredProposal(String),
    UnsafePath {
        line: usize,
        path: String,
    },
    InvalidGraph(String),
}

impl fmt::Display for GraphDeltaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BetaOptInRequired => {
                write!(formatter, "graph-delta beta was not explicitly enabled")
            }
            Self::InvalidLimits(reason) => {
                write!(formatter, "invalid graph-delta limits: {reason}")
            }
            Self::LimitExceeded {
                resource,
                actual,
                limit,
            } => write!(formatter, "{resource} count {actual} exceeds limit {limit}"),
            Self::UnsupportedProposal(reason) => {
                write!(formatter, "unsupported proposal: {reason}")
            }
            Self::MalformedProposal { line, reason } => {
                write!(formatter, "malformed proposal at line {line}: {reason}")
            }
            Self::MalformedStructuredProposal(reason) => {
                write!(formatter, "malformed structured proposal: {reason}")
            }
            Self::UnsafePath { line, path } => {
                write!(formatter, "unsafe proposal path at line {line}: {path:?}")
            }
            Self::InvalidGraph(reason) => write!(formatter, "invalid graph delta input: {reason}"),
        }
    }
}

impl Error for GraphDeltaError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraphDeltaLimits {
    pub(crate) proposal_bytes: usize,
    pub(crate) proposal_files: usize,
    pub(crate) proposal_hunks: usize,
    pub(crate) changed_lines: usize,
    pub(crate) overlay_edges: usize,
    pub(crate) endpoint_pairs: usize,
    pub(crate) equal_paths: usize,
    pub(crate) visited_nodes: usize,
    pub(crate) path_hops: usize,
}

impl Default for GraphDeltaLimits {
    fn default() -> Self {
        Self {
            proposal_bytes: HARD_MAX_PROPOSAL_BYTES,
            proposal_files: 20,
            proposal_hunks: 200,
            changed_lines: 2_000,
            overlay_edges: 256,
            endpoint_pairs: 32,
            equal_paths: 8,
            visited_nodes: 5_000,
            path_hops: 64,
        }
    }
}

impl GraphDeltaLimits {
    fn validate(&self) -> Result<(), GraphDeltaError> {
        let checks = [
            (
                self.proposal_bytes,
                HARD_MAX_PROPOSAL_BYTES,
                "proposal_bytes",
            ),
            (
                self.proposal_files,
                HARD_MAX_PROPOSAL_FILES,
                "proposal_files",
            ),
            (
                self.proposal_hunks,
                HARD_MAX_PROPOSAL_HUNKS,
                "proposal_hunks",
            ),
            (self.changed_lines, HARD_MAX_CHANGED_LINES, "changed_lines"),
            (self.overlay_edges, HARD_MAX_OVERLAY_EDGES, "overlay_edges"),
            (
                self.endpoint_pairs,
                HARD_MAX_ENDPOINT_PAIRS,
                "endpoint_pairs",
            ),
            (self.equal_paths, HARD_MAX_EQUAL_PATHS, "equal_paths"),
            (self.visited_nodes, HARD_MAX_VISITED_NODES, "visited_nodes"),
            (self.path_hops, HARD_MAX_PATH_HOPS, "path_hops"),
        ];
        if let Some((_, _, name)) = checks
            .into_iter()
            .find(|(value, hard_limit, _)| *value == 0 || value > hard_limit)
        {
            return Err(GraphDeltaError::InvalidLimits(name));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceSpan {
    pub(crate) root: String,
    pub(crate) path: String,
    pub(crate) start_line: u32,
    pub(crate) end_line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProposalLine {
    pub(crate) root: String,
    pub(crate) path: String,
    pub(crate) proposal_line: u32,
    pub(crate) old_line: Option<u32>,
    pub(crate) new_line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EvidenceGrounding {
    CurrentSource(SourceSpan),
    Proposal(ProposalLine),
}

impl EvidenceGrounding {
    fn validate(&self) -> Result<(), GraphDeltaError> {
        match self {
            Self::CurrentSource(span) => {
                validate_root_qualifier(&span.root)?;
                validate_relative_path(&span.path, 0)?;
                if span.start_line == 0 || span.end_line < span.start_line {
                    return Err(GraphDeltaError::InvalidGraph(
                        "source grounding has an invalid line range".to_owned(),
                    ));
                }
            }
            Self::Proposal(line) => {
                validate_root_qualifier(&line.root)?;
                validate_relative_path(&line.path, line.proposal_line as usize)?;
                if line.proposal_line == 0 {
                    return Err(GraphDeltaError::InvalidGraph(
                        "proposal grounding uses line zero".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn stable_hydration_key(&self) -> String {
        match self {
            Self::CurrentSource(span) => format!(
                "graph-delta:v1:source:{}:{}:{}:{}",
                encode_key_component(&span.root),
                encode_key_component(&span.path),
                span.start_line,
                span.end_line
            ),
            Self::Proposal(line) => format!(
                "graph-delta:v1:proposal:{}:{}:{}:{}:{}",
                encode_key_component(&line.root),
                encode_key_component(&line.path),
                line.proposal_line,
                line.old_line
                    .map_or_else(|| "-".to_owned(), |value| value.to_string()),
                line.new_line
                    .map_or_else(|| "-".to_owned(), |value| value.to_string())
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ImpactKind {
    EditableLocus,
    Test,
    StateOrApi,
    Caller,
    BehavioralAnalogue,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImpactEvidence {
    pub(crate) label: String,
    pub(crate) kind: ImpactKind,
    pub(crate) grounding: EvidenceGrounding,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphNode {
    pub(crate) id: String,
    pub(crate) kind: ImpactKind,
    pub(crate) grounding: EvidenceGrounding,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EdgeKey {
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WeightedEdge {
    pub(crate) key: EdgeKey,
    /// Positive path cost. Zero is rejected to keep shortest-path enumeration
    /// finite and deterministic.
    pub(crate) cost: u32,
    /// Lower values win only after total path cost ties.
    pub(crate) priority: u32,
    /// Lower values win after priority ties.
    pub(crate) registration_order: u32,
    pub(crate) grounding: EvidenceGrounding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraphSnapshot {
    pub(crate) nodes: Vec<GraphNode>,
    pub(crate) edges: Vec<WeightedEdge>,
}

pub(crate) const STRUCTURED_PROPOSAL_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChangedLineKind {
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangedLineFact {
    pub(crate) kind: ChangedLineKind,
    pub(crate) grounding: ProposalLine,
    /// Line text without the unified-diff marker. It is evidence for a live
    /// graph adapter, not a parser-level claim about graph relationships.
    pub(crate) text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangedHunkFact {
    pub(crate) proposal_header_line: u32,
    pub(crate) old_start: u32,
    pub(crate) old_count: u32,
    pub(crate) new_start: u32,
    pub(crate) new_count: u32,
    pub(crate) changed_lines: Vec<ChangedLineFact>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangedFileFact {
    pub(crate) root: String,
    pub(crate) path: String,
    pub(crate) hunks: Vec<ChangedHunkFact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InferredRelationshipKind {
    Call,
    Reference,
    Registration,
    AttributeOrStateReference,
}

impl InferredRelationshipKind {
    pub(crate) fn edge_kind(self) -> &'static str {
        match self {
            Self::Call => "calls",
            Self::Reference => "references",
            Self::Registration => "registers",
            Self::AttributeOrStateReference => "references_state",
        }
    }
}

/// A conservative proposal relationship awaiting live-graph endpoint
/// resolution. The pure parser identifies syntax and a qualified target; the
/// service adapter must corroborate exactly one current graph node before it
/// materializes an overlay edge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangedRelationshipFact {
    pub(crate) change: ChangedLineKind,
    pub(crate) kind: InferredRelationshipKind,
    pub(crate) qualifier: Option<String>,
    pub(crate) target: String,
    pub(crate) grounding: ProposalLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GraphDeltaCapability {
    ProposalParsing,
    LiveGraphInference,
    RouteAnalysis,
    ImpactTraversal,
    BehavioralAnalogueDiscovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CapabilityState {
    Ready,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapabilityReport {
    pub(crate) capability: GraphDeltaCapability,
    pub(crate) state: CapabilityState,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BehavioralDeltaKind {
    BypassedCall,
    BranchBehavior,
    Reconciliation,
    ErrorPath,
    Representation,
    StatePropagation,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BehavioralDelta {
    pub(crate) kind: BehavioralDeltaKind,
    pub(crate) label: String,
    pub(crate) changed_locus: EvidenceGrounding,
    pub(crate) analogue_locus: Option<EvidenceGrounding>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BehavioralAnalogue {
    pub(crate) label: String,
    pub(crate) changed_locus: ImpactEvidence,
    pub(crate) analogue_locus: ImpactEvidence,
    pub(crate) similarity_basis: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GraphDeltaOmissionCode {
    LiveGraphInferenceDeferred,
    ImpactTraversalUnavailable,
    BehavioralAnalogueUnavailable,
    CapabilityDegraded,
    TraversalLimited,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphDeltaOmission {
    pub(crate) code: GraphDeltaOmissionCode,
    pub(crate) detail: String,
    pub(crate) grounding: Option<EvidenceGrounding>,
    pub(crate) hydration_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StructuredProposal {
    pub(crate) schema_version: u16,
    #[serde(default)]
    pub(crate) changed_files: Vec<ChangedFileFact>,
    #[serde(default)]
    pub(crate) edge_additions: Vec<WeightedEdge>,
    #[serde(default)]
    pub(crate) edge_removals: Vec<EdgeKey>,
    #[serde(default)]
    pub(crate) relationships: Vec<ChangedRelationshipFact>,
    #[serde(default)]
    pub(crate) impacted: Vec<ImpactEvidence>,
    #[serde(default)]
    pub(crate) capabilities: Vec<CapabilityReport>,
    #[serde(default)]
    pub(crate) behavioral_deltas: Vec<BehavioralDelta>,
    #[serde(default)]
    pub(crate) analogues: Vec<BehavioralAnalogue>,
    #[serde(default)]
    pub(crate) omissions: Vec<GraphDeltaOmission>,
}

impl Default for StructuredProposal {
    fn default() -> Self {
        Self {
            schema_version: STRUCTURED_PROPOSAL_SCHEMA_VERSION,
            changed_files: Vec::new(),
            edge_additions: Vec::new(),
            edge_removals: Vec::new(),
            relationships: Vec::new(),
            impacted: Vec::new(),
            capabilities: Vec::new(),
            behavioral_deltas: Vec::new(),
            analogues: Vec::new(),
            omissions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProposalInput {
    UnifiedDiff(String),
    #[cfg(test)]
    Structured(StructuredProposal),
    StructuredJson(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BetaGraphDeltaRequest {
    /// Must be true. Merely supplying proposal data never opts a caller in.
    pub(crate) beta: bool,
    /// Stable workspace-root name qualifying every proposal/source anchor.
    pub(crate) root: String,
    pub(crate) proposal: ProposalInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct EphemeralOverlay {
    pub(crate) changed_files: Vec<ChangedFileFact>,
    pub(crate) edge_additions: Vec<WeightedEdge>,
    pub(crate) edge_removals: Vec<EdgeKey>,
    pub(crate) relationships: Vec<ChangedRelationshipFact>,
    pub(crate) impacted: Vec<ImpactEvidence>,
    pub(crate) capabilities: Vec<CapabilityReport>,
    pub(crate) behavioral_deltas: Vec<BehavioralDelta>,
    pub(crate) analogues: Vec<BehavioralAnalogue>,
    pub(crate) omissions: Vec<GraphDeltaOmission>,
}

pub(crate) fn parse_beta_proposal(
    request: BetaGraphDeltaRequest,
    limits: &GraphDeltaLimits,
) -> Result<EphemeralOverlay, GraphDeltaError> {
    limits.validate()?;
    if !request.beta {
        return Err(GraphDeltaError::BetaOptInRequired);
    }
    validate_root_qualifier(&request.root)?;
    let root = request.root;
    let overlay = match request.proposal {
        ProposalInput::UnifiedDiff(diff) => parse_unified_diff(&root, &diff, limits)?,
        #[cfg(test)]
        ProposalInput::Structured(proposal) => structured_overlay(proposal)?,
        ProposalInput::StructuredJson(json) => {
            enforce_limit("proposal bytes", json.len(), limits.proposal_bytes)?;
            structured_overlay(deserialize_structured_proposal(&json)?)?
        }
    };
    validate_overlay(&overlay, limits)?;
    validate_overlay_root(&overlay, &root)?;
    Ok(canonicalize_overlay(overlay))
}

pub(crate) fn deserialize_structured_proposal(
    json: &str,
) -> Result<StructuredProposal, GraphDeltaError> {
    serde_json::from_str(json)
        .map_err(|error| GraphDeltaError::MalformedStructuredProposal(error.to_string()))
}

fn structured_overlay(proposal: StructuredProposal) -> Result<EphemeralOverlay, GraphDeltaError> {
    if proposal.schema_version != STRUCTURED_PROPOSAL_SCHEMA_VERSION {
        return Err(GraphDeltaError::MalformedStructuredProposal(format!(
            "unsupported schema_version {}; expected {STRUCTURED_PROPOSAL_SCHEMA_VERSION}",
            proposal.schema_version
        )));
    }
    Ok(EphemeralOverlay {
        changed_files: proposal.changed_files,
        edge_additions: proposal.edge_additions,
        edge_removals: proposal.edge_removals,
        relationships: proposal.relationships,
        impacted: proposal.impacted,
        capabilities: proposal.capabilities,
        behavioral_deltas: proposal.behavioral_deltas,
        analogues: proposal.analogues,
        omissions: proposal.omissions,
    })
}

fn parse_unified_diff(
    root: &str,
    diff: &str,
    limits: &GraphDeltaLimits,
) -> Result<EphemeralOverlay, GraphDeltaError> {
    enforce_limit("proposal bytes", diff.len(), limits.proposal_bytes)?;
    validate_root_qualifier(root)?;
    if diff.contains('\0')
        || diff.lines().any(|line| {
            line.starts_with("GIT binary patch")
                || line.starts_with("Binary files ")
                || line.starts_with("diff --cc ")
                || line.starts_with("diff --combined ")
        })
    {
        return Err(GraphDeltaError::UnsupportedProposal(
            "binary and combined diffs are not accepted",
        ));
    }

    let mut file_paths = BTreeSet::new();
    let mut changed_files = Vec::new();
    let mut current_file: Option<PendingFile> = None;
    let mut current_hunk: Option<PendingHunk> = None;
    let mut hunks = 0usize;
    let mut changed = 0usize;
    let mut impacted = BTreeSet::new();

    for (index, line) in diff.lines().enumerate() {
        let line_number = index + 1;
        if let Some(header) = line.strip_prefix("diff --git ") {
            finish_pending_hunk(&mut current_hunk, current_file.as_mut())?;
            finish_pending_file(&mut current_file, &mut changed_files)?;
            let fields = header.split_ascii_whitespace().collect::<Vec<_>>();
            if fields.len() != 2 {
                return Err(GraphDeltaError::MalformedProposal {
                    line: line_number,
                    reason: "diff header must contain exactly two unquoted paths",
                });
            }
            let old = strip_diff_prefix(fields[0], "a/", line_number)?;
            let new = strip_diff_prefix(fields[1], "b/", line_number)?;
            if old != new {
                return Err(GraphDeltaError::UnsupportedProposal(
                    "renames require a structured proposal",
                ));
            }
            validate_relative_path(new, line_number)?;
            if !file_paths.insert(new.to_owned()) {
                return Err(GraphDeltaError::MalformedProposal {
                    line: line_number,
                    reason: "a file may appear only once in a unified proposal",
                });
            }
            enforce_limit("proposal files", file_paths.len(), limits.proposal_files)?;
            current_file = Some(PendingFile {
                root: root.to_owned(),
                path: new.to_owned(),
                header_line: line_number,
                old_marker_seen: false,
                new_marker_seen: false,
                hunks: Vec::new(),
            });
            continue;
        }

        if line.starts_with("rename from ")
            || line.starts_with("rename to ")
            || line.starts_with("similarity index ")
        {
            return Err(GraphDeltaError::UnsupportedProposal(
                "renames require a structured proposal",
            ));
        }
        if line.starts_with("new file mode ") || line.starts_with("deleted file mode ") {
            return Err(GraphDeltaError::UnsupportedProposal(
                "file creation and deletion require a structured proposal",
            ));
        }

        if line.starts_with("@@") {
            finish_pending_hunk(&mut current_hunk, current_file.as_mut())?;
            let file = current_file
                .as_ref()
                .ok_or(GraphDeltaError::MalformedProposal {
                    line: line_number,
                    reason: "hunk appears before a diff header",
                })?;
            if !file.old_marker_seen || !file.new_marker_seen {
                return Err(GraphDeltaError::MalformedProposal {
                    line: line_number,
                    reason: "hunk appears before matching --- and +++ file markers",
                });
            }
            let (old_range, new_range) = parse_hunk_header(line, line_number)?;
            hunks += 1;
            enforce_limit("proposal hunks", hunks, limits.proposal_hunks)?;
            current_hunk = Some(PendingHunk {
                proposal_header_line: line_number,
                old_range,
                new_range,
                old_seen: 0,
                new_seen: 0,
                changed_lines: Vec::new(),
                last_was_body: false,
            });
            continue;
        }

        if let Some(hunk) = current_hunk.as_mut() {
            let file = current_file
                .as_ref()
                .expect("an active hunk always belongs to a file");
            if let Some(text) = line.strip_prefix('+') {
                if hunk.new_seen >= hunk.new_range.count {
                    return Err(GraphDeltaError::MalformedProposal {
                        line: line_number,
                        reason: "hunk contains more new lines than its header declares",
                    });
                }
                changed += 1;
                enforce_limit("changed lines", changed, limits.changed_lines)?;
                let proposal = ProposalLine {
                    root: root.to_owned(),
                    path: file.path.clone(),
                    proposal_line: line_number as u32,
                    old_line: None,
                    new_line: Some(hunk.new_range.start.checked_add(hunk.new_seen).ok_or(
                        GraphDeltaError::MalformedProposal {
                            line: line_number,
                            reason: "new hunk line number overflows",
                        },
                    )?),
                };
                hunk.changed_lines.push(ChangedLineFact {
                    kind: ChangedLineKind::Added,
                    grounding: proposal.clone(),
                    text: text.to_owned(),
                });
                impacted.insert(ImpactEvidence {
                    label: file.path.clone(),
                    kind: ImpactKind::EditableLocus,
                    grounding: EvidenceGrounding::Proposal(proposal),
                });
                hunk.new_seen += 1;
                hunk.last_was_body = true;
            } else if let Some(text) = line.strip_prefix('-') {
                if hunk.old_seen >= hunk.old_range.count {
                    return Err(GraphDeltaError::MalformedProposal {
                        line: line_number,
                        reason: "hunk contains more old lines than its header declares",
                    });
                }
                changed += 1;
                enforce_limit("changed lines", changed, limits.changed_lines)?;
                let proposal = ProposalLine {
                    root: root.to_owned(),
                    path: file.path.clone(),
                    proposal_line: line_number as u32,
                    old_line: Some(hunk.old_range.start.checked_add(hunk.old_seen).ok_or(
                        GraphDeltaError::MalformedProposal {
                            line: line_number,
                            reason: "old hunk line number overflows",
                        },
                    )?),
                    new_line: None,
                };
                hunk.changed_lines.push(ChangedLineFact {
                    kind: ChangedLineKind::Removed,
                    grounding: proposal.clone(),
                    text: text.to_owned(),
                });
                impacted.insert(ImpactEvidence {
                    label: file.path.clone(),
                    kind: ImpactKind::EditableLocus,
                    grounding: EvidenceGrounding::Proposal(proposal),
                });
                hunk.old_seen += 1;
                hunk.last_was_body = true;
            } else if line.strip_prefix(' ').is_some() {
                if hunk.old_seen >= hunk.old_range.count || hunk.new_seen >= hunk.new_range.count {
                    return Err(GraphDeltaError::MalformedProposal {
                        line: line_number,
                        reason: "hunk context exceeds a declared range",
                    });
                }
                hunk.old_seen += 1;
                hunk.new_seen += 1;
                hunk.last_was_body = true;
            } else if line == "\\ No newline at end of file" {
                if !hunk.last_was_body {
                    return Err(GraphDeltaError::MalformedProposal {
                        line: line_number,
                        reason: "no-newline marker must follow a hunk body line",
                    });
                }
                hunk.last_was_body = false;
            } else {
                return Err(GraphDeltaError::MalformedProposal {
                    line: line_number,
                    reason: "unexpected line inside hunk body",
                });
            }
            continue;
        }

        let file = current_file
            .as_mut()
            .ok_or(GraphDeltaError::MalformedProposal {
                line: line_number,
                reason: "content appears before a diff header",
            })?;
        if let Some(marker) = line.strip_prefix("--- ") {
            if file.old_marker_seen || file.new_marker_seen {
                return Err(GraphDeltaError::MalformedProposal {
                    line: line_number,
                    reason: "duplicate or out-of-order --- file marker",
                });
            }
            let marker_path = strip_diff_prefix(marker, "a/", line_number)?;
            if marker_path != file.path {
                return Err(GraphDeltaError::MalformedProposal {
                    line: line_number,
                    reason: "--- marker does not match diff header path",
                });
            }
            file.old_marker_seen = true;
        } else if let Some(marker) = line.strip_prefix("+++ ") {
            if !file.old_marker_seen || file.new_marker_seen {
                return Err(GraphDeltaError::MalformedProposal {
                    line: line_number,
                    reason: "duplicate or out-of-order +++ file marker",
                });
            }
            let marker_path = strip_diff_prefix(marker, "b/", line_number)?;
            if marker_path != file.path {
                return Err(GraphDeltaError::MalformedProposal {
                    line: line_number,
                    reason: "+++ marker does not match diff header path",
                });
            }
            file.new_marker_seen = true;
        } else if line.starts_with("index ")
            || line.starts_with("old mode ")
            || line.starts_with("new mode ")
        {
            if file.old_marker_seen {
                return Err(GraphDeltaError::MalformedProposal {
                    line: line_number,
                    reason: "diff metadata must precede file markers",
                });
            }
        } else {
            return Err(GraphDeltaError::MalformedProposal {
                line: line_number,
                reason: "unexpected content outside a hunk",
            });
        }
    }

    finish_pending_hunk(&mut current_hunk, current_file.as_mut())?;
    finish_pending_file(&mut current_file, &mut changed_files)?;
    if changed_files.is_empty() || hunks == 0 || changed == 0 {
        return Err(GraphDeltaError::UnsupportedProposal(
            "a unified proposal must contain a complete changed hunk",
        ));
    }

    let first_grounding = impacted
        .iter()
        .next()
        .map(|evidence| evidence.grounding.clone());
    let relationships = changed_files
        .iter()
        .flat_map(|file| &file.hunks)
        .flat_map(|hunk| &hunk.changed_lines)
        .flat_map(infer_changed_line_relationships)
        .collect::<Vec<_>>();
    let behavioral_deltas = infer_changed_behavioral_deltas(&changed_files);

    Ok(EphemeralOverlay {
        changed_files,
        edge_additions: Vec::new(),
        edge_removals: Vec::new(),
        relationships,
        impacted: impacted.into_iter().collect(),
        capabilities: vec![
            CapabilityReport {
                capability: GraphDeltaCapability::ProposalParsing,
                state: CapabilityState::Ready,
                detail: "unified diff validated and materialized as changed-line facts".to_owned(),
            },
            CapabilityReport {
                capability: GraphDeltaCapability::LiveGraphInference,
                state: CapabilityState::Degraded,
                detail: "raw changed lines require the service adapter and live GraphState to infer relationships".to_owned(),
            },
        ],
        behavioral_deltas,
        analogues: Vec::new(),
        omissions: vec![GraphDeltaOmission {
            code: GraphDeltaOmissionCode::LiveGraphInferenceDeferred,
            detail: "no graph edges were inferred by the pure unified-diff parser; hydrate these changed-line facts against the live graph".to_owned(),
            hydration_key: first_grounding
                .as_ref()
                .map(EvidenceGrounding::stable_hydration_key),
            grounding: first_grounding,
        }],
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IdentifierToken {
    value: String,
    start: usize,
    end: usize,
}

/// Extract conservative syntax facts without claiming that a textual name is
/// a graph endpoint. Live graph resolution remains mandatory and fail-closed.
pub(crate) fn infer_changed_line_relationships(
    line: &ChangedLineFact,
) -> Vec<ChangedRelationshipFact> {
    let tokens = identifier_tokens(&line.text);
    let mut facts = BTreeSet::new();
    let trimmed = line.text.trim_start();
    let import_like = trimmed.starts_with("use ")
        || trimmed.starts_with("import ")
        || trimmed.starts_with("from ");

    if import_like {
        for token in &tokens {
            if !is_syntax_word(&token.value) {
                facts.insert(relationship_fact(
                    line,
                    InferredRelationshipKind::Reference,
                    None,
                    token.value.clone(),
                ));
            }
        }
    }

    for (index, token) in tokens.iter().enumerate() {
        let called = line.text[token.end..].trim_start().starts_with('(');
        let qualifier = token_qualifier(&line.text, &tokens, index);
        if called && is_registration_operation(&token.value) {
            for (target_index, target) in tokens.iter().enumerate().skip(index + 1).take(8) {
                if !is_syntax_word(&target.value)
                    && !line.text[target.end..].trim_start().starts_with('=')
                {
                    facts.insert(relationship_fact(
                        line,
                        InferredRelationshipKind::Registration,
                        token_qualifier(&line.text, &tokens, target_index),
                        target.value.clone(),
                    ));
                }
            }
        } else if called && !is_syntax_word(&token.value) {
            facts.insert(relationship_fact(
                line,
                InferredRelationshipKind::Call,
                qualifier,
                token.value.clone(),
            ));
        } else if let Some(qualifier) = qualifier
            && !called
        {
            facts.insert(relationship_fact(
                line,
                InferredRelationshipKind::AttributeOrStateReference,
                Some(qualifier),
                token.value.clone(),
            ));
        }
    }
    facts.into_iter().collect()
}

fn relationship_fact(
    line: &ChangedLineFact,
    kind: InferredRelationshipKind,
    qualifier: Option<String>,
    target: String,
) -> ChangedRelationshipFact {
    ChangedRelationshipFact {
        change: line.kind,
        kind,
        qualifier,
        target,
        grounding: line.grounding.clone(),
    }
}

fn identifier_tokens(text: &str) -> Vec<IdentifierToken> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut quote = None;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active) = quote {
            if byte == b'\\' {
                index = index.saturating_add(2);
                continue;
            }
            if byte == active {
                quote = None;
            }
            index += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"' | b'`') {
            quote = Some(byte);
            index += 1;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            break;
        }
        if byte == b'_' || byte.is_ascii_alphabetic() {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index] == b'_' || bytes[index].is_ascii_alphanumeric())
            {
                index += 1;
            }
            tokens.push(IdentifierToken {
                value: text[start..index].to_owned(),
                start,
                end: index,
            });
            continue;
        }
        index += 1;
    }
    tokens
}

fn token_qualifier(text: &str, tokens: &[IdentifierToken], index: usize) -> Option<String> {
    let token = tokens.get(index)?;
    let previous = index.checked_sub(1).and_then(|index| tokens.get(index))?;
    (text[previous.end..token.start].trim() == ".").then(|| previous.value.clone())
}

fn is_registration_operation(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "register"
            | "register_route"
            | "add_route"
            | "add_url_rule"
            | "mount"
            | "include_router"
            | "register_blueprint"
    )
}

fn is_syntax_word(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "as" | "async"
            | "await"
            | "case"
            | "catch"
            | "else"
            | "except"
            | "false"
            | "for"
            | "from"
            | "if"
            | "import"
            | "in"
            | "let"
            | "match"
            | "mod"
            | "none"
            | "null"
            | "raise"
            | "return"
            | "self"
            | "super"
            | "switch"
            | "throw"
            | "true"
            | "use"
            | "while"
    )
}

fn infer_changed_behavioral_deltas(files: &[ChangedFileFact]) -> Vec<BehavioralDelta> {
    let mut deltas = BTreeSet::new();
    for line in files
        .iter()
        .flat_map(|file| &file.hunks)
        .flat_map(|hunk| &hunk.changed_lines)
    {
        let tokens = identifier_tokens(&line.text)
            .into_iter()
            .map(|token| token.value.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        let mut classify = |kind, marker: &str| {
            deltas.insert(BehavioralDelta {
                kind,
                label: format!(
                    "{} proposal line contains `{marker}` behavior",
                    match line.kind {
                        ChangedLineKind::Added => "added",
                        ChangedLineKind::Removed => "removed",
                    }
                ),
                changed_locus: EvidenceGrounding::Proposal(line.grounding.clone()),
                analogue_locus: None,
            });
        };
        if let Some(marker) = first_marker(
            &tokens,
            &["case", "else", "elif", "guard", "if", "match", "switch"],
        ) {
            classify(BehavioralDeltaKind::BranchBehavior, marker);
        }
        if let Some(marker) = first_marker(
            &tokens,
            &[
                "dedupe",
                "deduplicate",
                "merge",
                "reconcile",
                "reconciliation",
                "sync",
                "synchronize",
            ],
        ) {
            classify(BehavioralDeltaKind::Reconciliation, marker);
        }
        if let Some(marker) = first_marker(
            &tokens,
            &[
                "decode",
                "deserialize",
                "encode",
                "format",
                "render",
                "representation",
                "serialize",
            ],
        ) {
            classify(BehavioralDeltaKind::Representation, marker);
        }
        if let Some(marker) = first_marker(
            &tokens,
            &[
                "bail", "catch", "err", "error", "except", "panic", "raise", "throw",
            ],
        ) {
            classify(BehavioralDeltaKind::ErrorPath, marker);
        }
        let has_state_reference = infer_changed_line_relationships(line)
            .iter()
            .any(|fact| fact.kind == InferredRelationshipKind::AttributeOrStateReference);
        if let Some(marker) = first_marker(
            &tokens,
            &["cache", "context", "metadata", "state", "status"],
        ) && (has_state_reference || line.text.contains('='))
        {
            classify(BehavioralDeltaKind::StatePropagation, marker);
        }
    }
    deltas.into_iter().collect()
}

fn first_marker<'a>(tokens: &BTreeSet<String>, markers: &[&'a str]) -> Option<&'a str> {
    markers
        .iter()
        .copied()
        .find(|marker| tokens.contains(*marker))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HunkRange {
    start: u32,
    count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingHunk {
    proposal_header_line: usize,
    old_range: HunkRange,
    new_range: HunkRange,
    old_seen: u32,
    new_seen: u32,
    changed_lines: Vec<ChangedLineFact>,
    last_was_body: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingFile {
    root: String,
    path: String,
    header_line: usize,
    old_marker_seen: bool,
    new_marker_seen: bool,
    hunks: Vec<ChangedHunkFact>,
}

fn finish_pending_hunk(
    pending: &mut Option<PendingHunk>,
    file: Option<&mut PendingFile>,
) -> Result<(), GraphDeltaError> {
    let Some(hunk) = pending.take() else {
        return Ok(());
    };
    if hunk.old_seen != hunk.old_range.count || hunk.new_seen != hunk.new_range.count {
        return Err(GraphDeltaError::MalformedProposal {
            line: hunk.proposal_header_line,
            reason: "hunk body line counts do not match its header",
        });
    }
    if hunk.changed_lines.is_empty() {
        return Err(GraphDeltaError::MalformedProposal {
            line: hunk.proposal_header_line,
            reason: "hunk contains no added or removed lines",
        });
    }
    let file = file.ok_or(GraphDeltaError::MalformedProposal {
        line: hunk.proposal_header_line,
        reason: "hunk has no owning file",
    })?;
    file.hunks.push(ChangedHunkFact {
        proposal_header_line: hunk.proposal_header_line as u32,
        old_start: hunk.old_range.start,
        old_count: hunk.old_range.count,
        new_start: hunk.new_range.start,
        new_count: hunk.new_range.count,
        changed_lines: hunk.changed_lines,
    });
    Ok(())
}

fn finish_pending_file(
    pending: &mut Option<PendingFile>,
    output: &mut Vec<ChangedFileFact>,
) -> Result<(), GraphDeltaError> {
    let Some(file) = pending.take() else {
        return Ok(());
    };
    if !file.old_marker_seen || !file.new_marker_seen || file.hunks.is_empty() {
        return Err(GraphDeltaError::MalformedProposal {
            line: file.header_line,
            reason: "file diff is missing markers or complete hunks",
        });
    }
    output.push(ChangedFileFact {
        root: file.root,
        path: file.path,
        hunks: file.hunks,
    });
    Ok(())
}

fn strip_diff_prefix<'a>(
    path: &'a str,
    prefix: &str,
    line: usize,
) -> Result<&'a str, GraphDeltaError> {
    path.strip_prefix(prefix)
        .ok_or(GraphDeltaError::MalformedProposal {
            line,
            reason: "diff paths must use a/ and b/ prefixes",
        })
}

fn parse_hunk_header(
    line: &str,
    line_number: usize,
) -> Result<(HunkRange, HunkRange), GraphDeltaError> {
    let end =
        line[2..]
            .find("@@")
            .map(|index| index + 2)
            .ok_or(GraphDeltaError::MalformedProposal {
                line: line_number,
                reason: "unterminated hunk header",
            })?;
    let ranges = line[2..end].split_ascii_whitespace().collect::<Vec<_>>();
    if ranges.len() != 2 {
        return Err(GraphDeltaError::MalformedProposal {
            line: line_number,
            reason: "hunk header must contain old and new ranges",
        });
    }
    Ok((
        parse_hunk_range(ranges[0], '-', line_number)?,
        parse_hunk_range(ranges[1], '+', line_number)?,
    ))
}

fn parse_hunk_range(range: &str, prefix: char, line: usize) -> Result<HunkRange, GraphDeltaError> {
    let raw = range
        .strip_prefix(prefix)
        .ok_or(GraphDeltaError::MalformedProposal {
            line,
            reason: "invalid hunk range prefix",
        })?;
    let fields = raw.split(',').collect::<Vec<_>>();
    if fields.is_empty() || fields.len() > 2 {
        return Err(GraphDeltaError::MalformedProposal {
            line,
            reason: "invalid hunk range",
        });
    }
    let start = fields[0]
        .parse::<u32>()
        .map_err(|_| GraphDeltaError::MalformedProposal {
            line,
            reason: "invalid hunk range start",
        })?;
    let count = match fields.get(1) {
        Some(value) => value
            .parse::<u32>()
            .map_err(|_| GraphDeltaError::MalformedProposal {
                line,
                reason: "invalid hunk range count",
            })?,
        None => 1,
    };
    if (start == 0 && count != 0) || (count > 0 && start.checked_add(count - 1).is_none()) {
        return Err(GraphDeltaError::MalformedProposal {
            line,
            reason: "hunk range is inconsistent or overflows",
        });
    }
    Ok(HunkRange { start, count })
}

fn validate_relative_path(path: &str, line: usize) -> Result<(), GraphDeltaError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('~')
        || path.contains('\\')
        || path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "..")
    {
        return Err(GraphDeltaError::UnsafePath {
            line,
            path: path.to_owned(),
        });
    }
    Ok(())
}

fn validate_root_qualifier(root: &str) -> Result<(), GraphDeltaError> {
    if root.trim().is_empty()
        || root.len() > 1_024
        || root.contains('\0')
        || root.chars().any(char::is_control)
    {
        return Err(GraphDeltaError::InvalidGraph(
            "root qualifier must be non-empty, bounded, and printable".to_owned(),
        ));
    }
    Ok(())
}

fn encode_key_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

fn enforce_limit(
    resource: &'static str,
    actual: usize,
    limit: usize,
) -> Result<(), GraphDeltaError> {
    if actual > limit {
        return Err(GraphDeltaError::LimitExceeded {
            resource,
            actual,
            limit,
        });
    }
    Ok(())
}

fn validate_overlay(
    overlay: &EphemeralOverlay,
    limits: &GraphDeltaLimits,
) -> Result<(), GraphDeltaError> {
    enforce_limit(
        "proposal files",
        overlay.changed_files.len(),
        limits.proposal_files,
    )?;
    let hunk_count = overlay
        .changed_files
        .iter()
        .map(|file| file.hunks.len())
        .sum::<usize>();
    enforce_limit("proposal hunks", hunk_count, limits.proposal_hunks)?;
    let changed_line_count = overlay
        .changed_files
        .iter()
        .flat_map(|file| &file.hunks)
        .map(|hunk| hunk.changed_lines.len())
        .sum::<usize>();
    enforce_limit("changed lines", changed_line_count, limits.changed_lines)?;
    let changed_text_bytes = overlay
        .changed_files
        .iter()
        .flat_map(|file| &file.hunks)
        .flat_map(|hunk| &hunk.changed_lines)
        .map(|line| line.text.len())
        .sum::<usize>();
    enforce_limit("proposal bytes", changed_text_bytes, limits.proposal_bytes)?;
    for file in &overlay.changed_files {
        validate_root_qualifier(&file.root)?;
        validate_relative_path(&file.path, 0)?;
        if file.hunks.is_empty() {
            return Err(GraphDeltaError::InvalidGraph(
                "changed file facts must contain at least one hunk".to_owned(),
            ));
        }
        for hunk in &file.hunks {
            if hunk.proposal_header_line == 0 || hunk.changed_lines.is_empty() {
                return Err(GraphDeltaError::InvalidGraph(
                    "changed hunk facts require a header line and changed lines".to_owned(),
                ));
            }
            if (hunk.old_start == 0 && hunk.old_count != 0)
                || (hunk.new_start == 0 && hunk.new_count != 0)
            {
                return Err(GraphDeltaError::InvalidGraph(
                    "changed hunk ranges are inconsistent".to_owned(),
                ));
            }
            let added_count = hunk
                .changed_lines
                .iter()
                .filter(|line| line.kind == ChangedLineKind::Added)
                .count();
            let removed_count = hunk.changed_lines.len() - added_count;
            if added_count > hunk.new_count as usize || removed_count > hunk.old_count as usize {
                return Err(GraphDeltaError::InvalidGraph(
                    "changed-line facts exceed their declared hunk ranges".to_owned(),
                ));
            }
            for line in &hunk.changed_lines {
                EvidenceGrounding::Proposal(line.grounding.clone()).validate()?;
                if line.grounding.root != file.root || line.grounding.path != file.path {
                    return Err(GraphDeltaError::InvalidGraph(
                        "changed-line grounding must match its owning root and file".to_owned(),
                    ));
                }
                let shape_is_valid = match line.kind {
                    ChangedLineKind::Added => {
                        line.grounding.old_line.is_none() && line.grounding.new_line.is_some()
                    }
                    ChangedLineKind::Removed => {
                        line.grounding.old_line.is_some() && line.grounding.new_line.is_none()
                    }
                };
                if !shape_is_valid {
                    return Err(GraphDeltaError::InvalidGraph(
                        "changed-line kind disagrees with old/new line grounding".to_owned(),
                    ));
                }
                let coordinate_is_valid = match line.kind {
                    ChangedLineKind::Added => line.grounding.new_line.is_some_and(|line| {
                        line_in_declared_range(line, hunk.new_start, hunk.new_count)
                    }),
                    ChangedLineKind::Removed => line.grounding.old_line.is_some_and(|line| {
                        line_in_declared_range(line, hunk.old_start, hunk.old_count)
                    }),
                };
                if !coordinate_is_valid {
                    return Err(GraphDeltaError::InvalidGraph(
                        "changed-line coordinate is outside its declared hunk range".to_owned(),
                    ));
                }
            }
        }
    }
    enforce_limit(
        "overlay edges",
        overlay.edge_additions.len() + overlay.edge_removals.len(),
        limits.overlay_edges,
    )?;
    for edge in &overlay.edge_additions {
        validate_edge(edge)?;
        if !matches!(edge.grounding, EvidenceGrounding::Proposal(_)) {
            return Err(GraphDeltaError::InvalidGraph(
                "overlay edge additions must be grounded in proposal lines".to_owned(),
            ));
        }
    }
    for key in &overlay.edge_removals {
        validate_edge_key(key)?;
    }
    enforce_limit(
        "relationship facts",
        overlay.relationships.len(),
        limits.changed_lines.saturating_mul(16),
    )?;
    for relationship in &overlay.relationships {
        EvidenceGrounding::Proposal(relationship.grounding.clone()).validate()?;
        if relationship.target.trim().is_empty()
            || relationship.target.len() > 1_024
            || relationship.target.chars().any(char::is_control)
            || relationship.qualifier.as_ref().is_some_and(|qualifier| {
                qualifier.trim().is_empty()
                    || qualifier.len() > 1_024
                    || qualifier.chars().any(char::is_control)
            })
        {
            return Err(GraphDeltaError::InvalidGraph(
                "relationship facts require bounded printable targets".to_owned(),
            ));
        }
    }
    for evidence in &overlay.impacted {
        if evidence.label.trim().is_empty() {
            return Err(GraphDeltaError::InvalidGraph(
                "impact evidence label must not be empty".to_owned(),
            ));
        }
        evidence.grounding.validate()?;
    }
    let mut capability_states = BTreeSet::new();
    for capability in &overlay.capabilities {
        if capability.detail.trim().is_empty() {
            return Err(GraphDeltaError::InvalidGraph(
                "capability reports require a detail".to_owned(),
            ));
        }
        if !capability_states.insert(capability.capability) {
            return Err(GraphDeltaError::InvalidGraph(
                "one capability may be reported only once".to_owned(),
            ));
        }
    }
    for delta in &overlay.behavioral_deltas {
        if delta.label.trim().is_empty() {
            return Err(GraphDeltaError::InvalidGraph(
                "behavioral deltas require a label".to_owned(),
            ));
        }
        delta.changed_locus.validate()?;
        if let Some(analogue) = &delta.analogue_locus {
            analogue.validate()?;
        }
    }
    for analogue in &overlay.analogues {
        if analogue.label.trim().is_empty() || analogue.similarity_basis.trim().is_empty() {
            return Err(GraphDeltaError::InvalidGraph(
                "behavioral analogues require labels and a similarity basis".to_owned(),
            ));
        }
        analogue.changed_locus.grounding.validate()?;
        analogue.analogue_locus.grounding.validate()?;
    }
    for omission in &overlay.omissions {
        if omission.detail.trim().is_empty() {
            return Err(GraphDeltaError::InvalidGraph(
                "graph-delta omissions require a detail".to_owned(),
            ));
        }
        if let Some(grounding) = &omission.grounding {
            grounding.validate()?;
            if omission
                .hydration_key
                .as_ref()
                .is_some_and(|key| key != &grounding.stable_hydration_key())
            {
                return Err(GraphDeltaError::InvalidGraph(
                    "omission hydration key does not match its grounding".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn line_in_declared_range(line: u32, start: u32, count: u32) -> bool {
    count > 0
        && line >= start
        && start
            .checked_add(count)
            .is_some_and(|exclusive_end| line < exclusive_end)
}

fn canonicalize_overlay(mut overlay: EphemeralOverlay) -> EphemeralOverlay {
    for file in &mut overlay.changed_files {
        for hunk in &mut file.hunks {
            hunk.changed_lines.sort_by(|left, right| {
                left.grounding
                    .proposal_line
                    .cmp(&right.grounding.proposal_line)
                    .then_with(|| left.kind.cmp(&right.kind))
                    .then_with(|| left.text.cmp(&right.text))
            });
            hunk.changed_lines.dedup();
        }
        file.hunks.sort();
        file.hunks.dedup();
    }
    overlay.changed_files.sort();
    overlay.changed_files.dedup();
    overlay
        .edge_additions
        .sort_by(|left, right| left.key.cmp(&right.key));
    overlay.edge_additions.dedup_by(|left, right| left == right);
    overlay.edge_removals.sort();
    overlay.edge_removals.dedup();
    overlay.relationships.sort();
    overlay.relationships.dedup();
    overlay.impacted.sort();
    overlay.impacted.dedup();
    overlay.capabilities.sort();
    overlay.capabilities.dedup();
    overlay.behavioral_deltas.sort();
    overlay.behavioral_deltas.dedup();
    overlay.analogues.sort();
    overlay.analogues.dedup();
    for omission in &mut overlay.omissions {
        if omission.hydration_key.is_none() {
            omission.hydration_key = omission
                .grounding
                .as_ref()
                .map(EvidenceGrounding::stable_hydration_key);
        }
    }
    overlay.omissions.sort();
    overlay.omissions.dedup();
    overlay
}

fn validate_overlay_root(
    overlay: &EphemeralOverlay,
    expected_root: &str,
) -> Result<(), GraphDeltaError> {
    let mut proposal_groundings = Vec::new();
    proposal_groundings.extend(overlay.edge_additions.iter().map(|edge| &edge.grounding));
    proposal_groundings.extend(overlay.impacted.iter().map(|impact| &impact.grounding));
    proposal_groundings.extend(
        overlay
            .behavioral_deltas
            .iter()
            .map(|delta| &delta.changed_locus),
    );
    proposal_groundings.extend(
        overlay
            .behavioral_deltas
            .iter()
            .filter_map(|delta| delta.analogue_locus.as_ref()),
    );
    proposal_groundings.extend(overlay.analogues.iter().flat_map(|analogue| {
        [
            &analogue.changed_locus.grounding,
            &analogue.analogue_locus.grounding,
        ]
    }));
    proposal_groundings.extend(
        overlay
            .omissions
            .iter()
            .filter_map(|omission| omission.grounding.as_ref()),
    );

    if overlay
        .changed_files
        .iter()
        .any(|file| file.root != expected_root)
        || overlay
            .relationships
            .iter()
            .any(|relationship| relationship.grounding.root != expected_root)
        || proposal_groundings.iter().any(|grounding| {
            matches!(grounding, EvidenceGrounding::Proposal(line) if line.root != expected_root)
        })
    {
        return Err(GraphDeltaError::InvalidGraph(
            "proposal grounding root does not match the requested workspace root".to_owned(),
        ));
    }
    Ok(())
}

fn validate_edge_key(key: &EdgeKey) -> Result<(), GraphDeltaError> {
    if key.from.trim().is_empty() || key.to.trim().is_empty() || key.kind.trim().is_empty() {
        return Err(GraphDeltaError::InvalidGraph(
            "edge identifiers and kind must not be empty".to_owned(),
        ));
    }
    Ok(())
}

fn validate_edge(edge: &WeightedEdge) -> Result<(), GraphDeltaError> {
    validate_edge_key(&edge.key)?;
    if edge.cost == 0 || edge.cost > HARD_MAX_EDGE_COST {
        return Err(GraphDeltaError::InvalidGraph(format!(
            "edge {:?} cost must be within 1..={HARD_MAX_EDGE_COST}",
            edge.key
        )));
    }
    edge.grounding.validate()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct EndpointPair {
    pub(crate) from: String,
    pub(crate) to: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroundedRouteStep {
    pub(crate) edge: EdgeKey,
    pub(crate) cost: u32,
    pub(crate) priority: u32,
    pub(crate) registration_order: u32,
    pub(crate) grounding: EvidenceGrounding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WeightedPath {
    pub(crate) total_cost: u64,
    pub(crate) nodes: Vec<String>,
    pub(crate) edges: Vec<EdgeKey>,
    pub(crate) grounded_steps: Vec<GroundedRouteStep>,
    pub(crate) priority_registration_key: Vec<(u32, u32, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathSet {
    /// All equal-minimum-cost alternatives, deterministically ordered. The
    /// first path is preferred by priority then registration order.
    pub(crate) alternatives: Vec<WeightedPath>,
}

impl PathSet {
    fn cost(&self) -> Option<u64> {
        self.alternatives.first().map(|path| path.total_cost)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RouteChange {
    Unchanged,
    AddedReachability,
    RemovedReachability,
    Shortened,
    Lengthened,
    EqualCostAlternativesChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteDelta {
    pub(crate) endpoints: EndpointPair,
    pub(crate) change: RouteChange,
    pub(crate) before: PathSet,
    pub(crate) after: PathSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum EdgeChange {
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangedEdge {
    pub(crate) change: EdgeChange,
    pub(crate) edge: WeightedEdge,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct HydrationTarget {
    pub(crate) stable_key: String,
    pub(crate) grounding: EvidenceGrounding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ChecklistRequirement {
    EditOrEvidenceBackedNoEdit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AffectedLocusChecklistItem {
    pub(crate) stable_id: String,
    pub(crate) label: String,
    pub(crate) kinds: BTreeSet<ImpactKind>,
    pub(crate) grounding: EvidenceGrounding,
    pub(crate) hydration: HydrationTarget,
    pub(crate) requirement: ChecklistRequirement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraphDeltaCard {
    pub(crate) beta: bool,
    pub(crate) capabilities: Vec<CapabilityReport>,
    pub(crate) changed_files: Vec<ChangedFileFact>,
    pub(crate) routes: Vec<RouteDelta>,
    pub(crate) changed_edges: Vec<ChangedEdge>,
    pub(crate) impacted_loci: Vec<ImpactEvidence>,
    pub(crate) impacted_tests: Vec<ImpactEvidence>,
    pub(crate) impacted_state_or_api: Vec<ImpactEvidence>,
    pub(crate) bypassed_loci: Vec<GraphNode>,
    pub(crate) behavioral_deltas: Vec<BehavioralDelta>,
    pub(crate) behavioral_analogues: Vec<BehavioralAnalogue>,
    pub(crate) affected_locus_checklist: Vec<AffectedLocusChecklistItem>,
    pub(crate) omissions: Vec<GraphDeltaOmission>,
}

pub(crate) fn analyze_graph_delta(
    base: &GraphSnapshot,
    overlay: &EphemeralOverlay,
    endpoint_pairs: &[EndpointPair],
    limits: &GraphDeltaLimits,
) -> Result<GraphDeltaCard, GraphDeltaError> {
    limits.validate()?;
    validate_overlay(overlay, limits)?;
    let overlay = canonicalize_overlay(overlay.clone());
    enforce_limit(
        "endpoint pairs",
        endpoint_pairs.len(),
        limits.endpoint_pairs,
    )?;

    let (nodes, before_edges) = canonical_graph(base)?;
    let mut after_edges = before_edges.clone();
    let mut changed_edges = Vec::new();

    for key in &overlay.edge_removals {
        let removed = after_edges.remove(key).ok_or_else(|| {
            GraphDeltaError::InvalidGraph(format!(
                "proposal removes absent edge {} -> {} ({})",
                key.from, key.to, key.kind
            ))
        })?;
        changed_edges.push(ChangedEdge {
            change: EdgeChange::Removed,
            edge: removed,
        });
    }
    for edge in &overlay.edge_additions {
        if !nodes.contains_key(&edge.key.from) || !nodes.contains_key(&edge.key.to) {
            return Err(GraphDeltaError::InvalidGraph(format!(
                "proposal edge references unknown endpoint: {:?}",
                edge.key
            )));
        }
        match after_edges.get(&edge.key) {
            Some(existing) if existing != edge => {
                return Err(GraphDeltaError::InvalidGraph(format!(
                    "proposal conflicts with existing edge {:?}",
                    edge.key
                )));
            }
            Some(_) => {}
            None => {
                after_edges.insert(edge.key.clone(), edge.clone());
                changed_edges.push(ChangedEdge {
                    change: EdgeChange::Added,
                    edge: edge.clone(),
                });
            }
        }
    }
    changed_edges.sort_by(|left, right| {
        left.edge
            .key
            .cmp(&right.edge.key)
            .then_with(|| left.change.cmp(&right.change))
    });

    let mut pairs = endpoint_pairs.to_vec();
    pairs.sort();
    pairs.dedup();
    let mut routes = Vec::new();
    let mut before_route_nodes = BTreeSet::new();
    let mut after_route_nodes = BTreeSet::new();
    for endpoints in pairs {
        if !nodes.contains_key(&endpoints.from) || !nodes.contains_key(&endpoints.to) {
            return Err(GraphDeltaError::InvalidGraph(format!(
                "path endpoint is absent: {} -> {}",
                endpoints.from, endpoints.to
            )));
        }
        let before = shortest_paths(&nodes, &before_edges, &endpoints, limits)?;
        let after = shortest_paths(&nodes, &after_edges, &endpoints, limits)?;
        for path in &before.alternatives {
            before_route_nodes.extend(path.nodes.iter().cloned());
        }
        for path in &after.alternatives {
            after_route_nodes.extend(path.nodes.iter().cloned());
        }
        let change = classify_route_change(&before, &after);
        routes.push(RouteDelta {
            endpoints,
            change,
            before,
            after,
        });
    }

    let mut impacted = overlay.impacted.clone();
    impacted.extend(changed_edges.iter().filter_map(|changed| {
        nodes
            .get(&changed.edge.key.to)
            .map(|target| ImpactEvidence {
                label: target.id.clone(),
                kind: target.kind,
                grounding: target.grounding.clone(),
            })
    }));
    impacted.extend(nearest_route_tests(
        &nodes,
        &before_edges,
        &after_edges,
        &changed_edges,
        &routes,
        limits,
    )?);
    impacted.sort();
    impacted.dedup();
    let impacted_tests = impacted
        .iter()
        .filter(|evidence| evidence.kind == ImpactKind::Test)
        .cloned()
        .collect();
    let impacted_state_or_api = impacted
        .iter()
        .filter(|evidence| evidence.kind == ImpactKind::StateOrApi)
        .cloned()
        .collect();
    let bypassed_loci = before_route_nodes
        .difference(&after_route_nodes)
        .filter_map(|id| nodes.get(id).cloned())
        .collect::<Vec<_>>();

    let mut behavioral_deltas = overlay.behavioral_deltas.clone();
    behavioral_deltas.extend(bypassed_loci.iter().map(|node| BehavioralDelta {
        kind: BehavioralDeltaKind::BypassedCall,
        label: format!("route no longer traverses {}", node.id),
        changed_locus: node.grounding.clone(),
        analogue_locus: None,
    }));
    behavioral_deltas.sort();
    behavioral_deltas.dedup();

    let mut capabilities = overlay.capabilities.clone();
    ensure_capability(
        &mut capabilities,
        GraphDeltaCapability::ProposalParsing,
        CapabilityState::Ready,
        "proposal overlay is validated and canonical",
    );
    ensure_capability(
        &mut capabilities,
        GraphDeltaCapability::RouteAnalysis,
        CapabilityState::Ready,
        "bounded equal-cost route analysis completed",
    );
    ensure_capability(
        &mut capabilities,
        GraphDeltaCapability::LiveGraphInference,
        CapabilityState::Degraded,
        "the overlay did not declare complete live-graph relationship inference",
    );
    ensure_capability(
        &mut capabilities,
        GraphDeltaCapability::ImpactTraversal,
        CapabilityState::Degraded,
        "the overlay did not declare complete transitive impact traversal",
    );
    ensure_capability(
        &mut capabilities,
        GraphDeltaCapability::BehavioralAnalogueDiscovery,
        if overlay.analogues.is_empty() {
            CapabilityState::Degraded
        } else {
            CapabilityState::Ready
        },
        if overlay.analogues.is_empty() {
            "the overlay supplied no grounded behavioral analogues"
        } else {
            "grounded behavioral analogues were supplied by the service adapter"
        },
    );
    capabilities.sort();
    capabilities.dedup();

    let affected_locus_checklist =
        build_affected_locus_checklist(&impacted, &bypassed_loci, &behavioral_deltas);
    let mut omissions = overlay.omissions.clone();
    for capability in capabilities
        .iter()
        .filter(|capability| capability.state != CapabilityState::Ready)
    {
        omissions.push(GraphDeltaOmission {
            code: GraphDeltaOmissionCode::CapabilityDegraded,
            detail: format!("{:?}: {}", capability.capability, capability.detail),
            grounding: None,
            hydration_key: None,
        });
    }
    omissions.sort();
    omissions.dedup();

    Ok(GraphDeltaCard {
        beta: true,
        capabilities,
        changed_files: overlay.changed_files.clone(),
        routes,
        changed_edges,
        impacted_loci: impacted,
        impacted_tests,
        impacted_state_or_api,
        bypassed_loci,
        behavioral_deltas,
        behavioral_analogues: overlay.analogues.clone(),
        affected_locus_checklist,
        omissions,
    })
}

fn ensure_capability(
    capabilities: &mut Vec<CapabilityReport>,
    capability: GraphDeltaCapability,
    state: CapabilityState,
    detail: &str,
) {
    if capabilities
        .iter()
        .all(|report| report.capability != capability)
    {
        capabilities.push(CapabilityReport {
            capability,
            state,
            detail: detail.to_owned(),
        });
    }
}

fn build_affected_locus_checklist(
    impacted: &[ImpactEvidence],
    bypassed: &[GraphNode],
    behavioral_deltas: &[BehavioralDelta],
) -> Vec<AffectedLocusChecklistItem> {
    let mut entries = BTreeMap::<String, AffectedLocusChecklistItem>::new();
    let mut add = |label: String, kind: ImpactKind, grounding: EvidenceGrounding| {
        let stable_id = grounding.stable_hydration_key();
        let entry =
            entries
                .entry(stable_id.clone())
                .or_insert_with(|| AffectedLocusChecklistItem {
                    stable_id: stable_id.clone(),
                    label: label.clone(),
                    kinds: BTreeSet::new(),
                    grounding: grounding.clone(),
                    hydration: HydrationTarget {
                        stable_key: stable_id,
                        grounding,
                    },
                    requirement: ChecklistRequirement::EditOrEvidenceBackedNoEdit,
                });
        entry.kinds.insert(kind);
        if label < entry.label {
            entry.label = label;
        }
    };

    for evidence in impacted {
        add(
            evidence.label.clone(),
            evidence.kind,
            evidence.grounding.clone(),
        );
    }
    for node in bypassed {
        add(
            format!("bypassed {}", node.id),
            ImpactKind::Caller,
            node.grounding.clone(),
        );
    }
    for delta in behavioral_deltas {
        add(
            delta.label.clone(),
            ImpactKind::Caller,
            delta.changed_locus.clone(),
        );
    }
    entries.into_values().collect()
}

type CanonicalGraph = (BTreeMap<String, GraphNode>, BTreeMap<EdgeKey, WeightedEdge>);

fn canonical_graph(graph: &GraphSnapshot) -> Result<CanonicalGraph, GraphDeltaError> {
    let mut nodes = BTreeMap::new();
    for node in &graph.nodes {
        if node.id.trim().is_empty() {
            return Err(GraphDeltaError::InvalidGraph(
                "node id must not be empty".to_owned(),
            ));
        }
        node.grounding.validate()?;
        match nodes.insert(node.id.clone(), node.clone()) {
            Some(existing) if existing != *node => {
                return Err(GraphDeltaError::InvalidGraph(format!(
                    "conflicting duplicate node {:?}",
                    node.id
                )));
            }
            _ => {}
        }
    }
    let mut edges = BTreeMap::new();
    for edge in &graph.edges {
        validate_edge(edge)?;
        if !nodes.contains_key(&edge.key.from) || !nodes.contains_key(&edge.key.to) {
            return Err(GraphDeltaError::InvalidGraph(format!(
                "edge references unknown node: {:?}",
                edge.key
            )));
        }
        match edges.insert(edge.key.clone(), edge.clone()) {
            Some(existing) if existing != *edge => {
                return Err(GraphDeltaError::InvalidGraph(format!(
                    "conflicting duplicate edge {:?}",
                    edge.key
                )));
            }
            _ => {}
        }
    }
    Ok((nodes, edges))
}

fn shortest_paths(
    _nodes: &BTreeMap<String, GraphNode>,
    edges: &BTreeMap<EdgeKey, WeightedEdge>,
    endpoints: &EndpointPair,
    limits: &GraphDeltaLimits,
) -> Result<PathSet, GraphDeltaError> {
    if endpoints.from == endpoints.to {
        return Ok(PathSet {
            alternatives: vec![WeightedPath {
                total_cost: 0,
                nodes: vec![endpoints.from.clone()],
                edges: Vec::new(),
                grounded_steps: Vec::new(),
                priority_registration_key: Vec::new(),
            }],
        });
    }

    let mut adjacency = BTreeMap::<String, Vec<&WeightedEdge>>::new();
    for edge in edges.values() {
        adjacency
            .entry(edge.key.from.clone())
            .or_default()
            .push(edge);
    }
    for outgoing in adjacency.values_mut() {
        outgoing.sort_by(|left, right| {
            left.key
                .to
                .cmp(&right.key.to)
                .then_with(|| left.key.kind.cmp(&right.key.kind))
                .then_with(|| left.priority.cmp(&right.priority))
                .then_with(|| left.registration_order.cmp(&right.registration_order))
        });
    }

    let mut distances = BTreeMap::<String, u64>::new();
    let mut predecessors = BTreeMap::<String, Vec<EdgeKey>>::new();
    let mut queue = BinaryHeap::new();
    distances.insert(endpoints.from.clone(), 0);
    queue.push(Reverse((0u64, endpoints.from.clone())));
    let mut visited = 0usize;

    while let Some(Reverse((distance, node))) = queue.pop() {
        if distances.get(&node).copied() != Some(distance) {
            continue;
        }
        visited += 1;
        enforce_limit("visited nodes", visited, limits.visited_nodes)?;
        if let Some(target_distance) = distances.get(&endpoints.to)
            && distance > *target_distance
        {
            break;
        }
        for edge in adjacency.get(&node).into_iter().flatten() {
            let candidate = distance + u64::from(edge.cost);
            match distances.get(&edge.key.to).copied() {
                None => {
                    distances.insert(edge.key.to.clone(), candidate);
                    predecessors.insert(edge.key.to.clone(), vec![edge.key.clone()]);
                    queue.push(Reverse((candidate, edge.key.to.clone())));
                }
                Some(existing) if candidate < existing => {
                    distances.insert(edge.key.to.clone(), candidate);
                    predecessors.insert(edge.key.to.clone(), vec![edge.key.clone()]);
                    queue.push(Reverse((candidate, edge.key.to.clone())));
                }
                Some(existing) if candidate == existing => {
                    predecessors
                        .entry(edge.key.to.clone())
                        .or_default()
                        .push(edge.key.clone());
                }
                _ => {}
            }
        }
    }

    let Some(total_cost) = distances.get(&endpoints.to).copied() else {
        return Ok(PathSet {
            alternatives: Vec::new(),
        });
    };
    for incoming in predecessors.values_mut() {
        incoming.sort();
        incoming.dedup();
    }

    let mut reversed_edges = Vec::new();
    let mut alternatives = Vec::new();
    collect_shortest_paths(
        &endpoints.to,
        &endpoints.from,
        total_cost,
        &predecessors,
        edges,
        limits,
        &mut reversed_edges,
        &mut alternatives,
    )?;
    alternatives.sort_by(|left, right| {
        left.priority_registration_key
            .cmp(&right.priority_registration_key)
            .then_with(|| left.nodes.cmp(&right.nodes))
            .then_with(|| left.edges.cmp(&right.edges))
    });
    Ok(PathSet { alternatives })
}

#[allow(clippy::too_many_arguments)]
fn collect_shortest_paths(
    current: &str,
    source: &str,
    total_cost: u64,
    predecessors: &BTreeMap<String, Vec<EdgeKey>>,
    edge_map: &BTreeMap<EdgeKey, WeightedEdge>,
    limits: &GraphDeltaLimits,
    reversed_edges: &mut Vec<EdgeKey>,
    output: &mut Vec<WeightedPath>,
) -> Result<(), GraphDeltaError> {
    if reversed_edges.len() > limits.path_hops {
        return Err(GraphDeltaError::LimitExceeded {
            resource: "path hops",
            actual: reversed_edges.len(),
            limit: limits.path_hops,
        });
    }
    if current == source {
        if output.len() >= limits.equal_paths {
            return Err(GraphDeltaError::LimitExceeded {
                resource: "equal-cost paths",
                actual: output.len() + 1,
                limit: limits.equal_paths,
            });
        }
        let path_edges = reversed_edges.iter().rev().cloned().collect::<Vec<_>>();
        let mut path_nodes = vec![source.to_owned()];
        let mut tie_break = Vec::new();
        let mut grounded_steps = Vec::new();
        for key in &path_edges {
            let edge = edge_map.get(key).ok_or_else(|| {
                GraphDeltaError::InvalidGraph("shortest-path edge disappeared".to_owned())
            })?;
            path_nodes.push(key.to.clone());
            tie_break.push((
                edge.priority,
                edge.registration_order,
                format!("{}:{}:{}", key.from, key.kind, key.to),
            ));
            grounded_steps.push(GroundedRouteStep {
                edge: key.clone(),
                cost: edge.cost,
                priority: edge.priority,
                registration_order: edge.registration_order,
                grounding: edge.grounding.clone(),
            });
        }
        output.push(WeightedPath {
            total_cost,
            nodes: path_nodes,
            edges: path_edges,
            grounded_steps,
            priority_registration_key: tie_break,
        });
        return Ok(());
    }

    for edge in predecessors.get(current).into_iter().flatten() {
        reversed_edges.push(edge.clone());
        collect_shortest_paths(
            &edge.from,
            source,
            total_cost,
            predecessors,
            edge_map,
            limits,
            reversed_edges,
            output,
        )?;
        reversed_edges.pop();
    }
    Ok(())
}

fn classify_route_change(before: &PathSet, after: &PathSet) -> RouteChange {
    match (before.cost(), after.cost()) {
        (None, None) => RouteChange::Unchanged,
        (None, Some(_)) => RouteChange::AddedReachability,
        (Some(_), None) => RouteChange::RemovedReachability,
        (Some(before_cost), Some(after_cost)) if after_cost < before_cost => RouteChange::Shortened,
        (Some(before_cost), Some(after_cost)) if after_cost > before_cost => {
            RouteChange::Lengthened
        }
        (Some(_), Some(_)) if before.alternatives != after.alternatives => {
            RouteChange::EqualCostAlternativesChanged
        }
        _ => RouteChange::Unchanged,
    }
}

fn nearest_route_tests(
    nodes: &BTreeMap<String, GraphNode>,
    before_edges: &BTreeMap<EdgeKey, WeightedEdge>,
    after_edges: &BTreeMap<EdgeKey, WeightedEdge>,
    changed_edges: &[ChangedEdge],
    routes: &[RouteDelta],
    limits: &GraphDeltaLimits,
) -> Result<Vec<ImpactEvidence>, GraphDeltaError> {
    let mut seeds = BTreeSet::new();
    for changed in changed_edges {
        seeds.insert(changed.edge.key.from.clone());
        seeds.insert(changed.edge.key.to.clone());
    }
    for route in routes
        .iter()
        .filter(|route| route.change != RouteChange::Unchanged)
    {
        seeds.insert(route.endpoints.from.clone());
        seeds.insert(route.endpoints.to.clone());
    }
    if seeds.is_empty() {
        return Ok(Vec::new());
    }

    let mut adjacency = BTreeMap::<String, BTreeSet<String>>::new();
    for edge in before_edges.values().chain(after_edges.values()) {
        adjacency
            .entry(edge.key.from.clone())
            .or_default()
            .insert(edge.key.to.clone());
        adjacency
            .entry(edge.key.to.clone())
            .or_default()
            .insert(edge.key.from.clone());
    }
    let mut queue = seeds
        .iter()
        .cloned()
        .map(|seed| (seed, 0usize))
        .collect::<VecDeque<_>>();
    let mut visited = seeds;
    let mut nearest_distance = None;
    let mut tests = BTreeSet::new();
    while let Some((node_id, distance)) = queue.pop_front() {
        enforce_limit("visited nodes", visited.len(), limits.visited_nodes)?;
        if nearest_distance.is_some_and(|nearest| distance > nearest) {
            break;
        }
        if let Some(node) = nodes.get(&node_id)
            && node.kind == ImpactKind::Test
        {
            nearest_distance.get_or_insert(distance);
            tests.insert(ImpactEvidence {
                label: node.id.clone(),
                kind: ImpactKind::Test,
                grounding: node.grounding.clone(),
            });
            continue;
        }
        if nearest_distance.is_some() {
            continue;
        }
        for neighbor in adjacency.get(&node_id).into_iter().flatten() {
            if visited.insert(neighbor.clone()) {
                queue.push_back((neighbor.clone(), distance.saturating_add(1)));
            }
        }
    }
    Ok(tests.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current(path: &str, line: u32) -> EvidenceGrounding {
        EvidenceGrounding::CurrentSource(SourceSpan {
            root: "workspace".to_owned(),
            path: path.to_owned(),
            start_line: line,
            end_line: line,
        })
    }

    fn proposed(path: &str, line: u32) -> EvidenceGrounding {
        EvidenceGrounding::Proposal(ProposalLine {
            root: "workspace".to_owned(),
            path: path.to_owned(),
            proposal_line: line,
            old_line: None,
            new_line: Some(line),
        })
    }

    fn changed(text: &str, line: u32) -> ChangedLineFact {
        ChangedLineFact {
            kind: ChangedLineKind::Added,
            grounding: ProposalLine {
                root: "workspace".to_owned(),
                path: "src/lib.rs".to_owned(),
                proposal_line: line,
                old_line: None,
                new_line: Some(line),
            },
            text: text.to_owned(),
        }
    }

    fn edge(from: &str, to: &str, cost: u32, priority: u32) -> WeightedEdge {
        WeightedEdge {
            key: EdgeKey {
                from: from.to_owned(),
                to: to.to_owned(),
                kind: "calls".to_owned(),
            },
            cost,
            priority,
            registration_order: priority,
            grounding: current("src/lib.rs", 1),
        }
    }

    fn graph() -> GraphSnapshot {
        GraphSnapshot {
            nodes: ["a", "b", "c", "d"]
                .into_iter()
                .map(|id| GraphNode {
                    id: id.to_owned(),
                    kind: ImpactKind::EditableLocus,
                    grounding: current(&format!("src/{id}.rs"), 1),
                })
                .collect(),
            edges: vec![
                edge("a", "b", 1, 2),
                edge("b", "d", 2, 2),
                edge("a", "c", 1, 1),
                edge("c", "d", 2, 1),
            ],
        }
    }

    #[test]
    fn beta_requires_explicit_opt_in() {
        let result = parse_beta_proposal(
            BetaGraphDeltaRequest {
                beta: false,
                root: "workspace".to_owned(),
                proposal: ProposalInput::Structured(StructuredProposal {
                    edge_additions: Vec::new(),
                    edge_removals: Vec::new(),
                    impacted: Vec::new(),
                    ..StructuredProposal::default()
                }),
            },
            &GraphDeltaLimits::default(),
        );
        assert_eq!(result, Err(GraphDeltaError::BetaOptInRequired));
    }

    #[test]
    fn unified_diff_is_bounded_and_source_grounded() {
        let overlay = parse_beta_proposal(
            BetaGraphDeltaRequest {
                beta: true,
                root: "workspace".to_owned(),
                proposal: ProposalInput::UnifiedDiff(
                    "diff --git a/src/state.rs b/src/state.rs\n\
                     --- a/src/state.rs\n\
                     +++ b/src/state.rs\n\
                     @@ -1,1 +1,1 @@\n\
                     -old\n\
                     +new\n"
                        .to_owned(),
                ),
            },
            &GraphDeltaLimits::default(),
        )
        .unwrap();
        assert_eq!(overlay.impacted.len(), 2);
        assert_eq!(overlay.changed_files.len(), 1);
        assert_eq!(overlay.changed_files[0].root, "workspace");
        assert_eq!(overlay.changed_files[0].hunks[0].changed_lines.len(), 2);
        assert!(overlay.edge_additions.is_empty());
        assert!(overlay.edge_removals.is_empty());
        assert!(overlay.impacted.iter().all(|evidence| matches!(
            &evidence.grounding,
            EvidenceGrounding::Proposal(line) if line.root == "workspace"
        )));
        assert!(overlay.omissions.iter().any(|omission| {
            omission.code == GraphDeltaOmissionCode::LiveGraphInferenceDeferred
        }));
    }

    #[test]
    fn changed_lines_infer_typed_registration_and_attribute_state_facts() {
        let registration =
            infer_changed_line_relationships(&changed("router.register(handler)", 8));
        assert!(registration.iter().any(|fact| {
            fact.kind == InferredRelationshipKind::Registration
                && fact.target == "handler"
                && fact.grounding.proposal_line == 8
        }));
        assert!(
            registration
                .iter()
                .all(|fact| fact.kind != InferredRelationshipKind::Call)
        );

        let state =
            infer_changed_line_relationships(&changed("endpoint.state = response.status", 9));
        assert!(state.iter().any(|fact| {
            fact.kind == InferredRelationshipKind::AttributeOrStateReference
                && fact.qualifier.as_deref() == Some("endpoint")
                && fact.target == "state"
        }));
        assert!(state.iter().any(|fact| {
            fact.kind == InferredRelationshipKind::AttributeOrStateReference
                && fact.qualifier.as_deref() == Some("response")
                && fact.target == "status"
        }));
    }

    #[test]
    fn behavioral_classes_are_proposal_grounded_and_deterministic() {
        let lines = vec![
            changed("if endpoint.state == None {", 10),
            changed("reconcile(value);", 11),
            changed("decode(payload);", 12),
            changed("raise(error);", 13),
            changed("cache.status = value;", 14),
        ];
        let files = vec![ChangedFileFact {
            root: "workspace".to_owned(),
            path: "src/lib.rs".to_owned(),
            hunks: vec![ChangedHunkFact {
                proposal_header_line: 9,
                old_start: 10,
                old_count: 0,
                new_start: 10,
                new_count: 5,
                changed_lines: lines.clone(),
            }],
        }];
        let forward = infer_changed_behavioral_deltas(&files);
        let mut reversed = files;
        reversed[0].hunks[0].changed_lines.reverse();
        let reverse = infer_changed_behavioral_deltas(&reversed);
        assert_eq!(forward, reverse);
        for kind in [
            BehavioralDeltaKind::BranchBehavior,
            BehavioralDeltaKind::Reconciliation,
            BehavioralDeltaKind::Representation,
            BehavioralDeltaKind::ErrorPath,
            BehavioralDeltaKind::StatePropagation,
        ] {
            assert!(forward.iter().any(|delta| {
                delta.kind == kind && matches!(delta.changed_locus, EvidenceGrounding::Proposal(_))
            }));
        }
    }

    #[test]
    fn equal_cost_paths_are_complete_and_priority_ordered() {
        let card = analyze_graph_delta(
            &graph(),
            &EphemeralOverlay {
                edge_additions: Vec::new(),
                edge_removals: Vec::new(),
                impacted: Vec::new(),
                ..EphemeralOverlay::default()
            },
            &[EndpointPair {
                from: "a".to_owned(),
                to: "d".to_owned(),
            }],
            &GraphDeltaLimits::default(),
        )
        .unwrap();
        assert_eq!(card.routes[0].before.alternatives.len(), 2);
        assert_eq!(card.routes[0].before.alternatives[0].nodes, ["a", "c", "d"]);
    }

    #[test]
    fn overlay_shortens_path_without_mutating_base() {
        let base = graph();
        let untouched = base.clone();
        let addition = WeightedEdge {
            key: EdgeKey {
                from: "a".to_owned(),
                to: "d".to_owned(),
                kind: "registers".to_owned(),
            },
            cost: 1,
            priority: 0,
            registration_order: 0,
            grounding: proposed("src/lib.rs", 8),
        };
        let card = analyze_graph_delta(
            &base,
            &EphemeralOverlay {
                edge_additions: vec![addition],
                edge_removals: Vec::new(),
                impacted: vec![ImpactEvidence {
                    label: "src/lib.rs".to_owned(),
                    kind: ImpactKind::EditableLocus,
                    grounding: proposed("src/lib.rs", 8),
                }],
                ..EphemeralOverlay::default()
            },
            &[EndpointPair {
                from: "a".to_owned(),
                to: "d".to_owned(),
            }],
            &GraphDeltaLimits::default(),
        )
        .unwrap();

        assert_eq!(card.routes[0].change, RouteChange::Shortened);
        assert_eq!(base, untouched);
        assert!(card.beta);
        assert!(!card.bypassed_loci.is_empty());
        assert!(card.behavioral_deltas.iter().any(|delta| {
            delta.kind == BehavioralDeltaKind::BypassedCall
                && matches!(delta.changed_locus, EvidenceGrounding::CurrentSource(_))
        }));
        assert_eq!(card.routes[0].after.alternatives[0].grounded_steps.len(), 1);
    }

    #[test]
    fn nearest_route_test_is_discovered_without_returning_farther_tests() {
        let mut base = graph();
        base.nodes.extend([
            GraphNode {
                id: "near_test".to_owned(),
                kind: ImpactKind::Test,
                grounding: current("tests/near.rs", 4),
            },
            GraphNode {
                id: "far_test".to_owned(),
                kind: ImpactKind::Test,
                grounding: current("tests/far.rs", 8),
            },
        ]);
        base.edges
            .extend([edge("near_test", "a", 1, 0), edge("far_test", "b", 1, 0)]);
        let card = analyze_graph_delta(
            &base,
            &EphemeralOverlay {
                edge_additions: vec![WeightedEdge {
                    key: EdgeKey {
                        from: "a".to_owned(),
                        to: "d".to_owned(),
                        kind: "registers".to_owned(),
                    },
                    cost: 1,
                    priority: 0,
                    registration_order: 0,
                    grounding: proposed("src/lib.rs", 8),
                }],
                ..EphemeralOverlay::default()
            },
            &[EndpointPair {
                from: "a".to_owned(),
                to: "d".to_owned(),
            }],
            &GraphDeltaLimits::default(),
        )
        .unwrap();

        assert_eq!(
            card.impacted_tests
                .iter()
                .map(|test| test.label.as_str())
                .collect::<Vec<_>>(),
            ["near_test"]
        );
        assert!(
            card.affected_locus_checklist.iter().any(|item| {
                item.label == "near_test" && item.kinds.contains(&ImpactKind::Test)
            })
        );
    }

    #[test]
    fn incomplete_hunk_is_rejected_instead_of_partially_parsed() {
        let result = parse_beta_proposal(
            BetaGraphDeltaRequest {
                beta: true,
                root: "workspace".to_owned(),
                proposal: ProposalInput::UnifiedDiff(
                    "diff --git a/src/lib.rs b/src/lib.rs\n\
                     --- a/src/lib.rs\n\
                     +++ b/src/lib.rs\n\
                     @@ -1,2 +1,1 @@\n\
                     -old\n\
                     +new\n"
                        .to_owned(),
                ),
            },
            &GraphDeltaLimits::default(),
        );

        assert!(matches!(
            result,
            Err(GraphDeltaError::MalformedProposal {
                reason: "hunk body line counts do not match its header",
                ..
            })
        ));
    }

    #[test]
    fn structured_proposal_rejects_unknown_fields_and_wrong_roots() {
        let unknown = parse_beta_proposal(
            BetaGraphDeltaRequest {
                beta: true,
                root: "workspace".to_owned(),
                proposal: ProposalInput::StructuredJson(
                    r#"{"schema_version":1,"unexpected":true}"#.to_owned(),
                ),
            },
            &GraphDeltaLimits::default(),
        );
        assert!(matches!(
            unknown,
            Err(GraphDeltaError::MalformedStructuredProposal(_))
        ));

        let wrong_root = parse_beta_proposal(
            BetaGraphDeltaRequest {
                beta: true,
                root: "workspace".to_owned(),
                proposal: ProposalInput::Structured(StructuredProposal {
                    impacted: vec![ImpactEvidence {
                        label: "changed".to_owned(),
                        kind: ImpactKind::EditableLocus,
                        grounding: EvidenceGrounding::Proposal(ProposalLine {
                            root: "other-root".to_owned(),
                            path: "src/lib.rs".to_owned(),
                            proposal_line: 1,
                            old_line: None,
                            new_line: Some(1),
                        }),
                    }],
                    ..StructuredProposal::default()
                }),
            },
            &GraphDeltaLimits::default(),
        );
        assert!(matches!(
            wrong_root,
            Err(GraphDeltaError::InvalidGraph(reason))
                if reason.contains("root does not match")
        ));
    }

    #[test]
    fn worse_priority_equal_cost_edge_does_not_displace_preferred_route() {
        let card = analyze_graph_delta(
            &graph(),
            &EphemeralOverlay {
                edge_additions: vec![WeightedEdge {
                    key: EdgeKey {
                        from: "a".to_owned(),
                        to: "d".to_owned(),
                        kind: "fallback".to_owned(),
                    },
                    cost: 3,
                    priority: 99,
                    registration_order: 99,
                    grounding: proposed("src/lib.rs", 9),
                }],
                ..EphemeralOverlay::default()
            },
            &[EndpointPair {
                from: "a".to_owned(),
                to: "d".to_owned(),
            }],
            &GraphDeltaLimits::default(),
        )
        .unwrap();

        assert_eq!(
            card.routes[0].change,
            RouteChange::EqualCostAlternativesChanged
        );
        assert_eq!(
            card.routes[0].before.alternatives[0].nodes,
            card.routes[0].after.alternatives[0].nodes
        );
        assert_eq!(card.routes[0].after.alternatives[0].nodes, ["a", "c", "d"]);
    }

    #[test]
    fn removing_the_only_edge_reports_lost_reachability() {
        let only_edge = edge("a", "b", 1, 0);
        let base = GraphSnapshot {
            nodes: ["a", "b"]
                .into_iter()
                .map(|id| GraphNode {
                    id: id.to_owned(),
                    kind: ImpactKind::EditableLocus,
                    grounding: current(&format!("src/{id}.rs"), 1),
                })
                .collect(),
            edges: vec![only_edge.clone()],
        };
        let card = analyze_graph_delta(
            &base,
            &EphemeralOverlay {
                edge_removals: vec![only_edge.key],
                ..EphemeralOverlay::default()
            },
            &[EndpointPair {
                from: "a".to_owned(),
                to: "b".to_owned(),
            }],
            &GraphDeltaLimits::default(),
        )
        .unwrap();

        assert_eq!(card.routes[0].change, RouteChange::RemovedReachability);
        assert!(card.routes[0].after.alternatives.is_empty());
        assert_eq!(card.changed_edges[0].change, EdgeChange::Removed);
    }

    #[test]
    fn cross_file_state_and_incomplete_capability_are_explicit() {
        let state_grounding = EvidenceGrounding::CurrentSource(SourceSpan {
            root: "dependency-root".to_owned(),
            path: "src/state.rs".to_owned(),
            start_line: 12,
            end_line: 14,
        });
        let card = analyze_graph_delta(
            &graph(),
            &EphemeralOverlay {
                impacted: vec![ImpactEvidence {
                    label: "shared state".to_owned(),
                    kind: ImpactKind::StateOrApi,
                    grounding: state_grounding.clone(),
                }],
                capabilities: vec![CapabilityReport {
                    capability: GraphDeltaCapability::ImpactTraversal,
                    state: CapabilityState::Unavailable,
                    detail: "live graph traversal was unavailable".to_owned(),
                }],
                ..EphemeralOverlay::default()
            },
            &[],
            &GraphDeltaLimits::default(),
        )
        .unwrap();

        assert_eq!(card.impacted_state_or_api.len(), 1);
        assert_eq!(
            card.affected_locus_checklist[0].hydration.stable_key,
            state_grounding.stable_hydration_key()
        );
        assert!(card.capabilities.iter().any(|capability| {
            capability.capability == GraphDeltaCapability::ImpactTraversal
                && capability.state == CapabilityState::Unavailable
        }));
        assert!(card.omissions.iter().any(|omission| {
            omission.code == GraphDeltaOmissionCode::CapabilityDegraded
                && omission.detail.contains("ImpactTraversal")
        }));
    }

    #[test]
    fn referenced_state_edge_surfaces_the_source_grounded_target_contract() {
        let base = GraphSnapshot {
            nodes: vec![
                GraphNode {
                    id: "handler".to_owned(),
                    kind: ImpactKind::EditableLocus,
                    grounding: current("src/handler.rs", 3),
                },
                GraphNode {
                    id: "endpoint_state".to_owned(),
                    kind: ImpactKind::StateOrApi,
                    grounding: current("src/state.rs", 7),
                },
            ],
            edges: Vec::new(),
        };
        let card = analyze_graph_delta(
            &base,
            &EphemeralOverlay {
                edge_additions: vec![WeightedEdge {
                    key: EdgeKey {
                        from: "handler".to_owned(),
                        to: "endpoint_state".to_owned(),
                        kind: InferredRelationshipKind::AttributeOrStateReference
                            .edge_kind()
                            .to_owned(),
                    },
                    cost: 1,
                    priority: 0,
                    registration_order: 12,
                    grounding: proposed("src/handler.rs", 12),
                }],
                ..EphemeralOverlay::default()
            },
            &[EndpointPair {
                from: "handler".to_owned(),
                to: "endpoint_state".to_owned(),
            }],
            &GraphDeltaLimits::default(),
        )
        .unwrap();

        assert_eq!(card.routes[0].change, RouteChange::AddedReachability);
        assert_eq!(card.impacted_state_or_api.len(), 1);
        assert_eq!(card.impacted_state_or_api[0].label, "endpoint_state");
        assert!(matches!(
            card.impacted_state_or_api[0].grounding,
            EvidenceGrounding::CurrentSource(_)
        ));
    }

    #[test]
    fn candidate_order_does_not_change_card() {
        let mut reversed = graph();
        reversed.nodes.reverse();
        reversed.edges.reverse();
        let overlay = EphemeralOverlay {
            edge_additions: Vec::new(),
            edge_removals: Vec::new(),
            impacted: Vec::new(),
            ..EphemeralOverlay::default()
        };
        let endpoints = [EndpointPair {
            from: "a".to_owned(),
            to: "d".to_owned(),
        }];
        let forward =
            analyze_graph_delta(&graph(), &overlay, &endpoints, &GraphDeltaLimits::default())
                .unwrap();
        let reverse = analyze_graph_delta(
            &reversed,
            &overlay,
            &endpoints,
            &GraphDeltaLimits::default(),
        )
        .unwrap();
        assert_eq!(forward, reverse);
        assert_eq!(
            format!("{forward:#?}").as_bytes(),
            format!("{reverse:#?}").as_bytes()
        );
    }
}
