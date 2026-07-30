//! Deterministic, fail-closed evidence for benchmark LSP completeness.
//!
//! It models the per-file contract, builds and persists canonical reports from
//! existing scan evidence, and evaluates readiness so scanners, CLI, and MCP
//! delivery share one definition.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as ShaDigest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::business_context::BusinessContextMode;
use crate::extract::lsp::work_items::{LspWorkItemRecord, LspWorkItemRecovery, LspWorkItemState};
use crate::extract::scan_stats::{
    LSP_VALIDATION_EVIDENCE_SCHEMA_VERSION, LspEnrichmentEntry, LspStatus, LspValidationEvidence,
    LspValidationStatus,
};
use crate::graph::{Edge, EdgeKind, ExtractionSource, Node, NodeKind};
use crate::server::{
    EnrichmentCapability, EnrichmentJobLedger, EnrichmentJobRecord, EnrichmentJobState,
    LspEvidenceReadiness,
};

pub const LSP_COMPLETENESS_SCHEMA_VERSION: u32 = 6;
pub const LSP_COMPLETENESS_REPORT_PATH: &str = ".oh/.cache/lsp_completeness.json";
pub const LSP_COMPLETENESS_SUMMARY_PATH: &str = ".oh/.cache/lsp_completeness_summary.json";
pub const LSP_COMPLETENESS_SUMMARY_COMMIT_PATH: &str =
    ".oh/.cache/lsp_completeness_summary.commit.json";
const LSP_COMPLETENESS_SUMMARY_SCHEMA_VERSION: u32 = 2;
const MAX_LSP_COMPLETENESS_SUMMARY_BYTES: usize = 4 * 1024;
const MAX_LSP_COMPLETENESS_SUMMARY_COMMIT_BYTES: usize = 2 * 1024;
static PERSIST_REPORT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub const FROZEN_SWEBENCH_COHORT_SIZE: u64 = 70;
const INVENTORY_POLICY_VERSION: &str = "swebench-file-inventory-v3";
const FROZEN_POPULATION_JSON: &[u8] =
    include_bytes!("../benchmark/swebench-act-context/population.json");
const FROZEN_PROTOCOL_LOCK_JSON: &[u8] =
    include_bytes!("../benchmark/swebench-act-context/protocol.lock.json");
const FROZEN_POPULATION_PATH: &str = "benchmark/swebench-act-context/population.json";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FileRole {
    Source,
    Test,
    Docs,
    Config,
    ExcludedGenerated,
    ExcludedBinary,
    ExcludedVendor,
    ExcludedData,
    ExcludedAsset,
}

