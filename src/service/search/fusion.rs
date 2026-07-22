//! Deterministic, scale-invariant fusion for agent-facing search evidence.
//!
//! Raw scorer values are meaningful only inside the channel that produced
//! them.  This module therefore uses raw values solely to establish a stable
//! within-channel rank, converts that rank to fixed-point credit, and combines
//! only those comparable credits.  A positive rescaling of a channel's raw
//! values cannot change the fused result as long as its within-channel order
//! is unchanged.
//!
//! The frozen strict semantic search path is intentionally not replaced by
//! the ordinary or task policies below.  `strict_semantic_isolation` exists as
//! a validation boundary for callers that need typed evidence: it accepts only
//! the sealed hybrid-RRF candidate set plus its exact reranker permutation,
//! forbids every fallback channel, and preserves reranker order.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

/// Fixed-point representation of a fully normalized within-channel rank.
pub(crate) const NORMALIZED_RANK_SCALE: u64 = 1_000_000;

/// Evidence lanes that can participate in product search ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum EvidenceChannel {
    ExactLexical,
    FullText,
    Vector,
    HybridRrf,
    Rerank,
    Structural,
    Graph,
}

impl EvidenceChannel {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::ExactLexical => "exact_lexical",
            Self::FullText => "fts",
            Self::Vector => "vector",
            Self::HybridRrf => "hybrid_rrf",
            Self::Rerank => "rerank",
            Self::Structural => "structural",
            Self::Graph => "graph",
        }
    }

    const fn accepts(self, kind: ScoreKind) -> bool {
        if matches!(kind, ScoreKind::WithinChannelRank) {
            return true;
        }
        match self {
            Self::ExactLexical => matches!(
                kind,
                ScoreKind::ExactMatchTier | ScoreKind::LexicalHeuristic
            ),
            Self::FullText => matches!(kind, ScoreKind::Bm25Score),
            Self::Vector => matches!(
                kind,
                ScoreKind::CosineDistance | ScoreKind::CosineSimilarity
            ),
            Self::HybridRrf => matches!(kind, ScoreKind::ReciprocalRankFusion),
            Self::Rerank => matches!(kind, ScoreKind::CrossEncoderScore),
            Self::Structural => matches!(
                kind,
                ScoreKind::PageRank | ScoreKind::EdgeDegree | ScoreKind::StructuralHeuristic
            ),
            Self::Graph => {
                matches!(kind, ScoreKind::GraphHeuristic | ScoreKind::GraphHops)
            }
        }
    }
}

/// The native meaning of a raw score before it is reduced to channel rank.
///
/// None of these values is assumed to be a calibrated probability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// The complete typed inventory is rendered in evidence output even though a
// particular build may not compile every optional scorer producer.
#[allow(dead_code)]
pub(crate) enum ScoreKind {
    /// The producer retained only its final within-channel order, not the
    /// backend's native scorer values. The raw value is the one-based rank.
    WithinChannelRank,
    ExactMatchTier,
    LexicalHeuristic,
    Bm25Score,
    CosineDistance,
    CosineSimilarity,
    ReciprocalRankFusion,
    CrossEncoderScore,
    PageRank,
    EdgeDegree,
    StructuralHeuristic,
    GraphHeuristic,
    GraphHops,
}

impl ScoreKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::WithinChannelRank => "within_channel_rank",
            Self::ExactMatchTier => "exact_match_tier",
            Self::LexicalHeuristic => "lexical_heuristic",
            Self::Bm25Score => "bm25_score",
            Self::CosineDistance => "cosine_distance",
            Self::CosineSimilarity => "cosine_similarity",
            Self::ReciprocalRankFusion => "rrf_score",
            Self::CrossEncoderScore => "cross_encoder_score",
            Self::PageRank => "pagerank",
            Self::EdgeDegree => "edge_degree",
            Self::StructuralHeuristic => "structural_heuristic",
            Self::GraphHeuristic => "graph_heuristic",
            Self::GraphHops => "graph_hops",
        }
    }

    const fn direction(self) -> ScoreDirection {
        match self {
            Self::WithinChannelRank | Self::CosineDistance | Self::GraphHops => {
                ScoreDirection::LowerIsBetter
            }
            Self::ExactMatchTier
            | Self::LexicalHeuristic
            | Self::Bm25Score
            | Self::CosineSimilarity
            | Self::ReciprocalRankFusion
            | Self::CrossEncoderScore
            | Self::PageRank
            | Self::EdgeDegree
            | Self::StructuralHeuristic
            | Self::GraphHeuristic => ScoreDirection::HigherIsBetter,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScoreDirection {
    HigherIsBetter,
    LowerIsBetter,
}

/// One scorer observation before within-channel ranking.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawCandidateScore {
    pub(crate) stable_id: String,
    pub(crate) raw_score: f64,
}

impl RawCandidateScore {
    pub(crate) fn new(stable_id: impl Into<String>, raw_score: f64) -> Self {
        Self {
            stable_id: stable_id.into(),
            raw_score,
        }
    }
}

/// A complete, single-kind scorer lane.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChannelInput {
    pub(crate) channel: EvidenceChannel,
    pub(crate) score_kind: ScoreKind,
    pub(crate) candidates: Vec<RawCandidateScore>,
}

impl ChannelInput {
    pub(crate) fn new(
        channel: EvidenceChannel,
        score_kind: ScoreKind,
        candidates: Vec<RawCandidateScore>,
    ) -> Self {
        Self {
            channel,
            score_kind,
            candidates,
        }
    }
}

