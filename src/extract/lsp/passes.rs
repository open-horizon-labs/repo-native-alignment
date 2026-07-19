//! LSP enrichment pass functions extracted from the monolithic `enrich()` method.
//!
//! Each pass is an `async fn` on `LspEnricher` that takes shared state and
//! appends results to the `EnrichmentResult`. The top-level `enrich()` orchestrates
//! them in sequence: Pass 0 -> Pass 1 -> Pass 2 -> Pass 4 -> Pass 5 -> Pass 3.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::Mutex;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

use super::policy::{
    LspDeclarationClass, LspQueryBudget, LspQueryOperation, LspQueryTelemetry,
    LspServerCapabilities,
};
use super::transport::{PipelinedTransport, path_to_uri, uri_to_relative_path};
use super::work_items::{LspWorkItemLedger, LspWorkItemSeed};
use super::{
    EnrichmentResult, LspEnricher, ZERO_EDGE_ABORT_THRESHOLD, ZERO_EDGE_MIN_WARMUP,
    ZERO_EDGE_TIMEOUT, lsp_job_timeout, lsp_language_id, materialize_document_symbols,
    normalized_document_symbol_evidence, read_lsp_text,
};
use crate::scanner::LspConfig;

const PASS1_DIAGNOSTIC_SAMPLE_LIMIT: usize = 5;
const PASS1_DEFAULT_NO_PROGRESS_TIMEOUT: Duration = Duration::from_secs(120);
const DID_OPEN_DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
struct EndpointLookupIndex {
    functions_by_file_and_name: HashMap<PathBuf, HashMap<String, Vec<NodeId>>>,
    enclosing_by_file: HashMap<PathBuf, EnclosingLineIndex>,
}

#[derive(Debug, Default)]
struct EnclosingLineIndex {
    changes: Vec<(usize, Option<NodeId>)>,
}

impl EnclosingLineIndex {
    fn build(nodes: &[Node]) -> Self {
        let indexed = nodes
            .iter()
            .filter(|node| {
                matches!(
                    node.id.kind,
                    NodeKind::Function
                        | NodeKind::Impl
                        | NodeKind::Struct
                        | NodeKind::Trait
                        | NodeKind::Enum
                        | NodeKind::TypeAlias
                        | NodeKind::Const
                )
            })
            .map(|node| {
                let line_end = node.line_end.max(node.line_start);
                (
                    line_end.saturating_sub(node.line_start),
                    node.stable_id(),
                    node.id.clone(),
                    node.line_start,
                    line_end,
                )
            })
            .collect::<Vec<_>>();
        let mut events = BTreeMap::<usize, (Vec<usize>, Vec<usize>)>::new();
        for (index, (_, _, _, line_start, line_end)) in indexed.iter().enumerate() {
            events.entry(*line_start).or_default().1.push(index);
            if let Some(after_end) = line_end.checked_add(1) {
                events.entry(after_end).or_default().0.push(index);
            }
        }

        let mut active = BTreeSet::<(usize, String, usize)>::new();
        let mut changes = Vec::new();
        for (line, (removals, additions)) in events {
            for index in removals {
                active.remove(&(indexed[index].0, indexed[index].1.clone(), index));
            }
            for index in additions {
                active.insert((indexed[index].0, indexed[index].1.clone(), index));
            }
            let selected = active
                .iter()
                .next()
                .map(|(_, _, index)| indexed[*index].2.clone());
            if changes
                .last()
                .is_none_or(|(_, previous)| previous != &selected)
            {
                changes.push((line, selected));
            }
        }
        Self { changes }
    }

    fn resolve(&self, line: usize) -> Option<NodeId> {
        let insertion = self
            .changes
            .partition_point(|(change_line, _)| *change_line <= line);
        insertion
            .checked_sub(1)
            .and_then(|index| self.changes[index].1.clone())
    }
}

impl EndpointLookupIndex {
    fn build(nodes_by_file: &HashMap<PathBuf, Vec<Node>>) -> Self {
        let mut functions_by_file_and_name = HashMap::new();
        let mut enclosing_by_file = HashMap::new();
        for (file, nodes) in nodes_by_file {
            let mut functions = HashMap::<String, Vec<NodeId>>::new();
            for node in nodes
                .iter()
                .filter(|node| node.id.kind == NodeKind::Function)
            {
                functions
                    .entry(node.id.name.clone())
                    .or_default()
                    .push(node.id.clone());
            }
            for ids in functions.values_mut() {
                ids.sort_by_key(NodeId::to_stable_id);
                ids.dedup();
            }
            functions_by_file_and_name.insert(file.clone(), functions);
            enclosing_by_file.insert(file.clone(), EnclosingLineIndex::build(nodes));
        }
        Self {
            functions_by_file_and_name,
            enclosing_by_file,
        }
    }

    fn unique_function(&self, file: &Path, name: &str) -> Option<NodeId> {
        let ids = self.functions_by_file_and_name.get(file)?.get(name)?;
        (ids.len() == 1).then(|| ids[0].clone())
    }

    fn enclosing_symbol(&self, file: &Path, line: usize) -> Option<NodeId> {
        self.enclosing_by_file.get(file)?.resolve(line)
    }
}

fn resolve_or_materialize_call_hierarchy_endpoint(
    item: &serde_json::Value,
    endpoint: &str,
    root: &Path,
    local_root: &str,
    language: &str,
    endpoint_index: &EndpointLookupIndex,
) -> Option<(NodeId, Option<Node>, Confidence)> {
    let endpoint = item.get(endpoint)?;
    let uri: lsp_types::Uri = endpoint.get("uri")?.as_str()?.parse().ok()?;
    let path = uri_to_relative_path(&uri, root);
    let name = endpoint
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let detail = endpoint
        .get("detail")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let start_line = endpoint["range"]["start"]["line"].as_u64().unwrap_or(0);
    let start_character = endpoint["range"]["start"]["character"]
        .as_u64()
        .unwrap_or(0);
    let end_line = endpoint["range"]["end"]["line"].as_u64().unwrap_or(0);
    let end_character = endpoint["range"]["end"]["character"].as_u64().unwrap_or(0);
    let line_start = start_line as usize + 1;
    let line_end = end_line as usize + 1;
    let range_disambiguator = format!("{start_line}:{start_character}-{end_line}:{end_character}");

    if !path.is_absolute() {
        if let Some(exact) = endpoint_index.unique_function(&path, name) {
            return Some((exact, None, Confidence::Confirmed));
        }
        if let Some(existing) = endpoint_index.enclosing_symbol(&path, line_start) {
            return Some((existing, None, Confidence::Confirmed));
        }

        let base_name = if name.is_empty() { detail } else { name };
        if base_name.is_empty() {
            return None;
        }
        let node_name = format!("{base_name}@lsp:{range_disambiguator}");
        let id = NodeId {
            root: local_root.to_string(),
            file: path,
            name: node_name,
            kind: NodeKind::Function,
        };
        let mut metadata = BTreeMap::new();
        metadata.insert("virtual".to_string(), "true".to_string());
        metadata.insert("lsp_call_hierarchy".to_string(), "true".to_string());
        if !name.is_empty() {
            metadata.insert("lsp_name".to_string(), name.to_string());
        }
        return Some((
            id.clone(),
            Some(Node {
                id,
                language: language.to_string(),
                line_start,
                line_end: line_end.max(line_start),
                signature: detail.to_string(),
                body: String::new(),
                metadata,
                source: ExtractionSource::Lsp,
            }),
            Confidence::Detected,
        ));
    }

    let fqn = if detail.is_empty() { name } else { detail };
    if fqn.is_empty() {
        return None;
    }
    let id = NodeId {
        root: "external".to_string(),
        file: PathBuf::new(),
        name: format!("{fqn}@lsp:{range_disambiguator}"),
        kind: NodeKind::Function,
    };
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "package".to_string(),
        fqn.split("::")
            .next()
            .unwrap_or(fqn)
            .split('.')
            .next()
            .unwrap_or(fqn)
            .to_string(),
    );
    metadata.insert("virtual".to_string(), "true".to_string());
    metadata.insert("external".to_string(), "true".to_string());
    metadata.insert("lsp_call_hierarchy".to_string(), "true".to_string());
    Some((
        id.clone(),
        Some(Node {
            id,
            language: language.to_string(),
            line_start: 0,
            line_end: 0,
            signature: fqn.to_string(),
            body: String::new(),
            metadata,
            source: ExtractionSource::Lsp,
        }),
        Confidence::Detected,
    ))
}

fn should_abort_zero_edge_pass(
    edge_producing_total: usize,
    edge_attempted: u32,
    emitted_edges: bool,
    elapsed: Duration,
) -> bool {
    edge_producing_total > 0
        && !emitted_edges
        && ((edge_attempted >= ZERO_EDGE_ABORT_THRESHOLD && elapsed >= ZERO_EDGE_MIN_WARMUP)
            || elapsed > ZERO_EDGE_TIMEOUT)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DidOpenStatus {
    Unopened,
    Opening,
    Opened,
    Failed,
}

struct DidOpenEntry {
    state: Mutex<DidOpenStatus>,
    notify: tokio::sync::Notify,
}

impl DidOpenEntry {
    fn new() -> Self {
        Self {
            state: Mutex::new(DidOpenStatus::Unopened),
            notify: tokio::sync::Notify::new(),
        }
    }
}

struct DidOpenCoordinator {
    server: String,
    inventory_language: String,
    files: Mutex<HashMap<PathBuf, Arc<DidOpenEntry>>>,
}

impl DidOpenCoordinator {
    fn new(server: String, inventory_language: String) -> Self {
        Self {
            server,
            inventory_language,
            files: Mutex::new(HashMap::new()),
        }
    }

    async fn ensure_open(
        &self,
        transport: &PipelinedTransport,
        root: &Path,
        rel_path: &Path,
        file_uri: &lsp_types::Uri,
    ) -> Result<bool> {
        self.ensure_open_with(rel_path, || async {
            self.send_did_open_once(transport, root, rel_path, file_uri)
                .await
        })
        .await
    }

    async fn ensure_open_with<F, Fut>(&self, rel_path: &Path, open: F) -> Result<bool>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let entry = {
            let mut files = self.files.lock().await;
            files
                .entry(rel_path.to_path_buf())
                .or_insert_with(|| Arc::new(DidOpenEntry::new()))
                .clone()
        };
        let mut open = Some(open);

        loop {
            {
                let mut state = entry.state.lock().await;
                match *state {
                    DidOpenStatus::Opened => return Ok(false),
                    DidOpenStatus::Opening => {}
                    DidOpenStatus::Unopened | DidOpenStatus::Failed => {
                        *state = DidOpenStatus::Opening;
                        drop(state);
                        let open_once = open
                            .take()
                            .expect("didOpen operation consumed exactly once");
                        let result = open_once().await;
                        let mut state = entry.state.lock().await;
                        *state = if result.is_ok() {
                            DidOpenStatus::Opened
                        } else {
                            DidOpenStatus::Failed
                        };
                        entry.notify.notify_waiters();
                        return result.map(|()| true);
                    }
                }
            }
            entry.notify.notified().await;
        }
    }

    async fn send_did_open_once(
        &self,
        transport: &PipelinedTransport,
        root: &Path,
        rel_path: &Path,
        file_uri: &lsp_types::Uri,
    ) -> Result<()> {
        let abs_path = root.join(rel_path);
        let content = read_lsp_text(&abs_path).with_context(|| {
            format!(
                "LSP didOpen failed: server={} file={} phase=read_file",
                self.server,
                rel_path.display()
            )
        })?;
        let lang_id = lsp_language_id(&self.inventory_language, &abs_path);
        let notify = transport.notify(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": file_uri.to_string(),
                    "languageId": lang_id,
                    "version": 1,
                    "text": content
                }
            }),
        );

        match tokio::time::timeout(did_open_timeout(), notify).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e).with_context(|| {
                format!(
                    "LSP didOpen failed: server={} file={} phase=notify_write",
                    self.server,
                    rel_path.display()
                )
            }),
            Err(_) => anyhow::bail!(
                "LSP didOpen timed out: server={} file={} phase=notify_write timeout_ms={}",
                self.server,
                rel_path.display(),
                did_open_timeout().as_millis()
            ),
        }
    }
}