impl FileRole {
    pub fn is_included(self) -> bool {
        matches!(self, Self::Source | Self::Test | Self::Docs | Self::Config)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Test => "test",
            Self::Docs => "docs",
            Self::Config => "config",
            Self::ExcludedGenerated => "excluded_generated",
            Self::ExcludedBinary => "excluded_binary",
            Self::ExcludedVendor => "excluded_vendor",
            Self::ExcludedData => "excluded_data",
            Self::ExcludedAsset => "excluded_asset",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReasonCode {
    Generated,
    Binary,
    Vendor,
    ConfiguredPolicy,
    NonLanguageData,
    Asset,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExclusionReason {
    pub code: ExclusionReasonCode,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ServerIdentity {
    pub name: String,
    pub version: Option<String>,
    pub executable_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct AdvertisedCapability {
    pub name: String,
    pub supported: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RequestOutcome {
    Completed,
    Unsupported,
    Failed,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct RequestAttempt {
    pub method: String,
    pub outcome: RequestOutcome,
    pub result_count: Option<u64>,
    pub duration_ms: Option<u64>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedResultKind {
    DocumentSymbol,
    Definition,
    Reference,
    CallHierarchy,
    DocumentLink,
    Diagnostic,
}

impl ExpectedResultKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::DocumentSymbol => "document_symbol",
            Self::Definition => "definition",
            Self::Reference => "reference",
            Self::CallHierarchy => "call_hierarchy",
            Self::DocumentLink => "document_link",
            Self::Diagnostic => "diagnostic",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct PersistedResults {
    pub document_symbols: u64,
    pub definitions: u64,
    pub references: u64,
    pub call_hierarchy_edges: u64,
    pub document_links: u64,
    pub diagnostics: u64,
    #[serde(default)]
    pub provenance: BTreeSet<String>,
}

impl PersistedResults {
    pub fn count(&self, kind: ExpectedResultKind) -> u64 {
        match kind {
            ExpectedResultKind::DocumentSymbol => self.document_symbols,
            ExpectedResultKind::Definition => self.definitions,
            ExpectedResultKind::Reference => self.references,
            ExpectedResultKind::CallHierarchy => self.call_hierarchy_edges,
            ExpectedResultKind::DocumentLink => self.document_links,
            ExpectedResultKind::Diagnostic => self.diagnostics,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FileTerminalStatus {
    Processed {
        /// Total results observed for the validation operation. Zero is a
        /// legitimate processed result and is not equivalent to skipped.
        result_count: u64,
    },
    MissingServer {
        detail: String,
    },
    UnsupportedExtension {
        detail: String,
    },
    NeverProcessed {
        detail: String,
    },
    Crashed {
        detail: String,
    },
    TimedOut {
        detail: String,
    },
    Partial {
        detail: String,
    },
    Degraded {
        detail: String,
    },
    Cancelled {
        detail: String,
    },
    Stale {
        detail: String,
    },
}

impl FileTerminalStatus {
    pub fn is_processed(&self) -> bool {
        matches!(self, Self::Processed { .. })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Processed { .. } => "processed",
            Self::MissingServer { .. } => "missing_server",
            Self::UnsupportedExtension { .. } => "unsupported_extension",
            Self::NeverProcessed { .. } => "never_processed",
            Self::Crashed { .. } => "crashed",
            Self::TimedOut { .. } => "timed_out",
            Self::Partial { .. } => "partial",
            Self::Degraded { .. } => "degraded",
            Self::Cancelled { .. } => "cancelled",
            Self::Stale { .. } => "stale",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileCoverageRecord {
    pub path: String,
    pub role: FileRole,
    pub language: Option<String>,
    pub expected_server: Option<ServerIdentity>,
    #[serde(default)]
    pub advertised_capabilities: Vec<AdvertisedCapability>,
    #[serde(default)]
    pub requests_attempted: Vec<RequestAttempt>,
    #[serde(default)]
    pub expected_results: BTreeSet<ExpectedResultKind>,
    /// Stable IDs emitted by completed LSP work and therefore required in the
    /// persisted graph. This detects partial persistence, not just total loss
    /// of a result kind.
    #[serde(default)]
    pub expected_result_ids: BTreeSet<String>,
    #[serde(default)]
    pub persisted_results: PersistedResults,
    pub terminal_status: FileTerminalStatus,
    pub exclusion: Option<ExclusionReason>,
}

impl FileCoverageRecord {
    pub fn canonicalize(&mut self) {
        if let Ok(path) = normalize_repo_relative_path(&self.path) {
            self.path = path;
        }
        self.advertised_capabilities.sort();
        self.advertised_capabilities.dedup();
        self.requests_attempted.sort();
        self.requests_attempted.dedup();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReportIdentity {
    pub schema_version: u32,
    pub checkout_sha: String,
    pub config_digest: String,
    pub policy_digest: String,
    pub context_mode: String,
    pub graph_schema_version: u32,
    pub enrichment_generation: String,
    /// Normalized upstream repository identity (`owner/repo`) when available.
    #[serde(default)]
    pub repository: String,
}

impl ReportIdentity {
    pub fn new(
        checkout_sha: impl Into<String>,
        config_digest: impl Into<String>,
        policy_digest: impl Into<String>,
        context_mode: impl Into<String>,
        graph_schema_version: u32,
        enrichment_generation: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: LSP_COMPLETENESS_SCHEMA_VERSION,
            checkout_sha: checkout_sha.into(),
            config_digest: config_digest.into(),
            policy_digest: policy_digest.into(),
            context_mode: context_mode.into(),
            graph_schema_version,
            enrichment_generation: enrichment_generation.into(),
            repository: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessViolationCode {
    InvalidPath,
    DuplicatePath,
    MissingServer,
    MissingServerVersion,
    MissingServerDigest,
    MissingAdvertisedCapabilities,
    MissingRequestEvidence,
    UnsupportedRelevantExtension,
    NotProcessed,
    MissingExpectedResult,
    InvalidEvidenceProvenance,
    MissingExclusionReason,
    IdentityMismatch,
    StaleReport,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReadinessViolation {
    pub code: ReadinessViolationCode,
    pub path: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReportSummary {
    pub total_files: u64,
    pub included_files: u64,
    pub excluded_files: u64,
    #[serde(default)]
    pub by_role: BTreeMap<String, u64>,
    #[serde(default)]
    pub by_status: BTreeMap<String, u64>,
    #[serde(default)]
    pub by_extension: BTreeMap<String, u64>,
}

/// Bounded query-path projection of the complete per-file report.
///
/// The full report can be hundreds of megabytes. Search reads only this fixed-
/// shape summary; detailed per-file evidence remains in the canonical report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LspCompletenessSummary {
    pub schema_version: u32,
    pub ready: bool,
    pub total_files: u64,
    pub included_files: u64,
    pub excluded_files: u64,
    pub violation_count: u64,
    pub report_digest: String,
    pub graph_snapshot_digest: String,
}

/// Last publication marker for the bounded summary/full-report pair.
///
/// The marker is published only after both files and binds the exact summary
/// bytes to independently observable full-report metadata. Search can reject
/// a stale, tampered, or interrupted summary without reading the full report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct LspCompletenessSummaryCommit {
    schema_version: u32,
    summary_sha256: String,
    report_digest: String,
    report_bytes: u64,
    report_modified_unix_nanos: u128,
}

impl LspCompletenessSummary {
    fn from_report(report: &LspCompletenessReport) -> Self {
        Self {
            schema_version: LSP_COMPLETENESS_SUMMARY_SCHEMA_VERSION,
            ready: report.is_ready(),
            total_files: report.summary.total_files,
            included_files: report.summary.included_files,
            excluded_files: report.summary.excluded_files,
            violation_count: report.violations.len() as u64,
            report_digest: report.digest.clone(),
            graph_snapshot_digest: report.graph_snapshot_digest.clone(),
        }
    }
}

/// Normalized proof for the current report generation. Inherited evidence is
/// portable only through a verified archive authorization; historical checkout
/// paths are deliberately absent from this identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LspEvidenceDisposition {
    Executed,
    VerifiedInherited,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct LspFileEvidence {
    pub path: String,
    pub disposition: LspEvidenceDisposition,
    pub generation: String,
    pub blob: String,
    pub partition_signature: String,
    #[serde(default)]
    pub input_hashes: Vec<String>,
    #[serde(default)]
    pub operations: Vec<String>,
    #[serde(default)]
    pub result_ids: Vec<String>,
    #[serde(default)]
    pub result_producers: Vec<crate::structural_cache::InheritedResultProducer>,
    pub base_archive_sha256: Option<String>,
    pub base_report_digest: Option<String>,
}

impl LspFileEvidence {
    fn canonicalize(&mut self) {
        if let Ok(path) = normalize_repo_relative_path(&self.path) {
            self.path = path;
        }
        self.input_hashes.sort();
        self.input_hashes.dedup();
        self.operations.sort();
        self.operations.dedup();
        self.result_ids.sort();
        self.result_ids.dedup();
        self.result_producers
            .sort_by(|left, right| left.result_id.cmp(&right.result_id));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LspCompletenessReport {
    pub identity: ReportIdentity,
    /// Canonical digest of the exact graph snapshot used to build this report.
    /// Unlike per-result IDs, this also binds legitimate zero-result reports.
    #[serde(default)]
    pub graph_snapshot_digest: String,
    #[serde(default)]
    pub files: Vec<FileCoverageRecord>,
    /// One current-generation evidence record per included file. Empty is
    /// accepted only for legacy/unit construction; producer reports always
    /// populate and validate this union.
    #[serde(default)]
    pub evidence: Vec<LspFileEvidence>,
    /// Exact readiness-validation requests represented by this report, keyed
    /// by descriptor language. Graph-enrichment operations remain separately
    /// accountable in the durable work ledger/execution receipt.
    #[serde(default)]
    pub readiness_validation_requests_by_language: BTreeMap<String, u64>,
    #[serde(default)]
    pub summary: ReportSummary,
    #[serde(default)]
    pub violations: Vec<ReadinessViolation>,
    pub digest: String,
}

impl LspCompletenessReport {
    pub fn new(identity: ReportIdentity, files: Vec<FileCoverageRecord>) -> Self {
        Self::new_bound(identity, files, &[], &[])
    }

    pub fn new_bound(
        identity: ReportIdentity,
        files: Vec<FileCoverageRecord>,
        nodes: &[Node],
        edges: &[Edge],
    ) -> Self {
        let mut report = Self {
            identity,
            graph_snapshot_digest: graph_snapshot_digest(nodes, edges),
            files,
            evidence: Vec::new(),
            readiness_validation_requests_by_language: BTreeMap::new(),
            summary: ReportSummary::default(),
            violations: Vec::new(),
            digest: String::new(),
        };
        report.finalize();
        report
    }

    fn new_bound_with_evidence(
        identity: ReportIdentity,
        files: Vec<FileCoverageRecord>,
        evidence: Vec<LspFileEvidence>,
        nodes: &[Node],
        edges: &[Edge],
    ) -> Self {
        let mut report = Self {
            identity,
            graph_snapshot_digest: graph_snapshot_digest(nodes, edges),
            files,
            evidence,
            readiness_validation_requests_by_language: BTreeMap::new(),
            summary: ReportSummary::default(),
            violations: Vec::new(),
            digest: String::new(),
        };
        report.finalize();
        report
    }

    pub fn finalize(&mut self) {
        for file in &mut self.files {
            file.canonicalize();
        }
        self.files.sort();
        for evidence in &mut self.evidence {
            evidence.canonicalize();
        }
        self.evidence.sort();
        self.summary = summarize(&self.files);
        self.violations = evaluate_files(&self.files);
        if !self.evidence.is_empty() {
            self.violations
                .extend(evaluate_evidence(&self.files, &self.evidence));
        }
        self.violations.sort();
        self.digest = self.compute_digest();
    }

    pub fn is_ready(&self) -> bool {
        self.violations.is_empty()
    }

    pub fn compute_digest(&self) -> String {
        #[derive(Serialize)]
        struct DigestPayload<'a> {
            identity: &'a ReportIdentity,
            graph_snapshot_digest: &'a str,
            files: &'a [FileCoverageRecord],
            evidence: &'a [LspFileEvidence],
            readiness_validation_requests_by_language: &'a BTreeMap<String, u64>,
            summary: &'a ReportSummary,
            violations: &'a [ReadinessViolation],
        }

        let bytes = serde_json::to_vec(&DigestPayload {
            identity: &self.identity,
            graph_snapshot_digest: &self.graph_snapshot_digest,
            files: &self.files,
            evidence: &self.evidence,
            readiness_validation_requests_by_language: &self
                .readiness_validation_requests_by_language,
            summary: &self.summary,
            violations: &self.violations,
        })
        .expect("LSP completeness report serialization cannot fail");
        blake3::hash(&bytes).to_hex().to_string()
    }

    pub fn integrity_violations(&self) -> Vec<ReadinessViolation> {
        let mut violations = Vec::new();
        if self.identity.schema_version != LSP_COMPLETENESS_SCHEMA_VERSION {
            violations.push(ReadinessViolation {
                code: ReadinessViolationCode::StaleReport,
                path: None,
                detail: format!(
                    "report schema {} does not match {}",
                    self.identity.schema_version, LSP_COMPLETENESS_SCHEMA_VERSION
                ),
            });
        }
        if self.digest != self.compute_digest() {
            violations.push(ReadinessViolation {
                code: ReadinessViolationCode::StaleReport,
                path: None,
                detail: "report digest does not match its canonical contents".to_string(),
            });
        }
        let expected_summary = summarize(&self.files);
        let mut expected_violations = evaluate_files(&self.files);
        if self.files.iter().any(|file| file.role.is_included()) && self.evidence.is_empty() {
            expected_violations.push(ReadinessViolation {
                code: ReadinessViolationCode::InvalidEvidenceProvenance,
                path: None,
                detail: "current-schema report has no per-file LSP evidence".to_string(),
            });
        } else {
            expected_violations.extend(evaluate_evidence(&self.files, &self.evidence));
        }
        expected_violations.sort();
        if self.summary != expected_summary || self.violations != expected_violations {
            violations.push(ReadinessViolation {
                code: ReadinessViolationCode::StaleReport,
                path: None,
                detail: "report summary/violations are not derived from current file evidence"
                    .to_string(),
            });
        }
        violations
    }

    pub fn compatibility_violations(&self, expected: &ReportIdentity) -> Vec<ReadinessViolation> {
        let mut violations = self.integrity_violations();
        for (field, actual, wanted) in [
            (
                "schema_version",
                self.identity.schema_version.to_string(),
                expected.schema_version.to_string(),
            ),
            (
                "checkout_sha",
                self.identity.checkout_sha.clone(),
                expected.checkout_sha.clone(),
            ),
            (
                "config_digest",
                self.identity.config_digest.clone(),
                expected.config_digest.clone(),
            ),
            (
                "policy_digest",
                self.identity.policy_digest.clone(),
                expected.policy_digest.clone(),
            ),
            (
                "context_mode",
                self.identity.context_mode.clone(),
                expected.context_mode.clone(),
            ),
            (
                "graph_schema_version",
                self.identity.graph_schema_version.to_string(),
                expected.graph_schema_version.to_string(),
            ),
            (
                "enrichment_generation",
                self.identity.enrichment_generation.clone(),
                expected.enrichment_generation.clone(),
            ),
            (
                "repository",
                self.identity.repository.clone(),
                expected.repository.clone(),
            ),
        ] {
            if actual != wanted {
                violations.push(ReadinessViolation {
                    code: if field == "enrichment_generation" {
                        ReadinessViolationCode::StaleReport
                    } else {
                        ReadinessViolationCode::IdentityMismatch
                    },
                    path: None,
                    detail: format!("{field} mismatch: report={actual:?}, expected={wanted:?}"),
                });
            }
        }
        violations.sort();
        violations.dedup();
        violations
    }
}

pub(crate) fn graph_snapshot_digest(nodes: &[Node], edges: &[Edge]) -> String {
    let mut entries = BTreeSet::new();
    for node in nodes {
        entries.insert(("node", node.stable_id(), format!("{:?}", node.source)));
    }
    for edge in edges {
        entries.insert(("edge", edge.stable_id(), format!("{:?}", edge.source)));
    }
    let bytes =
        serde_json::to_vec(&entries).expect("graph snapshot digest serialization cannot fail");
    blake3::hash(&bytes).to_hex().to_string()
}

fn evaluate_files(files: &[FileCoverageRecord]) -> Vec<ReadinessViolation> {
    let mut violations = Vec::new();
    let mut seen = BTreeSet::new();

    for file in files {
        let path = match normalize_repo_relative_path(&file.path) {
            Ok(path) => path,
            Err(detail) => {
                violations.push(ReadinessViolation {
                    code: ReadinessViolationCode::InvalidPath,
                    path: Some(file.path.clone()),
                    detail,
                });
                continue;
            }
        };

        if !seen.insert(path.clone()) {
            violations.push(ReadinessViolation {
                code: ReadinessViolationCode::DuplicatePath,
                path: Some(path.clone()),
                detail: "path appears more than once in completeness inventory".to_string(),
            });
        }

        if !file.role.is_included() {
            if file
                .exclusion
                .as_ref()
                .is_none_or(|reason| reason.detail.trim().is_empty())
            {
                violations.push(ReadinessViolation {
                    code: ReadinessViolationCode::MissingExclusionReason,
                    path: Some(path),
                    detail: "excluded file requires an explicit non-empty reason".to_string(),
                });
            }
            continue;
        }

        let Some(server) = &file.expected_server else {
            let code = if matches!(
                file.terminal_status,
                FileTerminalStatus::UnsupportedExtension { .. }
            ) {
                ReadinessViolationCode::UnsupportedRelevantExtension
            } else {
                ReadinessViolationCode::MissingServer
            };
            violations.push(ReadinessViolation {
                code,
                path: Some(path),
                detail: "included file has no locked language server".to_string(),
            });
            continue;
        };

        if server
            .version
            .as_ref()
            .is_none_or(|value| value.trim().is_empty())
        {
            violations.push(ReadinessViolation {
                code: ReadinessViolationCode::MissingServerVersion,
                path: Some(path.clone()),
                detail: format!("language server {} has no locked version", server.name),
            });
        }
        if server
            .executable_digest
            .as_ref()
            .is_none_or(|value| value.trim().is_empty())
        {
            violations.push(ReadinessViolation {
                code: ReadinessViolationCode::MissingServerDigest,
                path: Some(path.clone()),
                detail: format!("language server {} has no executable digest", server.name),
            });
        }

        if !file.terminal_status.is_processed() {
            let code = if matches!(
                file.terminal_status,
                FileTerminalStatus::UnsupportedExtension { .. }
            ) {
                ReadinessViolationCode::UnsupportedRelevantExtension
            } else {
                ReadinessViolationCode::NotProcessed
            };
            violations.push(ReadinessViolation {
                code,
                path: Some(path.clone()),
                detail: format!(
                    "included file ended with terminal status {}",
                    file.terminal_status.as_str()
                ),
            });
            continue;
        }

        if !file
            .advertised_capabilities
            .iter()
            .any(|capability| capability.supported)
        {
            violations.push(ReadinessViolation {
                code: ReadinessViolationCode::MissingAdvertisedCapabilities,
                path: Some(path.clone()),
                detail: "processed file has no supported negotiated operation capability"
                    .to_string(),
            });
        }
        if file.requests_attempted.is_empty() {
            violations.push(ReadinessViolation {
                code: ReadinessViolationCode::MissingRequestEvidence,
                path: Some(path.clone()),
                detail: "processed file has no persisted LSP request evidence".to_string(),
            });
        }
        for request in &file.requests_attempted {
            let required_capability = required_capability_for_method(&request.method);
            if let Some(capability_name) = required_capability
                && !file
                    .advertised_capabilities
                    .iter()
                    .any(|capability| capability.name == capability_name && capability.supported)
            {
                violations.push(ReadinessViolation {
                    code: ReadinessViolationCode::MissingAdvertisedCapabilities,
                    path: Some(path.clone()),
                    detail: format!(
                        "scheduled {} requires negotiated {capability_name}",
                        request.method
                    ),
                });
            }
        }
        for expected in &file.expected_results {
            if !file.requests_attempted.iter().any(|request| {
                request.outcome == RequestOutcome::Completed
                    && request_method_can_produce(&request.method, *expected)
            }) {
                violations.push(ReadinessViolation {
                    code: ReadinessViolationCode::MissingRequestEvidence,
                    path: Some(path.clone()),
                    detail: format!(
                        "applicable {} output has no completed scheduled request",
                        expected.as_str()
                    ),
                });
            }
        }

        for expected in &file.expected_results {
            if file.persisted_results.count(*expected) == 0 {
                violations.push(ReadinessViolation {
                    code: ReadinessViolationCode::MissingExpectedResult,
                    path: Some(path.clone()),
                    detail: format!(
                        "fixture declares applicable {} output but no result was persisted",
                        expected.as_str()
                    ),
                });
            }
        }
        for expected_id in file
            .expected_result_ids
            .difference(&file.persisted_results.provenance)
        {
            violations.push(ReadinessViolation {
                code: ReadinessViolationCode::MissingExpectedResult,
                path: Some(path.clone()),
                detail: format!("LSP result {expected_id} was emitted but not persisted"),
            });
        }
    }

    violations
}

fn required_capability_for_method(method: &str) -> Option<&'static str> {
    match method {
        "textDocument/documentSymbol" => Some("documentSymbolProvider"),
        "textDocument/documentLink" => Some("documentLinkProvider"),
        "textDocument/definition" => Some("definitionProvider"),
        "textDocument/references" => Some("referencesProvider"),
        "textDocument/implementation" => Some("implementationProvider"),
        "textDocument/prepareCallHierarchy+callHierarchy/*" => Some("callHierarchyProvider"),
        _ => None,
    }
}

fn request_method_can_produce(method: &str, expected: ExpectedResultKind) -> bool {
    match expected {
        ExpectedResultKind::DocumentSymbol => method == "textDocument/documentSymbol",
        ExpectedResultKind::Definition => matches!(
            method,
            "textDocument/definition"
                | "textDocument/implementation"
                | "textDocument/prepareTypeHierarchy+typeHierarchy/*"
        ),
        ExpectedResultKind::Reference => method == "textDocument/references",
        ExpectedResultKind::CallHierarchy => matches!(
            method,
            "textDocument/references" | "textDocument/prepareCallHierarchy+callHierarchy/*"
        ),
        ExpectedResultKind::DocumentLink => method == "textDocument/documentLink",
        ExpectedResultKind::Diagnostic => method == "textDocument/diagnostic",
    }
}

fn evaluate_evidence(
    files: &[FileCoverageRecord],
    evidence: &[LspFileEvidence],
) -> Vec<ReadinessViolation> {
    let included = files
        .iter()
        .filter(|file| file.role.is_included())
        .map(|file| (file.path.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    let mut by_path = BTreeMap::<&str, Vec<&LspFileEvidence>>::new();
    for item in evidence {
        by_path.entry(&item.path).or_default().push(item);
    }
    let mut violations = Vec::new();
    for (path, file) in included {
        let records = by_path.get(path).cloned().unwrap_or_default();
        if records.len() != 1 {
            violations.push(ReadinessViolation {
                code: ReadinessViolationCode::InvalidEvidenceProvenance,
                path: Some(path.to_string()),
                detail: format!(
                    "included file requires exactly one current-generation evidence record; found {}",
                    records.len()
                ),
            });
            continue;
        }
        let record = records[0];
        if record.generation.is_empty()
            || record.blob.len() != 40
            || !record.blob.bytes().all(|byte| byte.is_ascii_hexdigit())
            || record.partition_signature.len() != 64
            || !record
                .partition_signature
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || record.result_ids.iter().collect::<BTreeSet<_>>().len() != record.result_ids.len()
            || !file
                .expected_result_ids
                .iter()
                .all(|result| record.result_ids.contains(result))
        {
            violations.push(ReadinessViolation {
                code: ReadinessViolationCode::InvalidEvidenceProvenance,
                path: Some(path.to_string()),
                detail: "file evidence identity/result binding is incomplete".to_string(),
            });
        }
        match record.disposition {
            LspEvidenceDisposition::Executed => {
                if record.base_archive_sha256.is_some() || record.base_report_digest.is_some() {
                    violations.push(ReadinessViolation {
                        code: ReadinessViolationCode::InvalidEvidenceProvenance,
                        path: Some(path.to_string()),
                        detail: "executed evidence cannot claim an inherited base".to_string(),
                    });
                }
            }
            LspEvidenceDisposition::VerifiedInherited => {
                let archive_valid = record.base_archive_sha256.as_deref().is_some_and(|digest| {
                    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                });
                let producers = record
                    .result_producers
                    .iter()
                    .map(|lineage| (&lineage.result_id, &lineage.producer_ids))
                    .collect::<BTreeMap<_, _>>();
                let lineage_valid = record.result_ids.iter().all(|result| {
                    producers
                        .get(result)
                        .is_some_and(|producer_ids| !producer_ids.is_empty())
                });
                if !archive_valid
                    || record
                        .base_report_digest
                        .as_deref()
                        .is_none_or(str::is_empty)
                    || !lineage_valid
                {
                    violations.push(ReadinessViolation {
                        code: ReadinessViolationCode::InvalidEvidenceProvenance,
                        path: Some(path.to_string()),
                        detail:
                            "inherited evidence lacks verifier-bound archive/report/result lineage"
                                .to_string(),
                    });
                }
            }
        }
    }
    for item in evidence {
        if !files
            .iter()
            .any(|file| file.role.is_included() && file.path == item.path)
        {
            violations.push(ReadinessViolation {
                code: ReadinessViolationCode::InvalidEvidenceProvenance,
                path: Some(item.path.clone()),
                detail: "evidence path is not in the current included inventory".to_string(),
            });
        }
    }
    violations
}

fn summarize(files: &[FileCoverageRecord]) -> ReportSummary {
    let mut summary = ReportSummary {
        total_files: files.len() as u64,
        ..ReportSummary::default()
    };
    for file in files {
        if file.role.is_included() {
            summary.included_files += 1;
        } else {
            summary.excluded_files += 1;
        }
        *summary
            .by_role
            .entry(file.role.as_str().to_string())
            .or_default() += 1;
        *summary
            .by_status
            .entry(file.terminal_status.as_str().to_string())
            .or_default() += 1;
        *summary
            .by_extension
            .entry(extension_key(&file.path))
            .or_default() += 1;
    }
    summary
}

fn extension_key(path: &str) -> String {
    path.rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .filter(|extension| !extension.contains('/'))
        .unwrap_or_else(|| "<none>".to_string())
}

pub fn normalize_repo_relative_path(path: &str) -> Result<String, String> {
    if path.is_empty() || path.contains('\0') {
        return Err("path must be non-empty UTF-8 without NUL bytes".to_string());
    }
    let path = path.replace('\\', "/");
    if path.starts_with('/') || path.get(1..3) == Some(":/") {
        return Err("path must be repository-relative".to_string());
    }
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err("path escapes the repository root".to_string());
                }
            }
            value => components.push(value),
        }
    }
    if components.is_empty() {
        return Err("path resolves to the repository root".to_string());
    }
    Ok(components.join("/"))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AggregateCounts {
    pub checkouts: u64,
    pub unique_instances: u64,
    pub ready_checkouts: u64,
    pub files: u64,
    #[serde(default)]
    pub by_extension: BTreeMap<String, u64>,
    #[serde(default)]
    pub by_role: BTreeMap<String, u64>,
    #[serde(default)]
    pub by_status: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct AggregateCheckout {
    pub instance_id: String,
    pub repository: String,
    pub base_commit: String,
    pub checkout_sha: String,
    pub report_digest: String,
    pub ready: bool,
    pub file_count: u64,
    pub violation_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AggregateCompletenessReport {
    pub schema_version: u32,
    /// SHA-256 from the checked-in protocol lock for the frozen population.
    pub cohort_digest: String,
    pub checkouts: Vec<AggregateCheckout>,
    pub counts: AggregateCounts,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrozenCohortCase {
    pub instance_id: String,
    pub repository: String,
    pub base_commit: String,
    pub report_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrozenCohortManifest {
    pub schema_version: u32,
    pub cases: Vec<FrozenCohortCase>,
}

impl AggregateCompletenessReport {
    fn from_frozen_cases(
        cases: &[(FrozenCohortCase, LspCompletenessReport)],
        cohort_digest: String,
    ) -> Self {
        let mut checkouts = Vec::with_capacity(cases.len());
        let mut counts = AggregateCounts {
            checkouts: cases.len() as u64,
            ..AggregateCounts::default()
        };

        for (case, report) in cases {
            let ready = report.is_ready()
                && report.integrity_violations().is_empty()
                && report.identity.context_mode == "disabled"
                && report.identity.checkout_sha == case.base_commit
                && report.identity.repository == case.repository;
            if ready {
                counts.ready_checkouts += 1;
            }
            counts.files += report.summary.total_files;
            merge_counts(&mut counts.by_extension, &report.summary.by_extension);
            merge_counts(&mut counts.by_role, &report.summary.by_role);
            merge_counts(&mut counts.by_status, &report.summary.by_status);
            checkouts.push(AggregateCheckout {
                instance_id: case.instance_id.clone(),
                repository: case.repository.clone(),
                base_commit: case.base_commit.clone(),
                checkout_sha: report.identity.checkout_sha.clone(),
                report_digest: report.digest.clone(),
                ready,
                file_count: report.summary.total_files,
                violation_count: report.violations.len() as u64,
            });
        }
        checkouts.sort();
        counts.unique_instances = checkouts
            .iter()
            .map(|checkout| checkout.instance_id.as_str())
            .collect::<BTreeSet<_>>()
            .len() as u64;

        let mut aggregate = Self {
            schema_version: LSP_COMPLETENESS_SCHEMA_VERSION,
            cohort_digest,
            checkouts,
            counts,
            digest: String::new(),
        };
        aggregate.digest = aggregate.compute_digest();
        aggregate
    }

    pub fn compute_digest(&self) -> String {
        #[derive(Serialize)]
        struct DigestPayload<'a> {
            schema_version: u32,
            cohort_digest: &'a str,
            checkouts: &'a [AggregateCheckout],
            counts: &'a AggregateCounts,
        }
        let bytes = serde_json::to_vec(&DigestPayload {
            schema_version: self.schema_version,
            cohort_digest: &self.cohort_digest,
            checkouts: &self.checkouts,
            counts: &self.counts,
        })
        .expect("aggregate completeness serialization cannot fail");
        blake3::hash(&bytes).to_hex().to_string()
    }

    pub fn is_ready(&self) -> bool {
        let Ok((expected_digest, expected_cases)) = canonical_frozen_cohort() else {
            return false;
        };
        let actual_cases = self
            .checkouts
            .iter()
            .map(|checkout| {
                (
                    checkout.instance_id.clone(),
                    checkout.repository.clone(),
                    checkout.base_commit.clone(),
                )
            })
            .collect::<BTreeSet<_>>();
        self.cohort_digest == expected_digest
            && actual_cases == expected_cases
            && self.counts.checkouts == FROZEN_SWEBENCH_COHORT_SIZE
            && self.counts.unique_instances == FROZEN_SWEBENCH_COHORT_SIZE
            && self.counts.ready_checkouts == FROZEN_SWEBENCH_COHORT_SIZE
            && self.checkouts.iter().all(|checkout| checkout.ready)
    }
}

#[derive(Debug, Deserialize)]
struct CanonicalPopulation {
    instances: Vec<CanonicalPopulationCase>,
}

#[derive(Debug, Deserialize)]
struct CanonicalPopulationCase {
    instance_id: String,
    repo: String,
    base_commit: String,
    included: bool,
}

#[derive(Debug, Deserialize)]
struct ProtocolLock {
    files: Vec<ProtocolLockEntry>,
}

#[derive(Debug, Deserialize)]
struct ProtocolLockEntry {
    path: String,
    sha256: String,
}

type CanonicalCohortIdentity = (String, String, String);

fn canonical_frozen_cohort() -> Result<(String, BTreeSet<CanonicalCohortIdentity>)> {
    let lock: ProtocolLock = serde_json::from_slice(FROZEN_PROTOCOL_LOCK_JSON)
        .context("checked-in SWE-bench protocol lock is invalid")?;
    let locked_digest = lock
        .files
        .iter()
        .find(|entry| entry.path == FROZEN_POPULATION_PATH)
        .map(|entry| entry.sha256.as_str())
        .context("protocol lock does not seal the frozen SWE-bench population")?;
    let computed_digest = format!("{:x}", Sha256::digest(FROZEN_POPULATION_JSON));
    if computed_digest != locked_digest {
        anyhow::bail!(
            "checked-in frozen SWE-bench population digest does not match protocol.lock.json"
        );
    }
    let population: CanonicalPopulation = serde_json::from_slice(FROZEN_POPULATION_JSON)
        .context("checked-in frozen SWE-bench population is invalid")?;
    let cases = population
        .instances
        .into_iter()
        .filter(|case| case.included)
        .map(|case| (case.instance_id, case.repo, case.base_commit))
        .collect::<BTreeSet<_>>();
    if cases.len() as u64 != FROZEN_SWEBENCH_COHORT_SIZE {
        anyhow::bail!(
            "checked-in frozen SWE-bench population has {} included identities, expected {}",
            cases.len(),
            FROZEN_SWEBENCH_COHORT_SIZE
        );
    }
    Ok((locked_digest.to_string(), cases))
}

pub fn load_frozen_cohort_aggregate(path: &Path) -> Result<AggregateCompletenessReport> {
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read frozen cohort manifest {}", path.display()))?;
    let mut manifest: FrozenCohortManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid frozen cohort manifest {}", path.display()))?;
    if manifest.schema_version != LSP_COMPLETENESS_SCHEMA_VERSION {
        anyhow::bail!(
            "frozen cohort schema {} does not match {}",
            manifest.schema_version,
            LSP_COMPLETENESS_SCHEMA_VERSION
        );
    }
    manifest.cases.sort();
    let (cohort_digest, canonical_cases) = canonical_frozen_cohort()?;
    let supplied_cases = manifest
        .cases
        .iter()
        .map(|case| {
            (
                case.instance_id.clone(),
                case.repository.clone(),
                case.base_commit.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    if manifest.cases.len() as u64 != FROZEN_SWEBENCH_COHORT_SIZE
        || supplied_cases != canonical_cases
    {
        anyhow::bail!(
            "cohort manifest identities do not exactly match the locked N={} SWE-bench population",
            FROZEN_SWEBENCH_COHORT_SIZE
        );
    }
    let base = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let cases = manifest
        .cases
        .into_iter()
        .map(|case| {
            let report_path = if case.report_path.is_absolute() {
                case.report_path.clone()
            } else {
                base.join(&case.report_path)
            };
            let report = load_report_path(&report_path)?;
            Ok((case, report))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(AggregateCompletenessReport::from_frozen_cases(
        &cases,
        cohort_digest,
    ))
}

fn merge_counts(target: &mut BTreeMap<String, u64>, source: &BTreeMap<String, u64>) {
    for (key, value) in source {
        *target.entry(key.clone()).or_default() += value;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadinessCheck {
    pub report: LspCompletenessReport,
    #[serde(default)]
    pub compatibility_violations: Vec<ReadinessViolation>,
    pub ready: bool,
}

impl ReadinessCheck {
    pub fn from_report(report: LspCompletenessReport, expected: &ReportIdentity) -> Self {
        let compatibility_violations = report.compatibility_violations(expected);
        let ready = report.is_ready() && compatibility_violations.is_empty();
        Self {
            report,
            compatibility_violations,
            ready,
        }
    }

    pub fn human_summary(&self) -> String {
        format!(
            "LSP readiness: {} ({} files, {} included, {} excluded, {} coverage violation(s), {} compatibility violation(s), digest={})",
            if self.ready { "READY" } else { "BLOCKED" },
            self.report.summary.total_files,
            self.report.summary.included_files,
            self.report.summary.excluded_files,
            self.report.violations.len(),
            self.compatibility_violations.len(),
            self.report.digest,
        )
    }
}

/// Build and durably persist the report emitted by a full scan.
///
/// Coverage uses only work items updated during this scan. Older completed
/// records are intentionally ineligible, so a file cannot inherit success from
/// a previous checkout or enrichment generation.
pub fn build_and_persist_report(
    repo_root: &Path,
    context_mode: BusinessContextMode,
    nodes: &[Node],
    edges: &[Edge],
    lsp_entries: &[LspEnrichmentEntry],
    related_job_ids: &[String],
    scan_started_at_ms: u64,
) -> Result<LspCompletenessReport> {
    let inheritance = crate::structural_cache::load_verified_authorization(repo_root)?;
    let execution = crate::structural_cache::load_execution(repo_root)?;
    build_and_persist_report_from_evidence(
        repo_root,
        context_mode,
        nodes,
        edges,
        lsp_entries,
        related_job_ids,
        scan_started_at_ms,
        inheritance.as_ref(),
        execution.as_ref(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_and_persist_report_from_evidence(
    repo_root: &Path,
    context_mode: BusinessContextMode,
    nodes: &[Node],
    edges: &[Edge],
    lsp_entries: &[LspEnrichmentEntry],
    related_job_ids: &[String],
    scan_started_at_ms: u64,
    inheritance: Option<&crate::structural_cache::VerifiedStructuralCacheAuthorization>,
    execution: Option<&crate::structural_cache::StructuralCacheExecution>,
) -> Result<LspCompletenessReport> {
    let related_job_ids = report_related_job_ids(repo_root, related_job_ids, scan_started_at_ms)?;
    match (inheritance, execution) {
        (Some(authorization), Some(execution)) => {
            if execution.base_archive_sha256 != authorization.authorization.base_archive_sha256
                || execution.base_sidecar_sha256 != authorization.authorization.base_sidecar_sha256
                || execution.base_report_digest != authorization.authorization.base_report_digest
                || execution.target_commit != authorization.authorization.target_commit
                || execution.target_tree != authorization.authorization.target_tree
                || execution
                    .execution_job_id
                    .as_ref()
                    .is_some_and(|job_id| !related_job_ids.contains(job_id))
            {
                anyhow::bail!(
                    "structural cache execution evidence does not match authorization/current scan"
                );
            }
        }
        (None, None) => {}
        _ => anyhow::bail!(
            "structural cache authorization/execution evidence must be present as a verified pair"
        ),
    }
    let related_ids = related_job_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let authenticated_producer_ids = match (inheritance, execution) {
        (Some(_), Some(execution)) => Some(
            execution
                .executed_producer_work_ids
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>(),
        ),
        (None, None) => None,
        _ => unreachable!("authorization/execution pairing was validated above"),
    };
    let work_items = select_work_items_for_report(
        crate::extract::lsp::work_items::load_all_records(repo_root)?,
        &related_ids,
        scan_started_at_ms,
        nodes,
        authenticated_producer_ids.as_ref(),
    )?;
    let jobs = EnrichmentJobLedger::default()
        .all_jobs(repo_root)
        .into_iter()
        .filter(|job| {
            related_ids.contains(job.job_id.as_str())
                && job.capability == EnrichmentCapability::CallReferences
        })
        .collect::<Vec<_>>();
    let identity = current_report_identity(repo_root, context_mode)?;
    let report = build_report(
        repo_root,
        identity,
        nodes,
        edges,
        lsp_entries,
        &work_items,
        &jobs,
        inheritance,
        execution,
    )?;
    persist_report(repo_root, &report)?;
    Ok(report)
}

fn report_related_job_ids(
    repo_root: &Path,
    supplied_job_ids: &[String],
    scan_started_at_ms: u64,
) -> Result<Vec<String>> {
    let mut related_job_ids = supplied_job_ids.iter().cloned().collect::<BTreeSet<_>>();
    for snapshot in
        crate::extract::lsp::work_items::load_queue_snapshots_since(repo_root, scan_started_at_ms)?
    {
        related_job_ids.insert(snapshot.job_id);
    }
    Ok(related_job_ids.into_iter().collect())
}

#[cfg(test)]
fn filter_work_items_for_related_jobs(
    records: Vec<LspWorkItemRecord>,
    related_job_ids: &BTreeSet<&str>,
) -> Vec<LspWorkItemRecord> {
    records
        .into_iter()
        .filter(|record| related_job_ids.contains(record.job_id.as_str()))
        .collect()
}

fn select_work_items_for_report(
    records: Vec<LspWorkItemRecord>,
    related_job_ids: &BTreeSet<&str>,
    scan_started_at_ms: u64,
    nodes: &[Node],
    authenticated_producer_ids: Option<&BTreeSet<String>>,
) -> Result<Vec<LspWorkItemRecord>> {
    let nodes_by_id = nodes
        .iter()
        .map(|node| (node.stable_id(), node))
        .collect::<BTreeMap<_, _>>();
    let mut selected = Vec::new();
    let mut selected_identities = BTreeSet::new();
    let mut selected_producer_ids = BTreeSet::new();
    let mut observed_authenticated_producer_ids = BTreeSet::new();
    for record in records {
        let producer_id = format!("{}:{}", record.job_id, record.item_id);
        let current_job_record = record.updated_at_ms >= scan_started_at_ms
            && related_job_ids.contains(record.job_id.as_str());
        let authenticated_inherited_record = authenticated_producer_ids
            .is_some_and(|producer_ids| producer_ids.contains(&producer_id));
        let eligible = if authenticated_producer_ids.is_some() {
            authenticated_inherited_record
        } else {
            current_job_record
        };
        if !eligible {
            continue;
        }
        if !selected_producer_ids.insert(producer_id.clone()) {
            anyhow::bail!("duplicate LSP producer ID {producer_id}");
        }
        if authenticated_inherited_record && record.state != LspWorkItemState::Completed {
            anyhow::bail!("authenticated LSP producer {producer_id} is not completed");
        }
        if authenticated_inherited_record && !related_job_ids.contains(record.job_id.as_str()) {
            anyhow::bail!(
                "authenticated LSP producer {producer_id} is outside the current related jobs"
            );
        }
        if record.recovery == LspWorkItemRecovery::CarriedCompleted
            || authenticated_inherited_record
        {
            let node = nodes_by_id.get(&record.node_id).with_context(|| {
                format!(
                    "recovered LSP work {}:{} has no current graph input {}",
                    record.job_id, record.item_id, record.node_id
                )
            })?;
            let record_path =
                normalize_repo_relative_path(&record.file).map_err(anyhow::Error::msg)?;
            let node_path = normalize_repo_relative_path(&node.id.file.to_string_lossy())
                .map_err(anyhow::Error::msg)?;
            if record.root != node.id.root
                || record_path != node_path
                || record.input_hash != crate::extract::lsp::work_items::node_input_hash(node)
            {
                anyhow::bail!(
                    "recovered LSP work identity mismatch for {}:{}",
                    record.job_id,
                    record.item_id
                );
            }
            let output_ids = record
                .output_edges
                .iter()
                .map(Edge::stable_id)
                .chain(record.output_nodes.iter().map(Node::stable_id))
                .collect::<BTreeSet<_>>();
            if output_ids != record.produced_result_ids {
                anyhow::bail!(
                    "recovered LSP work output identity mismatch for {}:{}",
                    record.job_id,
                    record.item_id
                );
            }
        }
        let mut operations = record.requested_operations.clone();
        operations.sort();
        if operations.windows(2).any(|pair| pair[0] == pair[1]) {
            anyhow::bail!(
                "LSP work {}:{} contains duplicate requested operations",
                record.job_id,
                record.item_id
            );
        }
        let identity = (
            record.node_id.clone(),
            record.input_hash.clone(),
            operations,
        );
        if !selected_identities.insert(identity) {
            anyhow::bail!(
                "duplicate current LSP work identity for {}:{}",
                record.job_id,
                record.item_id
            );
        }
        if authenticated_inherited_record {
            observed_authenticated_producer_ids.insert(producer_id);
        }
        selected.push(record);
    }
    if authenticated_producer_ids
        .is_some_and(|producer_ids| producer_ids != &observed_authenticated_producer_ids)
    {
        anyhow::bail!("structural cache execution names missing LSP producer work");
    }
    selected.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then_with(|| left.job_id.cmp(&right.job_id))
            .then_with(|| left.item_id.cmp(&right.item_id))
    });
    Ok(selected)
}

pub fn load_readiness_check(
    repo_root: &Path,
    context_mode: BusinessContextMode,
) -> Result<ReadinessCheck> {
    let report = load_report(repo_root)?;
    let expected = current_report_identity(repo_root, context_mode)?;
    Ok(ReadinessCheck::from_report(report, &expected))
}

/// Reload a persisted report against the exact graph snapshot and installed
/// server binaries that will be delivered to the caller.
pub fn load_readiness_check_with_graph(
    repo_root: &Path,
    context_mode: BusinessContextMode,
    nodes: &[Node],
    edges: &[Edge],
) -> Result<ReadinessCheck> {
    let mut check = load_readiness_check(repo_root, context_mode)?;
    check
        .compatibility_violations
        .extend(runtime_compatibility_violations(
            &check.report,
            nodes,
            edges,
        ));
    check.compatibility_violations.sort();
    check.compatibility_violations.dedup();
    check.ready = check.report.is_ready() && check.compatibility_violations.is_empty();
    Ok(check)
}

pub fn report_path(repo_root: &Path) -> PathBuf {
    repo_root.join(LSP_COMPLETENESS_REPORT_PATH)
}

pub fn summary_path(repo_root: &Path) -> PathBuf {
    repo_root.join(LSP_COMPLETENESS_SUMMARY_PATH)
}

pub fn summary_commit_path(repo_root: &Path) -> PathBuf {
    repo_root.join(LSP_COMPLETENESS_SUMMARY_COMMIT_PATH)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn synced_json_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.update(b"\n");
    format!("{:x}", hasher.finalize())
}

fn report_file_identity(path: &Path) -> Result<(u64, u128)> {
    let metadata = fs::metadata(path).with_context(|| {
        format!(
            "failed to inspect LSP completeness report {}",
            path.display()
        )
    })?;
    let modified = metadata
        .modified()
        .with_context(|| format!("missing modification time for {}", path.display()))?
        .duration_since(UNIX_EPOCH)
        .with_context(|| {
            format!(
                "modification time predates UNIX epoch for {}",
                path.display()
            )
        })?;
    Ok((metadata.len(), modified.as_nanos()))
}

fn read_bounded_file(path: &Path, max_bytes: usize, label: &str) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("{label} is missing or unreadable at {}", path.display()))?;
    let mut bytes = Vec::with_capacity(max_bytes);
    file.take((max_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {label} at {}", path.display()))?;
    anyhow::ensure!(
        bytes.len() <= max_bytes,
        "{label} exceeds {max_bytes} bytes at {}",
        path.display()
    );
    Ok(bytes)
}

fn write_synced_temp(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn report_temp_paths(parent: &Path) -> [PathBuf; 3] {
    let sequence = PERSIST_REPORT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let suffix = format!("{}-{sequence}", std::process::id());
    [
        parent.join(format!(".lsp_completeness.json.tmp-{suffix}")),
        parent.join(format!(".lsp_completeness_summary.json.tmp-{suffix}")),
        parent.join(format!(
            ".lsp_completeness_summary.commit.json.tmp-{suffix}"
        )),
    ]
}

pub fn persist_report(repo_root: &Path, report: &LspCompletenessReport) -> Result<()> {
    let path = report_path(repo_root);
    let summary_path = summary_path(repo_root);
    let summary_commit_path = summary_commit_path(repo_root);
    let parent = path
        .parent()
        .context("LSP completeness report path has no parent")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create LSP completeness directory {}",
            parent.display()
        )
    })?;
    let [temp, summary_temp, summary_commit_temp] = report_temp_paths(parent);
    let bytes = serde_json::to_vec_pretty(report)?;
    let summary_bytes = serde_json::to_vec_pretty(&LspCompletenessSummary::from_report(report))?;
    anyhow::ensure!(
        summary_bytes.len() < MAX_LSP_COMPLETENESS_SUMMARY_BYTES,
        "LSP completeness summary exceeds {} bytes",
        MAX_LSP_COMPLETENESS_SUMMARY_BYTES
    );
    write_synced_temp(&temp, &bytes)?;
    write_synced_temp(&summary_temp, &summary_bytes)?;
    // Invalidate the old publication marker before changing either member of
    // the pair. Any interruption from here until the final marker rename is
    // reported as `status unverified`.
    for stale_path in [&summary_commit_path, &summary_path] {
        match fs::remove_file(stale_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                for temp_path in [&temp, &summary_temp, &summary_commit_temp] {
                    let _ = fs::remove_file(temp_path);
                }
                return Err(error).with_context(|| {
                    format!(
                        "failed to invalidate LSP completeness sidecar {}",
                        stale_path.display()
                    )
                });
            }
        }
    }
    fs::rename(&temp, &path).with_context(|| {
        format!(
            "failed to atomically replace LSP completeness report {}",
            path.display()
        )
    })?;
    fs::rename(&summary_temp, &summary_path).with_context(|| {
        format!(
            "failed to atomically replace LSP completeness summary {}",
            summary_path.display()
        )
    })?;
    let (report_bytes, report_modified_unix_nanos) = report_file_identity(&path)?;
    let summary_commit = LspCompletenessSummaryCommit {
        schema_version: LSP_COMPLETENESS_SUMMARY_SCHEMA_VERSION,
        summary_sha256: synced_json_sha256(&summary_bytes),
        report_digest: report.digest.clone(),
        report_bytes,
        report_modified_unix_nanos,
    };
    let summary_commit_bytes = serde_json::to_vec_pretty(&summary_commit)?;
    anyhow::ensure!(
        summary_commit_bytes.len() < MAX_LSP_COMPLETENESS_SUMMARY_COMMIT_BYTES,
        "LSP completeness summary commit exceeds {} bytes",
        MAX_LSP_COMPLETENESS_SUMMARY_COMMIT_BYTES
    );
    write_synced_temp(&summary_commit_temp, &summary_commit_bytes)?;
    fs::rename(&summary_commit_temp, &summary_commit_path).with_context(|| {
        format!(
            "failed to atomically publish LSP completeness summary commit {}",
            summary_commit_path.display()
        )
    })?;
    Ok(())
}

/// Invalidate the bounded readiness publication before the persisted graph is
/// mutated. The full report remains available for diagnostics, but readers
/// must not treat its summary as current until a report for the new graph has
/// been published.
pub(crate) fn invalidate_summary_publication(repo_root: &Path) -> Result<()> {
    let path = summary_commit_path(repo_root);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to invalidate LSP completeness publication {}",
                path.display()
            )
        }),
    }
}

pub fn load_summary(repo_root: &Path) -> Result<LspCompletenessSummary> {
    let path = summary_path(repo_root);
    let bytes = read_bounded_file(
        &path,
        MAX_LSP_COMPLETENESS_SUMMARY_BYTES,
        "LSP completeness summary",
    )?;
    let summary: LspCompletenessSummary = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid LSP completeness summary at {}", path.display()))?;
    anyhow::ensure!(
        summary.schema_version == LSP_COMPLETENESS_SUMMARY_SCHEMA_VERSION,
        "unsupported LSP completeness summary schema {} at {}",
        summary.schema_version,
        path.display()
    );
    let commit_path = summary_commit_path(repo_root);
    let commit_bytes = read_bounded_file(
        &commit_path,
        MAX_LSP_COMPLETENESS_SUMMARY_COMMIT_BYTES,
        "LSP completeness summary commit",
    )?;
    let commit: LspCompletenessSummaryCommit =
        serde_json::from_slice(&commit_bytes).with_context(|| {
            format!(
                "invalid LSP completeness summary commit at {}",
                commit_path.display()
            )
        })?;
    anyhow::ensure!(
        commit.schema_version == LSP_COMPLETENESS_SUMMARY_SCHEMA_VERSION,
        "unsupported LSP completeness summary commit schema {} at {}",
        commit.schema_version,
        commit_path.display()
    );
    anyhow::ensure!(
        commit.summary_sha256 == sha256_hex(&bytes),
        "LSP completeness summary does not match its publication commit at {}",
        path.display()
    );
    anyhow::ensure!(
        commit.report_digest == summary.report_digest,
        "LSP completeness summary digest does not match its publication commit at {}",
        path.display()
    );
    let report = report_path(repo_root);
    let (report_bytes, report_modified_unix_nanos) = report_file_identity(&report)?;
    anyhow::ensure!(
        report_bytes == commit.report_bytes
            && report_modified_unix_nanos == commit.report_modified_unix_nanos,
        "LSP completeness report identity does not match the bounded summary publication at {}",
        report.display()
    );
    Ok(summary)
}

pub fn load_report(repo_root: &Path) -> Result<LspCompletenessReport> {
    let path = report_path(repo_root);
    let bytes = fs::read(&path).with_context(|| {
        format!(
            "LSP completeness report is missing or unreadable at {}; run a full LSP scan first",
            path.display()
        )
    })?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid LSP completeness report at {}", path.display()))
}

pub fn load_report_path(path: &Path) -> Result<LspCompletenessReport> {
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read LSP completeness report {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid LSP completeness report at {}", path.display()))
}

pub fn persist_aggregate_report(
    path: &Path,
    aggregate: &AggregateCompletenessReport,
) -> Result<()> {
    let parent = aggregate_output_parent(path);
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".lsp_completeness_aggregate.json.tmp-{}",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temp)?;
    file.write_all(&serde_json::to_vec_pretty(aggregate)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temp, path).with_context(|| {
        format!(
            "failed to atomically replace aggregate LSP report {}",
            path.display()
        )
    })?;
    Ok(())
}

fn aggregate_output_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub fn current_report_identity(
    repo_root: &Path,
    context_mode: BusinessContextMode,
) -> Result<ReportIdentity> {
    let paths = inventory_paths(repo_root)?;
    let checkout_sha = checkout_identity(repo_root, &paths)?;
    let config_digest = config_digest(repo_root);
    let policy_digest = blake3::hash(INVENTORY_POLICY_VERSION.as_bytes())
        .to_hex()
        .to_string();
    let enrichment_generation = work_item_generation(repo_root)?;
    let mut identity = ReportIdentity::new(
        checkout_sha,
        config_digest,
        policy_digest,
        context_mode.to_string(),
        crate::graph::store::SCHEMA_VERSION,
        enrichment_generation,
    );
    identity.repository = repository_identity(repo_root).unwrap_or_default();
    Ok(identity)
}

#[allow(clippy::too_many_arguments)]
fn build_report(
    repo_root: &Path,
    identity: ReportIdentity,
    nodes: &[Node],
    edges: &[Edge],
    lsp_entries: &[LspEnrichmentEntry],
    work_items: &[LspWorkItemRecord],
    jobs: &[EnrichmentJobRecord],
    inheritance: Option<&crate::structural_cache::VerifiedStructuralCacheAuthorization>,
    execution: Option<&crate::structural_cache::StructuralCacheExecution>,
) -> Result<LspCompletenessReport> {
    let paths = inventory_paths(repo_root)?;
    let extracted_paths = nodes
        .iter()
        .filter_map(|node| normalize_repo_relative_path(&node.id.file.to_string_lossy()).ok())
        .collect::<BTreeSet<_>>();
    let mut work_by_path: BTreeMap<String, Vec<&LspWorkItemRecord>> = BTreeMap::new();
    for item in work_items {
        if let Ok(path) = normalize_repo_relative_path(&item.file) {
            work_by_path.entry(path).or_default().push(item);
        }
    }
    let mut result_producers = validation_result_producers(jobs);
    for item in work_items
        .iter()
        .filter(|item| item.state == LspWorkItemState::Completed)
    {
        let Ok(path) = normalize_repo_relative_path(&item.file) else {
            continue;
        };
        let producer_id = format!("{}:{}", item.job_id, item.item_id);
        let mut result_ids = item.produced_result_ids.clone();
        result_ids.extend(item.output_edges.iter().map(Edge::stable_id));
        result_ids.extend(item.output_nodes.iter().map(Node::stable_id));
        for result_id in result_ids {
            result_producers
                .entry((path.clone(), result_id))
                .or_default()
                .insert(producer_id.clone());
        }
    }
    let mut entries_by_language: BTreeMap<&str, Vec<&LspEnrichmentEntry>> = BTreeMap::new();
    for entry in lsp_entries {
        entries_by_language
            .entry(entry.language.as_str())
            .or_default()
            .push(entry);
    }
    let mut server_identities = BTreeMap::new();
    let persisted_by_path = persisted_results_by_path(nodes, edges);
    let blob_ids = crate::structural_cache::current_blob_ids(repo_root)?;
    let cache_identity =
        crate::structural_cache::current_identity(repo_root, BusinessContextMode::Disabled)?;
    let inherited_paths = execution
        .map(|execution| {
            execution
                .inherited_paths
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let base_files = inheritance
        .map(|inheritance| {
            inheritance
                .base_report
                .files
                .iter()
                .map(|file| (file.path.as_str(), file))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let mut readiness_validation_requests_by_language = BTreeMap::<String, u64>::new();
    for validation in jobs
        .iter()
        .filter_map(|job| job.lsp_evidence.as_ref())
        .flat_map(|evidence| evidence.validations.iter())
        .filter(|validation| validation.method.is_some())
    {
        *readiness_validation_requests_by_language
            .entry(validation.language.clone())
            .or_default() += 1;
    }
    if let (Some(inheritance), Some(execution)) = (inheritance, execution) {
        let inherited_paths = execution
            .inherited_paths
            .iter()
            .map(PathBuf::from)
            .collect::<BTreeSet<_>>();
        let inherited_requests =
            inheritance.inherited_readiness_validation_requests_by_language(&inherited_paths)?;
        let inherited_request_count = inherited_requests.values().sum::<u64>();
        for (language, count) in inherited_requests {
            *readiness_validation_requests_by_language
                .entry(language)
                .or_default() += count;
        }
        let executed_request_count = jobs
            .iter()
            .filter_map(|job| job.lsp_evidence.as_ref())
            .flat_map(|evidence| evidence.validations.iter())
            .filter(|validation| validation.method.is_some())
            .count() as u64;
        if inherited_request_count != execution.inherited_readiness_validation_request_count
            || executed_request_count != execution.executed_readiness_validation_request_count
        {
            anyhow::bail!(
                "structural cache readiness-validation request accounting mismatch: \
                 inherited report={inherited_request_count} execution={}, \
                 executed report={executed_request_count} execution={}",
                execution.inherited_readiness_validation_request_count,
                execution.executed_readiness_validation_request_count,
            );
        }
    }
    let mut files = Vec::with_capacity(paths.len());
    let mut evidence = Vec::new();

    for path in paths {
        let normalized =
            normalize_repo_relative_path(&path.to_string_lossy()).map_err(anyhow::Error::msg)?;
        let absolute = repo_root.join(&path);
        let (role, exclusion) = classify_file(&path, &absolute);
        if !role.is_included() {
            files.push(FileCoverageRecord {
                path: normalized,
                role,
                language: None,
                expected_server: None,
                advertised_capabilities: Vec::new(),
                requests_attempted: Vec::new(),
                expected_results: BTreeSet::new(),
                expected_result_ids: BTreeSet::new(),
                persisted_results: PersistedResults::default(),
                terminal_status: FileTerminalStatus::NeverProcessed {
                    detail: "excluded by versioned benchmark inventory policy".to_string(),
                },
                exclusion,
            });
            continue;
        }

        let descriptor =
            crate::extract::lsp::builtin_lsp_descriptor_for_inventory_file(&path, &absolute);
        let Some(descriptor) = descriptor else {
            files.push(FileCoverageRecord {
                path: normalized,
                role,
                language: None,
                expected_server: None,
                advertised_capabilities: Vec::new(),
                requests_attempted: Vec::new(),
                expected_results: BTreeSet::new(),
                expected_result_ids: BTreeSet::new(),
                persisted_results: PersistedResults::default(),
                terminal_status: FileTerminalStatus::UnsupportedExtension {
                    detail: format!(
                        "no built-in LSP descriptor covers extension {}",
                        extension_key(&path.to_string_lossy())
                    ),
                },
                exclusion: None,
            });
            continue;
        };

        let server = server_identities
            .entry(descriptor.command().to_string())
            .or_insert_with(|| probe_server_identity(descriptor.command()))
            .clone();
        let language_entries = entries_by_language
            .get(descriptor.language())
            .cloned()
            .unwrap_or_default();
        let records = work_by_path.get(&normalized).cloned().unwrap_or_default();
        let language_validations = job_validations_for_language(jobs, descriptor.language());
        let validations = language_validations
            .into_iter()
            .filter(|validation| validation_applies_to_path(validation, repo_root, &normalized))
            .collect::<Vec<_>>();
        let (advertised_capabilities, requests_attempted) =
            evidence_from_work_items(&records, &language_entries, &validations);
        let persisted_results = persisted_by_path
            .get(&normalized)
            .cloned()
            .unwrap_or_default();
        if inherited_paths.contains(&normalized) {
            let authorized = inheritance
                .and_then(|inheritance| inheritance.inherited_by_path.get(&normalized))
                .with_context(|| {
                    format!("execution inherited {normalized} without verified authorization")
                })?;
            let base = base_files.get(normalized.as_str()).with_context(|| {
                format!("base report has no inherited evidence for {normalized}")
            })?;
            if crate::structural_cache::canonical_json_sha256(*base)? != authorized.base_file_sha256
            {
                anyhow::bail!("inherited base file evidence digest mismatch for {normalized}");
            }
            let authorized_result_ids = authorized
                .expected_result_ids
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            if base.language.as_deref() != Some(descriptor.language())
                || base.role != role
                || base.expected_result_ids != authorized_result_ids
            {
                anyhow::bail!("inherited file evidence identity mismatch for {normalized}");
            }
            let partition_signature = cache_identity
                .partitions
                .get(descriptor.language())
                .map(|partition| partition.signature.clone())
                .with_context(|| format!("missing partition for {}", descriptor.language()))?;
            if partition_signature != authorized.partition_signature {
                anyhow::bail!("inherited partition changed for {normalized}");
            }
            files.push(FileCoverageRecord {
                path: normalized.clone(),
                role,
                language: Some(descriptor.language().to_string()),
                expected_server: Some(server),
                advertised_capabilities: base.advertised_capabilities.clone(),
                requests_attempted: base.requests_attempted.clone(),
                expected_results: base.expected_results.clone(),
                expected_result_ids: base.expected_result_ids.clone(),
                persisted_results,
                terminal_status: base.terminal_status.clone(),
                exclusion: None,
            });
            evidence.push(LspFileEvidence {
                path: normalized,
                disposition: LspEvidenceDisposition::VerifiedInherited,
                generation: identity.enrichment_generation.clone(),
                blob: authorized.blob.clone(),
                partition_signature,
                input_hashes: authorized.input_hashes.clone(),
                operations: authorized.operations.clone(),
                result_ids: authorized.expected_result_ids.clone(),
                result_producers: authorized.result_producers.clone(),
                base_archive_sha256: inheritance
                    .map(|inheritance| inheritance.authorization.base_archive_sha256.clone()),
                base_report_digest: inheritance
                    .map(|inheritance| inheritance.authorization.base_report_digest.clone()),
            });
            continue;
        }
        let (expected_results, expected_result_ids) =
            expected_evidence_from_work_items(&records, &validations, &normalized);
        let terminal_status = terminal_status_for_file(
            &normalized,
            &records,
            &language_entries,
            extracted_paths.contains(&normalized),
            &server,
            jobs,
            &validations,
        );
        evidence.push(LspFileEvidence {
            path: normalized.clone(),
            disposition: LspEvidenceDisposition::Executed,
            generation: identity.enrichment_generation.clone(),
            blob: blob_ids
                .get(&normalized)
                .cloned()
                .unwrap_or_else(|| "0".repeat(40)),
            partition_signature: cache_identity
                .partitions
                .get(descriptor.language())
                .map(|partition| partition.signature.clone())
                .unwrap_or_default(),
            input_hashes: records
                .iter()
                .map(|record| record.input_hash.clone())
                .collect(),
            operations: records
                .iter()
                .flat_map(|record| record.requested_operations.clone())
                .chain(
                    requests_attempted
                        .iter()
                        .map(|request| request.method.clone()),
                )
                .collect(),
            result_ids: expected_result_ids.iter().cloned().collect(),
            result_producers: expected_result_ids
                .iter()
                .map(
                    |result_id| crate::structural_cache::InheritedResultProducer {
                        result_id: result_id.clone(),
                        producer_ids: result_producers
                            .get(&(normalized.clone(), result_id.clone()))
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .collect(),
                    },
                )
                .collect(),
            base_archive_sha256: None,
            base_report_digest: None,
        });
        files.push(FileCoverageRecord {
            path: normalized,
            role,
            language: Some(descriptor.language().to_string()),
            expected_server: Some(server),
            advertised_capabilities,
            requests_attempted,
            expected_results,
            expected_result_ids,
            persisted_results,
            terminal_status,
            exclusion: None,
        });
    }

    let mut report =
        LspCompletenessReport::new_bound_with_evidence(identity, files, evidence, nodes, edges);
    report.readiness_validation_requests_by_language = readiness_validation_requests_by_language;
    report.finalize();
    Ok(report)
}

fn evidence_from_work_items(
    records: &[&LspWorkItemRecord],
    language_entries: &[&LspEnrichmentEntry],
    validations: &[&LspValidationEvidence],
) -> (Vec<AdvertisedCapability>, Vec<RequestAttempt>) {
    fn insert_negotiated_capabilities(
        capabilities: &mut BTreeSet<AdvertisedCapability>,
        validation: &LspValidationEvidence,
    ) {
        let Some(negotiated) = validation.negotiated_capabilities else {
            return;
        };
        for (name, supported) in [
            ("referencesProvider", negotiated.references_provider),
            ("callHierarchyProvider", negotiated.call_hierarchy_provider),
            ("definitionProvider", negotiated.definition_provider),
            ("implementationProvider", negotiated.implementation_provider),
            ("documentLinkProvider", negotiated.document_link_provider),
            (
                "documentSymbolProvider",
                negotiated.document_symbol_provider,
            ),
            ("codeActionProvider", negotiated.code_action_provider),
        ] {
            capabilities.insert(AdvertisedCapability {
                name: name.to_string(),
                supported,
            });
        }
    }

    let mut capabilities = BTreeSet::new();
    let mut requests = Vec::new();
    for entry in language_entries {
        if let Some(validation) = &entry.validation {
            insert_negotiated_capabilities(&mut capabilities, validation);
        }
    }
    for validation in validations {
        insert_negotiated_capabilities(&mut capabilities, validation);
        if validation.status == LspValidationStatus::Processed
            && let Some(method) = validation.method.as_deref()
            // A graph-backed work item already supplies the exact request and
            // result evidence for this file. Keep the validation's negotiated
            // capabilities, but do not project the initialization/readiness
            // request as a second attempt. Inventory-only files have no work
            // item, so their file-scoped validation remains the durable proof.
            && !(method == "textDocument/documentSymbol"
                && records.iter().any(|record| {
                    record
                        .requested_operations
                        .iter()
                        .any(|operation| operation == "document_symbols")
                }))
        {
            requests.push(RequestAttempt {
                method: method.to_string(),
                outcome: RequestOutcome::Completed,
                result_count: validation.symbol_count.map(|count| count as u64),
                duration_ms: validation.duration_ms,
                detail: validation.detail.clone(),
            });
        }
    }
    if !records.is_empty() {
        requests.push(RequestAttempt {
            method: "textDocument/didOpen".to_string(),
            outcome: aggregate_request_outcome(records),
            result_count: None,
            duration_ms: None,
            detail: None,
        });
    }
    for record in records {
        for operation in &record.requested_operations {
            let method = lsp_method(operation).to_string();
            requests.push(RequestAttempt {
                method,
                outcome: request_outcome(record.state),
                result_count: Some(record.observed_result_count),
                duration_ms: record
                    .started_at_ms
                    .zip(record.completed_at_ms)
                    .map(|(start, end)| end.saturating_sub(start)),
                detail: record.last_error.clone(),
            });
        }
    }
    (capabilities.into_iter().collect(), requests)
}

fn aggregate_request_outcome(records: &[&LspWorkItemRecord]) -> RequestOutcome {
    if records.iter().any(|record| {
        matches!(
            record.state,
            LspWorkItemState::Failed | LspWorkItemState::Exhausted
        )
    }) {
        RequestOutcome::Failed
    } else if records.iter().any(|record| {
        matches!(
            record.state,
            LspWorkItemState::Pending | LspWorkItemState::InFlight
        )
    }) {
        RequestOutcome::Cancelled
    } else if records
        .iter()
        .all(|record| record.state == LspWorkItemState::Skipped)
    {
        RequestOutcome::Unsupported
    } else {
        RequestOutcome::Completed
    }
}

fn request_outcome(state: LspWorkItemState) -> RequestOutcome {
    match state {
        LspWorkItemState::Completed => RequestOutcome::Completed,
        LspWorkItemState::Skipped => RequestOutcome::Unsupported,
        LspWorkItemState::Failed | LspWorkItemState::Exhausted => RequestOutcome::Failed,
        LspWorkItemState::Pending | LspWorkItemState::InFlight => RequestOutcome::Cancelled,
    }
}

fn lsp_method(operation: &str) -> &'static str {
    match operation {
        "call_hierarchy" => "textDocument/prepareCallHierarchy+callHierarchy/*",
        "references" => "textDocument/references",
        "definitions" => "textDocument/definition",
        "implementations" => "textDocument/implementation",
        "type_hierarchy" => "textDocument/prepareTypeHierarchy+typeHierarchy/*",
        "document_symbols" => "textDocument/documentSymbol",
        "document_links" => "textDocument/documentLink",
        _ => "unknown",
    }
}

fn persisted_results_by_path(nodes: &[Node], edges: &[Edge]) -> BTreeMap<String, PersistedResults> {
    let mut by_path = BTreeMap::<String, PersistedResults>::new();
    let persisted_lsp_node_ids = nodes
        .iter()
        .filter(|node| node.source == ExtractionSource::Lsp)
        .map(Node::stable_id)
        .collect::<BTreeSet<_>>();
    for node in nodes
        .iter()
        .filter(|node| node.source == ExtractionSource::Lsp)
    {
        let Ok(path) = normalize_repo_relative_path(&node.id.file.to_string_lossy()) else {
            continue;
        };
        let results = by_path.entry(path).or_default();
        results.provenance.insert(node.stable_id());
        match &node.id.kind {
            NodeKind::Other(kind) if kind == "lsp_document_symbol" => {
                results.document_symbols += 1;
            }
            NodeKind::Other(kind) if kind == "diagnostic" => {
                results.diagnostics += 1;
            }
            _ => {}
        }
    }
    for edge in edges
        .iter()
        .filter(|edge| edge.source == ExtractionSource::Lsp)
    {
        let mut endpoint_paths = [
            normalize_repo_relative_path(&edge.from.file.to_string_lossy()).ok(),
            normalize_repo_relative_path(&edge.to.file.to_string_lossy()).ok(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        endpoint_paths.sort();
        endpoint_paths.dedup();
        let stable_id = edge.stable_id();
        let persisted_endpoint_ids = [&edge.from, &edge.to]
            .into_iter()
            .map(|endpoint| endpoint.to_stable_id())
            .filter(|endpoint_id| persisted_lsp_node_ids.contains(endpoint_id))
            .collect::<Vec<_>>();
        for path in endpoint_paths {
            let results = by_path.entry(path).or_default();
            results.provenance.insert(stable_id.clone());
            // A call-hierarchy work item can materialize an endpoint in a
            // different file or with a path-free external identity. Attribute
            // only actually persisted LSP endpoint IDs through the exact
            // persisted LSP edge to each normalized non-empty endpoint path.
            // The node and edge remain independently required; no identity or
            // result count is changed by this evidence join.
            results.provenance.extend(persisted_endpoint_ids.clone());
            match edge.kind {
                EdgeKind::Calls => results.call_hierarchy_edges += 1,
                EdgeKind::ReferencedBy => results.references += 1,
                EdgeKind::Implements => results.definitions += 1,
                EdgeKind::DependsOn => results.document_links += 1,
                _ => {}
            }
        }
    }
    by_path
}

#[cfg(test)]
fn persisted_results_for_path(path: &str, nodes: &[Node], edges: &[Edge]) -> PersistedResults {
    let Ok(path) = normalize_repo_relative_path(path) else {
        return PersistedResults::default();
    };
    persisted_results_by_path(nodes, edges)
        .remove(&path)
        .unwrap_or_default()
}

fn job_validations_for_language<'a>(
    jobs: &'a [EnrichmentJobRecord],
    language: &str,
) -> Vec<&'a LspValidationEvidence> {
    jobs.iter()
        .filter_map(|job| job.lsp_evidence.as_ref())
        .flat_map(|evidence| evidence.validations.iter())
        .filter(|validation| validation.language == language)
        .collect()
}

pub(crate) fn validation_result_producers(
    jobs: &[EnrichmentJobRecord],
) -> BTreeMap<(String, String), BTreeSet<String>> {
    let mut producers = BTreeMap::<(String, String), BTreeSet<String>>::new();
    for job in jobs.iter().filter(|job| {
        job.state == EnrichmentJobState::Completed
            && job.capability == EnrichmentCapability::CallReferences
    }) {
        let producer_id = format!("enrichment-job:{}", job.job_id);
        for symbol in job
            .lsp_evidence
            .as_ref()
            .into_iter()
            .flat_map(|evidence| evidence.validations.iter())
            .filter(|validation| validation.status == LspValidationStatus::Processed)
            .flat_map(|validation| validation.document_symbols.iter())
        {
            let (Some(path), Some(result_id)) =
                (symbol.file.as_deref(), symbol.graph_result_id.as_deref())
            else {
                continue;
            };
            let Ok(path) = normalize_repo_relative_path(path) else {
                continue;
            };
            if result_id.is_empty() {
                continue;
            }
            producers
                .entry((path, result_id.to_string()))
                .or_default()
                .insert(producer_id.clone());
        }
    }
    producers
}

fn validation_applies_to_path(
    validation: &LspValidationEvidence,
    repo_root: &Path,
    normalized_path: &str,
) -> bool {
    let Some(request_uri) = validation.request_uri.as_deref() else {
        return true;
    };
    let Ok(url) = url::Url::parse(request_uri) else {
        return false;
    };
    let Ok(absolute) = url.to_file_path() else {
        return false;
    };
    let Ok(relative) = absolute.strip_prefix(repo_root) else {
        return false;
    };
    normalize_repo_relative_path(&relative.to_string_lossy())
        .is_ok_and(|path| path == normalized_path)
}

fn runtime_compatibility_violations(
    report: &LspCompletenessReport,
    nodes: &[Node],
    edges: &[Edge],
) -> Vec<ReadinessViolation> {
    let mut violations = Vec::new();
    let current_graph_digest = graph_snapshot_digest(nodes, edges);
    if report.graph_snapshot_digest != current_graph_digest {
        violations.push(ReadinessViolation {
            code: ReadinessViolationCode::StaleReport,
            path: None,
            detail: format!(
                "graph snapshot digest changed: report={}, current={}",
                report.graph_snapshot_digest, current_graph_digest
            ),
        });
    }
    let persisted_by_path = persisted_results_by_path(nodes, edges);
    let mut current_servers = BTreeMap::new();
    for file in report.files.iter().filter(|file| file.role.is_included()) {
        if let Some(expected) = &file.expected_server {
            let current = current_servers
                .entry(expected.name.clone())
                .or_insert_with(|| probe_server_identity(&expected.name));
            if current != expected {
                violations.push(ReadinessViolation {
                    code: ReadinessViolationCode::IdentityMismatch,
                    path: Some(file.path.clone()),
                    detail: format!(
                        "language server identity changed: report={expected:?}, current={current:?}"
                    ),
                });
            }
        }

        let persisted = persisted_by_path
            .get(&file.path)
            .cloned()
            .unwrap_or_default();
        for missing in file.expected_result_ids.difference(&persisted.provenance) {
            violations.push(ReadinessViolation {
                code: ReadinessViolationCode::StaleReport,
                path: Some(file.path.clone()),
                detail: format!("persisted graph no longer contains reported LSP result {missing}"),
            });
        }
    }
    violations
}

fn expected_evidence_from_work_items(
    records: &[&LspWorkItemRecord],
    validations: &[&LspValidationEvidence],
    path: &str,
) -> (BTreeSet<ExpectedResultKind>, BTreeSet<String>) {
    let mut expected = BTreeSet::new();
    let mut expected_ids = BTreeSet::new();
    for record in records
        .iter()
        .filter(|record| record.state == LspWorkItemState::Completed)
    {
        expected_ids.extend(record.output_edges.iter().map(Edge::stable_id));
        expected_ids.extend(record.output_nodes.iter().map(Node::stable_id));
        if record.observed_result_count > 0 {
            for operation in &record.requested_operations {
                let kind = match operation.as_str() {
                    "call_hierarchy" => Some(ExpectedResultKind::CallHierarchy),
                    "references" if record.node_kind == "function" => {
                        Some(ExpectedResultKind::CallHierarchy)
                    }
                    "references" => Some(ExpectedResultKind::Reference),
                    "definitions" | "implementations" | "type_hierarchy" => {
                        Some(ExpectedResultKind::Definition)
                    }
                    "document_symbols" => Some(ExpectedResultKind::DocumentSymbol),
                    "document_links" => Some(ExpectedResultKind::DocumentLink),
                    _ => None,
                };
                if let Some(kind) = kind {
                    expected.insert(kind);
                }
            }
        }
    }
    for symbol in validations
        .iter()
        .flat_map(|validation| validation.document_symbols.iter())
        .filter(|symbol| symbol.file.as_deref() == Some(path))
    {
        expected.insert(ExpectedResultKind::DocumentSymbol);
        if let Some(result_id) = &symbol.graph_result_id {
            expected_ids.insert(result_id.clone());
        }
    }
    (expected, expected_ids)
}

fn terminal_status_for_file(
    path: &str,
    records: &[&LspWorkItemRecord],
    language_entries: &[&LspEnrichmentEntry],
    extracted: bool,
    server: &ServerIdentity,
    jobs: &[EnrichmentJobRecord],
    validations: &[&LspValidationEvidence],
) -> FileTerminalStatus {
    if server.version.is_none() || server.executable_digest.is_none() {
        return FileTerminalStatus::MissingServer {
            detail: format!(
                "{} is absent or did not provide a deterministic --version response",
                server.name
            ),
        };
    }
    if jobs.is_empty() {
        return FileTerminalStatus::NeverProcessed {
            detail: "full scan produced no related durable call-reference job".to_string(),
        };
    }
    if language_entries
        .iter()
        .any(|entry| entry.status == LspStatus::NotFound)
    {
        return FileTerminalStatus::MissingServer {
            detail: format!("{} was not available during enrichment", server.name),
        };
    }
    if let Some(entry) = language_entries
        .iter()
        .find(|entry| matches!(entry.status, LspStatus::Aborted | LspStatus::Failed))
    {
        return failure_terminal_status(
            entry
                .remediation
                .clone()
                .unwrap_or_else(|| "language enrichment did not complete cleanly".to_string()),
        );
    }
    if language_entries.iter().any(|entry| {
        entry
            .validation
            .as_ref()
            .is_some_and(|validation| validation.status == LspValidationStatus::NotValidated)
    }) {
        return FileTerminalStatus::Degraded {
            detail: "language server never reached validated readiness".to_string(),
        };
    }
    if let Some(job) = jobs
        .iter()
        .find(|job| job.state != EnrichmentJobState::Completed)
    {
        let detail = job
            .failure
            .clone()
            .or_else(|| {
                job.lsp_evidence
                    .as_ref()
                    .and_then(|evidence| evidence.detail.clone())
            })
            .unwrap_or_else(|| format!("related LSP job {} ended {:?}", job.job_id, job.state));
        return match job.state {
            EnrichmentJobState::Cancelled | EnrichmentJobState::Superseded => {
                FileTerminalStatus::Cancelled { detail }
            }
            EnrichmentJobState::Degraded => FileTerminalStatus::Degraded { detail },
            EnrichmentJobState::Failed => failure_terminal_status(detail),
            EnrichmentJobState::Queued
            | EnrichmentJobState::Running
            | EnrichmentJobState::Persisting => FileTerminalStatus::Partial { detail },
            EnrichmentJobState::Completed => unreachable!("completed jobs were filtered"),
        };
    }
    if jobs.iter().any(|job| {
        job.lsp_evidence.as_ref().is_none_or(|evidence| {
            matches!(
                evidence.readiness,
                LspEvidenceReadiness::Partial | LspEvidenceReadiness::Unavailable
            )
        })
    }) {
        return FileTerminalStatus::Degraded {
            detail: "related LSP job lacks complete durable readiness evidence".to_string(),
        };
    }
    if validations.is_empty() {
        return FileTerminalStatus::Degraded {
            detail: "related LSP job has no durable validation for this language".to_string(),
        };
    }
    if validations.iter().any(|validation| {
        validation.schema_version != LSP_VALIDATION_EVIDENCE_SCHEMA_VERSION
            || validation.negotiated_capabilities.is_none()
    }) {
        return FileTerminalStatus::Degraded {
            detail:
                "durable language-server validation lacks current negotiated-capability evidence"
                    .to_string(),
        };
    }
    if validations.iter().any(|validation| {
        if validation.method.as_deref() != Some("textDocument/documentSymbol") {
            return !validation.document_symbols.is_empty();
        }
        let count = validation.symbol_count.unwrap_or_default();
        let request_uri = validation.request_uri.as_deref();
        request_uri.is_none()
            || validation.document_symbols.len() != count
            || validation.document_symbols.iter().any(|symbol| {
                symbol.payload_digest.is_empty()
                    || Some(symbol.uri.as_str()) != request_uri
                    || symbol.file.is_none()
                    || symbol.graph_result_id.is_none()
            })
    }) {
        return FileTerminalStatus::Degraded {
            detail: "file-scoped response evidence failed integrity validation".to_string(),
        };
    }
    if validations
        .iter()
        .any(|validation| validation.status == LspValidationStatus::NotValidated)
    {
        return FileTerminalStatus::Degraded {
            detail: "durable language-server validation did not reach readiness".to_string(),
        };
    }
    if let Some(record) = records.iter().find(|record| {
        matches!(
            record.state,
            LspWorkItemState::Failed | LspWorkItemState::Exhausted
        )
    }) {
        let detail = record
            .last_error
            .clone()
            .unwrap_or_else(|| "durable LSP work item failed".to_string());
        return failure_terminal_status(detail);
    }
    if records.iter().any(|record| {
        matches!(
            record.state,
            LspWorkItemState::Pending | LspWorkItemState::InFlight
        )
    }) {
        return FileTerminalStatus::Partial {
            detail: "durable LSP work remains pending or in flight".to_string(),
        };
    }
    if records
        .iter()
        .any(|record| record.state == LspWorkItemState::Skipped)
    {
        return FileTerminalStatus::Degraded {
            detail: "one or more durable LSP work items were skipped".to_string(),
        };
    }
    let completed = records
        .iter()
        .filter(|record| record.state == LspWorkItemState::Completed)
        .collect::<Vec<_>>();
    let file_probe_count = validations
        .iter()
        .find(|validation| {
            validation.status == LspValidationStatus::Processed && validation.request_uri.is_some()
        })
        .and_then(|validation| validation.symbol_count)
        .filter(|_| {
            !completed.iter().any(|record| {
                record
                    .requested_operations
                    .iter()
                    .any(|operation| operation == "document_symbols")
            })
        })
        .map(|count| count as u64);
    if !completed.is_empty() || file_probe_count.is_some() {
        return FileTerminalStatus::Processed {
            result_count: completed
                .iter()
                .map(|record| record.observed_result_count)
                .sum::<u64>()
                .saturating_add(file_probe_count.unwrap_or_default()),
        };
    }
    FileTerminalStatus::NeverProcessed {
        detail: if extracted {
            format!("{path} was extracted but produced no durable LSP work item")
        } else {
            format!("{path} was absent from the extracted graph and LSP work queue")
        },
    }
}

fn failure_terminal_status(detail: String) -> FileTerminalStatus {
    let normalized = detail.to_ascii_lowercase();
    if normalized.contains("timed out") || normalized.contains("timeout") {
        FileTerminalStatus::TimedOut { detail }
    } else if normalized.contains("cancelled") || normalized.contains("canceled") {
        FileTerminalStatus::Cancelled { detail }
    } else if normalized.contains("stale") {
        FileTerminalStatus::Stale { detail }
    } else if normalized.contains("crash")
        || normalized.contains("server exited")
        || normalized.contains("broken pipe")
    {
        FileTerminalStatus::Crashed { detail }
    } else {
        FileTerminalStatus::Degraded { detail }
    }
}

fn classify_file(path: &Path, absolute: &Path) -> (FileRole, Option<ExclusionReason>) {
    let components = lower_components(path);
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if components
        .first()
        .is_some_and(|component| component == ".oh")
    {
        return excluded(
            FileRole::ExcludedGenerated,
            ExclusionReasonCode::ConfiguredPolicy,
            "RNA business/cache artifacts are outside the frozen benchmark checkout input",
        );
    }
    if components.iter().any(|component| {
        matches!(
            component.as_str(),
            "vendor" | "node_modules" | "third_party" | "external" | ".venv" | "venv"
        )
    }) {
        return excluded(
            FileRole::ExcludedVendor,
            ExclusionReasonCode::Vendor,
            "path is under a versioned vendor/dependency directory",
        );
    }
    if components.iter().any(|component| {
        matches!(
            component.as_str(),
            "target"
                | "build"
                | "dist"
                | "out"
                | "__pycache__"
                | ".pytest_cache"
                | ".mypy_cache"
                | ".tox"
        )
    }) || matches!(extension.as_str(), "pyc" | "pyo" | "class" | "o" | "obj")
    {
        return excluded(
            FileRole::ExcludedGenerated,
            ExclusionReasonCode::Generated,
            "path or suffix is a versioned generated/build artifact",
        );
    }
    if is_binary_extension(&extension) || file_contains_nul(absolute) {
        return excluded(
            FileRole::ExcludedBinary,
            ExclusionReasonCode::Binary,
            "file is binary by suffix or contains a NUL byte in its prefix",
        );
    }
    if filename == "dockerfile" || filename.starts_with("dockerfile.") {
        return (FileRole::Config, None);
    }
    if is_text_asset_extension(&extension) {
        return excluded(
            FileRole::ExcludedAsset,
            ExclusionReasonCode::Asset,
            "presentation or secret-test asset has no language-server semantics",
        );
    }
    if is_text_data_extension(&extension) {
        return excluded(
            FileRole::ExcludedData,
            ExclusionReasonCode::NonLanguageData,
            "structured or tabular data has no language-server semantics",
        );
    }
    if matches!(filename.as_str(), ".gitkeep" | ".keep" | "py.typed") {
        return excluded(
            FileRole::ExcludedData,
            ExclusionReasonCode::NonLanguageData,
            "empty version-control or typing marker has no language-server semantics",
        );
    }
    if matches!(extension.as_str(), "mmd" | "vcg")
        || (matches!(extension.as_str(), "dot" | "puml")
            && components.iter().any(|component| component == "pyreverse"))
    {
        return excluded(
            FileRole::ExcludedData,
            ExclusionReasonCode::NonLanguageData,
            "frozen-cohort path is a generated graph output fixture",
        );
    }
    if filename == "not_utf8.sample" {
        return excluded(
            FileRole::ExcludedData,
            ExclusionReasonCode::NonLanguageData,
            "frozen-cohort path is a deliberate non-UTF-8 encoding fixture",
        );
    }
    if extension == "txt" {
        if filename.starts_with("requirements") {
            return (FileRole::Config, None);
        }
        if components
            .iter()
            .any(|component| matches!(component.as_str(), "doc" | "docs"))
            || is_project_document_filename(&filename)
        {
            return (FileRole::Docs, None);
        }
        return excluded(
            FileRole::ExcludedData,
            ExclusionReasonCode::NonLanguageData,
            "plain text outside project documentation/configuration is a dataset or fixture payload",
        );
    }
    if is_document_extension(&extension) || is_project_document_filename(&filename) {
        return (FileRole::Docs, None);
    }
    if is_config_extension(&extension)
        || is_config_filename(&filename)
        || filename.starts_with("requirements")
        || filename == "manifest.in"
        || filename == "tox.ini.sample"
    {
        return (FileRole::Config, None);
    }
    if extension.is_empty() && file_starts_with_shebang(absolute) {
        return (FileRole::Source, None);
    }
    if components
        .iter()
        .any(|component| matches!(component.as_str(), "test" | "tests" | "spec" | "specs"))
        || filename.starts_with("test_")
        || filename.contains("_test.")
        || filename.contains(".spec.")
        || filename.contains(".test.")
    {
        if is_known_test_fixture_extension(&extension) {
            return excluded(
                FileRole::ExcludedData,
                ExclusionReasonCode::NonLanguageData,
                "suffix is used by the cohort as a deliberate non-language test fixture",
            );
        }
        if extension.is_empty()
            && matches!(
                filename.as_str(),
                ".dot-file" | ".hidden" | "backup~" | "cvs" | "file_txt" | "visible"
            )
        {
            return excluded(
                FileRole::ExcludedData,
                ExclusionReasonCode::NonLanguageData,
                "extensionless frozen-cohort path is a deliberate test sentinel payload",
            );
        }
        return (FileRole::Test, None);
    }
    // Unknown and uncommon text stays mandatory. This fail-closed fallback is
    // what prevents a missing server from reclassifying real source as data.
    (FileRole::Source, None)
}

fn excluded(
    role: FileRole,
    code: ExclusionReasonCode,
    detail: &str,
) -> (FileRole, Option<ExclusionReason>) {
    (
        role,
        Some(ExclusionReason {
            code,
            detail: format!("{INVENTORY_POLICY_VERSION}: {detail}"),
        }),
    )
}

fn lower_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

fn is_binary_extension(extension: &str) -> bool {
    matches!(
        extension,
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "ico"
            | "pdf"
            | "zip"
            | "gz"
            | "bz2"
            | "xz"
            | "7z"
            | "tar"
            | "jar"
            | "so"
            | "dylib"
            | "dll"
            | "exe"
            | "woff"
            | "woff2"
            | "ttf"
            | "eot"
            | "mp3"
            | "mp4"
            | "mov"
            | "wav"
            | "sqlite"
            | "db"
    )
}

fn is_text_data_extension(extension: &str) -> bool {
    matches!(
        extension,
        "62-now"
            | "afm"
            | "csv"
            | "dat"
            | "dbout"
            | "ecsv"
            | "eml"
            | "fits"
            | "geojson"
            | "hdr"
            | "ict"
            | "interp"
            | "list"
            | "map"
            | "pristine"
            | "prj"
            | "rdb"
            | "tab"
            | "tokens"
            | "vrt"
    )
}

fn is_text_asset_extension(extension: &str) -> bool {
    matches!(extension, "enc" | "eps" | "graffle" | "pem" | "svg")
}

fn is_document_extension(extension: &str) -> bool {
    matches!(
        extension,
        "1" | "bib"
            | "breaking"
            | "bugfix"
            | "eopc04_iau2000"
            | "extension"
            | "false_negative"
            | "false_positive"
            | "feature"
            | "finals2000a"
            | "inc"
            | "internal"
            | "lesser"
            | "license"
            | "md"
            | "markdown"
            | "new_check"
            | "old"
            | "other"
            | "performance"
            | "pil"
            | "rst"
            | "rst_t"
            | "user_action"
            | "wx"
    )
}

fn is_config_extension(extension: &str) -> bool {
    matches!(
        extension,
        "cff"
            | "cfg"
            | "conf"
            | "ini"
            | "json"
            | "lock"
            | "mplstyle"
            | "rc"
            | "toml"
            | "yaml"
            | "yml"
    )
}

fn is_config_filename(filename: &str) -> bool {
    filename.starts_with("dockerfile.")
        || matches!(
            filename,
            "cargo.toml"
                | "dockerfile"
                | "makefile"
                | "package.json"
                | "pyproject.toml"
                | "setup.cfg"
                | "tox.ini"
        )
}

fn is_project_document_filename(filename: &str) -> bool {
    [
        "authors",
        "changes",
        "changelog",
        "copying",
        "history",
        "license",
        "news",
        "readme",
    ]
    .iter()
    .any(|prefix| filename.starts_with(prefix))
}

pub(crate) fn is_plaintext_document_path(path: &Path) -> bool {
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
        || is_project_document_filename(&filename)
}

fn is_known_test_fixture_extension(extension: &str) -> bool {
    matches!(
        extension,
        "foo" | "ignoreme" | "out" | "tmp" | "unkn" | "unknown" | "xyz"
    )
}

fn file_starts_with_shebang(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut prefix = [0_u8; 2];
    file.read_exact(&mut prefix).is_ok() && prefix == *b"#!"
}

fn file_contains_nul(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut prefix = [0_u8; 8192];
    file.read(&mut prefix)
        .ok()
        .is_some_and(|read| prefix[..read].contains(&0))
}

pub(crate) fn included_lsp_paths_by_language(
    repo_root: &Path,
) -> Result<BTreeMap<String, Vec<PathBuf>>> {
    let mut by_language: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for path in inventory_paths(repo_root)? {
        let absolute = repo_root.join(&path);
        let (role, _) = classify_file(&path, &absolute);
        if !role.is_included() {
            continue;
        }
        if let Some(descriptor) =
            crate::extract::lsp::builtin_lsp_descriptor_for_inventory_file(&path, &absolute)
        {
            by_language
                .entry(descriptor.language().to_string())
                .or_default()
                .push(path);
        }
    }
    for paths in by_language.values_mut() {
        paths.sort();
        paths.dedup();
    }
    Ok(by_language)
}

fn inventory_paths(repo_root: &Path) -> Result<Vec<PathBuf>> {
    if let Some(repository) = repository_at_root(repo_root) {
        let index = repository.index()?;
        let mut paths = Vec::with_capacity(index.len());
        for entry in index.iter() {
            let path = std::str::from_utf8(&entry.path)
                .context("git index contains a non-UTF-8 path that cannot be reported safely")?;
            let path = PathBuf::from(path);
            if repo_root.join(&path).is_file() {
                paths.push(path);
            }
        }
        paths.sort();
        paths.dedup();
        return Ok(paths);
    }
    let mut paths = Vec::new();
    collect_files_recursive(repo_root, repo_root, &mut paths)?;
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn collect_files_recursive(root: &Path, directory: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries = fs::read_dir(directory)
        .with_context(|| format!("failed to read inventory directory {}", directory.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if relative == Path::new(".git") || relative.starts_with(".git/") {
            continue;
        }
        if relative == Path::new(".oh/.cache") || relative.starts_with(".oh/.cache/") {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files_recursive(root, &path, paths)?;
        } else if file_type.is_file() || file_type.is_symlink() {
            paths.push(relative.to_path_buf());
        }
    }
    Ok(())
}

fn checkout_identity(repo_root: &Path, paths: &[PathBuf]) -> Result<String> {
    if let Some(repository) = repository_at_root(repo_root)
        && let Ok(head) = repository.head()
        && let Some(target) = head.target()
    {
        let mut status_options = git2::StatusOptions::new();
        status_options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .include_ignored(false);
        let dirty = repository
            .statuses(Some(&mut status_options))
            .map(|statuses| {
                statuses.iter().any(|entry| match entry.path() {
                    Ok(path) => path != ".oh/.cache" && !path.starts_with(".oh/.cache/"),
                    Err(_) => true,
                })
            })
            .unwrap_or(true);
        if !dirty {
            return Ok(target.to_string());
        }
        return Ok(format!(
            "{}+worktree:{}",
            target,
            content_tree_digest(repo_root, paths)?
        ));
    }
    Ok(format!("tree:{}", content_tree_digest(repo_root, paths)?))
}

fn repository_at_root(repo_root: &Path) -> Option<git2::Repository> {
    let repository = git2::Repository::discover(repo_root).ok()?;
    let workdir = repository.workdir()?.canonicalize().ok()?;
    (Some(workdir) == repo_root.canonicalize().ok()).then_some(repository)
}

fn repository_identity(repo_root: &Path) -> Option<String> {
    let repository = repository_at_root(repo_root)?;
    let remote = repository.find_remote("origin").ok()?;
    normalize_repository_url(remote.url().ok()?)
}

fn normalize_repository_url(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let path = if let Some(path) = trimmed.strip_prefix("git@github.com:") {
        path
    } else if let Some(path) = trimmed.strip_prefix("ssh://git@github.com/") {
        path
    } else if let Some(path) = trimmed.strip_prefix("https://github.com/") {
        path
    } else {
        trimmed.strip_prefix("http://github.com/")?
    };
    let mut components = path.split('/');
    let owner = components.next()?;
    let repository = components.next()?;
    if owner.is_empty() || repository.is_empty() || components.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repository}"))
}

fn content_tree_digest(repo_root: &Path, paths: &[PathBuf]) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    for path in paths {
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(&[0]);
        let bytes = fs::read(repo_root.join(path))?;
        hasher.update(&bytes);
        hasher.update(&[0xff]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn config_digest(repo_root: &Path) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(INVENTORY_POLICY_VERSION.as_bytes());
    if let Ok(bytes) = fs::read(repo_root.join(".oh/config.toml")) {
        hasher.update(&bytes);
    }
    for descriptor in crate::extract::lsp::builtin_lsp_descriptors() {
        hasher.update(descriptor.language().as_bytes());
        hasher.update(&[0]);
        hasher.update(descriptor.command().as_bytes());
        for extension in descriptor.extensions() {
            hasher.update(extension.as_bytes());
            hasher.update(&[0]);
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn work_item_generation(repo_root: &Path) -> Result<String> {
    let records = crate::extract::lsp::work_items::load_all_records(repo_root)?;
    if records.is_empty() {
        return Ok("none".to_string());
    }
    let payload = records
        .iter()
        .map(|record| {
            (
                &record.job_id,
                record.item_id,
                &record.file,
                &record.input_hash,
                &record.requested_operations,
                record.state,
                record.updated_at_ms,
            )
        })
        .collect::<Vec<_>>();
    Ok(blake3::hash(&serde_json::to_vec(&payload)?)
        .to_hex()
        .to_string())
}

fn probe_server_identity(command: &str) -> ServerIdentity {
    let Some(path) = resolve_executable(command) else {
        return ServerIdentity {
            name: command.to_string(),
            version: None,
            executable_digest: None,
        };
    };
    let executable_digest = fs::read(&path)
        .ok()
        .map(|bytes| format!("blake3:{}", blake3::hash(&bytes).to_hex()));
    ServerIdentity {
        name: command.to_string(),
        version: probe_version(&path),
        executable_digest,
    }
}

fn resolve_executable(command: &str) -> Option<PathBuf> {
    let direct = PathBuf::from(command);
    if direct.components().count() > 1 {
        return direct.is_file().then_some(direct);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(command))
            .find(|candidate| candidate.is_file())
    })
}

fn probe_version(path: &Path) -> Option<String> {
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let output = child.wait_with_output().ok()?;
                let text = if output.stdout.is_empty() {
                    String::from_utf8_lossy(&output.stderr).into_owned()
                } else {
                    String::from_utf8_lossy(&output.stdout).into_owned()
                };
                return text
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(|line| line.chars().take(256).collect());
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{Enricher, Extractor};
    use crate::graph::index::GraphIndex;
    use crate::graph::{Confidence, NodeId, NodeKind};

    fn identity(generation: &str) -> ReportIdentity {
        ReportIdentity::new("abc123", "config", "policy", "disabled", 24, generation)
    }

    fn server() -> ServerIdentity {
        ServerIdentity {
            name: "fixture-ls".to_string(),
            version: Some("1.0.0".to_string()),
            executable_digest: Some("sha256:fixture".to_string()),
        }
    }

    #[test]
    fn dockerfile_variants_share_config_inventory_and_descriptor() {
        let repo = tempfile::tempdir().unwrap();
        for relative in [
            "Dockerfile",
            "Dockerfile.prod",
            "Dockerfile.txt",
            "Dockerfile.md",
            "doc/Dockerfile.htmldoc",
        ] {
            let path = Path::new(relative);
            let absolute = repo.path().join(path);
            std::fs::create_dir_all(absolute.parent().unwrap()).unwrap();
            std::fs::write(&absolute, "FROM python:3.13\n").unwrap();
            assert_eq!(classify_file(path, &absolute).0, FileRole::Config);
            let descriptor =
                crate::extract::lsp::builtin_lsp_descriptor_for_inventory_file(path, &absolute)
                    .expect("Dockerfile variant has a descriptor");
            assert_eq!(descriptor.language(), "dockerfile");
            assert_eq!(descriptor.command(), "docker-langserver");
        }
        let negative = Path::new("doc/NotDockerfile.htmldoc");
        let negative_absolute = repo.path().join(negative);
        std::fs::write(&negative_absolute, "FROM python:3.13\n").unwrap();
        assert!(
            crate::extract::lsp::builtin_lsp_descriptor_for_inventory_file(
                negative,
                &negative_absolute
            )
            .is_none()
        );
    }

    fn completed_job(validations: Vec<LspValidationEvidence>) -> EnrichmentJobRecord {
        serde_json::from_value(serde_json::json!({
            "job_id": "job-complete",
            "repo": "/fixture",
            "root": null,
            "capability": "call_references",
            "scope": { "kind": "repo" },
            "trigger": "foreground_scan",
            "state": "completed",
            "phase": "complete",
            "counters": {
                "current": 1,
                "total": 1,
                "node_count": 1,
                "edge_count": 0
            },
            "created_at": 1,
            "updated_at": 2,
            "completed_at": 2,
            "failure": null,
            "lsp_evidence": {
                "readiness": "full",
                "scope": "repo",
                "declared_node_count": 1,
                "max_requests": null,
                "max_duration_ms": null,
                "scheduled_requests": 1,
                "elapsed_ms": 1,
                "circuit_open": false,
                "detail": null,
                "validations": validations
            },
            "superseded_by": null,
            "owner_id": null,
            "lease_expires_at": null,
            "schema_version": 1
        }))
        .unwrap()
    }

    #[test]
    fn incremental_report_must_include_pass1_job_ids_to_retain_target_work() {
        let records = vec![
            LspWorkItemRecord {
                job_id: "lsp-pass1-a".to_string(),
                item_id: 0,
                state: LspWorkItemState::Completed,
                ..Default::default()
            },
            LspWorkItemRecord {
                job_id: "lsp-pass1-b".to_string(),
                item_id: 0,
                state: LspWorkItemState::Completed,
                ..Default::default()
            },
        ];
        let enrichment_only = BTreeSet::from(["call_references-target"]);
        assert!(
            filter_work_items_for_related_jobs(records.clone(), &enrichment_only).is_empty(),
            "the retained case-2 enrichment job ID cannot name pass1 work records"
        );
        let complete = BTreeSet::from(["call_references-target", "lsp-pass1-a", "lsp-pass1-b"]);
        assert_eq!(
            filter_work_items_for_related_jobs(records, &complete).len(),
            2
        );
    }

    #[tokio::test]
    async fn report_joins_outer_enrichment_and_current_pass1_job_ids() {
        let repo = tempfile::tempdir().unwrap();
        let node = Node {
            id: NodeId {
                root: "fixture".to_string(),
                file: PathBuf::from("src/app.py"),
                name: "implementation".to_string(),
                kind: NodeKind::Function,
            },
            language: "python".to_string(),
            line_start: 1,
            line_end: 1,
            signature: "def implementation():".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let ledger = crate::extract::lsp::work_items::LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "lsp-pass1-current".to_string(),
            &[crate::extract::lsp::work_items::LspWorkItemSeed {
                item_id: 0,
                node,
                requested_operations: vec!["references".to_string()],
                attempt_count: 1,
            }],
        )
        .await
        .unwrap();
        ledger.flush().await.unwrap();

        let related =
            report_related_job_ids(repo.path(), &["call_references-current".to_string()], 0)
                .unwrap();
        assert_eq!(
            related,
            vec![
                "call_references-current".to_string(),
                "lsp-pass1-current".to_string()
            ]
        );
    }

    #[test]
    fn exact_recovered_work_refreshed_by_current_plan_is_reported() {
        let source = Node {
            id: NodeId {
                root: "fixture".to_string(),
                file: PathBuf::from("docs/guide.md"),
                name: "Guide".to_string(),
                kind: NodeKind::Other("markdown_section".to_string()),
            },
            language: "markdown".to_string(),
            line_start: 1,
            line_end: 4,
            signature: "# Guide".to_string(),
            body: "See the implementation.".to_string(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::Markdown,
        };
        let target = NodeId {
            root: "fixture".to_string(),
            file: PathBuf::from("src/app.py"),
            name: "implementation".to_string(),
            kind: NodeKind::Function,
        };
        let definition = Edge {
            from: source.id.clone(),
            to: target.clone(),
            kind: EdgeKind::Implements,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        let reference = Edge {
            from: target,
            to: source.id.clone(),
            kind: EdgeKind::ReferencedBy,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        let records = [("definitions", definition), ("references", reference)]
            .into_iter()
            .enumerate()
            .map(|(item_id, (operation, edge))| LspWorkItemRecord {
                job_id: "current-enrichment-job".to_string(),
                item_id,
                root: source.id.root.clone(),
                file: source.id.file.to_string_lossy().into_owned(),
                node_id: source.stable_id(),
                node_kind: source.id.kind.to_string(),
                input_hash: crate::extract::lsp::work_items::node_input_hash(&source),
                requested_operations: vec![operation.to_string()],
                state: LspWorkItemState::Completed,
                updated_at_ms: 10_000,
                recovery: LspWorkItemRecovery::CarriedCompleted,
                produced_result_ids: BTreeSet::from([edge.stable_id()]),
                output_edges: vec![edge],
                observed_result_count: 1,
                ..LspWorkItemRecord::default()
            })
            .collect::<Vec<_>>();

        let selected = select_work_items_for_report(
            records,
            &BTreeSet::from(["current-enrichment-job"]),
            10_000,
            std::slice::from_ref(&source),
            None,
        )
        .unwrap();
        assert_eq!(selected.len(), 2);
        let refs = selected.iter().collect::<Vec<_>>();
        let (_, requests) = evidence_from_work_items(&refs, &[], &[]);
        assert!(
            requests
                .iter()
                .any(|request| request.method == "textDocument/definition")
        );
        assert!(
            requests
                .iter()
                .any(|request| request.method == "textDocument/references")
        );
    }

    #[test]
    fn recovered_work_identity_mismatch_fails_closed() {
        let source = Node {
            id: NodeId {
                root: "fixture".to_string(),
                file: PathBuf::from("docs/guide.md"),
                name: "Guide".to_string(),
                kind: NodeKind::Other("markdown_section".to_string()),
            },
            language: "markdown".to_string(),
            line_start: 1,
            line_end: 1,
            signature: "# Guide".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::Markdown,
        };
        let tampered = LspWorkItemRecord {
            job_id: "current-job".to_string(),
            root: source.id.root.clone(),
            file: source.id.file.to_string_lossy().into_owned(),
            node_id: source.stable_id(),
            input_hash: "tampered".to_string(),
            requested_operations: vec!["definitions".to_string()],
            state: LspWorkItemState::Completed,
            updated_at_ms: 10_000,
            recovery: LspWorkItemRecovery::CarriedCompleted,
            ..LspWorkItemRecord::default()
        };
        let error = select_work_items_for_report(
            vec![tampered],
            &BTreeSet::from(["current-job"]),
            10_000,
            std::slice::from_ref(&source),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("identity mismatch"));
    }

    #[test]
    fn stale_recovered_operation_is_not_reported_without_current_plan_authority() {
        let source = Node {
            id: NodeId {
                root: "fixture".to_string(),
                file: PathBuf::from("docs/guide.md"),
                name: "Guide".to_string(),
                kind: NodeKind::Other("markdown_section".to_string()),
            },
            language: "markdown".to_string(),
            line_start: 1,
            line_end: 1,
            signature: "# Guide".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::Markdown,
        };
        let stale = LspWorkItemRecord {
            job_id: "prior-job".to_string(),
            root: source.id.root.clone(),
            file: source.id.file.to_string_lossy().into_owned(),
            node_id: source.stable_id(),
            input_hash: crate::extract::lsp::work_items::node_input_hash(&source),
            requested_operations: vec!["document_links".to_string()],
            state: LspWorkItemState::Completed,
            updated_at_ms: 1,
            recovery: LspWorkItemRecovery::CarriedCompleted,
            ..LspWorkItemRecord::default()
        };

        let selected = select_work_items_for_report(
            vec![stale],
            &BTreeSet::from(["current-job"]),
            10_000,
            std::slice::from_ref(&source),
            None,
        )
        .unwrap();
        assert!(selected.is_empty());
    }

    #[test]
    fn structural_execution_selects_exact_producer_ids_not_job_siblings() {
        let source = Node {
            id: NodeId {
                root: "fixture".to_string(),
                file: PathBuf::from("src/app.py"),
                name: "implementation".to_string(),
                kind: NodeKind::Function,
            },
            language: "python".to_string(),
            line_start: 1,
            line_end: 1,
            signature: "def implementation():".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let record = |item_id, operation: &str| LspWorkItemRecord {
            job_id: "pass1-job".to_string(),
            item_id,
            root: source.id.root.clone(),
            file: source.id.file.to_string_lossy().into_owned(),
            node_id: source.stable_id(),
            input_hash: crate::extract::lsp::work_items::node_input_hash(&source),
            requested_operations: vec![operation.to_string()],
            state: LspWorkItemState::Completed,
            updated_at_ms: 10_000,
            ..LspWorkItemRecord::default()
        };

        let authenticated = BTreeSet::from(["pass1-job:0".to_string()]);
        let selected = select_work_items_for_report(
            vec![record(0, "references"), record(1, "document_links")],
            &BTreeSet::from(["pass1-job"]),
            10_000,
            std::slice::from_ref(&source),
            Some(&authenticated),
        )
        .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].item_id, 0);
        assert_eq!(
            selected[0].requested_operations,
            vec!["references".to_string()]
        );
    }

    #[test]
    fn duplicate_current_work_identity_fails_closed() {
        let source = Node {
            id: NodeId {
                root: "fixture".to_string(),
                file: PathBuf::from("src/app.py"),
                name: "implementation".to_string(),
                kind: NodeKind::Function,
            },
            language: "python".to_string(),
            line_start: 1,
            line_end: 1,
            signature: "def implementation():".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let record = |item_id| LspWorkItemRecord {
            job_id: "current-job".to_string(),
            item_id,
            root: source.id.root.clone(),
            file: source.id.file.to_string_lossy().into_owned(),
            node_id: source.stable_id(),
            input_hash: crate::extract::lsp::work_items::node_input_hash(&source),
            requested_operations: vec!["references".to_string()],
            state: LspWorkItemState::Completed,
            updated_at_ms: 10_000,
            ..LspWorkItemRecord::default()
        };

        let error = select_work_items_for_report(
            vec![record(0), record(1)],
            &BTreeSet::from(["current-job"]),
            10_000,
            std::slice::from_ref(&source),
            None,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("duplicate current LSP work identity")
        );
    }

    fn included(path: &str, status: FileTerminalStatus) -> FileCoverageRecord {
        FileCoverageRecord {
            path: path.to_string(),
            role: FileRole::Source,
            language: Some("python".to_string()),
            expected_server: Some(server()),
            advertised_capabilities: vec![AdvertisedCapability {
                name: "referencesProvider".to_string(),
                supported: true,
            }],
            requests_attempted: vec![RequestAttempt {
                method: "textDocument/references".to_string(),
                outcome: RequestOutcome::Completed,
                result_count: Some(0),
                duration_ms: Some(1),
                detail: None,
            }],
            expected_results: BTreeSet::new(),
            expected_result_ids: BTreeSet::new(),
            persisted_results: PersistedResults::default(),
            terminal_status: status,
            exclusion: None,
        }
    }

    fn report(files: Vec<FileCoverageRecord>) -> LspCompletenessReport {
        let evidence = files
            .iter()
            .filter(|file| file.role.is_included())
            .map(|file| LspFileEvidence {
                path: file.path.clone(),
                disposition: LspEvidenceDisposition::Executed,
                generation: "generation-1".to_string(),
                blob: "a".repeat(40),
                partition_signature: "b".repeat(64),
                input_hashes: vec!["input".to_string()],
                operations: file
                    .requests_attempted
                    .iter()
                    .map(|request| request.method.clone())
                    .collect(),
                result_ids: file.expected_result_ids.iter().cloned().collect(),
                result_producers: Vec::new(),
                base_archive_sha256: None,
                base_report_digest: None,
            })
            .collect();
        LspCompletenessReport::new_bound_with_evidence(
            identity("generation-1"),
            files,
            evidence,
            &[],
            &[],
        )
    }

    fn inherited_evidence(path: &str, result_ids: Vec<String>) -> LspFileEvidence {
        LspFileEvidence {
            path: path.to_string(),
            disposition: LspEvidenceDisposition::VerifiedInherited,
            generation: "generation-1".to_string(),
            blob: "a".repeat(40),
            partition_signature: "b".repeat(64),
            input_hashes: vec!["input".to_string()],
            operations: vec!["textDocument/references".to_string()],
            result_producers: result_ids
                .iter()
                .map(
                    |result_id| crate::structural_cache::InheritedResultProducer {
                        result_id: result_id.clone(),
                        producer_ids: vec!["job:1".to_string()],
                    },
                )
                .collect(),
            result_ids,
            base_archive_sha256: Some("c".repeat(64)),
            base_report_digest: Some("base-report".to_string()),
        }
    }

    fn cohort_case(
        ordinal: usize,
        mut report: LspCompletenessReport,
    ) -> (FrozenCohortCase, LspCompletenessReport) {
        report.identity.repository = "owner/repo".to_string();
        report.finalize();
        (
            FrozenCohortCase {
                instance_id: format!("instance-{ordinal:02}"),
                repository: "owner/repo".to_string(),
                base_commit: report.identity.checkout_sha.clone(),
                report_path: PathBuf::from(format!("report-{ordinal:02}.json")),
            },
            report,
        )
    }

    #[test]
    fn stable_ordering_and_digest_ignore_input_order() {
        let a = included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 0 },
        );
        let b = included(
            "src/b.py",
            FileTerminalStatus::Processed { result_count: 1 },
        );
        let left = report(vec![b.clone(), a.clone()]);
        let right = report(vec![a, b]);
        assert_eq!(left.files, right.files);
        assert_eq!(left.digest, right.digest);
        assert!(left.is_ready());
    }

    #[test]
    fn identical_tree_inherited_evidence_reaches_fresh_ready_generation() {
        let file = included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 0 },
        );
        let report = LspCompletenessReport::new_bound_with_evidence(
            identity("generation-1"),
            vec![file],
            vec![inherited_evidence("src/a.py", Vec::new())],
            &[],
            &[],
        );

        assert!(report.is_ready(), "{:?}", report.violations);
        assert_eq!(
            report.evidence[0].disposition,
            LspEvidenceDisposition::VerifiedInherited
        );
        assert_eq!(
            report.evidence[0].base_archive_sha256.as_deref(),
            Some("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc")
        );
    }

    #[test]
    fn current_schema_report_without_evidence_is_not_verifier_clean() {
        let report = LspCompletenessReport::new(
            identity("generation-1"),
            vec![included(
                "src/a.py",
                FileTerminalStatus::Processed { result_count: 0 },
            )],
        );

        assert!(!report.integrity_violations().is_empty());
    }

    #[test]
    fn inherited_provenance_cannot_mask_discarded_or_unpersisted_results() {
        let mut file = included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 1 },
        );
        file.expected_result_ids.insert("result-1".to_string());
        let report = LspCompletenessReport::new_bound_with_evidence(
            identity("generation-1"),
            vec![file],
            vec![inherited_evidence("src/a.py", vec!["result-1".to_string()])],
            &[],
            &[],
        );

        assert!(!report.is_ready());
        assert!(report.violations.iter().any(|violation| {
            violation.code == ReadinessViolationCode::MissingExpectedResult
                && violation.path.as_deref() == Some("src/a.py")
        }));
    }

    #[test]
    fn missing_or_unlineaged_inherited_evidence_fails_closed() {
        let file = included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 0 },
        );
        let missing = LspCompletenessReport::new_bound_with_evidence(
            identity("generation-1"),
            vec![file.clone()],
            vec![inherited_evidence("src/other.py", Vec::new())],
            &[],
            &[],
        );
        assert!(missing.violations.iter().any(|violation| {
            violation.code == ReadinessViolationCode::InvalidEvidenceProvenance
        }));

        let mut expected = file;
        expected.expected_result_ids.insert("result-1".to_string());
        expected
            .persisted_results
            .provenance
            .insert("result-1".to_string());
        let mut evidence = inherited_evidence("src/a.py", vec!["result-1".to_string()]);
        evidence.result_producers.clear();
        let unlineaged = LspCompletenessReport::new_bound_with_evidence(
            identity("generation-1"),
            vec![expected],
            vec![evidence],
            &[],
            &[],
        );
        assert!(unlineaged.violations.iter().any(|violation| {
            violation.code == ReadinessViolationCode::InvalidEvidenceProvenance
        }));
    }

    #[test]
    fn duplicate_paths_fail_closed_after_normalization() {
        let report = report(vec![
            included(
                "src/./a.py",
                FileTerminalStatus::Processed { result_count: 0 },
            ),
            included(
                "src/a.py",
                FileTerminalStatus::Processed { result_count: 0 },
            ),
        ]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ReadinessViolationCode::DuplicatePath)
        );
    }

    #[test]
    fn missing_server_and_unsupported_extension_are_distinct() {
        let mut missing = included(
            "src/a.py",
            FileTerminalStatus::MissingServer {
                detail: "not installed".to_string(),
            },
        );
        missing.expected_server = None;
        let mut unsupported = included(
            "src/kernel.pyx",
            FileTerminalStatus::UnsupportedExtension {
                detail: "no locked descriptor".to_string(),
            },
        );
        unsupported.expected_server = None;
        let report = report(vec![missing, unsupported]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ReadinessViolationCode::MissingServer)
        );
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ReadinessViolationCode::UnsupportedRelevantExtension)
        );
    }

    #[test]
    fn processed_zero_is_success_but_skipped_zero_is_not() {
        let good = report(vec![included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 0 },
        )]);
        assert!(good.is_ready());

        let bad = report(vec![included(
            "src/a.py",
            FileTerminalStatus::NeverProcessed {
                detail: "scheduler skip".to_string(),
            },
        )]);
        assert!(
            bad.violations
                .iter()
                .any(|v| v.code == ReadinessViolationCode::NotProcessed)
        );
    }

    #[test]
    fn file_scoped_zero_result_is_durable_success_without_extracted_nodes() {
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join("docs")).unwrap();
        fs::write(repo.path().join("docs/empty.rst"), "").unwrap();
        let uri = url::Url::from_file_path(repo.path().join("docs/empty.rst"))
            .unwrap()
            .to_string();
        let validation = LspValidationEvidence::processed(
            "restructuredtext",
            "esbonio",
            "textDocument/documentSymbol",
            0,
        )
        .with_request_uri(Some(uri))
        .with_duration_ms(7)
        .with_negotiated_capabilities(
            crate::extract::scan_stats::LspNegotiatedCapabilities {
                document_symbol_provider: true,
                ..Default::default()
            },
        );
        let jobs = vec![completed_job(vec![validation.clone()])];
        let validations = job_validations_for_language(&jobs, "restructuredtext")
            .into_iter()
            .filter(|candidate| {
                validation_applies_to_path(candidate, repo.path(), "docs/empty.rst")
            })
            .collect::<Vec<_>>();
        let status = terminal_status_for_file(
            "docs/empty.rst",
            &[],
            &[],
            false,
            &server(),
            &jobs,
            &validations,
        );
        assert_eq!(status, FileTerminalStatus::Processed { result_count: 0 });
        let (_, requests) = evidence_from_work_items(&[], &[], &validations);
        assert!(requests.iter().any(|request| {
            request.method == "textDocument/documentSymbol"
                && request.outcome == RequestOutcome::Completed
                && request.result_count == Some(0)
                && request.duration_ms == Some(7)
        }));
    }

    #[test]
    fn file_scoped_validation_cannot_cover_a_different_path() {
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join("docs")).unwrap();
        let uri = url::Url::from_file_path(repo.path().join("docs/one.rst"))
            .unwrap()
            .to_string();
        let validation = LspValidationEvidence::processed(
            "restructuredtext",
            "esbonio",
            "textDocument/documentSymbol",
            0,
        )
        .with_request_uri(Some(uri));
        assert!(validation_applies_to_path(
            &validation,
            repo.path(),
            "docs/one.rst"
        ));
        assert!(!validation_applies_to_path(
            &validation,
            repo.path(),
            "docs/two.rst"
        ));
    }

    #[test]
    fn every_non_terminal_success_state_fails_readiness() {
        let statuses = vec![
            FileTerminalStatus::Crashed {
                detail: "crash".to_string(),
            },
            FileTerminalStatus::TimedOut {
                detail: "timeout".to_string(),
            },
            FileTerminalStatus::Partial {
                detail: "partial".to_string(),
            },
            FileTerminalStatus::Degraded {
                detail: "degraded".to_string(),
            },
            FileTerminalStatus::Cancelled {
                detail: "cancelled".to_string(),
            },
            FileTerminalStatus::Stale {
                detail: "stale".to_string(),
            },
        ];
        for status in statuses {
            let report = report(vec![included("src/a.py", status)]);
            assert!(!report.is_ready());
        }
    }

    #[test]
    fn lifecycle_failures_keep_their_terminal_reason() {
        assert!(matches!(
            failure_terminal_status("request timed out".to_string()),
            FileTerminalStatus::TimedOut { .. }
        ));
        assert!(matches!(
            failure_terminal_status("server exited during request".to_string()),
            FileTerminalStatus::Crashed { .. }
        ));
        assert!(matches!(
            failure_terminal_status("job cancelled".to_string()),
            FileTerminalStatus::Cancelled { .. }
        ));
        assert!(matches!(
            failure_terminal_status("stale generation".to_string()),
            FileTerminalStatus::Stale { .. }
        ));
    }

    #[test]
    fn applicable_expected_result_must_be_persisted() {
        let mut file = included(
            "docs/index.md",
            FileTerminalStatus::Processed { result_count: 0 },
        );
        file.role = FileRole::Docs;
        file.expected_results
            .insert(ExpectedResultKind::DocumentLink);
        let report = report(vec![file]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ReadinessViolationCode::MissingExpectedResult)
        );
    }

    #[test]
    fn completed_work_output_does_not_mask_missing_graph_persistence() {
        let edge = Edge {
            from: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/a.py"),
                name: "caller".to_string(),
                kind: NodeKind::Function,
            },
            to: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/b.py"),
                name: "target".to_string(),
                kind: NodeKind::Function,
            },
            kind: EdgeKind::ReferencedBy,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        let record = LspWorkItemRecord {
            file: "src/a.py".to_string(),
            requested_operations: vec!["references".to_string()],
            state: LspWorkItemState::Completed,
            output_edges: vec![edge.clone()],
            ..LspWorkItemRecord::default()
        };
        let records = vec![&record];
        let mut file = included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 1 },
        );
        (file.expected_results, file.expected_result_ids) =
            expected_evidence_from_work_items(&records, &[], "src/a.py");
        file.persisted_results = persisted_results_for_path("src/a.py", &[], &[]);
        let missing = report(vec![file.clone()]);
        assert!(
            missing.violations.iter().any(|violation| {
                violation.code == ReadinessViolationCode::MissingExpectedResult
            })
        );

        file.persisted_results = persisted_results_for_path("src/a.py", &[], &[edge]);
        assert!(report(vec![file]).is_ready());
    }

    #[test]
    fn materialized_call_hierarchy_output_satisfies_persistence_gate() {
        let target = NodeId {
            root: "root".to_string(),
            file: PathBuf::from("src/a.py"),
            name: "target".to_string(),
            kind: NodeKind::Function,
        };
        let materialized = Node {
            id: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/generated.py"),
                name: "generated_target".to_string(),
                kind: NodeKind::Function,
            },
            language: "python".to_string(),
            line_start: 50,
            line_end: 51,
            signature: "module.generated_target".to_string(),
            body: String::new(),
            metadata: BTreeMap::from([
                ("virtual".to_string(), "true".to_string()),
                ("lsp_call_hierarchy".to_string(), "true".to_string()),
            ]),
            source: ExtractionSource::Lsp,
        };
        let edge = Edge {
            from: target,
            to: materialized.id.clone(),
            kind: EdgeKind::Calls,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Detected,
            evidence: Vec::new(),
        };
        let record = LspWorkItemRecord {
            file: "src/a.py".to_string(),
            node_kind: "function".to_string(),
            requested_operations: vec!["call_hierarchy".to_string()],
            state: LspWorkItemState::Completed,
            observed_result_count: 1,
            output_edges: vec![edge.clone()],
            output_nodes: vec![materialized.clone()],
            ..LspWorkItemRecord::default()
        };
        let records = vec![&record];
        let mut file = included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 1 },
        );
        file.advertised_capabilities = vec![AdvertisedCapability {
            name: "callHierarchyProvider".to_string(),
            supported: true,
        }];
        file.requests_attempted = evidence_from_work_items(&records, &[], &[]).1;
        (file.expected_results, file.expected_result_ids) =
            expected_evidence_from_work_items(&records, &[], "src/a.py");

        file.persisted_results =
            persisted_results_for_path("src/a.py", &[materialized.clone()], &[edge.clone()]);
        assert!(report(vec![file.clone()]).is_ready());

        file.persisted_results = persisted_results_for_path("src/a.py", &[], &[edge.clone()]);
        assert!(
            report(vec![file.clone()])
                .violations
                .iter()
                .any(|violation| {
                    violation.code == ReadinessViolationCode::MissingExpectedResult
                })
        );

        file.persisted_results = persisted_results_for_path("src/a.py", &[materialized], &[]);
        assert!(
            report(vec![file]).violations.iter().any(|violation| {
                violation.code == ReadinessViolationCode::MissingExpectedResult
            })
        );
    }

    #[test]
    fn pathless_external_call_endpoint_is_proven_by_its_local_lsp_edge() {
        let local = NodeId {
            root: "root".to_string(),
            file: PathBuf::from("src/a.py"),
            name: "target".to_string(),
            kind: NodeKind::Function,
        };
        let external = Node {
            id: NodeId {
                root: "external".to_string(),
                file: PathBuf::new(),
                name: "builtins.open".to_string(),
                kind: NodeKind::Function,
            },
            language: "python".to_string(),
            line_start: 0,
            line_end: 0,
            signature: "builtins.open".to_string(),
            body: String::new(),
            metadata: BTreeMap::from([
                ("virtual".to_string(), "true".to_string()),
                ("external".to_string(), "true".to_string()),
                ("lsp_call_hierarchy".to_string(), "true".to_string()),
            ]),
            source: ExtractionSource::Lsp,
        };
        let edge = Edge {
            from: local,
            to: external.id.clone(),
            kind: EdgeKind::Calls,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Detected,
            evidence: Vec::new(),
        };
        let record = LspWorkItemRecord {
            file: "src/a.py".to_string(),
            node_kind: "function".to_string(),
            requested_operations: vec!["call_hierarchy".to_string()],
            state: LspWorkItemState::Completed,
            observed_result_count: 1,
            output_edges: vec![edge.clone()],
            output_nodes: vec![external.clone()],
            ..LspWorkItemRecord::default()
        };
        let records = vec![&record];
        let mut file = included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 1 },
        );
        file.advertised_capabilities = vec![AdvertisedCapability {
            name: "callHierarchyProvider".to_string(),
            supported: true,
        }];
        file.requests_attempted = evidence_from_work_items(&records, &[], &[]).1;
        (file.expected_results, file.expected_result_ids) =
            expected_evidence_from_work_items(&records, &[], "src/a.py");

        file.persisted_results =
            persisted_results_for_path("src/a.py", &[external.clone()], &[edge.clone()]);
        assert!(report(vec![file.clone()]).is_ready());
        assert!(
            file.persisted_results
                .provenance
                .contains(&external.stable_id())
        );

        file.persisted_results = persisted_results_for_path("src/a.py", &[], &[edge.clone()]);
        assert!(
            report(vec![file.clone()])
                .violations
                .iter()
                .any(|violation| {
                    violation.code == ReadinessViolationCode::MissingExpectedResult
                })
        );

        file.persisted_results = persisted_results_for_path("src/a.py", &[external.clone()], &[]);
        assert!(
            report(vec![file.clone()])
                .violations
                .iter()
                .any(|violation| {
                    violation.code == ReadinessViolationCode::MissingExpectedResult
                })
        );

        let mut non_lsp_external = external.clone();
        non_lsp_external.source = ExtractionSource::TreeSitter;
        file.persisted_results =
            persisted_results_for_path("src/a.py", &[non_lsp_external], &[edge.clone()]);
        assert!(
            report(vec![file.clone()])
                .violations
                .iter()
                .any(|violation| {
                    violation.code == ReadinessViolationCode::MissingExpectedResult
                })
        );

        let unrelated_edge = Edge {
            from: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/b.py"),
                name: "other".to_string(),
                kind: NodeKind::Function,
            },
            to: external.id.clone(),
            ..edge
        };
        file.persisted_results =
            persisted_results_for_path("src/a.py", &[external], &[unrelated_edge]);
        assert!(
            report(vec![file]).violations.iter().any(|violation| {
                violation.code == ReadinessViolationCode::MissingExpectedResult
            })
        );
    }

    #[test]
    fn persisted_results_index_counts_each_distinct_edge_endpoint_once() {
        let cross_file = Edge {
            from: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/a.py"),
                name: "caller".to_string(),
                kind: NodeKind::Function,
            },
            to: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/b.py"),
                name: "callee".to_string(),
                kind: NodeKind::Function,
            },
            kind: EdgeKind::Calls,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        let self_edge = Edge {
            from: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/a.py"),
                name: "caller".to_string(),
                kind: NodeKind::Function,
            },
            to: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/a.py"),
                name: "caller".to_string(),
                kind: NodeKind::Function,
            },
            kind: EdgeKind::ReferencedBy,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        let index = persisted_results_by_path(&[], &[cross_file.clone(), self_edge.clone()]);

        assert_eq!(index["src/a.py"].call_hierarchy_edges, 1);
        assert_eq!(index["src/b.py"].call_hierarchy_edges, 1);
        assert_eq!(index["src/a.py"].references, 1);
        assert_eq!(index["src/b.py"].references, 0);
        assert_eq!(index["src/a.py"].provenance.len(), 2);
        assert_eq!(index["src/b.py"].provenance.len(), 1);
        assert!(
            index["src/a.py"]
                .provenance
                .contains(&cross_file.stable_id())
        );
        assert!(
            index["src/a.py"]
                .provenance
                .contains(&self_edge.stable_id())
        );
    }

    #[test]
    fn unrelated_fresh_work_item_cannot_satisfy_scan_coverage() {
        let edge = Edge {
            from: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/a.py"),
                name: "caller".to_string(),
                kind: NodeKind::Function,
            },
            to: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/b.py"),
                name: "callee".to_string(),
                kind: NodeKind::Function,
            },
            kind: EdgeKind::Calls,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        let related = LspWorkItemRecord {
            job_id: "lsp-pass1-scan".to_string(),
            file: "src/a.py".to_string(),
            node_kind: "function".to_string(),
            requested_operations: vec!["references".to_string()],
            state: LspWorkItemState::Completed,
            output_edges: vec![edge.clone()],
            observed_result_count: 1,
            ..LspWorkItemRecord::default()
        };
        let unrelated = LspWorkItemRecord {
            job_id: "other-job".to_string(),
            file: "src/a.py".to_string(),
            node_kind: "function".to_string(),
            requested_operations: vec!["references".to_string()],
            state: LspWorkItemState::Completed,
            observed_result_count: 1,
            ..LspWorkItemRecord::default()
        };
        let related_ids = BTreeSet::from(["enrichment-job", "lsp-pass1-scan"]);
        let filtered = filter_work_items_for_related_jobs(vec![unrelated, related], &related_ids);
        let filtered = filtered.iter().collect::<Vec<_>>();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].job_id, "lsp-pass1-scan");
        assert_eq!(
            aggregate_request_outcome(&filtered),
            RequestOutcome::Completed
        );
        let validation = LspValidationEvidence::processed(
            "python",
            "fixture-ls",
            "textDocument/documentSymbol",
            0,
        )
        .with_request_uri(Some("file:///fixture/src/a.py".to_string()))
        .with_negotiated_capabilities(
            crate::extract::scan_stats::LspNegotiatedCapabilities {
                references_provider: true,
                document_symbol_provider: true,
                ..crate::extract::scan_stats::LspNegotiatedCapabilities::default()
            },
        );
        let jobs = vec![completed_job(vec![validation])];
        let validations = job_validations_for_language(&jobs, "python");
        let (capabilities, requests) = evidence_from_work_items(&filtered, &[], &validations);
        let (expected, expected_ids) =
            expected_evidence_from_work_items(&filtered, &[], "src/a.py");
        let status = terminal_status_for_file(
            "src/a.py",
            &filtered,
            &[],
            true,
            &server(),
            &jobs,
            &validations,
        );
        let mut file = included("src/a.py", status);
        file.advertised_capabilities = capabilities;
        file.requests_attempted = requests;
        file.expected_results = expected;
        file.expected_result_ids = expected_ids;
        file.persisted_results = persisted_results_for_path("src/a.py", &[], &[edge]);
        assert!(report(vec![file]).is_ready());
    }

    #[test]
    fn a_skipped_work_item_cannot_hide_behind_a_completed_one() {
        let completed = LspWorkItemRecord {
            state: LspWorkItemState::Completed,
            ..LspWorkItemRecord::default()
        };
        let skipped = LspWorkItemRecord {
            state: LspWorkItemState::Skipped,
            ..LspWorkItemRecord::default()
        };
        let validation = LspValidationEvidence::processed(
            "python",
            "fixture-ls",
            "textDocument/documentSymbol",
            0,
        )
        .with_request_uri(Some("file:///fixture/src/a.py".to_string()));
        let jobs = vec![completed_job(vec![validation])];
        let validations = job_validations_for_language(&jobs, "python");
        let status = terminal_status_for_file(
            "src/a.py",
            &[&completed, &skipped],
            &[],
            true,
            &server(),
            &jobs,
            &validations,
        );
        assert!(matches!(status, FileTerminalStatus::Degraded { .. }));
    }

    #[test]
    fn excluded_files_require_explicit_reason() {
        let mut file = included(
            "vendor/lib.py",
            FileTerminalStatus::NeverProcessed {
                detail: "excluded".to_string(),
            },
        );
        file.role = FileRole::ExcludedVendor;
        file.expected_server = None;
        let report = report(vec![file]);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.code == ReadinessViolationCode::MissingExclusionReason)
        );
    }

    #[test]
    fn stale_or_mismatched_reopen_fails_closed() {
        let report = report(vec![included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 0 },
        )]);
        let mut expected = identity("generation-2");
        expected.checkout_sha = "different".to_string();
        let violations = report.compatibility_violations(&expected);
        assert!(
            violations
                .iter()
                .any(|v| v.code == ReadinessViolationCode::IdentityMismatch)
        );
        assert!(
            violations
                .iter()
                .any(|v| v.code == ReadinessViolationCode::StaleReport)
        );
    }

    #[test]
    fn checkout_identity_ignores_only_rna_internal_cache_bytes() {
        let root = tempfile::tempdir().unwrap();
        let repository = git2::Repository::init(root.path()).unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/a.py"), "VALUE = 1\n").unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(Path::new("src/a.py")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repository.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("RNA Fixture", "fixture@example.invalid").unwrap();
        let commit = repository
            .commit(Some("HEAD"), &signature, &signature, "base", &tree, &[])
            .unwrap();
        drop(tree);

        std::fs::create_dir_all(root.path().join(".oh/.cache/lance")).unwrap();
        std::fs::write(root.path().join(".oh/.cache/lance/schema_version"), "24\n").unwrap();
        let paths = inventory_paths(root.path()).unwrap();
        assert_eq!(
            checkout_identity(root.path(), &paths).unwrap(),
            commit.to_string()
        );

        std::fs::write(root.path().join("untracked.txt"), "real worktree change\n").unwrap();
        assert!(
            checkout_identity(root.path(), &paths)
                .unwrap()
                .starts_with(&format!("{commit}+worktree:"))
        );
    }

    #[test]
    fn malformed_paths_are_rejected() {
        for path in ["", "/absolute.py", "../../escape.py", "C:\\outside.py"] {
            assert!(normalize_repo_relative_path(path).is_err(), "{path:?}");
        }
        assert_eq!(
            normalize_repo_relative_path("./src//nested/../a.py").unwrap(),
            "src/a.py"
        );
    }

    #[test]
    fn aggregate_counts_and_digest_are_deterministic() {
        let first = report(vec![included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 0 },
        )]);
        let mut second = report(vec![included(
            "docs/index.md",
            FileTerminalStatus::TimedOut {
                detail: "timeout".to_string(),
            },
        )]);
        second.identity.checkout_sha = "def456".to_string();
        second.finalize();

        let left = AggregateCompletenessReport::from_frozen_cases(
            &[
                cohort_case(1, second.clone()),
                cohort_case(0, first.clone()),
            ],
            "fixture-cohort".to_string(),
        );
        let right = AggregateCompletenessReport::from_frozen_cases(
            &[cohort_case(0, first), cohort_case(1, second)],
            "fixture-cohort".to_string(),
        );
        assert_eq!(left, right);
        assert_eq!(left.counts.checkouts, 2);
        assert_eq!(left.counts.unique_instances, 2);
        assert_eq!(left.counts.ready_checkouts, 1);
        assert_eq!(left.counts.by_extension.get("py"), Some(&1));
        assert_eq!(left.counts.by_extension.get("md"), Some(&1));
    }

    #[test]
    fn processed_files_require_capability_and_request_evidence() {
        let mut file = included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 0 },
        );
        file.advertised_capabilities.clear();
        file.requests_attempted.clear();
        let report = report(vec![file]);
        assert!(report.violations.iter().any(|violation| {
            violation.code == ReadinessViolationCode::MissingAdvertisedCapabilities
        }));
        assert!(
            report.violations.iter().any(|violation| {
                violation.code == ReadinessViolationCode::MissingRequestEvidence
            })
        );
    }

    #[test]
    fn deterministic_inventory_classifies_required_roles_and_exclusions() {
        let repo = tempfile::tempdir().unwrap();
        for directory in ["src", "tests", "docs", "vendor", "build", ".oh/.cache"] {
            fs::create_dir_all(repo.path().join(directory)).unwrap();
        }
        for (path, contents) in [
            ("src/app.py", "def app(): pass\n"),
            ("src/kernel.pyx", "cdef int value = 1\n"),
            ("src/unknown.language", "real source\n"),
            ("tests/test_app.py", "def test_app(): pass\n"),
            ("tests/data.txt", "1 2 3\n"),
            ("tests/bad.unknown", "fixture\n"),
            ("README.md", "# README\n"),
            ("docs/broken.rst", "Broken `link <missing\n"),
            ("pyproject.toml", "[project]\nname='fixture'\n"),
            ("vendor/dependency.py", "vendored = True\n"),
            ("build/generated.py", "generated = True\n"),
            (".oh/.cache/ignored.json", "{}\n"),
        ] {
            fs::write(repo.path().join(path), contents).unwrap();
        }
        fs::write(repo.path().join("logo.png"), [0_u8, 1, 2]).unwrap();

        let paths = inventory_paths(repo.path()).unwrap();
        assert!(!paths.iter().any(|path| path.starts_with(".oh/.cache")));
        let roles = paths
            .iter()
            .map(|path| {
                (
                    path.to_string_lossy().to_string(),
                    classify_file(path, &repo.path().join(path)).0,
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(roles["src/app.py"], FileRole::Source);
        assert_eq!(roles["src/kernel.pyx"], FileRole::Source);
        assert_eq!(roles["src/unknown.language"], FileRole::Source);
        assert_eq!(roles["tests/test_app.py"], FileRole::Test);
        assert_eq!(roles["tests/data.txt"], FileRole::ExcludedData);
        assert_eq!(roles["tests/bad.unknown"], FileRole::ExcludedData);
        assert_eq!(roles["README.md"], FileRole::Docs);
        assert_eq!(roles["docs/broken.rst"], FileRole::Docs);
        assert_eq!(roles["pyproject.toml"], FileRole::Config);
        assert_eq!(roles["vendor/dependency.py"], FileRole::ExcludedVendor);
        assert_eq!(roles["build/generated.py"], FileRole::ExcludedGenerated);
        assert_eq!(roles["logo.png"], FileRole::ExcludedBinary);
    }

    #[test]
    fn report_persistence_round_trips_and_detects_tampering() {
        let repo = tempfile::tempdir().unwrap();
        let original = report(vec![included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 0 },
        )]);
        persist_report(repo.path(), &original).unwrap();
        let loaded = load_report(repo.path()).unwrap();
        assert_eq!(loaded, original);

        let mut tampered = loaded;
        tampered.files[0].path = "src/other.py".to_string();
        persist_report(repo.path(), &tampered).unwrap();
        let loaded = load_report(repo.path()).unwrap();
        assert!(
            loaded
                .integrity_violations()
                .iter()
                .any(|violation| { violation.code == ReadinessViolationCode::StaleReport })
        );
    }

    #[test]
    fn completeness_summary_reader_rejects_oversized_sidecars() {
        let repo = tempfile::tempdir().unwrap();
        let path = summary_path(repo.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![b'x'; MAX_LSP_COMPLETENESS_SUMMARY_BYTES + 1]).unwrap();

        let error = load_summary(repo.path()).unwrap_err().to_string();
        assert!(error.contains("summary exceeds 4096 bytes"), "got: {error}");
    }

    #[test]
    fn completeness_summary_rejects_well_formed_tampering() {
        let repo = tempfile::tempdir().unwrap();
        let original = report(vec![included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 0 },
        )]);
        persist_report(repo.path(), &original).unwrap();
        let mut tampered = load_summary(repo.path()).unwrap();
        tampered.ready = !tampered.ready;
        std::fs::write(
            summary_path(repo.path()),
            serde_json::to_vec_pretty(&tampered).unwrap(),
        )
        .unwrap();

        let error = load_summary(repo.path()).unwrap_err().to_string();
        assert!(error.contains("publication commit"), "got: {error}");
    }

    #[test]
    fn completeness_summary_rejects_a_stale_but_valid_prior_summary() {
        let repo = tempfile::tempdir().unwrap();
        let first = report(vec![included(
            "src/first.py",
            FileTerminalStatus::Processed { result_count: 0 },
        )]);
        persist_report(repo.path(), &first).unwrap();
        let stale_summary = std::fs::read(summary_path(repo.path())).unwrap();

        let second = report(vec![included(
            "src/second.py",
            FileTerminalStatus::Processed { result_count: 0 },
        )]);
        persist_report(repo.path(), &second).unwrap();
        std::fs::write(summary_path(repo.path()), stale_summary).unwrap();

        let error = load_summary(repo.path()).unwrap_err().to_string();
        assert!(error.contains("publication commit"), "got: {error}");
    }

    #[test]
    fn completeness_summary_rejects_changed_full_report_identity() {
        let repo = tempfile::tempdir().unwrap();
        let original = report(vec![included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 0 },
        )]);
        persist_report(repo.path(), &original).unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(report_path(repo.path()))
            .unwrap();
        file.write_all(b" ").unwrap();
        file.sync_all().unwrap();

        let error = load_summary(repo.path()).unwrap_err().to_string();
        assert!(error.contains("report identity"), "got: {error}");
    }

    #[test]
    fn completeness_summary_rejects_an_interrupted_missing_commit() {
        let repo = tempfile::tempdir().unwrap();
        let original = report(vec![included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 0 },
        )]);
        persist_report(repo.path(), &original).unwrap();
        std::fs::remove_file(summary_commit_path(repo.path())).unwrap();

        let error = load_summary(repo.path()).unwrap_err().to_string();
        assert!(error.contains("commit is missing"), "got: {error}");
    }

    #[test]
    fn persist_report_cleans_temps_when_sidecar_invalidation_fails() {
        let repo = tempfile::tempdir().unwrap();
        let summary = summary_path(repo.path());
        std::fs::create_dir_all(&summary).unwrap();
        let report = report(vec![included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 0 },
        )]);

        let error = persist_report(repo.path(), &report)
            .unwrap_err()
            .to_string();
        assert!(error.contains("failed to invalidate"), "got: {error}");

        let parent = report_path(repo.path()).parent().unwrap().to_path_buf();
        let temp_names = std::fs::read_dir(parent)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp-"))
            .collect::<Vec<_>>();
        assert!(temp_names.is_empty(), "leftover temps: {temp_names:?}");
    }

    #[test]
    fn report_temp_paths_are_unique_per_invocation() {
        let parent = tempfile::tempdir().unwrap();
        let first = report_temp_paths(parent.path());
        let second = report_temp_paths(parent.path());

        assert!(
            first.iter().all(|path| !second.contains(path)),
            "overlapping publications must never share temp paths"
        );
    }

    #[test]
    fn document_symbol_output_must_survive_graph_persistence() {
        let node = Node {
            id: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("docs/guide.md"),
                name: "Guide@0123456789abcdef".to_string(),
                kind: NodeKind::Other("lsp_document_symbol".to_string()),
            },
            language: "markdown".to_string(),
            line_start: 1,
            line_end: 1,
            signature: "documentSymbol Guide (3)".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::Lsp,
        };
        let validation = LspValidationEvidence::processed(
            "markdown",
            "marksman",
            "textDocument/documentSymbol",
            1,
        )
        .with_request_uri(Some("file:///fixture/docs/guide.md".to_string()))
        .with_negotiated_capabilities(crate::extract::scan_stats::LspNegotiatedCapabilities {
            document_symbol_provider: true,
            ..crate::extract::scan_stats::LspNegotiatedCapabilities::default()
        })
        .with_document_symbols(vec![
            crate::extract::scan_stats::LspDocumentSymbolEvidence {
                uri: "file:///fixture/docs/guide.md".to_string(),
                name: "Guide".to_string(),
                kind: 3,
                start_line: 0,
                start_character: 0,
                end_line: 0,
                end_character: 5,
                payload_digest: "0123456789abcdef".to_string(),
                graph_result_id: Some(node.stable_id()),
                file: Some("docs/guide.md".to_string()),
            },
        ]);
        let mut file = included(
            "docs/guide.md",
            FileTerminalStatus::Processed { result_count: 1 },
        );
        file.role = FileRole::Docs;
        (file.advertised_capabilities, file.requests_attempted) =
            evidence_from_work_items(&[], &[], &[&validation]);
        (file.expected_results, file.expected_result_ids) =
            expected_evidence_from_work_items(&[], &[&validation], "docs/guide.md");
        file.persisted_results = persisted_results_for_path("docs/guide.md", &[], &[]);
        assert!(!report(vec![file.clone()]).is_ready());

        file.persisted_results = persisted_results_for_path("docs/guide.md", &[node], &[]);
        assert!(report(vec![file]).is_ready());
    }

    #[tokio::test]
    async fn multi_document_mock_requires_exact_file_scoped_symbol_persistence() {
        let repo = tempfile::tempdir().unwrap();
        for directory in ["docs", "src", "tests"] {
            std::fs::create_dir_all(repo.path().join(directory)).unwrap();
        }
        for path in [
            "README.md",
            "docs/guide.md",
            "src/app.py",
            "tests/test_app.py",
        ] {
            let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/lsp_capability_repo")
                .join(path);
            std::fs::copy(source, repo.path().join(path)).unwrap();
        }

        let markdown = crate::extract::markdown::MarkdownExtractor::new();
        let mut nodes = Vec::new();
        for path in ["README.md", "docs/guide.md"] {
            let content = std::fs::read_to_string(repo.path().join(path)).unwrap();
            nodes.extend(markdown.extract(Path::new(path), &content).unwrap().nodes);
        }
        let fixture_server =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lsp_capability_server.py");
        let enricher = crate::extract::lsp::LspEnricher::new(
            "markdown",
            "python3",
            &[
                fixture_server.to_str().expect("UTF-8 fixture path"),
                "document_features",
            ],
            &["md"],
        );
        let result = enricher
            .enrich(&nodes, &GraphIndex::new(), repo.path())
            .await
            .expect("two-document mock enrichment succeeds");
        assert!(!result.aborted, "mock enrichment aborted: {result:?}");
        let validation = result
            .lsp_validation
            .as_ref()
            .expect("mock retains language readiness evidence");
        let records = crate::extract::lsp::work_items::load_records_since(repo.path(), 0).unwrap();

        let mut files = Vec::new();
        for path in ["README.md", "docs/guide.md"] {
            let file_records = records
                .iter()
                .filter(|record| record.file == path)
                .collect::<Vec<_>>();
            let symbol_records = file_records
                .iter()
                .filter(|record| {
                    record
                        .requested_operations
                        .iter()
                        .any(|operation| operation == "document_symbols")
                })
                .collect::<Vec<_>>();
            assert_eq!(
                symbol_records.len(),
                1,
                "{path} must have one exact request"
            );
            assert_eq!(symbol_records[0].output_nodes.len(), 1);
            assert!(
                symbol_records[0]
                    .output_nodes
                    .iter()
                    .all(|node| node.id.file == PathBuf::from(path))
            );

            let (capabilities, requests) =
                evidence_from_work_items(&file_records, &[], &[validation]);
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.method == "textDocument/documentSymbol")
                    .count(),
                1,
                "warmup validation must not be projected onto {path}"
            );
            let (expected, expected_ids) =
                expected_evidence_from_work_items(&file_records, &[validation], path);
            let mut file = included(
                path,
                FileTerminalStatus::Processed {
                    result_count: expected_ids.len() as u64,
                },
            );
            file.role = FileRole::Docs;
            file.advertised_capabilities = capabilities;
            file.requests_attempted = requests;
            file.expected_results = expected;
            file.expected_result_ids = expected_ids;
            file.persisted_results =
                persisted_results_for_path(path, &result.new_nodes, &result.added_edges);
            files.push(file);
        }

        assert!(report(files.clone()).is_ready());
        let nested_path = "docs/guide.md";
        let retained_nodes = result
            .new_nodes
            .iter()
            .filter(|node| node.id.file != PathBuf::from(nested_path))
            .cloned()
            .collect::<Vec<_>>();
        files[1].persisted_results =
            persisted_results_for_path(nested_path, &retained_nodes, &result.added_edges);
        assert!(
            !report(files).is_ready(),
            "nested document cannot inherit README symbol persistence"
        );
    }

    #[tokio::test]
    async fn dockerfile_mock_persists_symbols_and_failure_modes_block_readiness() {
        let repo = tempfile::tempdir().unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/lsp_capability_repo/Dockerfile");
        std::fs::copy(&source, repo.path().join("Dockerfile")).unwrap();
        let content = std::fs::read_to_string(source).unwrap();
        let nodes = crate::extract::dockerfile::DockerfileExtractor::new()
            .extract(Path::new("Dockerfile"), &content)
            .unwrap()
            .nodes;
        assert!(nodes.iter().any(|node| {
            node.language == "dockerfile"
                && node.id.file == PathBuf::from("Dockerfile")
                && node.id.name == "builder"
        }));

        let fixture_server =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lsp_capability_server.py");
        let enricher = crate::extract::lsp::LspEnricher::new(
            "dockerfile",
            "python3",
            &[
                fixture_server.to_str().expect("UTF-8 fixture path"),
                "dockerfile_features",
            ],
            &["<none>"],
        );
        let result = enricher
            .enrich(&nodes, &GraphIndex::new(), repo.path())
            .await
            .expect("mock Dockerfile enrichment succeeds");
        assert!(!result.aborted, "mock enrichment aborted: {result:?}");
        let validation = result
            .lsp_validation
            .as_ref()
            .expect("mock retains Dockerfile readiness evidence");
        assert_eq!(validation.language, "dockerfile");
        let negotiated = validation
            .negotiated_capabilities
            .expect("Dockerfile fixture retains negotiated capabilities");
        assert!(negotiated.document_symbol_provider);
        assert!(negotiated.document_link_provider);
        assert!(negotiated.definition_provider);
        assert!(!negotiated.references_provider);
        assert!(!negotiated.call_hierarchy_provider);
        assert_eq!(validation.symbol_count, Some(2));
        assert_eq!(validation.document_symbols.len(), 2);
        assert!(
            validation
                .document_symbols
                .iter()
                .all(|symbol| symbol.file.as_deref() == Some("Dockerfile")
                    && symbol.graph_result_id.is_some())
        );

        let records = crate::extract::lsp::work_items::load_records_since(repo.path(), 0).unwrap();
        let file_records = records
            .iter()
            .filter(|record| record.file == "Dockerfile")
            .collect::<Vec<_>>();
        let symbol_record = file_records
            .iter()
            .find(|record| {
                record
                    .requested_operations
                    .iter()
                    .any(|operation| operation == "document_symbols")
            })
            .expect("Dockerfile document-symbol request is durable");
        assert_eq!(symbol_record.state, LspWorkItemState::Completed);
        assert_eq!(symbol_record.observed_result_count, 2);
        assert_eq!(symbol_record.output_nodes.len(), 2);
        let operation_records = file_records
            .iter()
            .flat_map(|record| {
                record
                    .requested_operations
                    .iter()
                    .map(move |operation| (operation.as_str(), *record))
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            operation_records.keys().copied().collect::<BTreeSet<_>>(),
            BTreeSet::from(["document_links", "document_symbols"])
        );
        let document_links = operation_records["document_links"];
        assert_eq!(document_links.state, LspWorkItemState::Completed);
        assert_eq!(document_links.observed_result_count, 0);
        assert!(document_links.output_nodes.is_empty());
        assert!(document_links.output_edges.is_empty());
        assert!(file_records.iter().all(
            |record| record.state == LspWorkItemState::Completed && record.last_error.is_none()
        ));

        let jobs = vec![completed_job(vec![validation.clone()])];
        let status = terminal_status_for_file(
            "Dockerfile",
            &file_records,
            &[],
            true,
            &server(),
            &jobs,
            &[validation],
        );
        let (capabilities, requests) = evidence_from_work_items(&file_records, &[], &[validation]);
        for record in &file_records {
            for operation in &record.requested_operations {
                let method = lsp_method(operation);
                assert!(requests.iter().any(|request| {
                    request.method == method
                        && request.outcome == RequestOutcome::Completed
                        && request.result_count == Some(record.observed_result_count)
                }));
            }
        }
        assert!(requests.iter().any(|request| {
            request.method == "textDocument/documentLink"
                && request.outcome == RequestOutcome::Completed
                && request.result_count == Some(0)
        }));
        assert!(
            !requests
                .iter()
                .any(|request| request.method == "textDocument/references")
        );
        assert!(
            !requests
                .iter()
                .any(|request| request.method == "textDocument/definition")
        );
        let (expected, expected_ids) =
            expected_evidence_from_work_items(&file_records, &[validation], "Dockerfile");
        let mut file = included("Dockerfile", status);
        file.role = FileRole::Config;
        file.advertised_capabilities = capabilities;
        file.requests_attempted = requests;
        file.expected_results = expected;
        file.expected_result_ids = expected_ids;
        file.persisted_results =
            persisted_results_for_path("Dockerfile", &result.new_nodes, &result.added_edges);
        assert!(report(vec![file.clone()]).is_ready());

        let mut missing_required_request = file.clone();
        missing_required_request
            .requests_attempted
            .retain(|request| request.method != "textDocument/documentSymbol");
        assert!(
            !report(vec![missing_required_request]).is_ready(),
            "missing required Dockerfile document-symbol request evidence must block readiness"
        );

        let zero_work = included(
            "Dockerfile",
            FileTerminalStatus::NeverProcessed {
                detail: "zero Dockerfile LSP operations were executed".to_string(),
            },
        );
        assert!(!report(vec![zero_work]).is_ready());

        let mut failed_record = (*symbol_record).clone();
        failed_record.state = LspWorkItemState::Failed;
        failed_record.last_error = Some("fixture failure".to_string());
        let failed_status = terminal_status_for_file(
            "Dockerfile",
            &[&failed_record],
            &[],
            true,
            &server(),
            &jobs,
            &[validation],
        );
        assert!(matches!(failed_status, FileTerminalStatus::Degraded { .. }));

        file.persisted_results = PersistedResults::default();
        assert!(
            !report(vec![file]).is_ready(),
            "discarded Dockerfile document symbols must block readiness"
        );
    }

    #[tokio::test]
    async fn binding_final_review_definition_error_persists_failure_and_blocks_readiness() {
        let repo = tempfile::tempdir().unwrap();
        for directory in ["docs", "src", "tests"] {
            std::fs::create_dir_all(repo.path().join(directory)).unwrap();
        }
        for path in [
            "README.md",
            "docs/guide.md",
            "src/app.py",
            "tests/test_app.py",
        ] {
            let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/lsp_capability_repo")
                .join(path);
            std::fs::copy(source, repo.path().join(path)).unwrap();
        }

        let markdown = crate::extract::markdown::MarkdownExtractor::new();
        let mut nodes = Vec::new();
        for path in ["README.md", "docs/guide.md"] {
            let content = std::fs::read_to_string(repo.path().join(path)).unwrap();
            nodes.extend(markdown.extract(Path::new(path), &content).unwrap().nodes);
        }
        let fixture_server =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lsp_capability_server.py");
        let enricher = crate::extract::lsp::LspEnricher::new(
            "markdown",
            "python3",
            &[
                fixture_server.to_str().expect("UTF-8 fixture path"),
                "document_definition_error",
            ],
            &["md"],
        );
        let result = enricher
            .enrich(&nodes, &GraphIndex::new(), repo.path())
            .await
            .expect("mock definition failure is represented as enrichment evidence");
        assert!(result.error_count > 0);

        let records = crate::extract::lsp::work_items::load_records_since(repo.path(), 0).unwrap();
        let file_records = records
            .iter()
            .filter(|record| record.file == "docs/guide.md")
            .collect::<Vec<_>>();
        let definition = file_records
            .iter()
            .find(|record| {
                record
                    .requested_operations
                    .iter()
                    .any(|operation| operation == "definitions")
            })
            .expect("guide definition request is durable");
        assert_eq!(definition.state, LspWorkItemState::Failed);
        assert!(definition.last_error.is_some());

        let validation = result
            .lsp_validation
            .as_ref()
            .expect("mock retains language readiness evidence");
        let jobs = vec![completed_job(vec![validation.clone()])];
        let status = terminal_status_for_file(
            "docs/guide.md",
            &file_records,
            &[],
            true,
            &server(),
            &jobs,
            &[validation],
        );
        match &status {
            FileTerminalStatus::Degraded { detail } => {
                assert!(detail.contains("one or more LSP operations failed"));
            }
            other => panic!("failed definition request must degrade the file, got {other:?}"),
        }
        let (capabilities, requests) = evidence_from_work_items(&file_records, &[], &[validation]);
        assert!(requests.iter().any(|request| {
            request.method == "textDocument/definition" && request.outcome == RequestOutcome::Failed
        }));
        let (expected, expected_ids) =
            expected_evidence_from_work_items(&file_records, &[validation], "docs/guide.md");
        let mut file = included("docs/guide.md", status);
        file.role = FileRole::Docs;
        file.advertised_capabilities = capabilities;
        file.requests_attempted = requests;
        file.expected_results = expected;
        file.expected_result_ids = expected_ids;
        file.persisted_results =
            persisted_results_for_path("docs/guide.md", &result.new_nodes, &result.added_edges);
        assert!(!report(vec![file]).is_ready());
    }

    #[test]
    fn mock_fixture_proves_document_requirement_accounting() {
        assert!(
            include_str!("../tests/fixtures/lsp_capability_repo/docs/guide.md")
                .contains("the source")
        );

        let doc_node = Node {
            id: NodeId {
                root: "fixture".to_string(),
                file: PathBuf::from("docs/guide.md"),
                name: "Fixture guide@docproof".to_string(),
                kind: NodeKind::Other("lsp_document_symbol".to_string()),
            },
            language: "markdown".to_string(),
            line_start: 1,
            line_end: 3,
            signature: "documentSymbol Fixture guide (3)".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::Lsp,
        };
        let doc_validation = LspValidationEvidence::processed(
            "markdown",
            "marksman",
            "textDocument/documentSymbol",
            1,
        )
        .with_request_uri(Some("file:///fixture/docs/guide.md".to_string()))
        .with_negotiated_capabilities(crate::extract::scan_stats::LspNegotiatedCapabilities {
            references_provider: true,
            definition_provider: true,
            document_link_provider: false,
            document_symbol_provider: true,
            ..crate::extract::scan_stats::LspNegotiatedCapabilities::default()
        })
        .with_document_symbols(vec![
            crate::extract::scan_stats::LspDocumentSymbolEvidence {
                uri: "file:///fixture/docs/guide.md".to_string(),
                name: "Fixture guide".to_string(),
                kind: 3,
                start_line: 0,
                start_character: 0,
                end_line: 2,
                end_character: 68,
                payload_digest: "docproof".to_string(),
                graph_result_id: Some(doc_node.stable_id()),
                file: Some("docs/guide.md".to_string()),
            },
        ]);
        let doc_target = NodeId {
            root: "fixture".to_string(),
            file: PathBuf::from("src/app.py"),
            name: "greet".to_string(),
            kind: NodeKind::Function,
        };
        let doc_edges = [
            Edge {
                from: doc_node.id.clone(),
                to: doc_target.clone(),
                kind: EdgeKind::Implements,
                source: ExtractionSource::Lsp,
                confidence: Confidence::Confirmed,
                evidence: Vec::new(),
            },
            Edge {
                from: doc_target.clone(),
                to: doc_node.id.clone(),
                kind: EdgeKind::ReferencedBy,
                source: ExtractionSource::Lsp,
                confidence: Confidence::Confirmed,
                evidence: Vec::new(),
            },
        ];
        let mut doc_records = [
            ("definitions", doc_edges[0].clone()),
            ("references", doc_edges[1].clone()),
        ]
        .into_iter()
        .map(|(operation, edge)| LspWorkItemRecord {
            file: "docs/guide.md".to_string(),
            node_kind: "markdown_section".to_string(),
            requested_operations: vec![operation.to_string()],
            state: LspWorkItemState::Completed,
            output_edges: vec![edge],
            observed_result_count: 1,
            ..LspWorkItemRecord::default()
        })
        .collect::<Vec<_>>();
        doc_records.push(LspWorkItemRecord {
            file: "docs/guide.md".to_string(),
            node_kind: "markdown_section".to_string(),
            requested_operations: vec!["document_symbols".to_string()],
            state: LspWorkItemState::Completed,
            output_nodes: vec![doc_node.clone()],
            observed_result_count: 1,
            ..LspWorkItemRecord::default()
        });
        let doc_record_refs = doc_records.iter().collect::<Vec<_>>();
        let (doc_capabilities, doc_requests) =
            evidence_from_work_items(&doc_record_refs, &[], &[&doc_validation]);
        let (doc_expected, doc_expected_ids) = expected_evidence_from_work_items(
            &doc_record_refs,
            &[&doc_validation],
            "docs/guide.md",
        );
        let mut docs_file = included(
            "docs/guide.md",
            FileTerminalStatus::Processed { result_count: 3 },
        );
        docs_file.role = FileRole::Docs;
        docs_file.advertised_capabilities = doc_capabilities;
        docs_file.requests_attempted = doc_requests;
        docs_file.expected_results = doc_expected;
        docs_file.expected_result_ids = doc_expected_ids;
        docs_file.persisted_results = persisted_results_for_path(
            "docs/guide.md",
            std::slice::from_ref(&doc_node),
            &doc_edges,
        );
        assert!(
            !docs_file
                .expected_results
                .contains(&ExpectedResultKind::DocumentLink)
        );
        assert!(
            !docs_file
                .requests_attempted
                .iter()
                .any(|request| request.method == "textDocument/documentLink")
        );
        assert!(docs_file.advertised_capabilities.iter().any(|capability| {
            capability.name == "documentLinkProvider" && !capability.supported
        }));
        assert!(report(vec![docs_file.clone()]).is_ready());

        let mut missing_definition_capability = docs_file.clone();
        missing_definition_capability
            .advertised_capabilities
            .retain(|capability| capability.name != "definitionProvider");
        assert!(!report(vec![missing_definition_capability]).is_ready());

        let mut missing_definition_request = docs_file;
        missing_definition_request
            .requests_attempted
            .retain(|request| request.method != "textDocument/definition");
        assert!(!report(vec![missing_definition_request]).is_ready());
    }

    #[test]
    fn reopen_rejects_reported_output_missing_from_current_graph() {
        let edge = Edge {
            from: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/a.py"),
                name: "caller".to_string(),
                kind: NodeKind::Function,
            },
            to: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/b.py"),
                name: "target".to_string(),
                kind: NodeKind::Function,
            },
            kind: EdgeKind::ReferencedBy,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        let mut file = included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 1 },
        );
        file.expected_result_ids.insert(edge.stable_id());
        file.persisted_results = persisted_results_for_path("src/a.py", &[], &[edge]);
        let persisted = report(vec![file]);

        let violations = runtime_compatibility_violations(&persisted, &[], &[]);
        assert!(violations.iter().any(|violation| {
            violation.code == ReadinessViolationCode::StaleReport
                && violation.detail.contains("no longer contains")
        }));
    }

    #[test]
    fn durable_validation_is_request_and_capability_evidence() {
        let validation =
            LspValidationEvidence::processed("python", "pyrefly", "workspace/symbol", 2)
                .with_negotiated_capabilities(
                    crate::extract::scan_stats::LspNegotiatedCapabilities {
                        references_provider: true,
                        call_hierarchy_provider: false,
                        definition_provider: true,
                        implementation_provider: true,
                        document_link_provider: false,
                        document_symbol_provider: true,
                        code_action_provider: false,
                    },
                );
        let (capabilities, requests) = evidence_from_work_items(&[], &[], &[&validation]);
        assert!(
            capabilities.iter().any(|capability| {
                capability.name == "referencesProvider" && capability.supported
            })
        );
        assert!(capabilities.iter().any(|capability| {
            capability.name == "callHierarchyProvider" && !capability.supported
        }));
        assert!(
            capabilities
                .iter()
                .all(|capability| capability.name != "workspace/symbol")
        );
        assert_eq!(requests[0].result_count, Some(2));
        assert_eq!(requests[0].outcome, RequestOutcome::Completed);
    }

    #[test]
    fn work_item_requests_do_not_self_advertise_capability() {
        let record = LspWorkItemRecord {
            requested_operations: vec!["references".to_string()],
            state: LspWorkItemState::Completed,
            observed_result_count: 3,
            ..LspWorkItemRecord::default()
        };
        let (capabilities, requests) = evidence_from_work_items(&[&record], &[], &[]);
        assert!(capabilities.is_empty());
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].result_count, Some(3));
    }

    #[test]
    fn raw_applicable_response_without_mapped_output_fails_closed() {
        let record = LspWorkItemRecord {
            file: "src/a.py".to_string(),
            node_kind: "function".to_string(),
            requested_operations: vec!["references".to_string()],
            state: LspWorkItemState::Completed,
            observed_result_count: 1,
            ..LspWorkItemRecord::default()
        };
        let mut file = included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 1 },
        );
        (file.expected_results, file.expected_result_ids) =
            expected_evidence_from_work_items(&[&record], &[], "src/a.py");
        assert!(
            file.expected_results
                .contains(&ExpectedResultKind::CallHierarchy)
        );
        let report = report(vec![file]);
        assert!(
            report.violations.iter().any(|violation| {
                violation.code == ReadinessViolationCode::MissingExpectedResult
            })
        );
    }

    #[test]
    fn missing_related_job_or_language_validation_fails_closed() {
        let completed = LspWorkItemRecord {
            state: LspWorkItemState::Completed,
            ..LspWorkItemRecord::default()
        };
        let no_job =
            terminal_status_for_file("src/a.py", &[&completed], &[], true, &server(), &[], &[]);
        assert!(matches!(no_job, FileTerminalStatus::NeverProcessed { .. }));

        let jobs = vec![completed_job(Vec::new())];
        let no_validation =
            terminal_status_for_file("src/a.py", &[&completed], &[], true, &server(), &jobs, &[]);
        assert!(matches!(no_validation, FileTerminalStatus::Degraded { .. }));
    }

    #[test]
    fn processed_zero_report_is_bound_to_exact_graph_snapshot() {
        let node = Node {
            id: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/a.py"),
                name: "symbol".to_string(),
                kind: NodeKind::Function,
            },
            language: "python".to_string(),
            line_start: 1,
            line_end: 1,
            signature: "def symbol():".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let persisted = LspCompletenessReport::new_bound(
            identity("generation-1"),
            Vec::new(),
            std::slice::from_ref(&node),
            &[],
        );
        let violations = runtime_compatibility_violations(&persisted, &[], &[]);
        assert!(violations.iter().any(|violation| {
            violation.code == ReadinessViolationCode::StaleReport
                && violation.detail.contains("graph snapshot digest changed")
        }));
    }

    #[test]
    fn related_job_terminal_failure_blocks_every_file() {
        let job: EnrichmentJobRecord = serde_json::from_value(serde_json::json!({
            "job_id": "job-1",
            "repo": "/fixture",
            "root": null,
            "capability": "call_references",
            "scope": { "kind": "repo" },
            "trigger": "foreground_scan",
            "state": "failed",
            "phase": "lsp",
            "counters": {
                "current": 0,
                "total": null,
                "node_count": null,
                "edge_count": null
            },
            "created_at": 1,
            "updated_at": 2,
            "completed_at": 2,
            "failure": "server crashed",
            "superseded_by": null,
            "owner_id": null,
            "lease_expires_at": null,
            "schema_version": 1
        }))
        .unwrap();
        let completed = LspWorkItemRecord {
            state: LspWorkItemState::Completed,
            ..LspWorkItemRecord::default()
        };
        let status =
            terminal_status_for_file("src/a.py", &[&completed], &[], true, &server(), &[job], &[]);
        assert!(matches!(status, FileTerminalStatus::Crashed { .. }));
    }

    #[test]
    fn relative_aggregate_output_uses_current_directory() {
        assert_eq!(
            aggregate_output_parent(Path::new("aggregate.json")),
            Path::new(".")
        );
    }

    #[test]
    fn aggregate_gate_requires_exact_frozen_population() {
        let (cohort_digest, identities) = canonical_frozen_cohort().unwrap();
        let cases = identities
            .into_iter()
            .enumerate()
            .map(|(ordinal, (instance_id, repository, base_commit))| {
                let mut ready = report(vec![included(
                    "src/a.py",
                    FileTerminalStatus::Processed { result_count: 0 },
                )]);
                ready.identity.checkout_sha = base_commit.clone();
                ready.identity.repository = repository.clone();
                ready.finalize();
                (
                    FrozenCohortCase {
                        instance_id,
                        repository,
                        base_commit,
                        report_path: PathBuf::from(format!("report-{ordinal:02}.json")),
                    },
                    ready,
                )
            })
            .collect::<Vec<_>>();
        let seventy = AggregateCompletenessReport::from_frozen_cases(&cases, cohort_digest.clone());
        assert!(seventy.is_ready());
        let sixty_nine =
            AggregateCompletenessReport::from_frozen_cases(&cases[..69], cohort_digest.clone());
        assert!(!sixty_nine.is_ready());

        let duplicated = AggregateCompletenessReport::from_frozen_cases(
            &vec![cases[0].clone(); 70],
            cohort_digest.clone(),
        );
        assert!(!duplicated.is_ready());

        let invented = (0..70)
            .map(|ordinal| {
                let mut ready = report(vec![included(
                    "src/a.py",
                    FileTerminalStatus::Processed { result_count: 0 },
                )]);
                ready.identity.checkout_sha = "reused-base".to_string();
                cohort_case(ordinal, ready)
            })
            .collect::<Vec<_>>();
        let invented = AggregateCompletenessReport::from_frozen_cases(&invented, cohort_digest);
        assert!(!invented.is_ready());
    }

    #[test]
    fn aggregate_gate_rejects_business_context_enabled_reports() {
        let mut enabled = report(vec![included(
            "src/a.py",
            FileTerminalStatus::Processed { result_count: 0 },
        )]);
        enabled.identity.context_mode = "enabled".to_string();
        enabled.finalize();
        let aggregate = AggregateCompletenessReport::from_frozen_cases(
            &[cohort_case(0, enabled)],
            "fixture-cohort".to_string(),
        );
        assert_eq!(aggregate.counts.ready_checkouts, 0);
        assert!(!aggregate.is_ready());
    }
}