/// Stable names for the versioned fusion policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// The strict policy is exercised as an isolation contract while the sealed
// strict service path deliberately bypasses product fusion.
#[allow(dead_code)]
pub(crate) enum FusionPolicyName {
    OrdinarySearchV1,
    TaskSearchV1,
    StrictSemanticIsolationV1,
}

impl FusionPolicyName {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::OrdinarySearchV1 => "ordinary_search_v1",
            Self::TaskSearchV1 => "task_search_v1",
            Self::StrictSemanticIsolationV1 => "strict_semantic_isolation_v1",
        }
    }
}

/// A channel's bounded influence in one named policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChannelRule {
    pub(crate) channel: EvidenceChannel,
    pub(crate) weight: u32,
    pub(crate) depth: u32,
    pub(crate) required: bool,
}

impl ChannelRule {
    const fn new(channel: EvidenceChannel, weight: u32, depth: u32, required: bool) -> Self {
        Self {
            channel,
            weight,
            depth,
            required,
        }
    }
}

// Rule order is also the deterministic per-channel tie-break order.
const ORDINARY_SEARCH_RULES: [ChannelRule; 7] = [
    ChannelRule::new(EvidenceChannel::Rerank, 8, 20, false),
    ChannelRule::new(EvidenceChannel::ExactLexical, 8, 20, false),
    ChannelRule::new(EvidenceChannel::Vector, 6, 20, false),
    ChannelRule::new(EvidenceChannel::FullText, 4, 20, false),
    ChannelRule::new(EvidenceChannel::HybridRrf, 6, 20, false),
    ChannelRule::new(EvidenceChannel::Graph, 2, 20, false),
    ChannelRule::new(EvidenceChannel::Structural, 1, 20, false),
];

const TASK_SEARCH_RULES: [ChannelRule; 7] = [
    ChannelRule::new(EvidenceChannel::ExactLexical, 10, 20, false),
    ChannelRule::new(EvidenceChannel::Rerank, 6, 20, false),
    ChannelRule::new(EvidenceChannel::Vector, 5, 20, false),
    ChannelRule::new(EvidenceChannel::FullText, 3, 20, false),
    ChannelRule::new(EvidenceChannel::HybridRrf, 5, 20, false),
    ChannelRule::new(EvidenceChannel::Graph, 5, 20, false),
    ChannelRule::new(EvidenceChannel::Structural, 1, 20, false),
];

// Hybrid RRF is qualification evidence only here.  Giving it zero weight
// preserves the existing strict contract: the exact cross-encoder permutation
// determines final order after hybrid retrieval succeeds.
#[allow(dead_code)]
const STRICT_SEMANTIC_RULES: [ChannelRule; 2] = [
    ChannelRule::new(EvidenceChannel::Rerank, 1, 50, true),
    ChannelRule::new(EvidenceChannel::HybridRrf, 0, 50, true),
];

/// Versioned, immutable fusion behavior selected by the shared service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FusionPolicy {
    pub(crate) name: FusionPolicyName,
    rules: &'static [ChannelRule],
    strict: bool,
}

impl FusionPolicy {
    pub(crate) const fn ordinary_search() -> Self {
        Self {
            name: FusionPolicyName::OrdinarySearchV1,
            rules: &ORDINARY_SEARCH_RULES,
            strict: false,
        }
    }

    pub(crate) const fn task_search() -> Self {
        Self {
            name: FusionPolicyName::TaskSearchV1,
            rules: &TASK_SEARCH_RULES,
            strict: false,
        }
    }

    #[allow(dead_code)]
    pub(crate) const fn strict_semantic_isolation() -> Self {
        Self {
            name: FusionPolicyName::StrictSemanticIsolationV1,
            rules: &STRICT_SEMANTIC_RULES,
            strict: true,
        }
    }

    #[allow(dead_code)]
    pub(crate) const fn rules(self) -> &'static [ChannelRule] {
        self.rules
    }

    fn rule(self, channel: EvidenceChannel) -> Option<ChannelRule> {
        self.rules
            .iter()
            .copied()
            .find(|rule| rule.channel == channel)
    }
}

/// Auditable evidence for one candidate in one channel.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChannelEvidence {
    pub(crate) channel: EvidenceChannel,
    pub(crate) score_kind: ScoreKind,
    pub(crate) raw_score: f64,
    pub(crate) rank: u32,
    pub(crate) depth: u32,
    pub(crate) normalized_rank_micros: u64,
    pub(crate) weight: u32,
    pub(crate) contribution: u64,
}

/// Typed explanation from which the concise #810 ordering reason is rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FinalOrderingReason {
    pub(crate) policy: FusionPolicyName,
    pub(crate) leading_channel: EvidenceChannel,
    pub(crate) contributing_channels: Vec<EvidenceChannel>,
}

impl FinalOrderingReason {
    pub(crate) fn summary(&self) -> String {
        let contributors = self
            .contributing_channels
            .iter()
            .map(|channel| channel.label())
            .collect::<Vec<_>>()
            .join("+");
        format!(
            "{}: {} led normalized rank fusion ({})",
            self.policy.label(),
            self.leading_channel.label(),
            contributors
        )
    }
}

/// One field in the deterministic rank-vector tie break.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChannelRankTieBreak {
    pub(crate) channel: EvidenceChannel,
    pub(crate) rank: Option<u32>,
}

/// The exact evidence used after equal fixed-point totals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TieBreakEvidence {
    pub(crate) channel_ranks: Vec<ChannelRankTieBreak>,
    pub(crate) stable_id_utf8: String,
}

