use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::graph::{Node, NodeKind};

/// Semantic LSP operation used for both admission and yield accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LspQueryOperation {
    CallHierarchy,
    References,
    Definitions,
    Implementations,
    TypeHierarchy,
    DocumentLinks,
}

impl LspQueryOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CallHierarchy => "call_hierarchy",
            Self::References => "references",
            Self::Definitions => "definitions",
            Self::Implementations => "implementations",
            Self::TypeHierarchy => "type_hierarchy",
            Self::DocumentLinks => "document_links",
        }
    }

    pub(crate) const fn phase(self) -> &'static str {
        match self {
            Self::CallHierarchy => "requesting_call_hierarchy",
            Self::References => "requesting_references",
            Self::Definitions => "requesting_definitions",
            Self::Implementations => "requesting_implementations",
            Self::TypeHierarchy => "requesting_type_hierarchy",
            Self::DocumentLinks => "requesting_document_links",
        }
    }
}

impl std::fmt::Display for LspQueryOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stable declaration bucket used in admission rules and telemetry dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LspDeclarationClass {
    Function,
    Trait,
    Struct,
    Enum,
    TypeAlias,
    Const,
    Other,
}

impl LspDeclarationClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Trait => "trait",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::TypeAlias => "type_alias",
            Self::Const => "const",
            Self::Other => "other",
        }
    }

    pub(crate) fn from_kind(kind: &NodeKind) -> Option<Self> {
        match kind {
            NodeKind::Function => Some(Self::Function),
            NodeKind::Trait => Some(Self::Trait),
            NodeKind::Struct => Some(Self::Struct),
            NodeKind::Enum => Some(Self::Enum),
            NodeKind::TypeAlias => Some(Self::TypeAlias),
            NodeKind::Const => Some(Self::Const),
            NodeKind::Other(_) | NodeKind::MarkdownSection => Some(Self::Other),
            _ => None,
        }
    }
}

impl std::fmt::Display for LspDeclarationClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Negotiated capabilities that participate in admission.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LspServerCapabilities {
    pub references: bool,
    pub call_hierarchy: bool,
    pub definitions: bool,
    pub implementations: bool,
    pub type_hierarchy: bool,
    pub document_links: bool,
}

impl LspServerCapabilities {
    fn supports(self, operation: LspQueryOperation) -> bool {
        match operation {
            LspQueryOperation::CallHierarchy => self.call_hierarchy,
            LspQueryOperation::References => self.references,
            LspQueryOperation::Definitions => self.definitions,
            LspQueryOperation::Implementations => self.implementations,
            LspQueryOperation::TypeHierarchy => self.type_hierarchy,
            LspQueryOperation::DocumentLinks => self.document_links,
        }
    }
}

/// Per-run request budget. The profile establishes the seam; #769 can supply
/// scope-specific finite limits without changing scheduling again.
#[derive(Debug, Clone)]
pub(crate) struct LspQueryBudget {
    remaining: BTreeMap<LspQueryOperation, usize>,
}

impl LspQueryBudget {
    fn from_limits(limits: &BTreeMap<LspQueryOperation, usize>) -> Self {
        Self {
            remaining: limits.clone(),
        }
    }

    fn reserve(&mut self, operation: LspQueryOperation) -> bool {
        let remaining = self.remaining.entry(operation).or_insert(usize::MAX);
        if *remaining == 0 {
            return false;
        }
        *remaining = remaining.saturating_sub(1);
        true
    }
}

/// Shared hard boundary for one explicit broad-reference request.
///
/// The same value is attached to every language enricher in the bus, so the
/// request limit is global rather than multiplied by the number of servers.
#[derive(Debug)]
pub(crate) struct LspBroadReferenceBudget {
    max_requests: usize,
    max_duration: Duration,
    started_at: Instant,
    scheduled_requests: AtomicUsize,
    circuit_open: AtomicBool,
    circuit_reason: Mutex<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LspBroadReferenceBudgetSnapshot {
    pub max_requests: usize,
    pub max_duration_ms: u64,
    pub scheduled_requests: usize,
    pub elapsed_ms: u64,
    pub circuit_open: bool,
    pub circuit_reason: Option<String>,
}

impl LspBroadReferenceBudget {
    pub(crate) fn new(max_requests: usize, max_duration: Duration) -> Self {
        Self {
            max_requests,
            max_duration,
            started_at: Instant::now(),
            scheduled_requests: AtomicUsize::new(0),
            circuit_open: AtomicBool::new(false),
            circuit_reason: Mutex::new(None),
        }
    }

