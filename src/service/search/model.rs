//! Typed contracts for search selection, projection, hydration, and accounting.

use std::collections::BTreeMap;
use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $label:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub(crate) enum $name { $($variant),+ }

        impl $name {
            pub(crate) const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $label),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

string_enum!(SearchIntent {
    Discover => "discover",
    Implement => "implement",
    Debug => "debug",
    Review => "review",
    Hydrate => "hydrate",
});
string_enum!(SearchProjection { Agent => "agent", Evidence => "evidence" });
string_enum!(BodyPolicy {
    Complete => "complete",
    FocusedSpan => "focused_span",
    SignatureOnly => "signature_only",
    Minified => "minified",
    NoBody => "none",
});
string_enum!(BodyRepresentation {
    Complete => "complete",
    FocusedSpan => "focused_span",
    SignatureOnly => "signature_only",
    Minified => "minified",
    Truncated => "truncated",
});
string_enum!(ContextRole {
    EditableSource => "editable_source",
    DefinitionOrApiState => "definition_or_api_state",
    Test => "test",
    BehavioralAnalogue => "behavioral_analogue",
    DirectDependency => "direct_dependency",
    CallerOrImpact => "caller_or_impact",
    ProposalDelta => "proposal_delta",
});
string_enum!(RetrievalLane {
    ExactReference => "exact_reference",
    EditableSource => "editable_source",
    DefinitionOrState => "definition_or_state",
    Tests => "tests",
    Analogues => "analogues",
    Dependencies => "dependencies",
    GraphImpact => "graph_impact",
    ProposalDelta => "proposal_delta",
});
string_enum!(SelectionChannel {
    Exact => "exact",
    Lexical => "lexical",
    Semantic => "semantic",
    Graph => "graph",
    Artifact => "artifact",
    Markdown => "markdown",
});
string_enum!(CandidateDisposition {
    Selected => "selected",
    Omitted => "omitted",
});
string_enum!(SpanCoverage { Complete => "complete", Partial => "partial" });
string_enum!(HydrationKind { Source => "source", Evidence => "evidence" });
string_enum!(CapabilityState {
    Ready => "ready",
    Degraded => "degraded",
    Unavailable => "unavailable",
});
string_enum!(OmissionCode {
    NoBodyPolicy => "no_body_policy",
    MissingSource => "missing_source",
    InvalidFocusedSpan => "invalid_focused_span",
    SourceUnavailable => "source_unavailable",
    PerRecordBodyCap => "per_record_body_cap",
    TotalBodyCap => "total_body_cap",
    RenderBudget => "render_budget",
    MinificationFailed => "minification_failed",
});
string_enum!(CostSection {
    Headers => "headers",
    Bodies => "bodies",
    Relationships => "relationships",
    Metadata => "metadata",
    Footer => "footer",
});

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct SourceSpan {
    pub(crate) root: String,
    pub(crate) path: String,
    pub(crate) start_line: u32,
    pub(crate) end_line: u32,
}

impl SourceSpan {
    pub(crate) fn is_valid(&self) -> bool {
        self.start_line > 0 && self.end_line >= self.start_line
    }

    pub(crate) fn contains(&self, other: &Self) -> bool {
        self.root == other.root
            && self.path == other.path
            && self.start_line <= other.start_line
            && self.end_line >= other.end_line
    }

    pub(crate) fn stable_id(&self) -> String {
        format!(
            "source:v1:{}:{}:{}:{}",
            hex(self.root.as_bytes()),
            hex(self.path.as_bytes()),
            self.start_line,
            self.end_line
        )
    }

