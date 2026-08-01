use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::business_context::{
    BusinessContextAdmission, BusinessContextExclusionCounts, BusinessContextMode,
};

use super::enrichment_jobs::{EnrichmentCapability, EnrichmentScope};

const SCHEMA_VERSION: u32 = 1;
const DEFAULT_REPORT_LIMIT: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationReport {
    pub schema_version: u32,
    pub operation_id: String,
    pub operation: OperationKind,
    pub trigger: OperationTrigger,
    pub repo: String,
    #[serde(default)]
    pub business_context_mode: BusinessContextMode,
    #[serde(default)]
    pub business_context_exclusions: BusinessContextExclusionCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub state: OperationState,
    pub started_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub phases: Vec<PhaseReport>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityReport>,
    pub outputs: OutputReport,
    #[serde(default)]
    pub degradation: Vec<DegradationNotice>,
    #[serde(default)]
    pub next_steps: Vec<NextStep>,
    #[serde(default)]
    pub diagnostics: Vec<DiagnosticNotice>,
    #[serde(default)]
    pub related_job_ids: Vec<String>,
    #[serde(default)]
    pub lsp_work_item_queues: Vec<crate::extract::lsp::work_items::LspWorkItemQueueSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

impl OperationReport {
    pub fn new(operation: OperationKind, trigger: OperationTrigger, repo: &Path) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            operation_id: new_operation_id(operation),
            operation,
            trigger,
            repo: repo.display().to_string(),
            business_context_mode: BusinessContextMode::default(),
            business_context_exclusions: BusinessContextExclusionCounts::default(),
            scope: None,
            state: OperationState::Running,
            started_at: unix_now(),
            completed_at: None,
            duration_ms: None,
            phases: Vec::new(),
            capabilities: Vec::new(),
            outputs: OutputReport::default(),
            degradation: Vec::new(),
            next_steps: Vec::new(),
            diagnostics: Vec::new(),
            related_job_ids: Vec::new(),
            lsp_work_item_queues: Vec::new(),
            failure: None,
        }
    }

    pub fn complete(mut self, duration: Duration) -> Self {
        self.state = OperationState::Completed;
        self.completed_at = Some(unix_now());
        self.duration_ms = Some(duration_to_ms(duration));
        self
    }

    pub fn fail(mut self, duration: Duration, failure: impl Into<String>) -> Self {
        self.state = OperationState::Failed;
        self.completed_at = Some(unix_now());
        self.duration_ms = Some(duration_to_ms(duration));
        self.failure = Some(failure.into());
        self
    }

    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = Some(scope.into());
        self
    }

    pub fn record_business_context(&mut self, business_context: &BusinessContextAdmission) {
        self.business_context_mode = business_context.mode();
        self.business_context_exclusions = business_context.counts();
    }

    pub fn add_phase(&mut self, phase: PhaseReport) {
        self.phases.push(phase);
    }

    pub fn add_capability(&mut self, capability: CapabilityReport) {
        self.capabilities.push(capability);
    }

    pub fn add_degradation(&mut self, query_class: QueryClass, reason: impl Into<String>) {
        self.degradation.push(DegradationNotice {
            query_class,
            reason: reason.into(),
        });
    }

    pub fn add_next_step(&mut self, command: impl Into<String>, reason: impl Into<String>) {
        self.next_steps.push(NextStep {
            command: command.into(),
            reason: reason.into(),
        });
    }

    pub fn add_diagnostic(&mut self, severity: DiagnosticSeverity, message: impl Into<String>) {
        self.diagnostics.push(DiagnosticNotice {
            severity,
            message: message.into(),
        });
    }

    pub fn render_cli(&self, include_timings: bool) -> String {
        let mut lines = Vec::new();
        let duration = self
            .duration_ms
            .map(format_duration_ms)
            .unwrap_or_else(|| "in progress".to_string());
        let scope = self
            .scope
            .as_ref()
            .map(|scope| format!(" ({scope})"))
            .unwrap_or_default();
        let state = self.state.as_label();
        lines.push(format!(
            "{}{} {} in {}",
            self.operation.as_label(),
            scope,
            state,
            duration
        ));
        lines.push(format!(
            "  business context: {}",
            self.business_context_mode
        ));
        lines.push(format!(
            "  excluded producer inputs: {} .oh file(s), {} Git-history producer(s)",
            self.business_context_exclusions.business_artifact_files,
            self.business_context_exclusions.git_history_producers
        ));

        let mut output_parts = Vec::new();
        if let Some(count) = self.outputs.symbol_count {
            output_parts.push(format!("{} symbols", format_count(count)));
        }
        if let Some(count) = self.outputs.edge_count {
            output_parts.push(format!("{} edges", format_count(count)));
        }
        if let Some(count) = self.outputs.file_count {
            output_parts.push(format!("{} files", format_count(count)));
        }
        if let Some(count) = self.outputs.embedding_count {
            output_parts.push(format!("{} embeddings", format_count(count)));
        }
        if let Some(count) = self.outputs.lsp_edge_count {
            output_parts.push(format!("{} LSP call/reference edges", format_count(count)));
        }
        if !output_parts.is_empty() {
            lines.push(format!("  output: {}", output_parts.join(", ")));
        }

        if !self.capabilities.is_empty() {
            lines.push("".to_string());
            lines.push("Capabilities:".to_string());
            for capability in &self.capabilities {
                lines.push(format!("  - {}", capability.render_cli()));
            }
        }

        if !self.lsp_work_item_queues.is_empty() {
            lines.push("".to_string());
            lines.push("LSP work queues:".to_string());
            for snapshot in &self.lsp_work_item_queues {
                lines.push(format!("  - {}", snapshot.render()));
            }
        }

        if !self.degradation.is_empty() {
            lines.push("".to_string());
            lines.push("Degraded queries:".to_string());
            for notice in &self.degradation {
                lines.push(format!(
                    "  - {}: {}",
                    notice.query_class.as_label(),
                    notice.reason
                ));
            }
        }

        if include_timings {
            lines.push("".to_string());
            lines.push("Timings:".to_string());
            if self.phases.is_empty() {
                lines.push("  - no measured phases recorded".to_string());
            } else {
                for phase in &self.phases {
                    lines.push(format!("  - {}", phase.render_cli()));
                }
            }
        }

        if !self.next_steps.is_empty() {
            lines.push("".to_string());
            lines.push("Next:".to_string());
            for step in &self.next_steps {
                lines.push(format!("  - {} — {}", step.command, step.reason));
            }
        }

        if !self.diagnostics.is_empty() {
            lines.push("".to_string());
            lines.push("Diagnostics:".to_string());
            for diagnostic in &self.diagnostics {
                lines.push(format!(
                    "  - {}: {}",
                    diagnostic.severity.as_label(),
                    diagnostic.message
                ));
            }
        }

        if let Some(failure) = &self.failure {
            lines.push("".to_string());
            lines.push(format!("Failure: {failure}"));
        }

        lines.join("\n")
    }

    pub fn render_markdown(&self) -> String {
        let mut out = format!(
            "### {} — {}\n\n",
            self.operation.as_label(),
            self.state.as_label()
        );
        out.push_str(&self.render_cli(true));
        out
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Scan,
    FullRebuild,
    IncrementalRefresh,
    ExtractOnly,
    Enrich,
    CacheLoad,
    StartupIndex,
}

