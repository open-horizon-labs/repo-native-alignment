use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use crate::graph::{Node, NodeKind};

/// Semantic LSP operation used for both admission and yield accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LspQueryOperation {
    CallHierarchy,
    References,
    Implementations,
    TypeHierarchy,
    DocumentLinks,
}

impl LspQueryOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CallHierarchy => "call_hierarchy",
            Self::References => "references",
            Self::Implementations => "implementations",
            Self::TypeHierarchy => "type_hierarchy",
            Self::DocumentLinks => "document_links",
        }
    }

    pub(crate) const fn phase(self) -> &'static str {
        match self {
            Self::CallHierarchy => "requesting_call_hierarchy",
            Self::References => "requesting_references",
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
            NodeKind::Other(_) => Some(Self::Other),
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
    pub implementations: bool,
    pub type_hierarchy: bool,
    pub document_links: bool,
}

impl LspServerCapabilities {
    fn supports(self, operation: LspQueryOperation) -> bool {
        match operation {
            LspQueryOperation::CallHierarchy => self.call_hierarchy,
            LspQueryOperation::References => self.references,
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

/// Shared language/server query profile used by construction and every symbol
/// scheduling pass.
#[derive(Debug, Clone)]
pub(crate) struct LspQueryProfile {
    language: String,
    server: String,
    allowed_kinds: Option<Vec<NodeKind>>,
    allow_declared_const_references: bool,
    operation_limits: BTreeMap<LspQueryOperation, usize>,
}

impl LspQueryProfile {
    pub(crate) fn new(language: &str, server: &str) -> Self {
        Self {
            language: language.to_string(),
            server: server.to_string(),
            allowed_kinds: None,
            allow_declared_const_references: false,
            operation_limits: BTreeMap::new(),
        }
    }

    pub(crate) fn with_allowed_kinds(mut self, kinds: &'static [NodeKind]) -> Self {
        self.allowed_kinds = Some(kinds.to_vec());
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
    pub(crate) fn allowed_kinds(&self) -> Option<&[NodeKind]> {
        self.allowed_kinds.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn with_declared_const_references(mut self, allow: bool) -> Self {
        self.allow_declared_const_references = allow;
        self
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
            .as_deref()
            .is_some_and(|kinds| !kinds.contains(&node.id.kind))
        {
            return false;
        }

        let Some(declaration) = LspDeclarationClass::from_kind(&node.id.kind) else {
            return false;
        };
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
                ) && (declaration != LspDeclarationClass::Const
                    || self.allow_declared_const_references)
            }
            LspQueryOperation::Implementations => declaration == LspDeclarationClass::Trait,
            LspQueryOperation::TypeHierarchy => matches!(
                declaration,
                LspDeclarationClass::Trait
                    | LspDeclarationClass::Struct
                    | LspDeclarationClass::Enum
            ),
            LspQueryOperation::DocumentLinks => declaration == LspDeclarationClass::Other,
        };

        operation_matches && budget.reserve(operation)
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
    pending_work: BTreeMap<LspQueryMetricKey, usize>,
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
        operation: LspQueryOperation,
        declaration: LspDeclarationClass,
    ) {
        let key = LspQueryMetricKey {
            operation,
            declaration,
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *state.pending_work.entry(key).or_default() += 1;
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
        let key = LspQueryMetricKey {
            operation,
            declaration,
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(pending) = state.pending_work.get_mut(&key) {
            *pending = pending.saturating_sub(1);
        }
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
    /// items have already decremented their pending count in `record`, so this
    /// drains only queued or in-flight operations and cannot double count them.
    pub(crate) fn record_job_timeout(&self, latency: Duration) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let pending = std::mem::take(&mut state.pending_work);
        for (key, count) in pending {
            if count == 0 {
                continue;
            }
            let metric = state.metrics.entry(key.clone()).or_insert_with(|| LspQueryMetric {
                language: self.language.clone(),
                server: self.server.clone(),
                operation: key.operation.as_str().to_string(),
                declaration_class: key.declaration.as_str().to_string(),
                scheduled_requests: 0,
                non_empty_responses: 0,
                emitted_edges: 0,
                latency_ms: 0,
                timeouts: 0,
                errors: 0,
            });
            metric.latency_ms = metric.latency_ms.saturating_add(
                latency
                    .as_millis()
                    .saturating_mul(count as u128)
                    .min(u64::MAX as u128) as u64,
            );
            metric.timeouts = metric.timeouts.saturating_add(count);
            metric.errors = metric.errors.saturating_add(count);
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
        assert!(profile.clone().with_declared_const_references(true).admits(
            &node(NodeKind::Const),
            LspQueryOperation::References,
            capabilities,
            &mut profile.budget()
        ));
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
        telemetry.register_work_item(
            LspQueryOperation::CallHierarchy,
            LspDeclarationClass::Function,
        );
        telemetry.register_work_item(
            LspQueryOperation::CallHierarchy,
            LspDeclarationClass::Function,
        );
        telemetry.record(
            LspQueryOperation::CallHierarchy,
            LspDeclarationClass::Function,
            3,
            1,
            2,
            Duration::from_millis(10),
            0,
            0,
        );

        telemetry.record_job_timeout(Duration::from_millis(50));

        let metrics = telemetry.snapshot();
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].scheduled_requests, 3);
        assert_eq!(metrics[0].timeouts, 1);
        assert_eq!(metrics[0].errors, 1);
        assert_eq!(metrics[0].latency_ms, 60);
    }
}