    fn open_circuit(&self, reason: impl Into<String>) {
        self.circuit_open.store(true, Ordering::Release);
        let mut stored = self
            .circuit_reason
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if stored.is_none() {
            *stored = Some(reason.into());
        }
    }

    fn reserve_reference(&self) -> bool {
        if self.started_at.elapsed() >= self.max_duration {
            self.open_circuit(format!(
                "broad-reference time budget exhausted after {}ms",
                self.max_duration.as_millis()
            ));
            return false;
        }
        if self.circuit_open.load(Ordering::Acquire) {
            return false;
        }
        let reserved = self
            .scheduled_requests
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < self.max_requests).then_some(current + 1)
            })
            .is_ok();
        if !reserved {
            self.open_circuit(format!(
                "broad-reference request budget exhausted at {} requests",
                self.max_requests
            ));
        }
        reserved
    }

    pub(crate) fn remaining_duration(&self) -> Option<Duration> {
        self.max_duration.checked_sub(self.started_at.elapsed())
    }

    pub(crate) fn open_time_circuit(&self) {
        self.open_circuit(format!(
            "broad-reference time budget exhausted after {}ms",
            self.max_duration.as_millis()
        ));
    }

    pub(crate) fn snapshot(&self) -> LspBroadReferenceBudgetSnapshot {
        LspBroadReferenceBudgetSnapshot {
            max_requests: self.max_requests,
            max_duration_ms: self.max_duration.as_millis().min(u64::MAX as u128) as u64,
            scheduled_requests: self.scheduled_requests.load(Ordering::Acquire),
            elapsed_ms: self.started_at.elapsed().as_millis().min(u64::MAX as u128) as u64,
            circuit_open: self.circuit_open.load(Ordering::Acquire),
            circuit_reason: self
                .circuit_reason
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone(),
        }
    }
}

/// Shared language/server query profile used by construction and every symbol
/// scheduling pass.
#[derive(Debug, Clone)]
pub(crate) struct LspQueryProfile {
    language: String,
    server: String,
    allowed_kinds: Option<HashSet<NodeKind>>,
    allow_declared_const_references: bool,
    allow_broad_references: bool,
    broad_reference_budget: Option<std::sync::Arc<LspBroadReferenceBudget>>,
    operation_limits: BTreeMap<LspQueryOperation, usize>,
}

impl LspQueryProfile {
    pub(crate) fn new(language: &str, server: &str) -> Self {
        Self {
            language: language.to_string(),
            server: server.to_string(),
            allowed_kinds: None,
            allow_declared_const_references: false,
            allow_broad_references: false,
            broad_reference_budget: None,
            operation_limits: BTreeMap::new(),
        }
    }

    pub(crate) fn with_allowed_kinds(mut self, kinds: &'static [NodeKind]) -> Self {
        self.allowed_kinds = Some(kinds.iter().cloned().collect());
        self
    }

    #[cfg(test)]
    pub(crate) fn with_operation_limit(
        mut self,
        operation: LspQueryOperation,
        limit: usize,
    ) -> Self {
        self.operation_limits.insert(operation, limit);
        self
    }

    pub(crate) fn budget(&self) -> LspQueryBudget {
        LspQueryBudget::from_limits(&self.operation_limits)
    }

    pub(crate) fn language(&self) -> &str {
        &self.language
    }

    pub(crate) fn server(&self) -> &str {
        &self.server
    }

    #[cfg(test)]
    pub(crate) fn allows_declared_const_references(&self) -> bool {
        self.allow_declared_const_references
    }

    #[cfg(test)]
    pub(crate) fn allowed_kinds(&self) -> Option<&HashSet<NodeKind>> {
        self.allowed_kinds.as_ref()
    }