impl TieBreakEvidence {
    fn compare(&self, other: &Self) -> Ordering {
        for (left, right) in self.channel_ranks.iter().zip(&other.channel_ranks) {
            debug_assert_eq!(left.channel, right.channel);
            let ordering = left
                .rank
                .unwrap_or(u32::MAX)
                .cmp(&right.rank.unwrap_or(u32::MAX));
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        self.stable_id_utf8
            .as_bytes()
            .cmp(other.stable_id_utf8.as_bytes())
    }
}

/// A fused result ready for projection or task-role selection.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FusedCandidate {
    pub(crate) stable_id: String,
    pub(crate) channels: Vec<ChannelEvidence>,
    pub(crate) final_score: u64,
    pub(crate) final_reason: FinalOrderingReason,
    pub(crate) tie_break: TieBreakEvidence,
}

/// Fail-closed contract violations at the fusion boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FusionError {
    DuplicateChannel(EvidenceChannel),
    UnexpectedChannel {
        policy: FusionPolicyName,
        channel: EvidenceChannel,
    },
    StrictFallbackForbidden(EvidenceChannel),
    MissingRequiredChannel(EvidenceChannel),
    EmptyRequiredChannel(EvidenceChannel),
    StrictChannelDepthExceeded {
        channel: EvidenceChannel,
        candidates: usize,
        depth: u32,
    },
    StrictCandidateSetMismatch {
        expected: EvidenceChannel,
        actual: EvidenceChannel,
    },
    CompositeAndComponentChannels,
    ScoreKindMismatch {
        channel: EvidenceChannel,
        score_kind: ScoreKind,
    },
    DuplicateCandidate {
        channel: EvidenceChannel,
        stable_id: String,
    },
    NonFiniteScore {
        channel: EvidenceChannel,
        stable_id: String,
    },
    ContributionOverflow(String),
}

impl fmt::Display for FusionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateChannel(channel) => {
                write!(formatter, "duplicate {} fusion channel", channel.label())
            }
            Self::UnexpectedChannel { policy, channel } => write!(
                formatter,
                "{} does not accept the {} channel",
                policy.label(),
                channel.label()
            ),
            Self::StrictFallbackForbidden(channel) => write!(
                formatter,
                "strict semantic isolation forbids {} fallback evidence",
                channel.label()
            ),
            Self::MissingRequiredChannel(channel) => {
                write!(formatter, "missing required {} channel", channel.label())
            }
            Self::EmptyRequiredChannel(channel) => {
                write!(formatter, "required {} channel is empty", channel.label())
            }
            Self::StrictChannelDepthExceeded {
                channel,
                candidates,
                depth,
            } => write!(
                formatter,
                "strict {} channel has {} candidates, exceeding fixed depth {}",
                channel.label(),
                candidates,
                depth
            ),
            Self::StrictCandidateSetMismatch { expected, actual } => write!(
                formatter,
                "strict {} candidate set is not an exact permutation of {}",
                actual.label(),
                expected.label()
            ),
            Self::CompositeAndComponentChannels => write!(
                formatter,
                "hybrid RRF cannot be fused alongside its FTS/vector components"
            ),
            Self::ScoreKindMismatch {
                channel,
                score_kind,
            } => write!(
                formatter,
                "{} is not a valid raw score kind for the {} channel",
                score_kind.label(),
                channel.label()
            ),
            Self::DuplicateCandidate { channel, stable_id } => write!(
                formatter,
                "candidate {stable_id} appears twice in the {} channel",
                channel.label()
            ),
            Self::NonFiniteScore { channel, stable_id } => write!(
                formatter,
                "candidate {stable_id} has a non-finite {} score",
                channel.label()
            ),
            Self::ContributionOverflow(stable_id) => {
                write!(formatter, "fusion contribution overflow for {stable_id}")
            }
        }
    }
}

impl Error for FusionError {}

/// Fuse independent scorer lanes using only their within-channel ranks.
pub(crate) fn fuse_ranked_channels(
    policy: FusionPolicy,
    inputs: &[ChannelInput],
) -> Result<Vec<FusedCandidate>, FusionError> {
    validate_inputs(policy, inputs)?;

    let mut ranked_by_channel: BTreeMap<EvidenceChannel, Vec<RankedObservation>> = BTreeMap::new();
    for input in inputs {
        let rule = policy
            .rule(input.channel)
            .expect("validated channel must have a policy rule");
        ranked_by_channel.insert(input.channel, rank_channel(input, rule)?);
    }

    let mut by_candidate: BTreeMap<String, BTreeMap<EvidenceChannel, ChannelEvidence>> =
        BTreeMap::new();
    for (channel, ranked) in ranked_by_channel {
        for observation in ranked {
            by_candidate
                .entry(observation.stable_id)
                .or_default()
                .insert(channel, observation.evidence);
        }
    }

    let mut fused = Vec::with_capacity(by_candidate.len());
    for (stable_id, evidence_by_channel) in by_candidate {
        // Strict candidate-set equality was validated before ranking.  This
        // defensive check keeps future policy edits fail-closed.
        if policy.strict
            && policy
                .rules
                .iter()
                .filter(|rule| rule.required)
                .any(|rule| !evidence_by_channel.contains_key(&rule.channel))
        {
            return Err(FusionError::ContributionOverflow(stable_id));
        }

        let channels: Vec<ChannelEvidence> = policy
            .rules
            .iter()
            .filter_map(|rule| evidence_by_channel.get(&rule.channel).cloned())
            .collect();
        let final_score = channels.iter().try_fold(0_u64, |total, evidence| {
            total
                .checked_add(evidence.contribution)
                .ok_or_else(|| FusionError::ContributionOverflow(stable_id.clone()))
        })?;
        // `channels` is already in policy-rule order. Preserve the first item
        // on equal contribution so the explanation uses the same tie order as
        // candidate ranking, independent of enum declaration order.
        let leading_channel = channels
            .iter()
            .filter(|evidence| evidence.weight > 0)
            .fold(None, |best, evidence| match best {
                None => Some(evidence),
                Some(current) if evidence.contribution > current.contribution => Some(evidence),
                Some(current) => Some(current),
            })
            .map(|evidence| evidence.channel)
            .expect("named policies always have a positive-weight channel");
        let contributing_channels = channels
            .iter()
            .filter(|evidence| evidence.weight > 0)
            .map(|evidence| evidence.channel)
            .collect();
        let channel_ranks = policy
            .rules
            .iter()
            .map(|rule| ChannelRankTieBreak {
                channel: rule.channel,
                rank: evidence_by_channel
                    .get(&rule.channel)
                    .map(|evidence| evidence.rank),
            })
            .collect();

        fused.push(FusedCandidate {
            stable_id: stable_id.clone(),
            channels,
            final_score,
            final_reason: FinalOrderingReason {
                policy: policy.name,
                leading_channel,
                contributing_channels,
            },
            tie_break: TieBreakEvidence {
                channel_ranks,
                stable_id_utf8: stable_id,
            },
        });
    }

    fused.sort_by(|left, right| {
        right
            .final_score
            .cmp(&left.final_score)
            .then_with(|| left.tie_break.compare(&right.tie_break))
    });
    Ok(fused)
}