impl OperationKind {
    pub fn as_label(self) -> &'static str {
        match self {
            Self::Scan => "Scan",
            Self::FullRebuild => "Full rebuild",
            Self::IncrementalRefresh => "Incremental refresh",
            Self::ExtractOnly => "Extract-only scan",
            Self::Enrich => "Enrichment",
            Self::CacheLoad => "Cache load",
            Self::StartupIndex => "Startup index",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationTrigger {
    Cli,
    Mcp,
    ForegroundScan,
    BackgroundScan,
    Startup,
    Explicit,
    Test,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Queued,
    Running,
    Persisting,
    Completed,
    Failed,
    Cancelled,
    Superseded,
    Stale,
}

impl OperationState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Superseded | Self::Stale
        )
    }

    pub fn as_label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Persisting => "persisting",
            Self::Completed => "complete",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Superseded => "superseded",
            Self::Stale => "stale",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PhaseReport {
    pub phase: PhaseKind,
    pub state: PhaseState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl PhaseReport {
    pub fn ran(phase: PhaseKind, duration: Duration) -> Self {
        Self {
            phase,
            state: PhaseState::Ran,
            duration_ms: Some(duration_to_ms(duration)),
            detail: None,
        }
    }

    pub fn skipped(phase: PhaseKind, reason: impl Into<String>) -> Self {
        Self {
            phase,
            state: PhaseState::Skipped,
            duration_ms: None,
            detail: Some(reason.into()),
        }
    }

    pub fn unavailable(phase: PhaseKind, reason: impl Into<String>) -> Self {
        Self {
            phase,
            state: PhaseState::Unavailable,
            duration_ms: None,
            detail: Some(reason.into()),
        }
    }

    pub fn failed(phase: PhaseKind, duration: Duration, reason: impl Into<String>) -> Self {
        Self {
            phase,
            state: PhaseState::Failed,
            duration_ms: Some(duration_to_ms(duration)),
            detail: Some(reason.into()),
        }
    }

    fn render_cli(&self) -> String {
        let timing = self
            .duration_ms
            .map(format_duration_ms)
            .unwrap_or_else(|| self.state.as_label().to_string());
        match &self.detail {
            Some(detail) => format!("{}: {} ({})", self.phase.as_label(), timing, detail),
            None => format!("{}: {}", self.phase.as_label(), timing),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PhaseKind {
    DiscoverFiles,
    LoadCache,
    Extract,
    PostPasses,
    Lsp,
    Embeddings,
    PersistGraph,
    ScannerState,
    Total,
}

impl PhaseKind {
    fn as_label(self) -> &'static str {
        match self {
            Self::DiscoverFiles => "discover files",
            Self::LoadCache => "load cache",
            Self::Extract => "extract",
            Self::PostPasses => "post-passes",
            Self::Lsp => "LSP call references",
            Self::Embeddings => "embeddings",
            Self::PersistGraph => "persist graph",
            Self::ScannerState => "scanner state",
            Self::Total => "total",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PhaseState {
    Ran,
    Skipped,
    Joined,
    Failed,
    Unavailable,
    NotMeasured,
}

impl PhaseState {
    fn as_label(self) -> &'static str {
        match self {
            Self::Ran => "ran",
            Self::Skipped => "skipped",
            Self::Joined => "joined",
            Self::Failed => "failed",
            Self::Unavailable => "unavailable",
            Self::NotMeasured => "not measured",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityReport {
    pub capability: EnrichmentCapability,
    pub state: CapabilityState,
    pub requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CapabilityReport {
    pub fn new(
        capability: EnrichmentCapability,
        state: CapabilityState,
        requested: bool,
        scope: Option<String>,
        detail: Option<String>,
    ) -> Self {
        Self {
            capability,
            state,
            requested,
            scope,
            freshness: None,
            detail,
        }
    }

    fn render_cli(&self) -> String {
        let requested = if self.requested {
            "requested"
        } else {
            "not requested"
        };
        let scope = self
            .scope
            .as_ref()
            .map(|scope| format!(" scope={scope}"))
            .unwrap_or_default();
        let detail = self
            .detail
            .as_ref()
            .map(|detail| format!(" — {detail}"))
            .unwrap_or_default();
        format!(
            "{}: {} ({requested}{scope}){detail}",
            self.capability.as_str(),
            self.state.as_label()
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    Requested,
    Skipped,
    Running,
    Completed,
    Degraded,
    Failed,
    Unavailable,
    Stale,
    Superseded,
}

impl CapabilityState {
    fn as_label(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Skipped => "skipped",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
            Self::Unavailable => "unavailable",
            Self::Stale => "stale",
            Self::Superseded => "superseded",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lsp_edge_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DegradationNotice {
    pub query_class: QueryClass,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryClass {
    ExactSearch,
    SemanticSearch,
    Rerank,
    GraphNeighbors,
    GlobalImpact,
    DeadCodePrerequisites,
}

impl QueryClass {
    fn as_label(self) -> &'static str {
        match self {
            Self::ExactSearch => "exact search",
            Self::SemanticSearch => "semantic search",
            Self::Rerank => "rerank",
            Self::GraphNeighbors => "graph neighbors",
            Self::GlobalImpact => "global impact",
            Self::DeadCodePrerequisites => "dead-code prerequisites",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NextStep {
    pub command: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticNotice {
    pub severity: DiagnosticSeverity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

impl DiagnosticSeverity {
    fn as_label(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct OperationReportStore {
    pub schema_version: u32,
    #[serde(default)]
    pub reports: Vec<OperationReport>,
}

impl OperationReportStore {
    pub fn read(repo_root: &Path) -> Result<Self> {
        Self::read_with_recovery(repo_root, true)
    }

    /// Read reports exactly as persisted, without rewriting non-terminal
    /// records. Cache-only diagnostics use this path to preserve immutability.
    pub fn read_only(repo_root: &Path) -> Result<Self> {
        Self::read_with_recovery(repo_root, false)
    }

    fn read_with_recovery(repo_root: &Path, recover_stale: bool) -> Result<Self> {
        let path = reports_path(repo_root);
        if !path.exists() {
            return Ok(Self {
                schema_version: SCHEMA_VERSION,
                reports: Vec::new(),
            });
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("read operation report store {}", path.display()))?;
        let mut store: Self = serde_json::from_str(&raw)
            .with_context(|| format!("parse operation report store {}", path.display()))?;
        if store.schema_version == 0 {
            store.schema_version = SCHEMA_VERSION;
        }
        if recover_stale && store.mark_non_terminal_stale() {
            let raw = serde_json::to_string_pretty(&store)?;
            std::fs::write(&path, raw).with_context(|| {
                format!("write recovered operation report store {}", path.display())
            })?;
        }
        Ok(store)
    }

    pub fn recent(repo_root: &Path, limit: usize) -> Vec<OperationReport> {
        match Self::read(repo_root) {
            Ok(mut store) => {
                store.reports.sort_by_key(|report| report.started_at);
                store.reports.into_iter().rev().take(limit).collect()
            }
            Err(err) => {
                tracing::warn!("failed to read operation report history: {err:#}");
                Vec::new()
            }
        }
    }

    pub fn recent_read_only(repo_root: &Path, limit: usize) -> Vec<OperationReport> {
        match Self::read_only(repo_root) {
            Ok(mut store) => {
                store.reports.sort_by_key(|report| report.started_at);
                store.reports.into_iter().rev().take(limit).collect()
            }
            Err(err) => {
                tracing::warn!("failed to read operation report history: {err:#}");
                Vec::new()
            }
        }
    }

    pub fn record(repo_root: &Path, report: OperationReport) -> Result<()> {
        Self::record_with_limit(repo_root, report, DEFAULT_REPORT_LIMIT)
    }

    /// Attach current-scan durable work-item queues and their exact job IDs to
    /// the caller-owned report before any downstream evidence consumer uses it.
    pub fn hydrate_lsp_work_item_evidence(repo_root: &Path, report: &mut OperationReport) {
        if report.lsp_work_item_queues.is_empty() {
            report.lsp_work_item_queues =
                crate::extract::lsp::work_items::load_queue_snapshots_since(
                    repo_root,
                    report
                        .started_at
                        .saturating_mul(1_000)
                        .saturating_sub(1_000),
                )
                .unwrap_or_else(|error| {
                    tracing::warn!(
                        %error,
                        "Could not attach LSP work-item snapshots to operation report"
                    );
                    Vec::new()
                });
        }
        let mut related_job_ids = report
            .related_job_ids
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        for snapshot in &report.lsp_work_item_queues {
            if related_job_ids.insert(snapshot.job_id.clone()) {
                report.related_job_ids.push(snapshot.job_id.clone());
            }
        }
    }

    pub fn record_with_limit(
        repo_root: &Path,
        mut report: OperationReport,
        limit: usize,
    ) -> Result<()> {
        Self::hydrate_lsp_work_item_evidence(repo_root, &mut report);
        let mut store = match Self::read(repo_root) {
            Ok(store) => store,
            Err(err) => {
                tracing::warn!(
                    "recovering operation report store after read failure; replacing corrupt history: {err:#}"
                );
                Self {
                    schema_version: SCHEMA_VERSION,
                    reports: Vec::new(),
                }
            }
        };
        store.schema_version = SCHEMA_VERSION;
        store
            .reports
            .retain(|existing| existing.operation_id != report.operation_id);
        store.reports.push(report);
        store.reports.sort_by_key(|report| report.started_at);
        if store.reports.len() > limit {
            let remove = store.reports.len() - limit;
            store.reports.drain(0..remove);
        }
        store.write(repo_root)
    }

    pub fn write(&self, repo_root: &Path) -> Result<()> {
        let path = reports_path(repo_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("create operation report cache dir {}", parent.display())
            })?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, raw)
            .with_context(|| format!("write operation report store {}", path.display()))
    }

    fn mark_non_terminal_stale(&mut self) -> bool {
        let mut changed = false;
        for report in &mut self.reports {
            if !report.state.is_terminal() {
                changed = true;
                report.state = OperationState::Stale;
                let completed_at = unix_now();
                report.completed_at = Some(completed_at);
                report.duration_ms = Some(
                    completed_at
                        .saturating_sub(report.started_at)
                        .saturating_mul(1000),
                );
                report.failure.get_or_insert_with(|| {
                    "operation was non-terminal when read from disk; previous process likely exited"
                        .to_string()
                });
                report.add_diagnostic(
                    DiagnosticSeverity::Warning,
                    "non-terminal persisted operation marked stale after restart/read",
                );
            }
        }
        changed
    }
}

pub fn reports_path(repo_root: &Path) -> PathBuf {
    repo_root
        .join(".oh")
        .join(".cache")
        .join("operation_reports.json")
}

pub fn render_recent_reports_markdown(repo_root: &Path, limit: usize) -> String {
    render_reports_markdown(OperationReportStore::recent(repo_root, limit))
}

pub fn render_recent_reports_markdown_read_only(repo_root: &Path, limit: usize) -> String {
    render_reports_markdown(OperationReportStore::recent_read_only(repo_root, limit))
}

fn render_reports_markdown(reports: Vec<OperationReport>) -> String {
    if reports.is_empty() {
        return String::new();
    }
    let mut out = format!("\n\n## Recent Operations\n\n{} operation(s)", reports.len());
    for report in reports {
        out.push_str("\n\n");
        out.push_str(&report.render_markdown());
    }
    out
}

pub fn lsp_capability_from_status(
    enrichment: super::enrichment_jobs::ScanEnrichmentOptions,
    state: super::state::LspState,
    diagnostic: Option<&str>,
    lsp_edge_count: usize,
    has_related_job: bool,
) -> (CapabilityState, Option<String>) {
    if !enrichment.runs_lsp() {
        return (CapabilityState::Skipped, None);
    }
    match state {
        super::state::LspState::Complete if lsp_edge_count > 0 => (
            CapabilityState::Completed,
            Some(format!(
                "{} persisted call/reference edges available",
                lsp_edge_count
            )),
        ),
        super::state::LspState::Complete => (
            CapabilityState::Stale,
            Some(
                "LSP completed with 0 persisted call/reference edges; enriched workflows may be false-negative prone".to_string(),
            ),
        ),
        super::state::LspState::Running | super::state::LspState::ServerFound => (
            CapabilityState::Running,
            Some("call-reference enrichment is running or queued".to_string()),
        ),
        super::state::LspState::Failed => (
            CapabilityState::Failed,
            Some("call-reference enrichment failed; inspect enrichment job history".to_string()),
        ),
        super::state::LspState::Degraded => (
            CapabilityState::Degraded,
            Some(format!(
                "call-reference enrichment finalized with {} partial edges: {}",
                lsp_edge_count,
                diagnostic.unwrap_or("degraded without a diagnostic")
            )),
        ),
        super::state::LspState::Unavailable => (
            CapabilityState::Unavailable,
            Some("no call-reference LSP server was available".to_string()),
        ),
        super::state::LspState::NotStarted if has_related_job => (
            CapabilityState::Running,
            Some("call-reference enrichment job exists but has not completed".to_string()),
        ),
        super::state::LspState::NotStarted if lsp_edge_count > 0 => (
            CapabilityState::Stale,
            Some(format!(
                "{} persisted call/reference edges exist, but no completed call-reference job or live LSP status proves complete coverage",
                lsp_edge_count
            )),
        ),
        super::state::LspState::NotStarted => (
            CapabilityState::Requested,
            Some("call-reference enrichment was requested but has not completed".to_string()),
        ),
    }
}

pub fn embedding_capability_from_availability(
    enrichment: super::enrichment_jobs::ScanEnrichmentOptions,
    embeddings_attached: bool,
) -> (CapabilityState, Option<String>) {
    if embeddings_attached {
        return (
            CapabilityState::Completed,
            Some("semantic embedding index is loaded".to_string()),
        );
    }
    if enrichment.runs_embeddings() {
        (
            CapabilityState::Unavailable,
            Some(
                "embedding enrichment was requested but no queryable index is attached".to_string(),
            ),
        )
    } else {
        (
            CapabilityState::Skipped,
            Some("embedding enrichment was not requested and no index is attached".to_string()),
        )
    }
}

pub fn scan_capability_reports(
    enrichment: super::enrichment_jobs::ScanEnrichmentOptions,
    embedding_state: CapabilityState,
    embedding_detail: Option<String>,
    lsp_state: CapabilityState,
    lsp_detail: Option<String>,
    scope: Option<String>,
) -> Vec<CapabilityReport> {
    let mut capabilities = vec![CapabilityReport::new(
        EnrichmentCapability::ExtractedGraph,
        CapabilityState::Completed,
        true,
        scope.clone(),
        Some("exact search, symbol search, and graph neighbors are available".to_string()),
    )];
    capabilities.push(CapabilityReport::new(
        EnrichmentCapability::Embeddings,
        embedding_state,
        enrichment.runs_embeddings(),
        scope.clone(),
        embedding_detail,
    ));
    capabilities.push(CapabilityReport::new(
        EnrichmentCapability::CallReferences,
        if enrichment.runs_lsp() {
            lsp_state
        } else {
            CapabilityState::Skipped
        },
        enrichment.runs_lsp(),
        scope,
        if enrichment.runs_lsp() {
            lsp_detail
        } else {
            None
        },
    ));
    capabilities
}

pub fn add_scan_degradation_and_next_steps(
    report: &mut OperationReport,
    repo_root: &Path,
    enrichment: super::enrichment_jobs::ScanEnrichmentOptions,
    embedding_state: CapabilityState,
    lsp_state: CapabilityState,
) {
    let embedding_degraded = matches!(
        embedding_state,
        CapabilityState::Requested
            | CapabilityState::Running
            | CapabilityState::Failed
            | CapabilityState::Degraded
            | CapabilityState::Unavailable
            | CapabilityState::Stale
            | CapabilityState::Superseded
            | CapabilityState::Skipped
    );
    if embedding_degraded {
        let reason = match embedding_state {
            CapabilityState::Completed => "embedding capability is ready",
            CapabilityState::Degraded => "embedding enrichment completed with degraded output",
            CapabilityState::Running => "embedding enrichment is still running",
            CapabilityState::Failed => "embedding enrichment failed",
            CapabilityState::Unavailable => "embedding capability is unavailable",
            CapabilityState::Requested => "embedding enrichment has not completed yet",
            CapabilityState::Stale => "embedding index is stale",
            CapabilityState::Superseded => "embedding enrichment was superseded",
            CapabilityState::Skipped => {
                "embedding capability is not ready; keyword/structural search still works"
            }
        };
        report.add_degradation(QueryClass::SemanticSearch, reason);
        report.add_degradation(QueryClass::Rerank, "rerank requires embeddings");
        report.add_next_step(
            format!(
                "repo-native-alignment enrich --capability embeddings --scope repo --repo {}",
                shell_escape_path(repo_root)
            ),
            "enable semantic search and rerank",
        );
    }
    let lsp_degraded = !enrichment.runs_lsp()
        || matches!(
            lsp_state,
            CapabilityState::Requested
                | CapabilityState::Running
                | CapabilityState::Failed
                | CapabilityState::Degraded
                | CapabilityState::Unavailable
                | CapabilityState::Stale
                | CapabilityState::Superseded
        );
    if lsp_degraded {
        let reason = match lsp_state {
            CapabilityState::Running => "call-reference enrichment is still running",
            CapabilityState::Failed => "call-reference enrichment failed",
            CapabilityState::Degraded => {
                "call-reference enrichment finalized with partial/degraded coverage"
            }
            CapabilityState::Unavailable => "call-reference enrichment is unavailable",
            CapabilityState::Requested => "call-reference enrichment has not completed yet",
            CapabilityState::Stale => "call-reference enrichment is stale",
            CapabilityState::Superseded => "call-reference enrichment was superseded",
            CapabilityState::Skipped | CapabilityState::Completed => {
                "complete cross-file call/reference coverage requires call-reference enrichment"
            }
        };
        report.add_degradation(QueryClass::GlobalImpact, reason);
        report.add_degradation(
            QueryClass::DeadCodePrerequisites,
            "dead-code analysis requires completed call-reference coverage",
        );
        report.add_next_step(
            format!(
                "repo-native-alignment enrich --capability call-references --scope repo --repo {}",
                shell_escape_path(repo_root)
            ),
            "enable complete call/reference coverage",
        );
    }
}

pub fn duration_to_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

pub fn format_duration_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let minutes = ms / 60_000;
        let seconds = (ms % 60_000) / 1000;
        if seconds == 0 {
            format!("{}m", minutes)
        } else {
            format!("{}m {}s", minutes, seconds)
        }
    }
}

fn format_count(count: usize) -> String {
    let raw = count.to_string();
    let mut out = String::new();
    for (idx, ch) in raw.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn new_operation_id(kind: OperationKind) -> String {
    format!(
        "{}-{}",
        match kind {
            OperationKind::Scan => "scan",
            OperationKind::FullRebuild => "full-rebuild",
            OperationKind::IncrementalRefresh => "incremental-refresh",
            OperationKind::ExtractOnly => "extract-only",
            OperationKind::Enrich => "enrich",
            OperationKind::CacheLoad => "cache-load",
            OperationKind::StartupIndex => "startup-index",
        },
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn shell_escape_path(path: &Path) -> String {
    let text = path.display().to_string();
    if text
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-'))
    {
        text
    } else {
        format!("'{}'", text.replace('\'', "'\"'\"'"))
    }
}

pub fn scope_key(scope: &EnrichmentScope) -> String {
    scope.stable_key()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_renderer_preserves_structured_capability_states() {
        let repo = Path::new("/tmp/repo");
        let mut report =
            OperationReport::new(OperationKind::ExtractOnly, OperationTrigger::Test, repo)
                .complete(Duration::from_millis(1250));
        report.outputs.symbol_count = Some(1234);
        report.outputs.edge_count = Some(5678);
        report.add_capability(CapabilityReport::new(
            EnrichmentCapability::ExtractedGraph,
            CapabilityState::Completed,
            true,
            Some("repo".to_string()),
            Some("exact search ready".to_string()),
        ));
        report.add_capability(CapabilityReport::new(
            EnrichmentCapability::Embeddings,
            CapabilityState::Skipped,
            false,
            Some("repo".to_string()),
            None,
        ));
        report.add_degradation(QueryClass::SemanticSearch, "embeddings skipped");
        report.add_next_step(
            "repo-native-alignment enrich --capability embeddings --scope repo --repo /tmp/repo",
            "enable semantic search",
        );

        let rendered = report.render_cli(false);
        assert!(rendered.contains("Extract-only scan complete in 1.2s"));
        assert!(rendered.contains("extracted_graph: completed"));
        assert!(rendered.contains("embeddings: skipped"));
        assert!(rendered.contains("semantic search: embeddings skipped"));
        assert!(rendered.contains("repo-native-alignment enrich --capability embeddings"));
    }

    #[test]
    fn operation_report_json_and_cli_deliver_business_context_mode_and_counts() {
        let business_context = BusinessContextAdmission::new(BusinessContextMode::Disabled);
        let mut files = vec![PathBuf::from(".oh/outcomes/leak.md")];
        business_context.retain_repository_files(&mut files);
        assert!(!business_context.admit_git_history_producer());

        let mut report = OperationReport::new(
            OperationKind::ExtractOnly,
            OperationTrigger::Test,
            Path::new("/tmp/repo"),
        );
        report.record_business_context(&business_context);

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["business_context_mode"], "disabled");
        assert_eq!(
            json["business_context_exclusions"]["business_artifact_files"],
            1
        );
        assert_eq!(
            json["business_context_exclusions"]["git_history_producers"],
            1
        );

        let rendered = report.render_cli(false);
        assert!(rendered.contains("business context: disabled"));
        assert!(rendered.contains("1 .oh file(s), 1 Git-history producer(s)"));
    }

    #[tokio::test]
    async fn persisted_lsp_work_queue_is_attached_and_rendered_for_list_roots() {
        use std::collections::BTreeMap;

        use crate::extract::lsp::work_items::{LspWorkItemLedger, LspWorkItemSeed};
        use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

        let repo = tempfile::tempdir().unwrap();
        let node = Node {
            id: NodeId {
                root: "fixture".to_string(),
                file: PathBuf::from("src/lib.rs"),
                name: "queued_symbol".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 1,
            signature: "fn queued_symbol()".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let ledger = LspWorkItemLedger::begin(
            repo.path(),
            &[LspWorkItemSeed {
                item_id: 0,
                node,
                requested_operations: vec!["textDocument/references".to_string()],
                attempt_count: 1,
                toolchain_contract: "fixture-toolchain-v1".to_string(),
            }],
        )
        .await
        .unwrap();
        ledger.mark_phase(0, "requesting_references").await.unwrap();
        ledger.age_records_for_test(Duration::from_secs(10));
        ledger.flush().await.unwrap();

        let mut report =
            OperationReport::new(OperationKind::Enrich, OperationTrigger::Test, repo.path())
                .complete(Duration::from_millis(10));
        report.started_at = unix_now().saturating_sub(30);
        OperationReportStore::hydrate_lsp_work_item_evidence(repo.path(), &mut report);
        assert_eq!(report.lsp_work_item_queues.len(), 1);
        assert_eq!(
            report.related_job_ids,
            [report.lsp_work_item_queues[0].job_id.clone()]
        );
        OperationReportStore::record(repo.path(), report).unwrap();

        let persisted = OperationReportStore::recent(repo.path(), 1);
        assert_eq!(persisted[0].lsp_work_item_queues.len(), 1);
        assert_eq!(persisted[0].related_job_ids.len(), 1);
        assert_eq!(
            persisted[0].related_job_ids[0],
            persisted[0].lsp_work_item_queues[0].job_id
        );
        let rendered = render_recent_reports_markdown(repo.path(), 1);
        assert!(rendered.contains("LSP work queues:"));
        assert!(rendered.contains("in_flight=1"));
        assert!(rendered.contains("phase=requesting_references"));
    }

    #[test]
    fn timings_renderer_shows_failed_and_skipped_phases() {
        let repo = Path::new("/tmp/repo");
        let mut report = OperationReport::new(OperationKind::Scan, OperationTrigger::Test, repo)
            .fail(Duration::from_secs(2), "boom");
        report.add_phase(PhaseReport::ran(
            PhaseKind::Extract,
            Duration::from_millis(500),
        ));
        report.add_phase(PhaseReport::skipped(PhaseKind::Embeddings, "--no-embed"));
        report.add_phase(PhaseReport::failed(
            PhaseKind::Lsp,
            Duration::from_millis(700),
            "server exited",
        ));

        let rendered = report.render_cli(true);
        assert!(rendered.contains("extract: 500ms"));
        assert!(rendered.contains("embeddings: skipped (--no-embed)"));
        assert!(rendered.contains("LSP call references: 700ms (server exited)"));
        assert!(rendered.contains("Failure: boom"));
    }

    #[test]
    fn requested_lsp_with_zero_edges_is_not_marked_unavailable() {
        let repo = Path::new("/tmp/repo");
        let mut report = OperationReport::new(OperationKind::Scan, OperationTrigger::Test, repo)
            .complete(Duration::from_millis(10));
        for capability in scan_capability_reports(
            super::super::enrichment_jobs::ScanEnrichmentOptions::all(),
            CapabilityState::Completed,
            Some("semantic index loaded".to_string()),
            CapabilityState::Completed,
            Some("zero edges can be valid".to_string()),
            Some("repo".to_string()),
        ) {
            report.add_capability(capability);
        }
        add_scan_degradation_and_next_steps(
            &mut report,
            repo,
            super::super::enrichment_jobs::ScanEnrichmentOptions::all(),
            CapabilityState::Completed,
            CapabilityState::Completed,
        );

        let call_refs = report
            .capabilities
            .iter()
            .find(|capability| capability.capability == EnrichmentCapability::CallReferences)
            .unwrap();
        assert_eq!(call_refs.state, CapabilityState::Completed);
        assert!(
            !report
                .degradation
                .iter()
                .any(|notice| notice.query_class == QueryClass::GlobalImpact)
        );
    }

    #[test]
    fn running_lsp_is_reported_as_degraded_not_completed() {
        let repo = Path::new("/tmp/repo");
        let mut report = OperationReport::new(OperationKind::Scan, OperationTrigger::Test, repo)
            .complete(Duration::from_millis(10));
        for capability in scan_capability_reports(
            super::super::enrichment_jobs::ScanEnrichmentOptions::all(),
            CapabilityState::Completed,
            Some("semantic index loaded".to_string()),
            CapabilityState::Running,
            Some("call-reference enrichment is running".to_string()),
            Some("repo".to_string()),
        ) {
            report.add_capability(capability);
        }
        add_scan_degradation_and_next_steps(
            &mut report,
            repo,
            super::super::enrichment_jobs::ScanEnrichmentOptions::all(),
            CapabilityState::Completed,
            CapabilityState::Running,
        );

        let call_refs = report
            .capabilities
            .iter()
            .find(|capability| capability.capability == EnrichmentCapability::CallReferences)
            .unwrap();
        assert_eq!(call_refs.state, CapabilityState::Running);
        assert!(
            report
                .degradation
                .iter()
                .any(|notice| notice.query_class == QueryClass::GlobalImpact)
        );
    }

    #[test]
    fn lsp_capability_complete_with_zero_edges_is_stale() {
        let (state, detail) = lsp_capability_from_status(
            super::super::enrichment_jobs::ScanEnrichmentOptions::all(),
            super::super::state::LspState::Complete,
            None,
            0,
            false,
        );

        assert_eq!(state, CapabilityState::Stale);
        assert!(
            detail
                .as_deref()
                .unwrap_or_default()
                .contains("0 persisted call/reference edges"),
            "got: {:?}",
            detail
        );
    }

    #[test]
    fn lsp_capability_does_not_complete_from_edge_count_alone() {
        let (state, detail) = lsp_capability_from_status(
            super::super::enrichment_jobs::ScanEnrichmentOptions::all(),
            super::super::state::LspState::NotStarted,
            None,
            12,
            false,
        );

        assert_eq!(state, CapabilityState::Stale);
        assert!(
            detail
                .as_deref()
                .unwrap_or_default()
                .contains("no completed call-reference job"),
            "got: {:?}",
            detail
        );
    }

    #[test]
    fn lsp_capability_reports_running_when_related_job_exists() {
        let (state, detail) = lsp_capability_from_status(
            super::super::enrichment_jobs::ScanEnrichmentOptions::all(),
            super::super::state::LspState::NotStarted,
            None,
            12,
            true,
        );

        assert_eq!(state, CapabilityState::Running);
        assert!(
            detail.as_deref().unwrap_or_default().contains("job exists"),
            "got: {:?}",
            detail
        );
    }

    #[test]
    fn lsp_capability_preserves_degraded_coverage_and_diagnostic() {
        let (state, detail) = lsp_capability_from_status(
            super::super::enrichment_jobs::ScanEnrichmentOptions::all(),
            super::super::state::LspState::Degraded,
            Some("forced no-progress abort after 11 attempted nodes"),
            7,
            true,
        );

        assert_eq!(state, CapabilityState::Degraded);
        let detail = detail.expect("degraded capability has actionable detail");
        assert!(detail.contains("7 partial edges"), "got: {detail}");
        assert!(detail.contains("forced no-progress abort"), "got: {detail}");
    }

    #[test]
    fn store_marks_non_terminal_reports_stale_on_read() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path();
        let running = OperationReport::new(OperationKind::Enrich, OperationTrigger::Test, repo);
        let store = OperationReportStore {
            schema_version: SCHEMA_VERSION,
            reports: vec![running],
        };
        store.write(repo).unwrap();

        let read = OperationReportStore::read(repo).unwrap();
        assert_eq!(read.reports[0].state, OperationState::Stale);
        assert!(
            read.reports[0]
                .failure
                .as_deref()
                .unwrap()
                .contains("non-terminal")
        );
        let persisted = OperationReportStore::read(repo).unwrap();
        assert_eq!(persisted.reports[0].state, OperationState::Stale);
    }

    #[test]
    fn read_only_store_does_not_recover_or_rewrite_non_terminal_reports() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path();
        let running = OperationReport::new(OperationKind::Enrich, OperationTrigger::Test, repo);
        let store = OperationReportStore {
            schema_version: SCHEMA_VERSION,
            reports: vec![running],
        };
        store.write(repo).unwrap();
        let path = reports_path(repo);
        let before = std::fs::read(&path).unwrap();

        let read = OperationReportStore::read_only(repo).unwrap();

        assert!(!read.reports[0].state.is_terminal());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn record_recovers_from_corrupt_history_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path();
        let path = reports_path(repo);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json").unwrap();

        let report = OperationReport::new(OperationKind::Scan, OperationTrigger::Test, repo)
            .complete(Duration::from_millis(1));
        OperationReportStore::record(repo, report).unwrap();

        let read = OperationReportStore::read(repo).unwrap();
        assert_eq!(read.reports.len(), 1);
        assert_eq!(read.reports[0].operation, OperationKind::Scan);
    }

    #[test]
    fn store_prunes_old_reports() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path();
        for _ in 0..3 {
            let report = OperationReport::new(OperationKind::Scan, OperationTrigger::Test, repo)
                .complete(Duration::from_millis(1));
            OperationReportStore::record_with_limit(repo, report, 2).unwrap();
        }
        let read = OperationReportStore::read(repo).unwrap();
        assert_eq!(read.reports.len(), 2);
    }
}