    pub(crate) fn with_declared_const_references(mut self, allow: bool) -> Self {
        self.allow_declared_const_references = allow;
        self
    }

    pub(crate) fn with_broad_references(
        mut self,
        budget: std::sync::Arc<LspBroadReferenceBudget>,
    ) -> Self {
        self.allow_broad_references = true;
        self.broad_reference_budget = Some(budget);
        self
    }

    pub(crate) fn with_broad_references_unbudgeted(mut self) -> Self {
        self.allow_broad_references = true;
        self
    }

    pub(crate) fn broad_reference_budget(
        &self,
    ) -> Option<&std::sync::Arc<LspBroadReferenceBudget>> {
        self.broad_reference_budget.as_ref()
    }

    pub(crate) fn admits(
        &self,
        node: &Node,
        operation: LspQueryOperation,
        capabilities: LspServerCapabilities,
        budget: &mut LspQueryBudget,
    ) -> bool {
        if !self.accepts_declaration(node) || !capabilities.supports(operation) {
            return false;
        }

        if self
            .allowed_kinds
            .as_ref()
            .is_some_and(|kinds| !kinds.contains(&node.id.kind))
        {
            return false;
        }

        let Some(declaration) = LspDeclarationClass::from_kind(&node.id.kind) else {
            return false;
        };
        let broad_reference = operation == LspQueryOperation::References
            && matches!(
                declaration,
                LspDeclarationClass::Struct
                    | LspDeclarationClass::Enum
                    | LspDeclarationClass::TypeAlias
                    | LspDeclarationClass::Const
            );
        if broad_reference && !self.allow_broad_references {
            return false;
        }

        let operation_matches = match operation {
            LspQueryOperation::CallHierarchy => declaration == LspDeclarationClass::Function,
            LspQueryOperation::References => {
                matches!(
                    declaration,
                    LspDeclarationClass::Function
                        | LspDeclarationClass::Struct
                        | LspDeclarationClass::Enum
                        | LspDeclarationClass::TypeAlias
                        | LspDeclarationClass::Const
                        | LspDeclarationClass::Other
                ) && (declaration != LspDeclarationClass::Const
                    || self.allow_declared_const_references)
            }
            LspQueryOperation::Definitions => declaration == LspDeclarationClass::Other,
            LspQueryOperation::Implementations => declaration == LspDeclarationClass::Trait,
            LspQueryOperation::TypeHierarchy => matches!(
                declaration,
                LspDeclarationClass::Trait
                    | LspDeclarationClass::Struct
                    | LspDeclarationClass::Enum
            ),
            LspQueryOperation::DocumentLinks => declaration == LspDeclarationClass::Other,
        };

        operation_matches
            && budget.reserve(operation)
            && (!broad_reference
                || self
                    .broad_reference_budget
                    .as_ref()
                    .is_none_or(|budget| budget.reserve_reference()))
    }