    /// Recover the authoritative source span bound into a coalesced hydration
    /// identity. All variable-width fields are hex encoded, so the shape is
    /// unambiguous and malformed identities fail closed.
    pub(crate) fn from_stable_id(value: &str) -> Result<Self, String> {
        let parts: Vec<_> = value.split(':').collect();
        if parts.len() != 6 || parts[0] != "source" || parts[1] != "v1" {
            return Err("invalid source span identity shape".to_string());
        }
        let span = Self {
            root: unhex(parts[2])?,
            path: unhex(parts[3])?,
            start_line: parts[4]
                .parse()
                .map_err(|_| "invalid source span start line")?,
            end_line: parts[5]
                .parse()
                .map_err(|_| "invalid source span end line")?,
        };
        if !span.is_valid() || span.stable_id() != value {
            return Err("invalid source span identity".to_string());
        }
        Ok(span)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HydrationHandle {
    pub(crate) kind: HydrationKind,
    pub(crate) record_id: String,
    pub(crate) source: Option<SourceSpan>,
}

impl HydrationHandle {
    pub(crate) fn is_encoded(value: &str) -> bool {
        value.starts_with("rna-hydrate-v1:") || value.starts_with("rna-h2:")
    }

    pub(crate) fn source(record_id: impl Into<String>, source: SourceSpan) -> Self {
        Self {
            kind: HydrationKind::Source,
            record_id: record_id.into(),
            source: Some(source),
        }
    }

    pub(crate) fn evidence(record_id: impl Into<String>) -> Self {
        Self {
            kind: HydrationKind::Evidence,
            record_id: record_id.into(),
            source: None,
        }
    }

    /// A self-describing handle; no process-local lookup table is required to hydrate it.
    pub(crate) fn encode(&self) -> String {
        let (root, path, start, end) = self.source.as_ref().map_or_else(
            || (String::new(), String::new(), 0, 0),
            |span| {
                (
                    hex(span.root.as_bytes()),
                    hex(span.path.as_bytes()),
                    span.start_line,
                    span.end_line,
                )
            },
        );
        let payload = format!(
            "rna-hydrate-v1:{}:{}:{root}:{path}:{start}:{end}",
            self.kind,
            hex(self.record_id.as_bytes())
        );
        let checksum = blake3::hash(payload.as_bytes()).to_hex();
        format!("{payload}:{}", &checksum.as_str()[..16])
    }

    /// Compact, self-describing form used in rendered packets. Unlike a
    /// process-local alias this remains independently resolvable after a
    /// restart; URL-safe base64 avoids the v1 hex expansion.
    pub(crate) fn encode_compact(&self) -> String {
        let (root, path, start, end) = self.source.as_ref().map_or_else(
            || (String::new(), String::new(), 0, 0),
            |span| {
                (
                    span.root.clone(),
                    span.path.clone(),
                    span.start_line,
                    span.end_line,
                )
            },
        );
        let payload = serde_json::to_vec(&(self.record_id.as_str(), root, path, start, end))
            .expect("hydration tuple serialization is infallible");
        let encoded = URL_SAFE_NO_PAD.encode(payload);
        let kind = match self.kind {
            HydrationKind::Source => "s",
            HydrationKind::Evidence => "e",
        };
        let prefix = format!("rna-h2:{kind}:{encoded}");
        let checksum = blake3::hash(prefix.as_bytes()).to_hex();
        format!("{prefix}:{}", &checksum.as_str()[..16])
    }

    pub(crate) fn decode(value: &str) -> Result<Self, String> {
        if value.starts_with("rna-h2:") {
            return Self::decode_compact(value);
        }
        let parts: Vec<_> = value.split(':').collect();
        if parts.len() != 8 || parts[0] != "rna-hydrate-v1" {
            return Err("invalid hydration handle shape".to_string());
        }
        let payload = parts[..7].join(":");
        let expected = blake3::hash(payload.as_bytes()).to_hex();
        if parts[7] != &expected.as_str()[..16] {
            return Err("hydration handle checksum mismatch".to_string());
        }
        let kind = match parts[1] {
            "source" => HydrationKind::Source,
            "evidence" => HydrationKind::Evidence,
            _ => return Err("invalid hydration handle kind".to_string()),
        };
        let record_id = unhex(parts[2])?;
        let start_line = parts[5]
            .parse()
            .map_err(|_| "invalid hydration start line")?;
        let end_line = parts[6].parse().map_err(|_| "invalid hydration end line")?;
        let source = if kind == HydrationKind::Source {
            let span = SourceSpan {
                root: unhex(parts[3])?,
                path: unhex(parts[4])?,
                start_line,
                end_line,
            };
            if !span.is_valid() {
                return Err("invalid hydration source span".to_string());
            }
            Some(span)
        } else if parts[3].is_empty() && parts[4].is_empty() && start_line == 0 && end_line == 0 {
            None
        } else {
            return Err("evidence handle unexpectedly contains a source span".to_string());
        };
        Ok(Self {
            kind,
            record_id,
            source,
        })
    }

    fn decode_compact(value: &str) -> Result<Self, String> {
        let parts = value.split(':').collect::<Vec<_>>();
        if parts.len() != 4 || parts[0] != "rna-h2" {
            return Err("invalid compact hydration handle shape".into());
        }
        let prefix = parts[..3].join(":");
        let expected = blake3::hash(prefix.as_bytes()).to_hex();
        if parts[3] != &expected.as_str()[..16] {
            return Err("hydration handle checksum mismatch".into());
        }
        let kind = match parts[1] {
            "s" => HydrationKind::Source,
            "e" => HydrationKind::Evidence,
            _ => return Err("invalid hydration handle kind".into()),
        };
        let bytes = URL_SAFE_NO_PAD
            .decode(parts[2])
            .map_err(|_| "invalid compact hydration payload")?;
        let (record_id, root, path, start_line, end_line): (String, String, String, u32, u32) =
            serde_json::from_slice(&bytes).map_err(|_| "invalid compact hydration tuple")?;
        let source = if kind == HydrationKind::Source {
            let span = SourceSpan {
                root,
                path,
                start_line,
                end_line,
            };
            if !span.is_valid() {
                return Err("invalid hydration source span".into());
            }
            Some(span)
        } else if root.is_empty() && path.is_empty() && start_line == 0 && end_line == 0 {
            None
        } else {
            return Err("evidence handle unexpectedly contains a source span".into());
        };
        Ok(Self {
            kind,
            record_id,
            source,
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn unhex(value: &str) -> Result<String, String> {
    if !value.len().is_multiple_of(2) {
        return Err("invalid hydration hex".to_string());
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let digit = |byte| match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            _ => Err("invalid hydration hex".to_string()),
        };
        bytes.push((digit(pair[0])? << 4) | digit(pair[1])?);
    }
    String::from_utf8(bytes).map_err(|_| "hydration handle is not UTF-8".to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RecordIdentity {
    pub(crate) node_id: String,
    pub(crate) source: Option<SourceSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SymbolSummary {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) language: String,
    pub(crate) signature: String,
    pub(crate) extraction_source: Option<String>,
    pub(crate) declared_metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SelectionSummary {
    pub(crate) channel: SelectionChannel,
    pub(crate) reason: String,
    pub(crate) role: Option<ContextRole>,
    pub(crate) lane: Option<RetrievalLane>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct EvidenceProvenance {
    pub(crate) source: String,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SelectionEvidence {
    pub(crate) raw_scores: BTreeMap<String, String>,
    pub(crate) content_hash: Option<String>,
    pub(crate) candidate_rank: Option<usize>,
    pub(crate) provenance: Vec<EvidenceProvenance>,
    pub(crate) diagnostics: BTreeMap<String, String>,
}

/// One member of the complete, bounded candidate set considered for a query.
///
/// Candidate audit is intentionally separate from [`SelectedRecord`]: the
/// default agent projection renders only selected context, while the evidence
/// projection retains both selected and omitted candidates with the exact
/// disposition reason and retrieval observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CandidateAudit {
    pub(crate) candidate_rank: usize,
    pub(crate) identity: RecordIdentity,
    pub(crate) disposition: CandidateDisposition,
    pub(crate) reason: String,
    pub(crate) evidence: SelectionEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SelectedRecord {
    pub(crate) selection_rank: usize,
    pub(crate) identity: RecordIdentity,
    pub(crate) symbol: SymbolSummary,
    pub(crate) selection: SelectionSummary,
    pub(crate) evidence: SelectionEvidence,
    /// Present only when the service can resolve this exact selection evidence
    /// without fabricating a graph identity for a non-graph result.
    pub(crate) evidence_hydration: Option<HydrationHandle>,
    pub(crate) focused_span: Option<SourceSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProjectedRecord {
    pub(crate) selection_rank: usize,
    pub(crate) identity: RecordIdentity,
    pub(crate) symbol: SymbolSummary,
    pub(crate) selection: SelectionSummary,
    pub(crate) evidence: SelectionEvidence,
    pub(crate) body: BodyRepresentation,
    pub(crate) span_ids: Vec<String>,
    pub(crate) source_handle: Option<HydrationHandle>,
    pub(crate) evidence_handle: Option<HydrationHandle>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SpanMapping {
    pub(crate) record_id: String,
    pub(crate) selection_rank: usize,
    pub(crate) selection: SelectionSummary,
    pub(crate) requested: SourceSpan,
    pub(crate) coverage: SpanCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProjectedSpan {
    pub(crate) source: SourceSpan,
    pub(crate) text: String,
    pub(crate) representation: BodyRepresentation,
    pub(crate) mappings: Vec<SpanMapping>,
    pub(crate) hydration: HydrationHandle,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct ProjectedRelationship {
    pub(crate) from: String,
    pub(crate) kind: String,
    pub(crate) to: String,
    pub(crate) reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProjectionOmission {
    pub(crate) record_id: Option<String>,
    pub(crate) source: Option<SourceSpan>,
    pub(crate) code: OmissionCode,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct CapabilityStatus {
    pub(crate) capability: String,
    pub(crate) state: CapabilityState,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProjectionBudget {
    pub(crate) max_rendered_bytes: Option<usize>,
    pub(crate) max_estimated_tokens: Option<usize>,
    pub(crate) per_record_body_bytes: Option<usize>,
    pub(crate) total_body_bytes: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProjectionRequest {
    pub(crate) intent: SearchIntent,
    pub(crate) projection: SearchProjection,
    pub(crate) body_policy: BodyPolicy,
    pub(crate) budget: ProjectionBudget,
}

impl Default for ProjectionRequest {
    fn default() -> Self {
        Self {
            intent: SearchIntent::Discover,
            projection: SearchProjection::Agent,
            body_policy: BodyPolicy::SignatureOnly,
            budget: ProjectionBudget::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProjectionInput {
    pub(crate) records: Vec<SelectedRecord>,
    pub(crate) candidate_audit: Vec<CandidateAudit>,
    pub(crate) relationships: Vec<ProjectedRelationship>,
    pub(crate) omissions: Vec<ProjectionOmission>,
    pub(crate) capabilities: Vec<CapabilityStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProjectionPlan {
    pub(crate) request: ProjectionRequest,
    pub(crate) records: Vec<ProjectedRecord>,
    pub(crate) candidate_audit: Vec<CandidateAudit>,
    pub(crate) spans: Vec<ProjectedSpan>,
    pub(crate) relationships: Vec<ProjectedRelationship>,
    pub(crate) omissions: Vec<ProjectionOmission>,
    pub(crate) capabilities: Vec<CapabilityStatus>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RenderCost {
    pub(crate) utf8_bytes: usize,
    pub(crate) unicode_chars: usize,
    pub(crate) estimated_tokens: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RenderAccounting {
    pub(crate) total: RenderCost,
    pub(crate) sections: BTreeMap<CostSection, RenderCost>,
    pub(crate) estimate_name: String,
    pub(crate) provider_usage: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RenderedResponse {
    pub(crate) text: String,
    pub(crate) accounting: RenderAccounting,
    pub(crate) plan: ProjectionPlan,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hydration_handles_are_stable_and_self_describing() {
        let handle = HydrationHandle::source(
            "node:β",
            SourceSpan {
                root: "repo".into(),
                path: "src/ü.rs".into(),
                start_line: 2,
                end_line: 7,
            },
        );
        assert_eq!(HydrationHandle::decode(&handle.encode()).unwrap(), handle);
        assert_eq!(handle.encode(), handle.encode());
        assert_eq!(
            HydrationHandle::decode(&handle.encode_compact()).unwrap(),
            handle
        );
        assert_eq!(handle.encode_compact(), handle.encode_compact());
        assert!(handle.encode_compact().len() < handle.encode().len());
    }

    #[test]
    fn source_span_identity_roundtrips_for_coalesced_hydration() {
        let span = SourceSpan {
            root: "repo:β".into(),
            path: "src/ü.rs".into(),
            start_line: 2,
            end_line: 300,
        };

        assert_eq!(SourceSpan::from_stable_id(&span.stable_id()).unwrap(), span);
        assert!(SourceSpan::from_stable_id("source:v1:00:00:0:1").is_err());
    }

    #[test]
    fn evidence_handles_cannot_smuggle_a_source() {
        let handle = HydrationHandle::evidence("node");
        assert_eq!(HydrationHandle::decode(&handle.encode()).unwrap(), handle);
        assert_eq!(
            HydrationHandle::decode(&handle.encode_compact()).unwrap(),
            handle
        );
    }

    #[test]
    fn hydration_handle_tampering_fails_closed() {
        let handle = HydrationHandle::evidence("node").encode();
        let (payload, checksum) = handle
            .rsplit_once(':')
            .expect("encoded handle has a checksum segment");
        let mut checksum = checksum.as_bytes().to_vec();
        checksum[0] = if checksum[0] == b'0' { b'1' } else { b'0' };
        let tampered = format!(
            "{payload}:{}",
            String::from_utf8(checksum).expect("checksum remains valid UTF-8")
        );

        assert_eq!(tampered.split(':').count(), 8);
        assert_eq!(
            HydrationHandle::decode(&tampered),
            Err("hydration handle checksum mismatch".to_string())
        );
    }
}