#[derive(Debug)]
struct RankedObservation {
    stable_id: String,
    evidence: ChannelEvidence,
}

fn validate_inputs(policy: FusionPolicy, inputs: &[ChannelInput]) -> Result<(), FusionError> {
    let mut seen_channels = BTreeSet::new();
    for input in inputs {
        if !seen_channels.insert(input.channel) {
            return Err(FusionError::DuplicateChannel(input.channel));
        }
        let Some(rule) = policy.rule(input.channel) else {
            return Err(if policy.strict {
                FusionError::StrictFallbackForbidden(input.channel)
            } else {
                FusionError::UnexpectedChannel {
                    policy: policy.name,
                    channel: input.channel,
                }
            });
        };
        if !input.channel.accepts(input.score_kind) {
            return Err(FusionError::ScoreKindMismatch {
                channel: input.channel,
                score_kind: input.score_kind,
            });
        }
        validate_candidates(input)?;
        if policy.strict && input.candidates.len() > rule.depth as usize {
            return Err(FusionError::StrictChannelDepthExceeded {
                channel: input.channel,
                candidates: input.candidates.len(),
                depth: rule.depth,
            });
        }
    }

    if seen_channels.contains(&EvidenceChannel::HybridRrf)
        && (seen_channels.contains(&EvidenceChannel::FullText)
            || seen_channels.contains(&EvidenceChannel::Vector))
    {
        return Err(FusionError::CompositeAndComponentChannels);
    }

    for rule in policy.rules.iter().filter(|rule| rule.required) {
        let Some(input) = inputs.iter().find(|input| input.channel == rule.channel) else {
            return Err(FusionError::MissingRequiredChannel(rule.channel));
        };
        if input.candidates.is_empty() {
            return Err(FusionError::EmptyRequiredChannel(rule.channel));
        }
    }

    if policy.strict {
        validate_strict_candidate_sets(policy, inputs)?;
    }
    Ok(())
}

fn validate_candidates(input: &ChannelInput) -> Result<(), FusionError> {
    let mut seen = BTreeSet::new();
    for candidate in &input.candidates {
        if !candidate.raw_score.is_finite() {
            return Err(FusionError::NonFiniteScore {
                channel: input.channel,
                stable_id: candidate.stable_id.clone(),
            });
        }
        if !seen.insert(candidate.stable_id.as_str()) {
            return Err(FusionError::DuplicateCandidate {
                channel: input.channel,
                stable_id: candidate.stable_id.clone(),
            });
        }
    }
    Ok(())
}

fn validate_strict_candidate_sets(
    policy: FusionPolicy,
    inputs: &[ChannelInput],
) -> Result<(), FusionError> {
    let mut required = policy.rules.iter().filter(|rule| rule.required);
    let first_rule = required
        .next()
        .expect("strict policy must require at least one channel");
    let first_input = inputs
        .iter()
        .find(|input| input.channel == first_rule.channel)
        .expect("required channels were validated");
    let expected: BTreeSet<&str> = first_input
        .candidates
        .iter()
        .map(|candidate| candidate.stable_id.as_str())
        .collect();

    for rule in required {
        let input = inputs
            .iter()
            .find(|input| input.channel == rule.channel)
            .expect("required channels were validated");
        let actual: BTreeSet<&str> = input
            .candidates
            .iter()
            .map(|candidate| candidate.stable_id.as_str())
            .collect();
        if actual != expected {
            return Err(FusionError::StrictCandidateSetMismatch {
                expected: first_rule.channel,
                actual: rule.channel,
            });
        }
    }
    Ok(())
}