    pub(crate) fn accepts_declaration(&self, node: &Node) -> bool {
        node.language == self.language
            && node.metadata.get("synthetic").map(String::as_str) != Some("true")
            && !matches!(&node.id.kind, NodeKind::Other(kind) if kind == "crate" || kind == "diagnostic")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspQueryMetric {
    pub language: String,
    pub server: String,
    pub operation: String,
    pub declaration_class: String,
    pub scheduled_requests: usize,
    pub non_empty_responses: usize,
    pub emitted_edges: usize,
    pub latency_ms: u64,
    pub timeouts: usize,
    pub errors: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LspQueryMetricKey {
    operation: LspQueryOperation,
    declaration: LspDeclarationClass,
}

#[derive(Debug, Default)]
pub(crate) struct LspQueryTelemetry {
    language: String,
    server: String,
    state: Mutex<LspQueryTelemetryState>,
}

#[derive(Debug, Default)]
struct LspQueryTelemetryState {
    metrics: BTreeMap<LspQueryMetricKey, LspQueryMetric>,
    pending_work: BTreeMap<usize, PendingLspQuery>,
    deadline_closed: bool,
}

#[derive(Debug)]
struct PendingLspQuery {
    key: LspQueryMetricKey,
    scheduled_requests: usize,
    started_at: std::time::Instant,
}

impl LspQueryTelemetry {
    pub(crate) fn new(profile: &LspQueryProfile) -> Self {
        Self {
            language: profile.language().to_string(),
            server: profile.server().to_string(),
            state: Mutex::new(LspQueryTelemetryState::default()),
        }
    }

    pub(crate) fn register_work_item(
        &self,
        work_item_id: usize,
        operation: LspQueryOperation,
        declaration: LspDeclarationClass,
    ) -> bool {
        let key = LspQueryMetricKey {
            operation,
            declaration,
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.deadline_closed {
            return false;
        }
        state.pending_work.insert(
            work_item_id,
            PendingLspQuery {
                key,
                scheduled_requests: 0,
                started_at: std::time::Instant::now(),
            },
        );
        true
    }

    pub(crate) fn note_requests_started(&self, work_item_id: usize, count: usize) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(pending) = state.pending_work.get_mut(&work_item_id) {
            pending.scheduled_requests = pending.scheduled_requests.saturating_add(count);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record(
        &self,
        operation: LspQueryOperation,
        declaration: LspDeclarationClass,
        scheduled_requests: usize,
        non_empty_responses: usize,
        emitted_edges: usize,
        latency: Duration,
        errors: usize,
        timeouts: usize,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.record_locked(
            &mut state,
            LspQueryMetricKey {
                operation,
                declaration,
            },
            scheduled_requests,
            non_empty_responses,
            emitted_edges,
            latency,
            errors,
            timeouts,
        );
    }

    /// Complete a registered Pass 1 item exactly once. If the outer deadline
    /// already claimed the item, a late worker completion is ignored.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_work_item(
        &self,
        work_item_id: usize,
        non_empty_responses: usize,
        emitted_edges: usize,
        latency: Duration,
        errors: usize,
        timeouts: usize,
    ) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(pending) = state.pending_work.remove(&work_item_id) else {
            return false;
        };
        self.record_locked(
            &mut state,
            pending.key,
            pending.scheduled_requests,
            non_empty_responses,
            emitted_edges,
            latency,
            errors,
            timeouts,
        );
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn record_locked(
        &self,
        state: &mut LspQueryTelemetryState,
        key: LspQueryMetricKey,
        scheduled_requests: usize,
        non_empty_responses: usize,
        emitted_edges: usize,
        latency: Duration,
        errors: usize,
        timeouts: usize,
    ) {
        let operation = key.operation;
        let declaration = key.declaration;
        let metric = state.metrics.entry(key).or_insert_with(|| LspQueryMetric {
            language: self.language.clone(),
            server: self.server.clone(),
            operation: operation.as_str().to_string(),
            declaration_class: declaration.as_str().to_string(),
            scheduled_requests: 0,
            non_empty_responses: 0,
            emitted_edges: 0,
            latency_ms: 0,
            timeouts: 0,
            errors: 0,
        });
        metric.scheduled_requests += scheduled_requests;
        metric.non_empty_responses += non_empty_responses;
        metric.emitted_edges += emitted_edges;
        metric.latency_ms = metric
            .latency_ms
            .saturating_add(latency.as_millis().min(u64::MAX as u128) as u64);
        metric.errors += errors;
        metric.timeouts += timeouts;
    }

    /// Attribute work items cancelled by the outer job deadline. Completed
    /// items have already atomically removed their IDs, so this drains only
    /// in-flight operations and owns their terminal outcome.
    pub(crate) fn record_job_timeout(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.deadline_closed = true;
        let pending = std::mem::take(&mut state.pending_work);
        for (_, pending) in pending {
            self.record_locked(
                &mut state,
                pending.key,
                pending.scheduled_requests,
                0,
                0,
                pending.started_at.elapsed(),
                1,
                1,
            );
        }
    }

    pub(crate) fn snapshot(&self) -> Vec<LspQueryMetric> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .metrics
            .values()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use crate::graph::{ExtractionSource, NodeId};

    use super::*;

    fn node(kind: NodeKind) -> Node {
        Node {
            id: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/lib.rs"),
                name: "target".to_string(),
                kind,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 1,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    #[test]
    fn operation_admission_is_capability_kind_and_budget_aware() {
        let profile = LspQueryProfile::new("rust", "rust-analyzer")
            .with_operation_limit(LspQueryOperation::CallHierarchy, 1);
        let capabilities = LspServerCapabilities {
            call_hierarchy: true,
            ..Default::default()
        };
        let mut budget = profile.budget();

        assert!(profile.admits(
            &node(NodeKind::Function),
            LspQueryOperation::CallHierarchy,
            capabilities,
            &mut budget
        ));
        assert!(!profile.admits(
            &node(NodeKind::Function),
            LspQueryOperation::CallHierarchy,
            capabilities,
            &mut budget
        ));
        assert!(!profile.admits(
            &node(NodeKind::Struct),
            LspQueryOperation::CallHierarchy,
            capabilities,
            &mut profile.budget()
        ));
    }

    #[test]
    fn server_profile_applies_to_references_implementations_and_type_hierarchy() {
        static FUNCTION_AND_TRAIT: &[NodeKind] = &[NodeKind::Function, NodeKind::Trait];
        let profile = LspQueryProfile::new("python", "pyright-langserver")
            .with_allowed_kinds(FUNCTION_AND_TRAIT);
        let capabilities = LspServerCapabilities {
            references: true,
            implementations: true,
            type_hierarchy: true,
            ..Default::default()
        };
        let mut function = node(NodeKind::Function);
        function.language = "python".to_string();
        let mut trait_node = node(NodeKind::Trait);
        trait_node.language = "python".to_string();
        let mut struct_node = node(NodeKind::Struct);
        struct_node.language = "python".to_string();

        assert!(profile.admits(
            &function,
            LspQueryOperation::References,
            capabilities,
            &mut profile.budget()
        ));
        assert!(profile.admits(
            &trait_node,
            LspQueryOperation::Implementations,
            capabilities,
            &mut profile.budget()
        ));
        assert!(!profile.admits(
            &struct_node,
            LspQueryOperation::References,
            capabilities,
            &mut profile.budget()
        ));
        assert!(!profile.admits(
            &struct_node,
            LspQueryOperation::TypeHierarchy,
            capabilities,
            &mut profile.budget()
        ));
    }

    #[test]
    fn synthetic_and_declared_const_references_are_default_denied() {
        let profile = LspQueryProfile::new("rust", "rust-analyzer");
        let capabilities = LspServerCapabilities {
            references: true,
            ..Default::default()
        };
        let mut synthetic = node(NodeKind::Function);
        synthetic
            .metadata
            .insert("synthetic".to_string(), "true".to_string());

        assert!(!profile.admits(
            &synthetic,
            LspQueryOperation::References,
            capabilities,
            &mut profile.budget()
        ));
        assert!(!profile.admits(
            &node(NodeKind::Const),
            LspQueryOperation::References,
            capabilities,
            &mut profile.budget()
        ));
        let broad_budget =
            std::sync::Arc::new(LspBroadReferenceBudget::new(1, Duration::from_secs(1)));
        assert!(
            profile
                .clone()
                .with_declared_const_references(true)
                .with_broad_references(broad_budget)
                .admits(
                    &node(NodeKind::Const),
                    LspQueryOperation::References,
                    capabilities,
                    &mut profile.budget()
                )
        );
    }

    #[test]
    fn broad_references_are_default_denied_and_share_one_request_circuit() {
        let capabilities = LspServerCapabilities {
            references: true,
            ..Default::default()
        };
        let budget = std::sync::Arc::new(LspBroadReferenceBudget::new(1, Duration::from_secs(1)));
        let profile =
            LspQueryProfile::new("rust", "rust-analyzer").with_broad_references(budget.clone());

        assert!(profile.admits(
            &node(NodeKind::Struct),
            LspQueryOperation::References,
            capabilities,
            &mut profile.budget()
        ));
        assert!(!profile.admits(
            &node(NodeKind::Enum),
            LspQueryOperation::References,
            capabilities,
            &mut profile.budget()
        ));
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.scheduled_requests, 1);
        assert!(snapshot.circuit_open);
        assert!(
            snapshot
                .circuit_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("request budget exhausted"))
        );
    }

    #[test]
    fn rejected_profile_work_does_not_consume_the_shared_request_budget() {
        let capabilities = LspServerCapabilities {
            references: true,
            ..Default::default()
        };
        let shared = std::sync::Arc::new(LspBroadReferenceBudget::new(1, Duration::from_secs(1)));
        let rejecting = LspQueryProfile::new("rust", "rust-analyzer")
            .with_operation_limit(LspQueryOperation::References, 0)
            .with_broad_references(shared.clone());
        let accepting =
            LspQueryProfile::new("rust", "rust-analyzer").with_broad_references(shared.clone());

        assert!(!rejecting.admits(
            &node(NodeKind::Struct),
            LspQueryOperation::References,
            capabilities,
            &mut rejecting.budget()
        ));
        assert_eq!(shared.snapshot().scheduled_requests, 0);
        assert!(accepting.admits(
            &node(NodeKind::Enum),
            LspQueryOperation::References,
            capabilities,
            &mut accepting.budget()
        ));
        let snapshot = shared.snapshot();
        assert_eq!(snapshot.scheduled_requests, 1);
        assert!(!snapshot.circuit_open);
    }

    #[test]
    fn expired_time_budget_opens_circuit_before_scheduling() {
        let budget = std::sync::Arc::new(LspBroadReferenceBudget::new(10, Duration::ZERO));
        let profile =
            LspQueryProfile::new("rust", "rust-analyzer").with_broad_references(budget.clone());
        let capabilities = LspServerCapabilities {
            references: true,
            ..Default::default()
        };

        assert!(!profile.admits(
            &node(NodeKind::Struct),
            LspQueryOperation::References,
            capabilities,
            &mut profile.budget()
        ));
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.scheduled_requests, 0);
        assert!(snapshot.circuit_open);
        assert!(
            snapshot
                .circuit_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("time budget exhausted"))
        );
    }

    #[test]
    fn telemetry_aggregates_by_operation_and_declaration() {
        let profile = LspQueryProfile::new("rust", "rust-analyzer");
        let telemetry = LspQueryTelemetry::new(&profile);
        telemetry.record(
            LspQueryOperation::References,
            LspDeclarationClass::Struct,
            1,
            1,
            2,
            Duration::from_millis(7),
            0,
            0,
        );
        telemetry.record(
            LspQueryOperation::References,
            LspDeclarationClass::Struct,
            1,
            0,
            0,
            Duration::from_millis(3),
            1,
            1,
        );

        assert_eq!(
            telemetry.snapshot(),
            vec![LspQueryMetric {
                language: "rust".to_string(),
                server: "rust-analyzer".to_string(),
                operation: "references".to_string(),
                declaration_class: "struct".to_string(),
                scheduled_requests: 2,
                non_empty_responses: 1,
                emitted_edges: 2,
                latency_ms: 10,
                timeouts: 1,
                errors: 1,
            }]
        );
    }

    #[test]
    fn job_timeout_attributes_only_unfinished_work_items() {
        let profile = LspQueryProfile::new("rust", "rust-analyzer");
        let telemetry = LspQueryTelemetry::new(&profile);
        assert!(telemetry.register_work_item(
            1,
            LspQueryOperation::CallHierarchy,
            LspDeclarationClass::Function,
        ));
        assert!(telemetry.register_work_item(
            2,
            LspQueryOperation::CallHierarchy,
            LspDeclarationClass::Function,
        ));
        telemetry.note_requests_started(1, 3);
        telemetry.note_requests_started(2, 2);
        assert!(telemetry.record_work_item(1, 1, 2, Duration::from_millis(10), 0, 0,));

        telemetry.record_job_timeout();
        assert!(!telemetry.register_work_item(
            3,
            LspQueryOperation::References,
            LspDeclarationClass::Struct,
        ));
        assert!(!telemetry.record_work_item(2, 1, 1, Duration::from_millis(60), 0, 0,));

        let metrics = telemetry.snapshot();
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].scheduled_requests, 5);
        assert_eq!(metrics[0].timeouts, 1);
        assert_eq!(metrics[0].errors, 1);
        assert!(metrics[0].latency_ms >= 10);
    }
}