#[derive(Debug, Clone)]
struct LspPass1WorkItem {
    id: usize,
    node: Node,
    requested_operations: Vec<LspQueryOperation>,
    attempt_count: u32,
}

fn pass1_work_item_files(work_items: &[LspPass1WorkItem]) -> Vec<PathBuf> {
    let mut files = work_items
        .iter()
        .map(|item| item.node.id.file.clone())
        .collect::<Vec<_>>();
    files.sort_unstable();
    files.dedup();
    files
}

#[derive(Debug)]
struct Pass1TaskResult {
    edges: Vec<Edge>,
    new_nodes: Vec<Node>,
    had_error: bool,
    edge_producing: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct QueryObservation {
    pub(super) scheduled_requests: usize,
    pub(super) non_empty_responses: usize,
    /// Raw applicable result items returned by the server before graph mapping.
    pub(super) result_count: usize,
    pub(super) errors: usize,
    pub(super) timeouts: usize,
}

impl QueryObservation {
    pub(super) fn record_error(&mut self, error: &anyhow::Error) {
        self.errors += 1;
        let text = error.to_string().to_ascii_lowercase();
        if text.contains("timed out") || text.contains("timeout") {
            self.timeouts += 1;
        }
    }
}

fn record_type_hierarchy_observation(
    telemetry: &LspQueryTelemetry,
    node: &Node,
    observation: QueryObservation,
    emitted_edges: usize,
    latency: Duration,
) {
    if let Some(declaration) = LspDeclarationClass::from_kind(&node.id.kind) {
        telemetry.record(
            LspQueryOperation::TypeHierarchy,
            declaration,
            observation.scheduled_requests,
            observation.non_empty_responses,
            emitted_edges,
            latency,
            observation.errors,
            observation.timeouts,
        );
    }
}

fn retain_unique_document_link_file(
    node: &Node,
    operation: LspQueryOperation,
    seen_files: &mut HashSet<PathBuf>,
) -> bool {
    operation != LspQueryOperation::DocumentLinks || seen_files.insert(node.id.file.clone())
}

fn runnable_pass1_work_items(
    work_items: Vec<LspPass1WorkItem>,
    ledger: &LspWorkItemLedger,
) -> Vec<LspPass1WorkItem> {
    work_items
        .into_iter()
        .filter_map(|mut item| {
            if !ledger.should_run(item.id) {
                return None;
            }
            if let Some(attempt_count) = ledger.attempt_count(item.id) {
                item.attempt_count = attempt_count;
            }
            Some(item)
        })
        .collect()
}

fn spawn_pass1_workers<Executor, Execution>(
    work_items: Vec<LspPass1WorkItem>,
    ledger: &Arc<LspWorkItemLedger>,
    max_concurrency: usize,
    executor: Executor,
) -> (
    usize,
    Arc<LspPass1Diagnostics>,
    tokio::task::JoinSet<()>,
    tokio::sync::mpsc::Receiver<Pass1TaskResult>,
)
where
    Executor: Fn(LspPass1WorkItem, Arc<LspPass1Diagnostics>) -> Execution + Send + Sync + 'static,
    Execution: std::future::Future<Output = Pass1TaskResult> + Send + 'static,
{
    let work_items = Arc::new(runnable_pass1_work_items(work_items, ledger));
    let total_nodes = work_items.len();
    let channel_capacity = max_concurrency.max(1);
    let worker_count = total_nodes.clamp(1, channel_capacity);
    let diagnostics = Arc::new(LspPass1Diagnostics::with_ledger(
        total_nodes,
        Some(Arc::clone(ledger)),
    ));
    let next_work_index = Arc::new(AtomicUsize::new(0));
    let executor = Arc::new(executor);
    let (result_tx, result_rx) = tokio::sync::mpsc::channel::<Pass1TaskResult>(channel_capacity);
    let mut join_set = tokio::task::JoinSet::new();

    for _ in 0..worker_count {
        let diagnostics = Arc::clone(&diagnostics);
        let work_items = Arc::clone(&work_items);
        let next_work_index = Arc::clone(&next_work_index);
        let executor = Arc::clone(&executor);
        let result_tx = result_tx.clone();

        join_set.spawn(async move {
            loop {
                let index = next_work_index.fetch_add(1, Ordering::Relaxed);
                let Some(item) = work_items.get(index).cloned() else {
                    break;
                };
                let result = executor(item, Arc::clone(&diagnostics)).await;
                if result_tx.send(result).await.is_err() {
                    break;
                }
            }
        });
    }
    drop(result_tx);

    (total_nodes, diagnostics, join_set, result_rx)
}

fn recovery_failure_state(ledger: &LspWorkItemLedger) -> (u32, bool, Option<String>) {
    let exhausted = ledger.exhausted_count();
    if exhausted == 0 {
        return (0, false, None);
    }
    (
        exhausted.try_into().unwrap_or(u32::MAX),
        true,
        Some(format!(
            "{exhausted} LSP work item(s) exhausted the retry budget; inspect list_roots for the failed phase, then retry with narrower scope or fix the language server"
        )),
    )
}

fn extend_unique_edges(
    target: &mut Vec<Edge>,
    seen: &mut std::collections::HashSet<String>,
    incoming: impl IntoIterator<Item = Edge>,
) {
    for edge in incoming {
        if seen.insert(edge.stable_id()) {
            target.push(edge);
        }
    }
}

fn extend_unique_nodes(
    target: &mut Vec<Node>,
    seen: &mut std::collections::HashSet<String>,
    incoming: impl IntoIterator<Item = Node>,
) {
    for node in incoming {
        if seen.insert(node.stable_id()) {
            target.push(node);
        }
    }
}

#[derive(Debug, Clone)]
struct InFlightTaskDiagnostic {
    file: PathBuf,
    node: String,
    phase: &'static str,
    attempt_count: u32,
    started_at: Instant,
}

#[derive(Debug, Clone)]
struct LspPass1DiagnosticSnapshot {
    total: usize,
    completed: u64,
    failed: u64,
    pending: u64,
    in_flight: usize,
    phase_counts: BTreeMap<&'static str, usize>,
    oldest: Vec<String>,
    last_success: Option<String>,
}

impl LspPass1DiagnosticSnapshot {
    fn render(&self) -> String {
        let phases = if self.phase_counts.is_empty() {
            "none".to_string()
        } else {
            self.phase_counts
                .iter()
                .map(|(phase, count)| format!("{phase}={count}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let oldest = if self.oldest.is_empty() {
            "none".to_string()
        } else {
            self.oldest.join("; ")
        };
        let last_success = self.last_success.as_deref().unwrap_or("none");
        format!(
            "pass=lsp_pass1_references completed={}/{} pending={} in_flight={} failed={} phases=[{}] oldest=[{}] last_success={}",
            self.completed,
            self.total,
            self.pending,
            self.in_flight,
            self.failed,
            phases,
            oldest,
            last_success
        )
    }
}

struct LspPass1Diagnostics {
    total: usize,
    in_flight: Mutex<HashMap<usize, InFlightTaskDiagnostic>>,
    completed: AtomicI64,
    failed: AtomicI64,
    last_success: Mutex<Option<String>>,
    work_items: Option<Arc<LspWorkItemLedger>>,
}

impl LspPass1Diagnostics {
    #[cfg(test)]
    fn new(total: usize) -> Self {
        Self::with_ledger(total, None)
    }

    fn with_ledger(total: usize, work_items: Option<Arc<LspWorkItemLedger>>) -> Self {
        Self {
            total,
            in_flight: Mutex::new(HashMap::new()),
            completed: AtomicI64::new(0),
            failed: AtomicI64::new(0),
            last_success: Mutex::new(None),
            work_items,
        }
    }

    async fn set_phase(&self, item: &LspPass1WorkItem, phase: &'static str) {
        let mut in_flight = self.in_flight.lock().await;
        in_flight.insert(
            item.id,
            InFlightTaskDiagnostic {
                file: item.node.id.file.clone(),
                node: item.node.id.name.clone(),
                phase,
                attempt_count: item.attempt_count,
                started_at: Instant::now(),
            },
        );
        drop(in_flight);
        if let Some(work_items) = &self.work_items
            && let Err(error) = work_items.mark_phase(item.id, phase).await
        {
            tracing::warn!(
                item_id = item.id,
                %error,
                "Failed to persist LSP work-item phase"
            );
        }
    }

    async fn finish(
        &self,
        item: &LspPass1WorkItem,
        success: bool,
        edges: &[Edge],
        nodes: &[Node],
        observed_result_count: usize,
    ) {
        self.finish_with_error(
            item,
            success,
            (!success).then(|| "one or more LSP operations failed".to_string()),
            edges,
            nodes,
            observed_result_count,
        )
        .await;
    }

    async fn finish_failed(&self, item: &LspPass1WorkItem, error: impl Into<String>) {
        self.finish_with_error(item, false, Some(error.into()), &[], &[], 0)
            .await;
    }

    async fn finish_with_error(
        &self,
        item: &LspPass1WorkItem,
        success: bool,
        error: Option<String>,
        edges: &[Edge],
        nodes: &[Node],
        observed_result_count: usize,
    ) {
        {
            let mut in_flight = self.in_flight.lock().await;
            in_flight.remove(&item.id);
        }
        self.completed.fetch_add(1, Ordering::Relaxed);
        if success {
            let mut last_success = self.last_success.lock().await;
            *last_success = Some(format!(
                "{}:{}",
                item.node.id.file.display(),
                item.node.id.name
            ));
        } else {
            self.failed.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(work_items) = &self.work_items {
            let result = if success {
                work_items
                    .mark_completed_with_output(
                        item.id,
                        edges,
                        nodes,
                        observed_result_count.try_into().unwrap_or(u64::MAX),
                    )
                    .await
            } else {
                work_items
                    .mark_failed(
                        item.id,
                        error.unwrap_or_else(|| "LSP work item failed".to_string()),
                    )
                    .await
            };
            if let Err(error) = result {
                tracing::warn!(
                    item_id = item.id,
                    %error,
                    "Failed to persist terminal LSP work-item state"
                );
            }
        }
    }

    async fn snapshot(&self) -> LspPass1DiagnosticSnapshot {
        let completed = self.completed.load(Ordering::Relaxed).max(0) as u64;
        let failed = self.failed.load(Ordering::Relaxed).max(0) as u64;
        let in_flight = self.in_flight.lock().await;
        let in_flight_count = in_flight.len();
        let mut phase_counts = BTreeMap::new();
        for task in in_flight.values() {
            *phase_counts.entry(task.phase).or_insert(0) += 1;
        }
        let mut oldest_tasks = in_flight.values().cloned().collect::<Vec<_>>();
        oldest_tasks.sort_by_key(|task| std::cmp::Reverse(task.started_at.elapsed()));
        let oldest = oldest_tasks
            .into_iter()
            .take(PASS1_DIAGNOSTIC_SAMPLE_LIMIT)
            .map(|task| {
                format!(
                    "file={} node={} phase={} attempt={} age_ms={}",
                    task.file.display(),
                    task.node,
                    task.phase,
                    task.attempt_count,
                    task.started_at.elapsed().as_millis()
                )
            })
            .collect();
        drop(in_flight);
        let last_success = self.last_success.lock().await.clone();
        let accounted = completed.saturating_add(in_flight_count as u64);
        let pending = (self.total as u64).saturating_sub(accounted);
        LspPass1DiagnosticSnapshot {
            total: self.total,
            completed,
            failed,
            pending,
            in_flight: in_flight_count,
            phase_counts,
            oldest,
            last_success,
        }
    }
}

fn did_open_timeout() -> Duration {
    duration_from_env("RNA_LSP_DID_OPEN_TIMEOUT_MS", DID_OPEN_DEFAULT_TIMEOUT)
}

fn pass1_no_progress_timeout() -> Duration {
    duration_from_env(
        "RNA_LSP_PASS1_NO_PROGRESS_TIMEOUT_MS",
        PASS1_DEFAULT_NO_PROGRESS_TIMEOUT,
    )
}

fn duration_from_env(name: &str, default: Duration) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or(default)
}

impl LspEnricher {
    // ------------------------------------------------------------------
    // Pass 0: crate-level dependency graph via rust-analyzer/viewCrateGraph.
    //
    // Single request; returns the entire workspace crate graph as a DOT
    // string. Runs unconditionally (no per-node cost, no quiescence
    // requirement). Only emits nodes+edges for Rust roots.
    // ------------------------------------------------------------------
    pub(super) async fn run_pass0_crate_graph(
        &self,
        transport: &PipelinedTransport,
        matching_nodes: &[&Node],
        result: &mut EnrichmentResult,
    ) {
        if self.language != "rust" {
            return;
        }

        let pass0_start = std::time::Instant::now();
        let root_id = matching_nodes
            .first()
            .map(|n| n.id.root.clone())
            .unwrap_or_default();

        match Self::fetch_crate_graph(transport).await {
            Ok((crate_names, pairs)) if !crate_names.is_empty() => {
                let pair_count = pairs.len();
                Self::emit_crate_graph_edges(&crate_names, &pairs, &root_id, result);
                tracing::info!(
                    "LSP Pass 0 complete in {:?}: {} crate nodes, {} DependsOn edges",
                    pass0_start.elapsed(),
                    crate_names.len(),
                    pair_count
                );
            }
            Ok(_) => {
                tracing::debug!("LSP Pass 0: viewCrateGraph returned no crates");
            }
            Err(e) => {
                tracing::debug!("LSP Pass 0: viewCrateGraph failed: {}", e);
            }
        }
    }

    // ------------------------------------------------------------------
    // Pass 1: call hierarchy, find_implementations, references, and document links.
    // Pipelined with adaptive concurrency (TCP slow-start).
    //
    // Returns (attempted, errors, aborted, abort diagnostic).
    // ------------------------------------------------------------------
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_pass1_references(
        &self,
        transport: &Arc<PipelinedTransport>,
        root: &Path,
        matching_nodes: &[&Node],
        matching_nodes_owned: &Arc<Vec<Node>>,
        refs_by_file_shared: &Arc<HashMap<PathBuf, Vec<Node>>>,
        capabilities: LspServerCapabilities,
        budget: &mut LspQueryBudget,
        telemetry: &Arc<LspQueryTelemetry>,
        result: &mut EnrichmentResult,
        job_deadline: tokio::time::Instant,
    ) -> (u32, u32, bool, Option<String>) {
        let pass1_start = std::time::Instant::now();
        let language = self.language.clone();

        // Filter to only nodes that need LSP requests:
        // Functions (call hierarchy), Traits (implementations), and Other (document links).
        // Skip test functions -- they don't have meaningful cross-file callers
        // and halve the total RPC count.
        // Also skip diagnostic nodes (Other("diagnostic")) to prevent them from being
        // re-enriched via the generic Other/documentLink path on subsequent passes --
        // which would generate spurious DependsOn edges from diagnostics.
        let candidate_nodes: Vec<&Node> = matching_nodes
            .iter()
            .filter(|n| {
                matches!(
                    n.id.kind,
                    NodeKind::Function
                        | NodeKind::Trait
                        | NodeKind::Other(_)
                        | NodeKind::MarkdownSection
                        | NodeKind::Struct
                        | NodeKind::Enum
                        | NodeKind::TypeAlias
                        | NodeKind::Const
                )
            })
            .filter(|n| !matches!(&n.id.kind, NodeKind::Other(s) if s == "diagnostic"))
            .filter(|n| {
                // Skip test functions (have #[test] or #[tokio::test] decorator)
                if n.id.kind == NodeKind::Function {
                    if let Some(decorators) = n.metadata.get("decorators")
                        && (decorators.contains("#[test]") || decorators.contains("#[tokio::test]"))
                    {
                        return false;
                    }
                    // Also skip functions in test files
                    if crate::ranking::is_test_file(n) {
                        return false;
                    }
                }
                true
            })
            .copied()
            .collect();

        let mut admitted_nodes: Vec<(&Node, LspQueryOperation)> = Vec::new();
        if capabilities.document_symbols {
            let mut document_representatives = BTreeMap::<PathBuf, Vec<&Node>>::new();
            for node in matching_nodes {
                document_representatives
                    .entry(node.id.file.clone())
                    .or_default()
                    .push(*node);
            }
            for mut candidates in document_representatives.into_values() {
                candidates.sort_by_key(|node| node.stable_id());
                for node in candidates {
                    if self.query_profile.admits(
                        node,
                        LspQueryOperation::DocumentSymbols,
                        capabilities,
                        budget,
                    ) {
                        admitted_nodes.push((node, LspQueryOperation::DocumentSymbols));
                        break;
                    }
                }
            }
        }

        let mut document_link_files = HashSet::new();
        for node in candidate_nodes {
            let operations: &[LspQueryOperation] = match node.id.kind {
                NodeKind::Function if capabilities.call_hierarchy => {
                    &[LspQueryOperation::CallHierarchy]
                }
                NodeKind::Function => &[LspQueryOperation::References],
                NodeKind::Trait => &[LspQueryOperation::Implementations],
                NodeKind::Struct | NodeKind::Enum | NodeKind::TypeAlias | NodeKind::Const => {
                    &[LspQueryOperation::References]
                }
                NodeKind::MarkdownSection
                    if node.metadata.get("markdown_kind").map(String::as_str) == Some("link") =>
                {
                    &[
                        LspQueryOperation::DocumentLinks,
                        LspQueryOperation::Definitions,
                        LspQueryOperation::References,
                    ]
                }
                NodeKind::MarkdownSection => &[LspQueryOperation::DocumentLinks],
                NodeKind::Other(_) => &[LspQueryOperation::DocumentLinks],
                _ => &[],
            };
            for &operation in operations {
                if retain_unique_document_link_file(node, operation, &mut document_link_files)
                    && self
                        .query_profile
                        .admits(node, operation, capabilities, budget)
                {
                    admitted_nodes.push((node, operation));
                }
            }
        }

        let ref_eligible = admitted_nodes
            .iter()
            .filter(|(n, _)| {
                matches!(
                    n.id.kind,
                    NodeKind::Struct | NodeKind::Enum | NodeKind::TypeAlias | NodeKind::Const
                )
            })
            .count();
        tracing::info!(
            "LSP pipeline: {} enrichable nodes out of {} total ({}f, {}t, {}r, {}o) [references={}, call_hierarchy={}]",
            admitted_nodes.len(),
            matching_nodes.len(),
            admitted_nodes
                .iter()
                .filter(|(n, _)| n.id.kind == NodeKind::Function)
                .count(),
            admitted_nodes
                .iter()
                .filter(|(n, _)| n.id.kind == NodeKind::Trait)
                .count(),
            ref_eligible,
            admitted_nodes
                .iter()
                .filter(|(n, _)| matches!(n.id.kind, NodeKind::Other(_)))
                .count(),
            capabilities.references,
            capabilities.call_hierarchy,
        );

        // Bounded queue substrate: materialize inspectable work items, then let a
        // fixed worker pool drain them. This avoids spawning one opaque task per
        // symbol and gives diagnostics a stable queue/in-flight model.
        const PIPELINE_MAX_CONCURRENCY: usize = 64;
        let work_items: Vec<LspPass1WorkItem> = admitted_nodes
            .iter()
            .enumerate()
            .map(|(id, (node, operation))| LspPass1WorkItem {
                id,
                node: (*node).clone(),
                requested_operations: vec![*operation],
                attempt_count: 1,
            })
            .collect();
        let persisted_seeds = work_items
            .iter()
            .map(|item| LspWorkItemSeed {
                item_id: item.id,
                node: item.node.clone(),
                requested_operations: item
                    .requested_operations
                    .iter()
                    .map(|operation| operation.to_string())
                    .collect(),
                attempt_count: item.attempt_count,
            })
            .collect::<Vec<_>>();
        let work_item_ledger = match LspWorkItemLedger::begin(root, &persisted_seeds).await {
            Ok(ledger) => ledger,
            Err(error) => {
                let diagnostic = format!(
                    "failed to persist LSP Pass 1 work-item ledger: {error}; no work was started"
                );
                tracing::warn!("{diagnostic}");
                return (0, 1, true, Some(diagnostic));
            }
        };
        let (recovered_edges, recovered_nodes) = work_item_ledger.recovered_output();
        let mut seen_edge_ids = result
            .added_edges
            .iter()
            .map(Edge::stable_id)
            .collect::<std::collections::HashSet<_>>();
        extend_unique_edges(&mut result.added_edges, &mut seen_edge_ids, recovered_edges);
        let mut seen_virtual_ids = result
            .new_nodes
            .iter()
            .map(Node::stable_id)
            .collect::<std::collections::HashSet<_>>();
        extend_unique_nodes(
            &mut result.new_nodes,
            &mut seen_virtual_ids,
            recovered_nodes,
        );
        let pass1_edge_baseline = result.added_edges.len();
        let (recovery_errors, recovery_aborted, recovery_diagnostic) =
            recovery_failure_state(&work_item_ledger);
        let work_items = runnable_pass1_work_items(work_items, &work_item_ledger);
        let edge_producing_total = work_items
            .iter()
            .filter(|item| {
                item.requested_operations
                    .first()
                    .is_some_and(|operation| *operation != LspQueryOperation::DocumentSymbols)
            })
            .count();
        let did_open = Arc::new(DidOpenCoordinator::new(
            self.server_command.clone(),
            self.language.clone(),
        ));
        // Send every document mutation before issuing any concurrent request.
        // Some standards-compliant servers cancel outstanding requests when a
        // later didOpen changes workspace state. Lazy per-worker opens therefore
        // make unrelated file requests race each other. Pre-opening the bounded,
        // deterministic file set preserves pipelined query concurrency without
        // interleaving mutations and requests.
        for rel_path in pass1_work_item_files(&work_items) {
            if tokio::time::Instant::now() >= job_deadline {
                break;
            }
            let file_uri = match path_to_uri(&root.join(&rel_path)) {
                Ok(uri) => uri,
                Err(error) => {
                    tracing::warn!(
                        "LSP Pass 1 pre-open skipped {} after URI failure: {}",
                        rel_path.display(),
                        error
                    );
                    continue;
                }
            };
            if let Err(error) = did_open
                .ensure_open(transport, root, &rel_path, &file_uri)
                .await
            {
                // The owning work item retries through the same coordinator so
                // the durable ledger, error accounting, and fail-closed behavior
                // remain unchanged.
                tracing::warn!("LSP Pass 1 pre-open failed: {error}");
            }
        }
        let error_count = Arc::new(AtomicI64::new(0));
        let transport = Arc::clone(transport);
        let root = root.to_path_buf();
        let matching_owned = Arc::clone(matching_nodes_owned);
        let refs_by_file = Arc::clone(refs_by_file_shared);
        let endpoint_index = Arc::new(EndpointLookupIndex::build(refs_by_file_shared));
        let worker_telemetry = Arc::clone(telemetry);
        let (total_nodes, diagnostics, mut join_set, mut result_rx) = spawn_pass1_workers(
            work_items,
            &work_item_ledger,
            PIPELINE_MAX_CONCURRENCY,
            move |item, diagnostics| {
                let transport = Arc::clone(&transport);
                let root = root.clone();
                let matching_owned = Arc::clone(&matching_owned);
                let refs_by_file = Arc::clone(&refs_by_file);
                let endpoint_index = Arc::clone(&endpoint_index);
                let language = language.clone();
                let error_count = Arc::clone(&error_count);
                let did_open = Arc::clone(&did_open);
                let telemetry = Arc::clone(&worker_telemetry);
                async move {
                    let operation = item.requested_operations.first().copied();
                    let declaration =
                        LspDeclarationClass::from_kind(&item.node.id.kind).or_else(|| {
                            (operation == Some(LspQueryOperation::DocumentSymbols))
                                .then_some(LspDeclarationClass::Other)
                        });
                    let registered =
                        if let (Some(operation), Some(declaration)) = (operation, declaration) {
                            telemetry.register_work_item(item.id, operation, declaration)
                        } else {
                            false
                        };
                    if !registered {
                        return Pass1TaskResult {
                            edges: Vec::new(),
                            new_nodes: Vec::new(),
                            had_error: false,
                            edge_producing: operation.is_some_and(|operation| {
                                operation != LspQueryOperation::DocumentSymbols
                            }),
                        };
                    }
                    Self::run_pass1_work_item(
                        &item,
                        &transport,
                        &root,
                        &matching_owned,
                        &refs_by_file,
                        &endpoint_index,
                        &language,
                        &did_open,
                        &diagnostics,
                        &error_count,
                        &telemetry,
                    )
                    .await
                }
            },
        );

        // Collect results from all queued work items. A no-progress watchdog emits
        // a bounded diagnostic snapshot before aborting workers, so stalls surface
        // by phase/file instead of waiting for the outer 30-minute job watchdog.
        let mut attempted = 0u32;
        let mut edge_attempted = 0u32;
        let mut errors = 0u32;
        let mut aborted = false;
        let mut abort_diagnostic = None;
        let mut last_progress_log = std::time::Instant::now();
        let mut last_logged_count = 0u64;
        let mut no_progress_deadline = Box::pin(tokio::time::sleep(pass1_no_progress_timeout()));
        let mut job_deadline = Box::pin(tokio::time::sleep_until(job_deadline));
        let broad_reference_budget = self.broad_reference_budget().cloned();
        let broad_reference_deadline = async {
            match broad_reference_budget.as_ref() {
                Some(budget) => {
                    if let Some(remaining) = budget.remaining_duration() {
                        tokio::time::sleep(remaining).await;
                    }
                    budget.open_time_circuit();
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(broad_reference_deadline);
        const PROGRESS_LOG_INTERVAL_SECS: u64 = 30;
        const PROGRESS_LOG_INTERVAL_NODES: u64 = 1_000;

        while attempted < total_nodes as u32 {
            tokio::select! {
                maybe_result = result_rx.recv() => {
                    let Some(task_result) = maybe_result else {
                        break;
                    };
                    attempted += 1;
                    edge_attempted += u32::from(task_result.edge_producing);
                    if task_result.had_error {
                        errors += 1;
                    }
                    extend_unique_edges(
                        &mut result.added_edges,
                        &mut seen_edge_ids,
                        task_result.edges,
                    );
                    extend_unique_nodes(
                        &mut result.new_nodes,
                        &mut seen_virtual_ids,
                        task_result.new_nodes,
                    );

                    no_progress_deadline.as_mut().reset(
                        tokio::time::Instant::now() + pass1_no_progress_timeout(),
                    );
                }
                _ = &mut no_progress_deadline => {
                    let snapshot = diagnostics.snapshot().await;
                    let rendered_snapshot = snapshot.render();
                    tracing::warn!(
                        "LSP: {} no progress for {}s; aborting Pass 1. Diagnostic snapshot: {}. Recommended next action: retry with a narrower scope or inspect the listed phase/file for language-server stalls.",
                        self.server_command,
                        pass1_no_progress_timeout().as_secs(),
                        rendered_snapshot,
                    );
                    errors += 1;
                    aborted = true;
                    abort_diagnostic = Some(format!(
                        "no progress for {}s; {}; recommended next action: retry with a narrower scope or inspect the listed phase/file for language-server stalls",
                        pass1_no_progress_timeout().as_secs(),
                        rendered_snapshot
                    ));
                    join_set.abort_all();
                    break;
                }
                _ = &mut job_deadline => {
                    let detail = format!(
                        "LSP enrichment timed out for {} after {}s; safely produced partial output was preserved",
                        self.server_command,
                        lsp_job_timeout().as_secs()
                    );
                    tracing::warn!("{detail}");
                    errors += 1;
                    aborted = true;
                    abort_diagnostic = Some(detail);
                    join_set.abort_all();
                    break;
                }
                _ = &mut broad_reference_deadline => {
                    let reason = broad_reference_budget
                        .as_ref()
                        .and_then(|budget| budget.snapshot().circuit_reason)
                        .unwrap_or_else(|| "broad-reference time budget exhausted".to_string());
                    tracing::warn!("LSP: {reason}; opening scoped circuit breaker");
                    errors += 1;
                    aborted = true;
                    abort_diagnostic = Some(reason);
                    join_set.abort_all();
                    break;
                }
            }

            // Log progress every 1,000 nodes or every 30 seconds (whichever comes first)
            let done = diagnostics.completed.load(Ordering::Relaxed).max(0) as u64;
            let elapsed_since_log = last_progress_log.elapsed().as_secs();
            let nodes_since_log = done.saturating_sub(last_logged_count);
            if done > 0
                && (nodes_since_log >= PROGRESS_LOG_INTERVAL_NODES
                    || elapsed_since_log >= PROGRESS_LOG_INTERVAL_SECS)
            {
                let elapsed_total = pass1_start.elapsed().as_secs_f64();
                let rate = done as f64 / elapsed_total;
                let remaining = if rate > 0.0 {
                    let remaining_nodes = (total_nodes as f64) - (done as f64);
                    let remaining_secs = remaining_nodes / rate;
                    if remaining_secs >= 120.0 {
                        format!("~{} min remaining", (remaining_secs / 60.0).round() as u64)
                    } else {
                        format!("~{}s remaining", remaining_secs.round() as u64)
                    }
                } else {
                    "estimating...".to_string()
                };
                tracing::info!(
                    "LSP: {} processing... {}/{} nodes ({} edges found, {})",
                    self.server_command,
                    done,
                    total_nodes,
                    result.added_edges.len(),
                    remaining,
                );
                last_progress_log = std::time::Instant::now();
                last_logged_count = done;
            }

            // Early abort: if we've processed >= 1,000 nodes AND warmed up for >= 30s,
            // OR spent >= 2 minutes with 0 edges, the language server is likely
            // misconfigured.
            if should_abort_zero_edge_pass(
                edge_producing_total,
                edge_attempted,
                result.added_edges.len() != pass1_edge_baseline,
                pass1_start.elapsed(),
            ) {
                let snapshot = diagnostics.snapshot().await;
                let rendered_snapshot = snapshot.render();
                tracing::warn!(
                    "LSP: {} produced 0 edges after {}/{} edge-producing requests ({:.1}s) -- aborting. Diagnostic snapshot: {}",
                    self.server_command,
                    edge_attempted,
                    edge_producing_total,
                    pass1_start.elapsed().as_secs_f64(),
                    rendered_snapshot,
                );
                aborted = true;
                abort_diagnostic = Some(format!(
                    "zero LSP edges after {edge_attempted}/{edge_producing_total} edge-producing requests; {rendered_snapshot}"
                ));
                join_set.abort_all();
                break;
            }
        }

        while let Some(worker_result) = join_set.join_next().await {
            if let Err(e) = worker_result
                && !aborted
            {
                errors += 1;
                tracing::debug!("LSP enrichment worker panicked: {}", e);
            }
        }

        if let Err(error) = work_item_ledger.flush().await {
            errors += 1;
            aborted = true;
            abort_diagnostic = Some(format!(
                "failed to flush LSP Pass 1 work-item ledger: {error}"
            ));
        }

        errors = errors.saturating_add(recovery_errors);
        if recovery_aborted {
            aborted = true;
            if abort_diagnostic.is_none() {
                abort_diagnostic = recovery_diagnostic;
            }
        }
        if aborted {
            telemetry.record_job_timeout();
        }

        tracing::info!(
            "LSP Pass 1 complete in {:?}: {} edges from {} nodes ({} errors)",
            pass1_start.elapsed(),
            result.added_edges.len(),
            attempted,
            errors,
        );

        (attempted, errors, aborted, abort_diagnostic)
    }

    // ------------------------------------------------------------------
    // Pass 1 helpers: per-node-kind enrichment functions
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    async fn run_pass1_work_item(
        item: &LspPass1WorkItem,
        transport: &PipelinedTransport,
        root: &Path,
        matching_owned: &Arc<Vec<Node>>,
        refs_by_file: &Arc<HashMap<PathBuf, Vec<Node>>>,
        endpoint_index: &EndpointLookupIndex,
        language: &str,
        did_open: &DidOpenCoordinator,
        diagnostics: &LspPass1Diagnostics,
        error_count: &AtomicI64,
        telemetry: &LspQueryTelemetry,
    ) -> Pass1TaskResult {
        let started_at = Instant::now();
        let operation = item.requested_operations.first().copied();
        let edge_producing =
            operation.is_some_and(|operation| operation != LspQueryOperation::DocumentSymbols);
        diagnostics.set_phase(item, "resolving_file_uri").await;
        let node = &item.node;
        let abs_path = root.join(&node.id.file);
        let file_uri = match path_to_uri(&abs_path) {
            Ok(uri) => uri,
            Err(e) => {
                tracing::debug!(
                    "LSP Pass 1 skipped {} after URI failure: {}",
                    node.id.file.display(),
                    e
                );
                diagnostics
                    .finish_failed(item, format!("failed to resolve file URI: {e}"))
                    .await;
                telemetry.record_work_item(item.id, 0, 0, started_at.elapsed(), 1, 0);
                return Pass1TaskResult {
                    edges: Vec::new(),
                    new_nodes: Vec::new(),
                    had_error: true,
                    edge_producing,
                };
            }
        };

        diagnostics.set_phase(item, "sending_did_open").await;
        if let Err(e) = did_open
            .ensure_open(transport, root, &node.id.file, &file_uri)
            .await
        {
            error_count.fetch_add(1, Ordering::Relaxed);
            tracing::warn!("{}", e);
            diagnostics
                .finish_failed(item, format!("failed to send textDocument/didOpen: {e}"))
                .await;
            let mut observation = QueryObservation::default();
            observation.record_error(&e);
            telemetry.record_work_item(
                item.id,
                0,
                0,
                started_at.elapsed(),
                observation.errors,
                observation.timeouts,
            );
            return Pass1TaskResult {
                edges: Vec::new(),
                new_nodes: Vec::new(),
                had_error: true,
                edge_producing,
            };
        }

        let phase = item
            .requested_operations
            .first()
            .map(|operation| operation.phase())
            .unwrap_or("requesting_lsp_operation");
        diagnostics.set_phase(item, phase).await;

        let (line, col) = Self::node_lsp_position(node);
        let mut edges = Vec::new();
        let mut new_nodes = Vec::new();
        let mut had_error = false;
        let observation = if operation == Some(LspQueryOperation::DocumentSymbols) {
            Self::enrich_document_symbols(
                transport,
                &file_uri,
                language,
                refs_by_file,
                root,
                item.id,
                telemetry,
                &mut new_nodes,
                &mut had_error,
                error_count,
            )
            .await
        } else {
            match node.id.kind {
                NodeKind::Function => {
                    Self::enrich_function_node(
                        transport,
                        &file_uri,
                        line,
                        col,
                        node,
                        endpoint_index,
                        root,
                        language,
                        operation == Some(LspQueryOperation::References),
                        operation == Some(LspQueryOperation::CallHierarchy),
                        item.id,
                        telemetry,
                        &mut edges,
                        &mut new_nodes,
                        &mut had_error,
                        error_count,
                    )
                    .await
                }
                NodeKind::Trait => {
                    Self::enrich_trait_node(
                        transport,
                        &file_uri,
                        line,
                        col,
                        node,
                        matching_owned,
                        root,
                        item.id,
                        telemetry,
                        &mut edges,
                    )
                    .await
                }
                NodeKind::Struct | NodeKind::Enum | NodeKind::TypeAlias | NodeKind::Const => {
                    if operation == Some(LspQueryOperation::References) {
                        Self::enrich_type_references(
                            transport,
                            &file_uri,
                            line,
                            col,
                            node,
                            endpoint_index,
                            root,
                            item.id,
                            telemetry,
                            &mut edges,
                            &mut had_error,
                            error_count,
                        )
                        .await
                    } else {
                        QueryObservation::default()
                    }
                }
                NodeKind::MarkdownSection => match operation {
                    Some(LspQueryOperation::DocumentLinks) => {
                        Self::enrich_document_links(
                            transport, &file_uri, node, root, item.id, telemetry, &mut edges,
                        )
                        .await
                    }
                    Some(
                        operation
                        @ (LspQueryOperation::Definitions | LspQueryOperation::References),
                    ) => {
                        Self::enrich_document_locations(
                            transport,
                            &file_uri,
                            line,
                            col,
                            node,
                            endpoint_index,
                            root,
                            operation,
                            item.id,
                            telemetry,
                            &mut edges,
                            &mut had_error,
                            error_count,
                        )
                        .await
                    }
                    _ => QueryObservation::default(),
                },
                _ => {
                    if matches!(node.id.kind, NodeKind::Other(_))
                        && operation == Some(LspQueryOperation::DocumentLinks)
                    {
                        Self::enrich_document_links(
                            transport, &file_uri, node, root, item.id, telemetry, &mut edges,
                        )
                        .await
                    } else {
                        QueryObservation::default()
                    }
                }
            }
        };
        had_error |= observation.errors > 0;

        telemetry.record_work_item(
            item.id,
            observation.non_empty_responses,
            edges.len(),
            started_at.elapsed(),
            observation.errors,
            observation.timeouts,
        );

        diagnostics
            .finish(
                item,
                !had_error,
                &edges,
                &new_nodes,
                observation.result_count,
            )
            .await;
        Pass1TaskResult {
            edges,
            new_nodes,
            had_error,
            edge_producing,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn enrich_function_node(
        transport: &PipelinedTransport,
        file_uri: &lsp_types::Uri,
        line: u32,
        col: u32,
        node: &Node,
        endpoint_index: &EndpointLookupIndex,
        root: &Path,
        language: &str,
        has_references: bool,
        has_call_hierarchy: bool,
        work_item_id: usize,
        telemetry: &LspQueryTelemetry,
        edges: &mut Vec<Edge>,
        new_nodes: &mut Vec<Node>,
        had_error: &mut bool,
        error_count: &AtomicI64,
    ) -> QueryObservation {
        let mut observation = QueryObservation::default();
        if !has_call_hierarchy && has_references {
            observation.scheduled_requests += 1;
            telemetry.note_requests_started(work_item_id, 1);
            match Self::find_references_p(transport, file_uri, line, col).await {
                Ok(locations) => {
                    observation.non_empty_responses += usize::from(!locations.is_empty());
                    for loc in &locations {
                        let ref_path = uri_to_relative_path(&loc.uri, root);
                        let ref_line = loc.range.start.line as usize + 1;

                        if ref_path.to_string_lossy().contains(".cargo")
                            || ref_path.to_string_lossy().contains("site-packages")
                        {
                            continue;
                        }
                        if ref_path == node.id.file
                            && ref_line >= node.line_start
                            && ref_line <= node.line_end
                        {
                            continue;
                        }
                        observation.result_count += 1;

                        let referrer_id =
                            endpoint_index.enclosing_symbol(ref_path.as_path(), ref_line);

                        if let Some(referrer) = referrer_id {
                            if referrer == node.id {
                                continue;
                            }
                            edges.push(Edge {
                                from: referrer,
                                to: node.id.clone(),
                                kind: EdgeKind::Calls,
                                source: ExtractionSource::Lsp,
                                confidence: Confidence::Detected,
                                evidence: Vec::new(),
                            });
                        }
                    }
                }
                Err(e) => {
                    *had_error = true;
                    error_count.fetch_add(1, Ordering::Relaxed);
                    observation.record_error(&e);
                    tracing::debug!("references lookup failed for {}: {}", node.id.name, e);
                }
            }
        } else if has_call_hierarchy {
            observation.scheduled_requests += 1;
            telemetry.note_requests_started(work_item_id, 1);
            match Self::prepare_call_hierarchy_p(transport, file_uri, line, col).await {
                Ok(Some(item)) => {
                    observation.non_empty_responses += 1;
                    observation.scheduled_requests += 2;
                    telemetry.note_requests_started(work_item_id, 2);
                    let (incoming_result, outgoing_result) = tokio::join!(
                        Self::incoming_calls_p(transport, &item),
                        Self::outgoing_calls_p(transport, &item),
                    );

                    // Process incoming calls
                    match incoming_result {
                        Ok(calls) => {
                            observation.non_empty_responses += usize::from(!calls.is_empty());
                            for call in &calls {
                                let Some((caller, materialized, confidence)) =
                                    resolve_or_materialize_call_hierarchy_endpoint(
                                        call,
                                        "from",
                                        root,
                                        &node.id.root,
                                        language,
                                        endpoint_index,
                                    )
                                else {
                                    continue;
                                };
                                if caller == node.id {
                                    continue;
                                }
                                observation.result_count += 1;
                                if let Some(materialized) = materialized {
                                    new_nodes.push(materialized);
                                }
                                edges.push(Edge {
                                    from: caller,
                                    to: node.id.clone(),
                                    kind: EdgeKind::Calls,
                                    source: ExtractionSource::Lsp,
                                    confidence,
                                    evidence: Vec::new(),
                                });
                            }
                        }
                        Err(e) => {
                            *had_error = true;
                            error_count.fetch_add(1, Ordering::Relaxed);
                            observation.record_error(&e);
                            tracing::debug!("incomingCalls failed for {}: {}", node.id.name, e);
                        }
                    }

                    // Process outgoing calls
                    match outgoing_result {
                        Ok(calls) => {
                            observation.non_empty_responses += usize::from(!calls.is_empty());
                            for call in &calls {
                                let Some((callee, materialized, confidence)) =
                                    resolve_or_materialize_call_hierarchy_endpoint(
                                        call,
                                        "to",
                                        root,
                                        &node.id.root,
                                        language,
                                        endpoint_index,
                                    )
                                else {
                                    continue;
                                };
                                if callee == node.id {
                                    continue;
                                }
                                observation.result_count += 1;
                                if let Some(materialized) = materialized {
                                    new_nodes.push(materialized);
                                }
                                edges.push(Edge {
                                    from: node.id.clone(),
                                    to: callee,
                                    kind: EdgeKind::Calls,
                                    source: ExtractionSource::Lsp,
                                    confidence,
                                    evidence: Vec::new(),
                                });
                            }
                        }
                        Err(e) => {
                            *had_error = true;
                            error_count.fetch_add(1, Ordering::Relaxed);
                            observation.record_error(&e);
                            tracing::debug!("outgoingCalls failed for {}: {}", node.id.name, e);
                        }
                    }
                }
                Ok(None) => {} // No call hierarchy item
                Err(e) => {
                    *had_error = true;
                    error_count.fetch_add(1, Ordering::Relaxed);
                    observation.record_error(&e);
                    tracing::debug!("prepareCallHierarchy failed for {}: {}", node.id.name, e);
                }
            }
        }
        observation
    }

    #[allow(clippy::too_many_arguments)]
    async fn enrich_trait_node(
        transport: &PipelinedTransport,
        file_uri: &lsp_types::Uri,
        line: u32,
        col: u32,
        node: &Node,
        matching_owned: &Arc<Vec<Node>>,
        root: &Path,
        work_item_id: usize,
        telemetry: &LspQueryTelemetry,
        edges: &mut Vec<Edge>,
    ) -> QueryObservation {
        let mut observation = QueryObservation {
            scheduled_requests: 1,
            ..Default::default()
        };
        telemetry.note_requests_started(work_item_id, 1);
        match Self::find_implementations_p(transport, file_uri, line, col).await {
            Ok(locations) => {
                observation.non_empty_responses += usize::from(!locations.is_empty());
                let matching_refs: Vec<&Node> = matching_owned.iter().collect();
                for loc in locations {
                    let impl_path = uri_to_relative_path(&loc.uri, root);
                    let impl_line = loc.range.start.line as usize + 1;

                    if impl_path.to_string_lossy().contains(".cargo") {
                        continue;
                    }
                    observation.result_count += 1;

                    let impl_id = matching_refs
                        .iter()
                        .filter(|n| n.id.file == impl_path)
                        .filter(|n| matches!(n.id.kind, NodeKind::Impl | NodeKind::Struct))
                        .filter(|n| n.line_start <= impl_line && n.line_end >= impl_line)
                        .min_by_key(|n| n.line_end - n.line_start)
                        .map(|n| n.id.clone());

                    if let Some(implementor) = impl_id {
                        edges.push(Edge {
                            from: implementor,
                            to: node.id.clone(),
                            kind: EdgeKind::Implements,
                            source: ExtractionSource::Lsp,
                            confidence: Confidence::Confirmed,
                            evidence: Vec::new(),
                        });
                    }
                }
            }
            Err(e) => {
                observation.record_error(&e);
                tracing::debug!("Implementation lookup failed for {}: {}", node.id.name, e);
            }
        }
        observation
    }

    #[allow(clippy::too_many_arguments)]
    async fn enrich_type_references(
        transport: &PipelinedTransport,
        file_uri: &lsp_types::Uri,
        line: u32,
        col: u32,
        node: &Node,
        endpoint_index: &EndpointLookupIndex,
        root: &Path,
        work_item_id: usize,
        telemetry: &LspQueryTelemetry,
        edges: &mut Vec<Edge>,
        had_error: &mut bool,
        error_count: &AtomicI64,
    ) -> QueryObservation {
        let mut observation = QueryObservation {
            scheduled_requests: 1,
            ..Default::default()
        };
        telemetry.note_requests_started(work_item_id, 1);
        match Self::find_references_p(transport, file_uri, line, col).await {
            Ok(locations) => {
                observation.non_empty_responses += usize::from(!locations.is_empty());
                for loc in &locations {
                    let ref_path = uri_to_relative_path(&loc.uri, root);
                    let ref_line = loc.range.start.line as usize + 1;

                    if ref_path.to_string_lossy().contains(".cargo") {
                        continue;
                    }

                    if ref_path == node.id.file
                        && ref_line >= node.line_start
                        && ref_line <= node.line_end
                    {
                        continue;
                    }
                    observation.result_count += 1;

                    let referrer_id = endpoint_index.enclosing_symbol(ref_path.as_path(), ref_line);

                    if let Some(referrer) = referrer_id {
                        if referrer == node.id {
                            continue;
                        }
                        edges.push(Edge {
                            from: referrer,
                            to: node.id.clone(),
                            kind: EdgeKind::ReferencedBy,
                            source: ExtractionSource::Lsp,
                            confidence: Confidence::Confirmed,
                            evidence: Vec::new(),
                        });
                    }
                }
            }
            Err(e) => {
                *had_error = true;
                error_count.fetch_add(1, Ordering::Relaxed);
                observation.record_error(&e);
                tracing::debug!("textDocument/references failed for {}: {}", node.id.name, e);
            }
        }
        observation
    }

    async fn enrich_document_links(
        transport: &PipelinedTransport,
        file_uri: &lsp_types::Uri,
        node: &Node,
        root: &Path,
        work_item_id: usize,
        telemetry: &LspQueryTelemetry,
        edges: &mut Vec<Edge>,
    ) -> QueryObservation {
        let mut observation = QueryObservation {
            scheduled_requests: 1,
            ..Default::default()
        };
        telemetry.note_requests_started(work_item_id, 1);
        match Self::document_links_p(transport, file_uri).await {
            Ok(links) => {
                observation.non_empty_responses += usize::from(!links.is_empty());
                for link in &links {
                    if let Some(target) = link.get("target").and_then(|t| t.as_str())
                        && let Some(target_path) = target.strip_prefix("file://")
                    {
                        let rel_target = PathBuf::from(target_path);
                        let rel_target = rel_target
                            .strip_prefix(root)
                            .unwrap_or(&rel_target)
                            .to_path_buf();

                        if rel_target.to_string_lossy().starts_with("http") {
                            continue;
                        }
                        observation.result_count += 1;

                        let target_id = NodeId {
                            root: node.id.root.clone(),
                            file: rel_target.clone(),
                            name: rel_target
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("unknown")
                                .to_string(),
                            kind: NodeKind::Module,
                        };

                        edges.push(Edge {
                            from: node.id.clone(),
                            to: target_id,
                            kind: EdgeKind::DependsOn,
                            source: ExtractionSource::Lsp,
                            confidence: Confidence::Confirmed,
                            evidence: Vec::new(),
                        });
                    }
                }
            }
            Err(e) => observation.record_error(&e),
        }
        observation
    }

    #[allow(clippy::too_many_arguments)]
    async fn enrich_document_symbols(
        transport: &PipelinedTransport,
        file_uri: &lsp_types::Uri,
        language: &str,
        refs_by_file: &HashMap<PathBuf, Vec<Node>>,
        root: &Path,
        work_item_id: usize,
        telemetry: &LspQueryTelemetry,
        new_nodes: &mut Vec<Node>,
        had_error: &mut bool,
        error_count: &AtomicI64,
    ) -> QueryObservation {
        let mut observation = QueryObservation {
            scheduled_requests: 1,
            ..Default::default()
        };
        telemetry.note_requests_started(work_item_id, 1);
        let response = transport
            .request(
                "textDocument/documentSymbol",
                serde_json::json!({ "textDocument": { "uri": file_uri.to_string() } }),
            )
            .await
            .and_then(|response| {
                normalized_document_symbol_evidence(&response, &file_uri.to_string())
            });
        match response {
            Ok(mut symbols) => {
                observation.non_empty_responses += usize::from(!symbols.is_empty());
                observation.result_count = symbols.len();
                match materialize_document_symbols(language, &mut symbols, root, |file| {
                    refs_by_file
                        .get(file)
                        .and_then(|nodes| nodes.first())
                        .map(|node| node.id.root.clone())
                }) {
                    Ok(nodes) => new_nodes.extend(nodes),
                    Err(error) => {
                        *had_error = true;
                        error_count.fetch_add(1, Ordering::Relaxed);
                        observation.record_error(&error);
                    }
                }
            }
            Err(error) => {
                *had_error = true;
                error_count.fetch_add(1, Ordering::Relaxed);
                observation.record_error(&error);
            }
        }
        observation
    }

    #[allow(clippy::too_many_arguments)]
    async fn enrich_document_locations(
        transport: &PipelinedTransport,
        file_uri: &lsp_types::Uri,
        line: u32,
        col: u32,
        node: &Node,
        endpoint_index: &EndpointLookupIndex,
        root: &Path,
        operation: LspQueryOperation,
        work_item_id: usize,
        telemetry: &LspQueryTelemetry,
        edges: &mut Vec<Edge>,
        had_error: &mut bool,
        error_count: &AtomicI64,
    ) -> QueryObservation {
        let mut observation = QueryObservation {
            scheduled_requests: 1,
            ..Default::default()
        };
        telemetry.note_requests_started(work_item_id, 1);
        let response = match operation {
            LspQueryOperation::Definitions => {
                Self::find_definitions_p(transport, file_uri, line, col).await
            }
            LspQueryOperation::References => {
                Self::find_references_p(transport, file_uri, line, col).await
            }
            _ => return observation,
        };
        match response {
            Ok(locations) => {
                observation.non_empty_responses += usize::from(!locations.is_empty());
                for location in locations {
                    observation.result_count += 1;
                    let target_path = uri_to_relative_path(&location.uri, root);
                    let target_line = location.range.start.line as usize + 1;
                    let target = endpoint_index
                        .enclosing_symbol(&target_path, target_line)
                        .unwrap_or_else(|| NodeId {
                            root: node.id.root.clone(),
                            file: target_path.clone(),
                            name: target_path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("unknown")
                                .to_string(),
                            kind: NodeKind::Module,
                        });
                    let (from, to, kind) = match operation {
                        LspQueryOperation::Definitions => {
                            (node.id.clone(), target, EdgeKind::Implements)
                        }
                        LspQueryOperation::References => {
                            (target, node.id.clone(), EdgeKind::ReferencedBy)
                        }
                        _ => unreachable!("document location operation checked above"),
                    };
                    if from != to {
                        edges.push(Edge {
                            from,
                            to,
                            kind,
                            source: ExtractionSource::Lsp,
                            confidence: Confidence::Confirmed,
                            evidence: Vec::new(),
                        });
                    }
                }
            }
            Err(error) => {
                *had_error = true;
                error_count.fetch_add(1, Ordering::Relaxed);
                observation.record_error(&error);
            }
        }
        observation
    }

    // ------------------------------------------------------------------
    // Pass 2: type hierarchy (sequential -- strike counting needs order)
    // ------------------------------------------------------------------
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_pass2_type_hierarchy(
        &self,
        transport: &Arc<PipelinedTransport>,
        root: &Path,
        matching_nodes: &[&Node],
        capabilities: LspServerCapabilities,
        budget: &mut LspQueryBudget,
        telemetry: &LspQueryTelemetry,
        mut type_hierarchy_strikes: u32,
        result: &mut EnrichmentResult,
    ) -> (bool, u32) {
        let mut has_type_hierarchy = capabilities.type_hierarchy;
        if !has_type_hierarchy {
            return (has_type_hierarchy, type_hierarchy_strikes);
        }

        let type_nodes: Vec<&Node> = matching_nodes
            .iter()
            .filter(|n| {
                matches!(
                    n.id.kind,
                    NodeKind::Trait | NodeKind::Struct | NodeKind::Enum
                )
            })
            .filter(|node| {
                self.query_profile.admits(
                    node,
                    LspQueryOperation::TypeHierarchy,
                    capabilities,
                    budget,
                )
            })
            .copied()
            .collect();

        if !type_nodes.is_empty() {
            tracing::debug!("Type hierarchy pass: {} eligible nodes", type_nodes.len());
        }

        let pass2_start = std::time::Instant::now();
        let mut pass2_done = 0u64;
        let pass2_total = type_nodes.len();
        let edges_before_pass2 = result.added_edges.len();
        let mut pass2_last_log = std::time::Instant::now();
        let mut pass2_last_count = 0u64;

        for node in &type_nodes {
            let query_started = Instant::now();
            let edges_before_query = result.added_edges.len();
            let abs_path = root.join(&node.id.file);
            let file_uri = match path_to_uri(&abs_path) {
                Ok(u) => u,
                Err(_) => continue,
            };
            let (line, col) = Self::node_lsp_position(node);

            let (ok, observation) = Self::enrich_type_hierarchy_p(
                transport,
                &file_uri,
                line,
                col,
                node,
                matching_nodes,
                root,
                result,
            )
            .await;

            let emitted_edges = result.added_edges.len() - edges_before_query;
            record_type_hierarchy_observation(
                telemetry,
                node,
                observation,
                emitted_edges,
                query_started.elapsed(),
            );

            Self::update_type_hierarchy_strikes(
                ok,
                &mut type_hierarchy_strikes,
                &mut has_type_hierarchy,
            );

            pass2_done += 1;

            // Log progress every 500 nodes or every 30 seconds
            let since_log = pass2_last_log.elapsed().as_secs();
            let nodes_since = pass2_done - pass2_last_count;
            if nodes_since >= 500 || since_log >= 30 {
                let elapsed = pass2_start.elapsed().as_secs_f64();
                let rate = pass2_done as f64 / elapsed;
                let remaining_secs = if rate > 0.0 {
                    ((pass2_total as f64) - (pass2_done as f64)) / rate
                } else {
                    0.0
                };
                let remaining = if remaining_secs >= 120.0 {
                    format!("~{} min remaining", (remaining_secs / 60.0).round() as u64)
                } else {
                    format!("~{}s remaining", remaining_secs.round() as u64)
                };
                tracing::info!(
                    "LSP: {} type hierarchy... {}/{} nodes ({} edges total, {})",
                    self.server_command,
                    pass2_done,
                    pass2_total,
                    result.added_edges.len(),
                    remaining,
                );
                pass2_last_log = std::time::Instant::now();
                pass2_last_count = pass2_done;
            }

            // Early abort: 0 new edges after 1,000 nodes + 30s warmup, OR 2 minutes
            if result.added_edges.len() == edges_before_pass2
                && ((pass2_done >= ZERO_EDGE_ABORT_THRESHOLD as u64
                    && pass2_start.elapsed() >= ZERO_EDGE_MIN_WARMUP)
                    || pass2_start.elapsed() > ZERO_EDGE_TIMEOUT)
            {
                tracing::warn!(
                    "LSP: {} type hierarchy produced 0 edges after {}/{} nodes ({:.1}s) -- aborting (likely misconfigured)",
                    self.server_command,
                    pass2_done,
                    pass2_total,
                    pass2_start.elapsed().as_secs_f64(),
                );
                break;
            }

            if !has_type_hierarchy {
                break;
            }
        }

        (has_type_hierarchy, type_hierarchy_strikes)
    }

    // ------------------------------------------------------------------
    // Pass 4: BelongsTo edges -- module hierarchy (#396).
    // ------------------------------------------------------------------
    pub(super) async fn run_pass4_belongs_to(
        &self,
        transport: &Arc<PipelinedTransport>,
        root: &Path,
        matching_nodes: &[&Node],
        result: &mut EnrichmentResult,
    ) {
        let pass4_start = std::time::Instant::now();
        let edges_before = result.added_edges.len();

        // Group matching_nodes by file
        let mut nodes_by_file: HashMap<PathBuf, Vec<&Node>> = HashMap::new();
        for n in matching_nodes {
            nodes_by_file.entry(n.id.file.clone()).or_default().push(n);
        }

        let has_parent_module = crate::extract::configs::config_for_language(&self.language)
            .map(|c| c.has_parent_module_request)
            .unwrap_or(false);

        for (rel_file, file_nodes) in &nodes_by_file {
            Self::emit_belongs_to_edges(
                transport,
                file_nodes,
                rel_file,
                root,
                has_parent_module,
                result,
            )
            .await;
        }

        // Remove duplicate module nodes (same stable_id emitted for multiple files in same dir)
        let mut deduplicated_new_nodes = Vec::with_capacity(result.new_nodes.len());
        let mut module_stable_ids_seen = std::collections::HashSet::new();
        for node in result.new_nodes.drain(..) {
            if matches!(node.id.kind, NodeKind::Module) {
                let sid = node.id.to_stable_id();
                if module_stable_ids_seen.insert(sid) {
                    deduplicated_new_nodes.push(node);
                }
                // else: skip duplicate
            } else {
                deduplicated_new_nodes.push(node);
            }
        }
        result.new_nodes = deduplicated_new_nodes;

        let belongs_to_count = result.added_edges.len() - edges_before;
        let module_node_count = result
            .new_nodes
            .iter()
            .filter(|n| matches!(n.id.kind, NodeKind::Module))
            .count();
        if belongs_to_count > 0 {
            tracing::info!(
                "LSP Pass 4 complete in {:?}: {} BelongsTo edges, {} module nodes",
                pass4_start.elapsed(),
                belongs_to_count,
                module_node_count
            );
        }
    }

    // ------------------------------------------------------------------
    // Pass 5: InlayHints -- inferred types in embeddings (#408).
    // ------------------------------------------------------------------
    pub(super) async fn run_pass5_inlay_hints(
        &self,
        transport: &Arc<PipelinedTransport>,
        root: &Path,
        matching_nodes: &[&Node],
        has_inlay_hints: bool,
        result: &mut EnrichmentResult,
    ) {
        if !has_inlay_hints {
            return;
        }

        let pass5_start = std::time::Instant::now();
        let mut hint_patches = 0usize;

        let mut nodes_by_file: HashMap<PathBuf, Vec<&Node>> = HashMap::new();
        for n in matching_nodes {
            nodes_by_file.entry(n.id.file.clone()).or_default().push(n);
        }

        for (rel_file, file_nodes) in &nodes_by_file {
            let abs_path = root.join(rel_file);
            let file_uri = match path_to_uri(&abs_path) {
                Ok(u) => u,
                Err(_) => continue,
            };

            let max_line = file_nodes
                .iter()
                .map(|n| n.line_end as u32)
                .max()
                .unwrap_or(0);

            match Self::inlay_hints_for_file(transport, &file_uri, max_line + 1).await {
                Ok(hints) if !hints.is_empty() => {
                    let type_map = Self::group_inlay_hints_by_node(&hints, file_nodes);
                    for (stable_id, type_str) in type_map {
                        result.updated_nodes.push((stable_id, {
                            let mut patch = std::collections::BTreeMap::new();
                            patch.insert("inferred_types".to_string(), type_str);
                            patch
                        }));
                        hint_patches += 1;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(
                        "textDocument/inlayHint failed for {}: {}",
                        rel_file.display(),
                        e
                    );
                }
            }
        }

        if hint_patches > 0 {
            tracing::info!(
                "LSP Pass 5 complete in {:?}: {} nodes patched with inferred_types",
                pass5_start.elapsed(),
                hint_patches
            );
        }
    }

    // ------------------------------------------------------------------
    // Pass 3: diagnostics.
    //
    // Strategy: prefer pull-based diagnostics (textDocument/diagnostic,
    // LSP 3.17+) when the server advertised `diagnosticProvider`. For
    // servers that only push, fall back to the pipelined reader loop capture.
    // ------------------------------------------------------------------
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_pass3_diagnostics(
        &self,
        transport: &Arc<PipelinedTransport>,
        root: &Path,
        matching_nodes: &[&Node],
        has_pull_diagnostics: bool,
        diag_sink: &Arc<std::sync::Mutex<HashMap<String, Vec<serde_json::Value>>>>,
        repo_root: &Path,
        result: &mut EnrichmentResult,
    ) {
        let diag_timestamp = {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_else(|_| "0".to_string())
        };

        let unique_files: Vec<PathBuf> = {
            let mut seen = std::collections::HashSet::new();
            matching_nodes
                .iter()
                .map(|n| n.id.file.clone())
                .filter(|f| seen.insert(f.clone()))
                .collect()
        };

        let root_id = matching_nodes
            .first()
            .map(|n| n.id.root.clone())
            .unwrap_or_default();

        let lsp_config = LspConfig::load(repo_root);
        let max_severity_int = lsp_config.diagnostic_min_severity.max_severity_int();

        if has_pull_diagnostics {
            tracing::info!(
                "LSP diagnostics pass: pull-based for {} files ({})",
                unique_files.len(),
                self.server_command
            );
            let mut pull_raw_total = 0usize;
            let mut pull_files_with_diags = 0usize;
            for rel_file in &unique_files {
                let abs_path = root.join(rel_file);
                let file_uri = match path_to_uri(&abs_path) {
                    Ok(u) => u,
                    Err(_) => continue,
                };
                match Self::pull_diagnostics_p(transport, &file_uri).await {
                    Ok(diags) => {
                        if !diags.is_empty() {
                            pull_raw_total += diags.len();
                            pull_files_with_diags += 1;
                            tracing::debug!(
                                "textDocument/diagnostic: {} raw items for {}",
                                diags.len(),
                                rel_file.display()
                            );
                        }
                        let nodes = Self::build_diagnostic_nodes(
                            file_uri.as_str(),
                            &diags,
                            root,
                            &root_id,
                            &self.server_command,
                            &self.language,
                            &diag_timestamp,
                            max_severity_int,
                        );
                        result.new_nodes.extend(nodes);
                    }
                    Err(e) => {
                        tracing::debug!(
                            "textDocument/diagnostic failed for {}: {}",
                            rel_file.display(),
                            e
                        );
                    }
                }
            }
            tracing::info!(
                "LSP diagnostics pass: pull complete -- {} raw items from {} files with diagnostics (out of {} files)",
                pull_raw_total,
                pull_files_with_diags,
                unique_files.len()
            );
        } else {
            let expected_uris: std::collections::HashSet<String> = unique_files
                .iter()
                .filter_map(|rel_file| {
                    path_to_uri(&root.join(rel_file))
                        .ok()
                        .map(|u| u.to_string())
                })
                .collect();

            let captured: HashMap<String, Vec<serde_json::Value>> = {
                let sink = diag_sink.lock().unwrap();
                sink.clone()
            };
            let relevant_count = captured
                .keys()
                .filter(|u| expected_uris.contains(*u))
                .count();
            tracing::info!(
                "LSP diagnostics pass: push-captured {}/{} relevant files with diagnostics ({})",
                relevant_count,
                captured.len(),
                self.server_command
            );
            for (uri, diags) in &captured {
                if !expected_uris.contains(uri) {
                    continue;
                }
                let nodes = Self::build_diagnostic_nodes(
                    uri,
                    diags,
                    root,
                    &root_id,
                    &self.server_command,
                    &self.language,
                    &diag_timestamp,
                    max_severity_int,
                );
                result.new_nodes.extend(nodes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn call_hierarchy_item(
        endpoint: &str,
        uri: &str,
        name: &str,
        detail: &str,
    ) -> serde_json::Value {
        call_hierarchy_item_at(endpoint, uri, name, detail, 7, 0, 9, 1)
    }

    #[allow(clippy::too_many_arguments)]
    fn call_hierarchy_item_at(
        endpoint: &str,
        uri: &str,
        name: &str,
        detail: &str,
        start_line: u64,
        start_character: u64,
        end_line: u64,
        end_character: u64,
    ) -> serde_json::Value {
        serde_json::json!({
            endpoint: {
                "uri": uri,
                "name": name,
                "detail": detail,
                "range": {
                    "start": { "line": start_line, "character": start_character },
                    "end": { "line": end_line, "character": end_character }
                }
            }
        })
    }

    #[test]
    fn call_hierarchy_endpoint_resolves_against_unscheduled_graph_nodes() {
        let existing = Node {
            id: NodeId {
                root: "repo".to_string(),
                file: PathBuf::from("src/caller.py"),
                name: "caller".to_string(),
                kind: NodeKind::Function,
            },
            language: "python".to_string(),
            line_start: 8,
            line_end: 10,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let refs = HashMap::from([(existing.id.file.clone(), vec![existing.clone()])]);
        let endpoint_index = EndpointLookupIndex::build(&refs);
        let item = call_hierarchy_item(
            "from",
            "file:///tmp/rna-call-hierarchy/src/caller.py",
            "caller",
            "caller",
        );

        let (resolved, materialized, confidence) = resolve_or_materialize_call_hierarchy_endpoint(
            &item,
            "from",
            Path::new("/tmp/rna-call-hierarchy"),
            "repo",
            "python",
            &endpoint_index,
        )
        .expect("existing endpoint must resolve");

        assert_eq!(resolved, existing.id);
        assert!(materialized.is_none());
        assert_eq!(confidence, Confidence::Confirmed);
    }

    #[test]
    fn unresolved_local_call_hierarchy_endpoint_materializes_stably() {
        let item = call_hierarchy_item(
            "to",
            "file:///tmp/rna-call-hierarchy/src/generated.py",
            "generated_target",
            "module.generated_target",
        );
        let endpoint_index = EndpointLookupIndex::default();
        let resolve = || {
            resolve_or_materialize_call_hierarchy_endpoint(
                &item,
                "to",
                Path::new("/tmp/rna-call-hierarchy"),
                "repo",
                "python",
                &endpoint_index,
            )
            .expect("valid local endpoint must materialize")
        };

        let (first_id, first_node, confidence) = resolve();
        let (second_id, _, _) = resolve();
        let first_node = first_node.expect("unresolved endpoint must produce a node");

        assert_eq!(first_id, second_id);
        assert_eq!(first_id.file, PathBuf::from("src/generated.py"));
        assert_eq!(first_id.name, "generated_target@lsp:7:0-9:1");
        assert_eq!(confidence, Confidence::Detected);
        assert_eq!(first_node.source, ExtractionSource::Lsp);
        assert_eq!(
            first_node
                .metadata
                .get("lsp_call_hierarchy")
                .map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn external_call_hierarchy_endpoint_uses_path_free_stable_identity() {
        let item = call_hierarchy_item(
            "to",
            "file:///opt/venv/lib/python3.13/site-packages/pkg/api.py",
            "target",
            "pkg.api.target",
        );
        let endpoint_index = EndpointLookupIndex::default();
        let (id, node, confidence) = resolve_or_materialize_call_hierarchy_endpoint(
            &item,
            "to",
            Path::new("/tmp/rna-call-hierarchy"),
            "repo",
            "python",
            &endpoint_index,
        )
        .expect("valid external endpoint must materialize");
        let node = node.expect("external endpoint must produce a node");

        assert_eq!(id.root, "external");
        assert!(id.file.as_os_str().is_empty());
        assert_eq!(id.name, "pkg.api.target@lsp:7:0-9:1");
        assert_eq!(confidence, Confidence::Detected);
        assert!(!id.to_stable_id().contains("/opt/venv"));
        assert_eq!(
            node.metadata.get("package").map(String::as_str),
            Some("pkg")
        );
        assert_eq!(
            node.metadata.get("external").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn same_name_unresolved_endpoints_use_range_disambiguators() {
        let index = EndpointLookupIndex::default();
        let local_first = call_hierarchy_item_at(
            "to",
            "file:///tmp/rna-call-hierarchy/src/generated.py",
            "generated_target",
            "module.generated_target",
            7,
            0,
            9,
            1,
        );
        let local_second = call_hierarchy_item_at(
            "to",
            "file:///tmp/rna-call-hierarchy/src/generated.py",
            "generated_target",
            "module.generated_target",
            17,
            2,
            19,
            3,
        );
        let external_first = call_hierarchy_item_at(
            "to",
            "file:///opt/venv/site-packages/pkg/api.py",
            "target",
            "pkg.api.target",
            3,
            0,
            4,
            1,
        );
        let external_second = call_hierarchy_item_at(
            "to",
            "file:///opt/venv/site-packages/pkg/api.py",
            "target",
            "pkg.api.target",
            13,
            4,
            14,
            5,
        );
        let resolve = |item: &serde_json::Value| {
            resolve_or_materialize_call_hierarchy_endpoint(
                item,
                "to",
                Path::new("/tmp/rna-call-hierarchy"),
                "repo",
                "python",
                &index,
            )
            .expect("valid unresolved endpoint must materialize")
            .0
        };

        let local_first = resolve(&local_first);
        let local_second = resolve(&local_second);
        let external_first = resolve(&external_first);
        let external_second = resolve(&external_second);
        assert_ne!(local_first, local_second);
        assert_ne!(external_first, external_second);
        assert_eq!(local_first.name, "generated_target@lsp:7:0-9:1");
        assert_eq!(local_second.name, "generated_target@lsp:17:2-19:3");
        assert_eq!(external_first.name, "pkg.api.target@lsp:3:0-4:1");
        assert_eq!(external_second.name, "pkg.api.target@lsp:13:4-14:5");
    }

    #[test]
    fn endpoint_lookup_index_resolves_narrowest_enclosing_symbol() {
        let make_node = |name: &str, kind: NodeKind, start: usize, end: usize| Node {
            id: NodeId {
                root: "repo".to_string(),
                file: PathBuf::from("src/nested.py"),
                name: name.to_string(),
                kind,
            },
            language: "python".to_string(),
            line_start: start,
            line_end: end,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let outer = make_node("Outer", NodeKind::Struct, 1, 20);
        let inner = make_node("inner", NodeKind::Function, 5, 10);
        let nodes = HashMap::from([(
            PathBuf::from("src/nested.py"),
            vec![outer.clone(), inner.clone()],
        )]);
        let index = EndpointLookupIndex::build(&nodes);

        assert_eq!(
            index.enclosing_symbol(Path::new("src/nested.py"), 7),
            Some(inner.id)
        );
        assert_eq!(
            index.enclosing_symbol(Path::new("src/nested.py"), 15),
            Some(outer.id)
        );
        assert_eq!(
            index.unique_function(Path::new("src/nested.py"), "inner"),
            Some(NodeId {
                root: "repo".to_string(),
                file: PathBuf::from("src/nested.py"),
                name: "inner".to_string(),
                kind: NodeKind::Function,
            })
        );
    }

    #[test]
    fn document_symbol_only_work_never_trips_zero_edge_watchdog() {
        assert!(!should_abort_zero_edge_pass(
            0,
            0,
            false,
            ZERO_EDGE_TIMEOUT + Duration::from_secs(1),
        ));
        assert!(should_abort_zero_edge_pass(
            1,
            ZERO_EDGE_ABORT_THRESHOLD,
            false,
            ZERO_EDGE_MIN_WARMUP,
        ));
        assert!(!should_abort_zero_edge_pass(
            1,
            ZERO_EDGE_ABORT_THRESHOLD,
            true,
            ZERO_EDGE_TIMEOUT + Duration::from_secs(1),
        ));
    }

    #[test]
    fn type_hierarchy_telemetry_uses_all_observed_requests() {
        let profile = super::super::policy::LspQueryProfile::new("rust", "rust-analyzer");
        let telemetry = LspQueryTelemetry::new(&profile);
        let node = Node {
            id: NodeId {
                root: "repo".to_string(),
                file: PathBuf::from("src/lib.rs"),
                name: "Child".to_string(),
                kind: NodeKind::Struct,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 2,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        record_type_hierarchy_observation(
            &telemetry,
            &node,
            QueryObservation {
                scheduled_requests: 2,
                non_empty_responses: 2,
                ..Default::default()
            },
            1,
            Duration::from_millis(5),
        );

        let metrics = telemetry.snapshot();
        assert_eq!(metrics[0].scheduled_requests, 2);
        assert_eq!(metrics[0].non_empty_responses, 2);
        assert_eq!(metrics[0].emitted_edges, 1);
    }

    #[test]
    fn document_link_work_is_deduplicated_by_file() {
        let make_node = |name: &str, file: &str| Node {
            id: NodeId {
                root: "repo".to_string(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind: NodeKind::Other("markdown_section".to_string()),
            },
            language: "markdown".to_string(),
            line_start: 1,
            line_end: 2,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let mut seen = HashSet::new();

        assert!(retain_unique_document_link_file(
            &make_node("first", "docs/one.md"),
            LspQueryOperation::DocumentLinks,
            &mut seen,
        ));
        assert!(!retain_unique_document_link_file(
            &make_node("second", "docs/one.md"),
            LspQueryOperation::DocumentLinks,
            &mut seen,
        ));
        assert!(retain_unique_document_link_file(
            &make_node("third", "docs/two.md"),
            LspQueryOperation::DocumentLinks,
            &mut seen,
        ));
    }

    #[test]
    fn pass1_preopen_files_are_deterministic_and_deduplicated() {
        let item = |id: usize, file: &str, operation| LspPass1WorkItem {
            id,
            node: Node {
                id: NodeId {
                    root: "repo".to_string(),
                    file: PathBuf::from(file),
                    name: format!("symbol-{id}"),
                    kind: NodeKind::Function,
                },
                language: "python".to_string(),
                line_start: 1,
                line_end: 1,
                signature: String::new(),
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            },
            requested_operations: vec![operation],
            attempt_count: 1,
        };
        let work_items = vec![
            item(0, "tests/test_app.py", LspQueryOperation::DocumentSymbols),
            item(1, "src/app.py", LspQueryOperation::CallHierarchy),
            item(2, "src/app.py", LspQueryOperation::DocumentSymbols),
        ];

        assert_eq!(
            pass1_work_item_files(&work_items),
            [
                PathBuf::from("src/app.py"),
                PathBuf::from("tests/test_app.py")
            ]
        );
    }

    #[tokio::test]
    async fn did_open_coordinator_dedupes_concurrent_same_file() {
        let coordinator = Arc::new(DidOpenCoordinator::new(
            "test-lsp".to_string(),
            "rust".to_string(),
        ));
        let attempts = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let mut tasks = tokio::task::JoinSet::new();

        for _ in 0..8 {
            let coordinator = Arc::clone(&coordinator);
            let attempts = Arc::clone(&attempts);
            let release = Arc::clone(&release);
            tasks.spawn(async move {
                coordinator
                    .ensure_open_with(Path::new("src/lib.rs"), || async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        release.notified().await;
                        Ok(())
                    })
                    .await
            });
        }

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if attempts.load(Ordering::SeqCst) == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("one didOpen attempt should start");
        release.notify_waiters();

        let mut opened = 0;
        let mut reused = 0;
        while let Some(result) = tasks.join_next().await {
            match result.unwrap().unwrap() {
                true => opened += 1,
                false => reused += 1,
            }
        }

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(opened, 1);
        assert_eq!(reused, 7);
    }

    #[tokio::test]
    async fn did_open_failure_does_not_suppress_retry() {
        let coordinator = DidOpenCoordinator::new("test-lsp".to_string(), "rust".to_string());
        let attempts = AtomicUsize::new(0);

        let first = coordinator
            .ensure_open_with(Path::new("src/lib.rs"), || async {
                attempts.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("write failed")
            })
            .await;
        assert!(first.is_err());

        let second = coordinator
            .ensure_open_with(Path::new("src/lib.rs"), || async {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap();
        let third = coordinator
            .ensure_open_with(Path::new("src/lib.rs"), || async {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap();

        assert!(second);
        assert!(!third);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn pass1_diagnostic_snapshot_is_bounded_and_phase_counted() {
        let diagnostics = LspPass1Diagnostics::new(10);
        for id in 0..8 {
            let item = LspPass1WorkItem {
                id,
                node: Node {
                    id: NodeId {
                        root: "repo".to_string(),
                        file: PathBuf::from(format!("src/file{id}.rs")),
                        kind: NodeKind::Function,
                        name: format!("func{id}"),
                    },
                    language: "rust".to_string(),
                    line_start: 1,
                    line_end: 1,
                    signature: format!("fn func{id}()"),
                    body: String::new(),
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::Lsp,
                },
                requested_operations: vec![LspQueryOperation::References],
                attempt_count: 1,
            };
            diagnostics.set_phase(&item, "requesting_references").await;
        }

        let snapshot = diagnostics.snapshot().await;
        let rendered = snapshot.render();
        assert_eq!(snapshot.in_flight, 8);
        assert_eq!(snapshot.oldest.len(), PASS1_DIAGNOSTIC_SAMPLE_LIMIT);
        assert!(rendered.contains("requesting_references=8"), "{rendered}");
        assert!(rendered.contains("attempt=1"), "{rendered}");
    }

    #[test]
    fn recovered_pass1_edges_are_applied_idempotently() {
        let edge = Edge {
            from: NodeId {
                root: "repo".to_string(),
                file: PathBuf::from("src/lib.rs"),
                kind: NodeKind::Function,
                name: "caller".to_string(),
            },
            to: NodeId {
                root: "repo".to_string(),
                file: PathBuf::from("src/lib.rs"),
                kind: NodeKind::Function,
                name: "callee".to_string(),
            },
            kind: EdgeKind::Calls,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        let mut edges = Vec::new();
        let mut seen = std::collections::HashSet::new();

        extend_unique_edges(&mut edges, &mut seen, [edge.clone()]);
        extend_unique_edges(&mut edges, &mut seen, [edge]);

        assert_eq!(edges.len(), 1);
    }

    #[tokio::test]
    async fn interrupt_restart_executor_fixture_invokes_only_retryable_items_once() {
        let repo = tempfile::tempdir().unwrap();
        let make_node = |id: usize| Node {
            id: NodeId {
                root: "repo".to_string(),
                file: PathBuf::from(format!("src/item_{id}.rs")),
                kind: NodeKind::Function,
                name: format!("item_{id}"),
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 1,
            signature: format!("fn item_{id}()"),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let work_items = (0..2)
            .map(|id| LspPass1WorkItem {
                id,
                node: make_node(id),
                requested_operations: vec![LspQueryOperation::References],
                attempt_count: 1,
            })
            .collect::<Vec<_>>();
        let seeds = work_items
            .iter()
            .map(|item| LspWorkItemSeed {
                item_id: item.id,
                node: item.node.clone(),
                requested_operations: item
                    .requested_operations
                    .iter()
                    .map(|operation| (*operation).to_string())
                    .collect(),
                attempt_count: item.attempt_count,
            })
            .collect::<Vec<_>>();
        let initial = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "executor-restart".to_string(),
            &seeds,
        )
        .await
        .unwrap();
        let recovered_edge = Edge {
            from: make_node(0).id,
            to: make_node(1).id,
            kind: EdgeKind::Calls,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        initial
            .mark_completed_with_output(0, std::slice::from_ref(&recovered_edge), &[], 1)
            .await
            .unwrap();
        initial
            .mark_phase(1, "requesting_references")
            .await
            .unwrap();
        initial.flush().await.unwrap();

        let resumed = LspWorkItemLedger::begin(repo.path(), &seeds).await.unwrap();
        let invocations = Arc::new(std::sync::Mutex::new([(0usize, 0u32); 2]));
        let (scheduled, _, mut workers, mut results) =
            spawn_pass1_workers(work_items, &resumed, 2, {
                let invocations = Arc::clone(&invocations);
                move |item, _| {
                    let invocations = Arc::clone(&invocations);
                    async move {
                        let mut invocations = invocations.lock().unwrap();
                        invocations[item.id].0 += 1;
                        invocations[item.id].1 = item.attempt_count;
                        Pass1TaskResult {
                            edges: Vec::new(),
                            new_nodes: Vec::new(),
                            had_error: false,
                            edge_producing: true,
                        }
                    }
                }
            });
        while results.recv().await.is_some() {}
        while let Some(worker) = workers.join_next().await {
            worker.unwrap();
        }

        assert_eq!(scheduled, 1);
        assert_eq!(*invocations.lock().unwrap(), [(0, 0), (1, 2)]);
        let (edges, _) = resumed.recovered_output();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].stable_id(), recovered_edge.stable_id());
    }

    #[tokio::test]
    async fn exhausted_recovery_fails_pass1_closed_without_invocation() {
        let repo = tempfile::tempdir().unwrap();
        let node = Node {
            id: NodeId {
                root: "repo".to_string(),
                file: PathBuf::from("src/exhausted.rs"),
                kind: NodeKind::Function,
                name: "exhausted".to_string(),
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 1,
            signature: "fn exhausted()".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let seed = LspWorkItemSeed {
            item_id: 0,
            node: node.clone(),
            requested_operations: vec!["textDocument/references".to_string()],
            attempt_count: 3,
        };
        let initial = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "exhausted-restart".to_string(),
            std::slice::from_ref(&seed),
        )
        .await
        .unwrap();
        initial
            .mark_failed(0, "server remained unavailable")
            .await
            .unwrap();
        initial.flush().await.unwrap();

        let resumed = LspWorkItemLedger::begin(repo.path(), &[seed])
            .await
            .unwrap();
        let work_items = vec![LspPass1WorkItem {
            id: 0,
            node,
            requested_operations: vec![LspQueryOperation::References],
            attempt_count: 3,
        }];

        let invocations = Arc::new(AtomicUsize::new(0));
        let (scheduled, _, mut workers, mut results) =
            spawn_pass1_workers(work_items, &resumed, 1, {
                let invocations = Arc::clone(&invocations);
                move |_, _| {
                    let invocations = Arc::clone(&invocations);
                    async move {
                        invocations.fetch_add(1, Ordering::Relaxed);
                        Pass1TaskResult {
                            edges: Vec::new(),
                            new_nodes: Vec::new(),
                            had_error: false,
                            edge_producing: true,
                        }
                    }
                }
            });
        while results.recv().await.is_some() {}
        while let Some(worker) = workers.join_next().await {
            worker.unwrap();
        }

        assert_eq!(scheduled, 0);
        assert_eq!(invocations.load(Ordering::Relaxed), 0);
        let (errors, aborted, diagnostic) = recovery_failure_state(&resumed);
        assert_eq!(errors, 1);
        assert!(aborted);
        assert!(diagnostic.unwrap().contains("exhausted the retry budget"));
    }
}