fn rank_channel(
    input: &ChannelInput,
    rule: ChannelRule,
) -> Result<Vec<RankedObservation>, FusionError> {
    debug_assert!(rule.depth > 0);
    let mut candidates = input.candidates.clone();
    candidates.sort_by(|left, right| {
        compare_raw_scores(
            left.raw_score,
            right.raw_score,
            input.score_kind.direction(),
        )
        .then_with(|| left.stable_id.as_bytes().cmp(right.stable_id.as_bytes()))
    });

    let mut ranked = Vec::with_capacity(candidates.len().min(rule.depth as usize));
    let mut previous_score = None;
    let mut rank = 0_u32;
    for (index, candidate) in candidates.into_iter().take(rule.depth as usize).enumerate() {
        let canonical_score = canonical_score(candidate.raw_score);
        if previous_score != Some(canonical_score) {
            rank = index as u32 + 1;
            previous_score = Some(canonical_score);
        }
        let normalized_rank_micros = normalized_rank(rank, rule.depth);
        let contribution = normalized_rank_micros
            .checked_mul(u64::from(rule.weight))
            .ok_or_else(|| FusionError::ContributionOverflow(candidate.stable_id.clone()))?;
        ranked.push(RankedObservation {
            stable_id: candidate.stable_id,
            evidence: ChannelEvidence {
                channel: input.channel,
                score_kind: input.score_kind,
                raw_score: canonical_score,
                rank,
                depth: rule.depth,
                normalized_rank_micros,
                weight: rule.weight,
                contribution,
            },
        });
    }
    Ok(ranked)
}

fn compare_raw_scores(left: f64, right: f64, direction: ScoreDirection) -> Ordering {
    let left = canonical_score(left);
    let right = canonical_score(right);
    match direction {
        ScoreDirection::HigherIsBetter => right.total_cmp(&left),
        ScoreDirection::LowerIsBetter => left.total_cmp(&right),
    }
}

fn canonical_score(score: f64) -> f64 {
    // Treat IEEE -0.0 and 0.0 as the same score so platform-specific sign-zero
    // details cannot alter channel ranks.
    if score == 0.0 { 0.0 } else { score }
}

fn normalized_rank(rank: u32, depth: u32) -> u64 {
    debug_assert!(rank >= 1 && rank <= depth);
    NORMALIZED_RANK_SCALE * u64::from(depth - rank + 1) / u64::from(depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(
        evidence_channel: EvidenceChannel,
        score_kind: ScoreKind,
        candidates: &[(&str, f64)],
    ) -> ChannelInput {
        ChannelInput::new(
            evidence_channel,
            score_kind,
            candidates
                .iter()
                .map(|(stable_id, raw_score)| RawCandidateScore::new(*stable_id, *raw_score))
                .collect(),
        )
    }

    #[derive(Debug, PartialEq, Eq)]
    struct InvariantCandidate {
        stable_id: String,
        final_score: u64,
        channels: Vec<(EvidenceChannel, u32, u64, u64)>,
        reason: FinalOrderingReason,
        tie_break: TieBreakEvidence,
    }

    fn invariant_snapshot(results: &[FusedCandidate]) -> Vec<InvariantCandidate> {
        results
            .iter()
            .map(|candidate| InvariantCandidate {
                stable_id: candidate.stable_id.clone(),
                final_score: candidate.final_score,
                channels: candidate
                    .channels
                    .iter()
                    .map(|evidence| {
                        (
                            evidence.channel,
                            evidence.rank,
                            evidence.normalized_rank_micros,
                            evidence.contribution,
                        )
                    })
                    .collect(),
                reason: candidate.final_reason.clone(),
                tie_break: candidate.tie_break.clone(),
            })
            .collect()
    }

    fn append_length_prefixed(bytes: &mut Vec<u8>, value: &[u8]) {
        let length = u64::try_from(value.len()).expect("test evidence length fits in u64");
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes.extend_from_slice(value);
    }

    /// Canonical, length-prefixed encoding of every ordering field the fusion
    /// kernel hands to the projection layer. This deliberately avoids a
    /// serializer's map/order defaults so the test asserts the exact byte
    /// contract, including raw score bits and the complete final tie vector.
    fn canonical_evidence_bytes(results: &[FusedCandidate]) -> Vec<u8> {
        let mut bytes = b"fusion-evidence-v1\0".to_vec();
        bytes.extend_from_slice(
            &u64::try_from(results.len())
                .expect("test result count fits in u64")
                .to_be_bytes(),
        );
        for candidate in results {
            append_length_prefixed(&mut bytes, candidate.stable_id.as_bytes());
            bytes.extend_from_slice(&candidate.final_score.to_be_bytes());
            append_length_prefixed(&mut bytes, candidate.final_reason.policy.label().as_bytes());
            append_length_prefixed(
                &mut bytes,
                candidate.final_reason.leading_channel.label().as_bytes(),
            );
            bytes.extend_from_slice(
                &u64::try_from(candidate.final_reason.contributing_channels.len())
                    .expect("test channel count fits in u64")
                    .to_be_bytes(),
            );
            for channel in &candidate.final_reason.contributing_channels {
                append_length_prefixed(&mut bytes, channel.label().as_bytes());
            }

            bytes.extend_from_slice(
                &u64::try_from(candidate.channels.len())
                    .expect("test evidence count fits in u64")
                    .to_be_bytes(),
            );
            for evidence in &candidate.channels {
                append_length_prefixed(&mut bytes, evidence.channel.label().as_bytes());
                append_length_prefixed(&mut bytes, evidence.score_kind.label().as_bytes());
                bytes.extend_from_slice(&evidence.raw_score.to_bits().to_be_bytes());
                bytes.extend_from_slice(&evidence.rank.to_be_bytes());
                bytes.extend_from_slice(&evidence.depth.to_be_bytes());
                bytes.extend_from_slice(&evidence.normalized_rank_micros.to_be_bytes());
                bytes.extend_from_slice(&evidence.weight.to_be_bytes());
                bytes.extend_from_slice(&evidence.contribution.to_be_bytes());
            }

            bytes.extend_from_slice(
                &u64::try_from(candidate.tie_break.channel_ranks.len())
                    .expect("test tie count fits in u64")
                    .to_be_bytes(),
            );
            for tie in &candidate.tie_break.channel_ranks {
                append_length_prefixed(&mut bytes, tie.channel.label().as_bytes());
                match tie.rank {
                    Some(rank) => {
                        bytes.push(1);
                        bytes.extend_from_slice(&rank.to_be_bytes());
                    }
                    None => bytes.push(0),
                }
            }
            append_length_prefixed(&mut bytes, candidate.tie_break.stable_id_utf8.as_bytes());
        }
        bytes
    }

    fn assert_channel_scale_invariant(
        inputs: &[ChannelInput],
        target_channel: EvidenceChannel,
        scale: f64,
    ) {
        assert!(scale.is_finite() && scale > 0.0);
        let original = fuse_ranked_channels(FusionPolicy::ordinary_search(), inputs).unwrap();
        let mut scaled = inputs.to_vec();
        let target = scaled
            .iter_mut()
            .find(|input| input.channel == target_channel)
            .expect("target channel should be present");
        for candidate in &mut target.candidates {
            candidate.raw_score *= scale;
        }
        let rescaled = fuse_ranked_channels(FusionPolicy::ordinary_search(), &scaled).unwrap();
        assert_eq!(
            invariant_snapshot(&original),
            invariant_snapshot(&rescaled),
            "channel {target_channel:?} changed under positive scale {scale}"
        );
    }

    fn candidate<'a>(results: &'a [FusedCandidate], stable_id: &str) -> &'a FusedCandidate {
        results
            .iter()
            .find(|candidate| candidate.stable_id == stable_id)
            .expect("candidate should be present")
    }

    #[test]
    fn multiplying_each_channel_alone_cannot_change_fusion() {
        let component_inputs = vec![
            channel(
                EvidenceChannel::ExactLexical,
                ScoreKind::LexicalHeuristic,
                &[("alpha", 10_000.0), ("beta", 8_000.0), ("gamma", 100.0)],
            ),
            channel(
                EvidenceChannel::FullText,
                ScoreKind::Bm25Score,
                &[("beta", 17.0), ("gamma", 3.0), ("alpha", 1.0)],
            ),
            channel(
                EvidenceChannel::Vector,
                ScoreKind::CosineDistance,
                &[("gamma", 0.05), ("alpha", 0.2), ("beta", 0.8)],
            ),
            channel(
                EvidenceChannel::Rerank,
                ScoreKind::CrossEncoderScore,
                &[("alpha", 0.9), ("gamma", 0.7), ("beta", 0.1)],
            ),
            channel(
                EvidenceChannel::Structural,
                ScoreKind::PageRank,
                &[("beta", 0.8), ("alpha", 0.3), ("gamma", 0.1)],
            ),
            channel(
                EvidenceChannel::Graph,
                ScoreKind::GraphHeuristic,
                &[("gamma", 90_000.0), ("beta", 100.0)],
            ),
        ];
        for target_channel in [
            EvidenceChannel::ExactLexical,
            EvidenceChannel::FullText,
            EvidenceChannel::Vector,
            EvidenceChannel::Rerank,
            EvidenceChannel::Structural,
            EvidenceChannel::Graph,
        ] {
            for scale in [0.000_001, 7.0, 1_000_000.0] {
                assert_channel_scale_invariant(&component_inputs, target_channel, scale);
            }
        }

        // Hybrid RRF is intentionally exclusive with its FTS/vector
        // components, so exercise it in the same multi-channel context with
        // those two lanes absent rather than weakening the double-count guard.
        let hybrid_inputs = vec![
            channel(
                EvidenceChannel::ExactLexical,
                ScoreKind::ExactMatchTier,
                &[("alpha", 3.0), ("beta", 2.0), ("gamma", 1.0)],
            ),
            channel(
                EvidenceChannel::HybridRrf,
                ScoreKind::ReciprocalRankFusion,
                &[("gamma", 0.9), ("alpha", 0.6), ("beta", 0.1)],
            ),
            channel(
                EvidenceChannel::Rerank,
                ScoreKind::CrossEncoderScore,
                &[("alpha", 0.8), ("gamma", 0.7), ("beta", 0.2)],
            ),
            channel(
                EvidenceChannel::Structural,
                ScoreKind::PageRank,
                &[("beta", 0.8), ("alpha", 0.3), ("gamma", 0.1)],
            ),
            channel(
                EvidenceChannel::Graph,
                ScoreKind::GraphHeuristic,
                &[("gamma", 9.0), ("beta", 1.0)],
            ),
        ];
        for scale in [0.000_001, 7.0, 1_000_000.0] {
            assert_channel_scale_invariant(&hybrid_inputs, EvidenceChannel::HybridRrf, scale);
        }
    }

    #[test]
    fn shuffled_channels_and_candidates_are_byte_stable() {
        let inputs = vec![
            channel(
                EvidenceChannel::ExactLexical,
                ScoreKind::ExactMatchTier,
                &[("node:zeta", 3.0), ("node:alpha", 3.0), ("node:beta", 2.0)],
            ),
            channel(
                EvidenceChannel::Vector,
                ScoreKind::CosineDistance,
                &[("node:beta", 0.1), ("node:zeta", 0.2), ("node:alpha", 0.2)],
            ),
            channel(
                EvidenceChannel::Graph,
                ScoreKind::GraphHeuristic,
                &[("node:alpha", 5.0), ("node:beta", 4.0)],
            ),
        ];
        let mut shuffled = inputs.clone();
        shuffled.reverse();
        for input in &mut shuffled {
            input.candidates.reverse();
        }

        let expected = fuse_ranked_channels(FusionPolicy::task_search(), &inputs).unwrap();
        let actual = fuse_ranked_channels(FusionPolicy::task_search(), &shuffled).unwrap();
        assert_eq!(
            canonical_evidence_bytes(&expected),
            canonical_evidence_bytes(&actual)
        );
        assert!(expected.iter().all(|candidate| {
            candidate.tie_break.stable_id_utf8 == candidate.stable_id
                && candidate.tie_break.channel_ranks.len()
                    == FusionPolicy::task_search().rules().len()
        }));
    }

    #[test]
    fn graph_only_evidence_promotes_while_weak_graph_does_not_overwhelm_semantic() {
        let semantic_candidates: Vec<(String, f64)> = (1_u32..=20)
            .map(|rank| (format!("semantic-{rank:02}"), f64::from(21_u32 - rank)))
            .collect();
        let semantic = ChannelInput::new(
            EvidenceChannel::Vector,
            ScoreKind::CosineSimilarity,
            semantic_candidates
                .iter()
                .map(|(stable_id, score)| RawCandidateScore::new(stable_id, *score))
                .collect(),
        );
        let graph_only = channel(
            EvidenceChannel::Graph,
            ScoreKind::GraphHeuristic,
            &[("graph-only", 100.0)],
        );
        let promoted =
            fuse_ranked_channels(FusionPolicy::ordinary_search(), &[semantic, graph_only]).unwrap();
        let graph_only_candidate = candidate(&promoted, "graph-only");
        assert_eq!(
            graph_only_candidate
                .channels
                .iter()
                .map(|evidence| evidence.channel)
                .collect::<Vec<_>>(),
            vec![EvidenceChannel::Graph]
        );
        assert_eq!(
            graph_only_candidate.final_reason.leading_channel,
            EvidenceChannel::Graph
        );
        assert!(
            promoted
                .iter()
                .position(|candidate| candidate.stable_id == "graph-only")
                < promoted
                    .iter()
                    .position(|candidate| candidate.stable_id == "semantic-15")
        );

        let mut weak_graph_candidates: Vec<(String, f64)> = (1..20)
            .map(|rank| (format!("graph-{rank:02}"), f64::from(100 - rank)))
            .collect();
        weak_graph_candidates.push(("weakly-connected".to_string(), 0.0));
        let weak_graph = ChannelInput::new(
            EvidenceChannel::Graph,
            ScoreKind::GraphHeuristic,
            weak_graph_candidates
                .iter()
                .map(|(stable_id, score)| RawCandidateScore::new(stable_id, *score))
                .collect(),
        );
        let weak_semantic = channel(
            EvidenceChannel::Vector,
            ScoreKind::CosineSimilarity,
            &[("semantic-first", 0.9), ("weakly-connected", 0.8)],
        );
        let weak = fuse_ranked_channels(
            FusionPolicy::ordinary_search(),
            &[weak_semantic, weak_graph],
        )
        .unwrap();
        assert!(
            weak.iter()
                .position(|candidate| candidate.stable_id == "semantic-first")
                < weak
                    .iter()
                    .position(|candidate| candidate.stable_id == "weakly-connected")
        );
        assert_eq!(
            candidate(&weak, "weakly-connected")
                .channels
                .iter()
                .find(|evidence| evidence.channel == EvidenceChannel::Graph)
                .expect("weak graph evidence should be retained")
                .rank,
            20
        );
    }

    #[test]
    fn giant_semantic_decoy_is_bounded_and_cannot_erase_exact_graph_evidence() {
        let mut semantic_decoys = vec![RawCandidateScore::new("giant-decoy", 1.0e300)];
        semantic_decoys.extend((0_u32..256).map(|rank| {
            RawCandidateScore::new(
                format!("semantic-decoy-{rank:03}"),
                f64::from(256_u32 - rank),
            )
        }));
        let inputs = vec![
            channel(
                EvidenceChannel::ExactLexical,
                ScoreKind::ExactMatchTier,
                &[("direct-relevant", 1.0)],
            ),
            ChannelInput::new(
                EvidenceChannel::Vector,
                ScoreKind::CosineSimilarity,
                semantic_decoys,
            ),
            channel(
                EvidenceChannel::Rerank,
                ScoreKind::CrossEncoderScore,
                &[("giant-decoy", 1.0e20), ("direct-relevant", 0.2)],
            ),
            channel(
                EvidenceChannel::Graph,
                ScoreKind::GraphHeuristic,
                &[("direct-relevant", 1.0)],
            ),
        ];
        let fused = fuse_ranked_channels(FusionPolicy::task_search(), &inputs).unwrap();

        assert_eq!(fused[0].stable_id, "direct-relevant");
        // The fusion kernel has no rendered-body cost input; its relevant
        // boundedness guarantee is that each lane contributes at most its
        // policy depth. Twenty semantic observations plus the exact/graph
        // candidate therefore produce exactly twenty-one fused candidates.
        assert_eq!(fused.len(), 21);
        assert!(
            candidate(&fused, "direct-relevant").final_score
                > candidate(&fused, "giant-decoy").final_score
        );
        assert_eq!(
            candidate(&fused, "giant-decoy")
                .channels
                .iter()
                .find(|evidence| evidence.channel == EvidenceChannel::Vector)
                .expect("giant semantic decoy should be retained")
                .rank,
            1
        );
    }

    #[test]
    fn equal_scores_close_with_stable_id_utf8_bytes() {
        let forward = fuse_ranked_channels(
            FusionPolicy::ordinary_search(),
            &[channel(
                EvidenceChannel::FullText,
                ScoreKind::Bm25Score,
                &[
                    ("node:\u{e9}", 4.0),
                    ("node:zeta", 4.0),
                    ("node:alpha", 4.0),
                ],
            )],
        )
        .unwrap();
        let reverse = fuse_ranked_channels(
            FusionPolicy::ordinary_search(),
            &[channel(
                EvidenceChannel::FullText,
                ScoreKind::Bm25Score,
                &[
                    ("node:alpha", 4.0),
                    ("node:zeta", 4.0),
                    ("node:\u{e9}", 4.0),
                ],
            )],
        )
        .unwrap();

        assert_eq!(
            forward
                .iter()
                .map(|candidate| candidate.stable_id.as_str())
                .collect::<Vec<_>>(),
            vec!["node:alpha", "node:zeta", "node:\u{e9}"]
        );
        assert!(
            forward
                .iter()
                .all(|candidate| candidate.channels[0].rank == 1)
        );
        assert!(
            forward
                .iter()
                .all(|candidate| candidate.final_score == forward[0].final_score)
        );
        assert!(
            forward
                .iter()
                .all(|candidate| { candidate.tie_break.stable_id_utf8 == candidate.stable_id })
        );
        assert_eq!(
            canonical_evidence_bytes(&forward),
            canonical_evidence_bytes(&reverse)
        );
    }

    #[test]
    fn leading_channel_uses_policy_order_when_contributions_tie() {
        let fused = fuse_ranked_channels(
            FusionPolicy::ordinary_search(),
            &[
                channel(
                    EvidenceChannel::Rerank,
                    ScoreKind::WithinChannelRank,
                    &[("node:a", 1.0)],
                ),
                channel(
                    EvidenceChannel::ExactLexical,
                    ScoreKind::WithinChannelRank,
                    &[("node:a", 1.0)],
                ),
            ],
        )
        .unwrap();

        assert_eq!(
            fused[0].final_reason.leading_channel,
            EvidenceChannel::Rerank
        );
        assert_eq!(
            fused[0]
                .channels
                .iter()
                .map(|evidence| evidence.channel)
                .collect::<Vec<_>>(),
            vec![EvidenceChannel::Rerank, EvidenceChannel::ExactLexical]
        );
    }

    #[test]
    fn within_channel_rank_is_a_truthful_order_only_score_for_every_lane() {
        let fused = fuse_ranked_channels(
            FusionPolicy::ordinary_search(),
            &[channel(
                EvidenceChannel::Vector,
                ScoreKind::WithinChannelRank,
                &[("node:second", 2.0), ("node:first", 1.0)],
            )],
        )
        .unwrap();

        assert_eq!(fused[0].stable_id, "node:first");
        assert_eq!(fused[0].channels[0].raw_score, 1.0);
        assert_eq!(
            fused[0].channels[0].score_kind,
            ScoreKind::WithinChannelRank
        );
    }

    #[test]
    fn strict_semantic_isolation_forbids_fallback_and_requires_exact_permutation() {
        let fallback = fuse_ranked_channels(
            FusionPolicy::strict_semantic_isolation(),
            &[channel(
                EvidenceChannel::Vector,
                ScoreKind::CosineDistance,
                &[("node:a", 0.1)],
            )],
        );
        assert_eq!(
            fallback,
            Err(FusionError::StrictFallbackForbidden(
                EvidenceChannel::Vector
            ))
        );

        let mismatch = fuse_ranked_channels(
            FusionPolicy::strict_semantic_isolation(),
            &[
                channel(
                    EvidenceChannel::HybridRrf,
                    ScoreKind::ReciprocalRankFusion,
                    &[("node:a", 0.9), ("node:b", 0.8)],
                ),
                channel(
                    EvidenceChannel::Rerank,
                    ScoreKind::CrossEncoderScore,
                    &[("node:a", 0.7)],
                ),
            ],
        );
        assert_eq!(
            mismatch,
            Err(FusionError::StrictCandidateSetMismatch {
                expected: EvidenceChannel::Rerank,
                actual: EvidenceChannel::HybridRrf,
            })
        );
    }

    #[test]
    fn strict_semantic_isolation_preserves_reranker_order() {
        let fused = fuse_ranked_channels(
            FusionPolicy::strict_semantic_isolation(),
            &[
                channel(
                    EvidenceChannel::HybridRrf,
                    ScoreKind::ReciprocalRankFusion,
                    &[("node:a", 0.9), ("node:b", 0.8)],
                ),
                channel(
                    EvidenceChannel::Rerank,
                    ScoreKind::CrossEncoderScore,
                    &[("node:b", 0.95), ("node:a", 0.5)],
                ),
            ],
        )
        .unwrap();

        assert_eq!(
            fused
                .iter()
                .map(|candidate| candidate.stable_id.as_str())
                .collect::<Vec<_>>(),
            vec!["node:b", "node:a"]
        );
        assert!(
            fused
                .iter()
                .all(|candidate| candidate.channels.iter().any(|evidence| {
                    evidence.channel == EvidenceChannel::HybridRrf && evidence.weight == 0
                }))
        );
    }
}
