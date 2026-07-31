use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::graph::{Edge, Node};

pub(crate) const STORE_SCHEMA_VERSION: u32 = 5;
const WORK_IDENTITY_SCHEMA_VERSION: u32 = 1;
const PLANNER_CONTRACT_VERSION: &str = "lsp-pass1-work-planner-v1";
const STORE_FILE: &str = "lsp_pass1_work_items.json";
const MAX_RETAINED_ACTIVE_JOBS: usize = 32;
const MAX_RETAINED_TERMINAL_JOBS: usize = 16;
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
const OLDEST_SAMPLE_LIMIT: usize = 5;
const LOCK_OWNER_INITIALIZATION_GRACE: Duration = Duration::from_secs(2);
const MAX_ATTEMPTS: u32 = 3;
const UNMATCHED_REQUIRED_WORK_ERROR: &str =
    "persisted work item is no longer present in the enrichable node set; skipped";

static JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static STORE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

#[derive(Debug, Clone)]
pub(crate) struct LspWorkItemSeed {
    pub item_id: usize,
    pub node: Node,
    pub requested_operations: Vec<String>,
    pub attempt_count: u32,
    pub toolchain_contract: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LspWorkItemState {
    #[default]
    Pending,
    InFlight,
    Completed,
    Failed,
    Skipped,
    Exhausted,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LspWorkItemRecovery {
    #[default]
    New,
    CarriedCompleted,
    CarriedSkipped,
    Retried,
    Exhausted,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LspWorkIdentity {
    pub schema_version: u32,
    pub source_snapshot: String,
    pub request_anchor: String,
    pub operations_digest: String,
    pub toolchain_contract: String,
    /// The planner-contract version live when this identity was computed.
    /// Compared explicitly in `identity_disposition` so a routine
    /// `PLANNER_CONTRACT_VERSION` bump maps to `RerunPlannerContract` instead
    /// of `RejectedTampered`; absent on pre-migration records (defaults to
    /// empty), which still mismatches the current version and reruns.
    #[serde(default)]
    pub planner_contract: String,
    pub digest: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LspRecoveryDisposition {
    #[default]
    New,
    CarriedExact,
    RerunSourceSnapshot,
    RerunRequestAnchor,
    RerunOperations,
    RerunToolchain,
    RerunPlannerContract,
    RerunSchema,
    RejectedTampered,
    RejectedDuplicate,
}

impl LspRecoveryDisposition {
    fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::CarriedExact => "carried_exact",
            Self::RerunSourceSnapshot => "rerun_source_snapshot",
            Self::RerunRequestAnchor => "rerun_request_anchor",
            Self::RerunOperations => "rerun_operations",
            Self::RerunToolchain => "rerun_toolchain",
            Self::RerunPlannerContract => "rerun_planner_contract",
            Self::RerunSchema => "rerun_schema",
            Self::RejectedTampered => "rejected_tampered",
            Self::RejectedDuplicate => "rejected_duplicate",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LspWorkItemRecord {
    #[serde(default)]
    pub schema_version: u32,
    pub job_id: String,
    pub item_id: usize,
    pub repo: String,
    pub root: String,
    pub file: String,
    pub node_id: String,
    pub node_name: String,
    pub node_kind: String,
    #[serde(default)]
    pub input_hash: String,
    #[serde(default)]
    pub work_identity: LspWorkIdentity,
    #[serde(default)]
    pub recovery_disposition: LspRecoveryDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_source_job_id: Option<String>,
    #[serde(default)]
    pub integrity_digest: String,
    #[serde(default)]
    pub requested_operations: Vec<String>,
    #[serde(default)]
    pub state: LspWorkItemState,
    #[serde(default)]
    pub attempt_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_phase: Option<String>,
    #[serde(default)]
    pub created_at_ms: u64,
    #[serde(default)]
    pub updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub recovery: LspWorkItemRecovery,
    #[serde(default)]
    pub output_edges: Vec<Edge>,
    #[serde(default)]
    pub output_nodes: Vec<Node>,
    /// Exact stable graph result IDs emitted by this producer. This durable
    /// lineage lets verified structural-cache reuse retain a shared result only
    /// while at least one authenticated producer remains valid.
    #[serde(default)]
    pub produced_result_ids: BTreeSet<String>,
    /// Number of raw, applicable LSP results observed before graph mapping.
    /// This remains non-zero when a server response cannot be mapped to a
    /// persistable graph node or edge, allowing readiness to fail closed.
    #[serde(default)]
    pub observed_result_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LspWorkItemStore {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    records: BTreeMap<String, LspWorkItemRecord>,
}

impl Default for LspWorkItemStore {
    fn default() -> Self {
        Self {
            schema_version: STORE_SCHEMA_VERSION,
            records: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LspWorkItemQueueSnapshot {
    pub schema_version: u32,
    pub job_id: String,
    pub repo: String,
    pub roots: Vec<String>,
    pub total: usize,
    pub pending: usize,
    pub in_flight: usize,
    pub completed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub exhausted: usize,
    pub resumed: usize,
    pub retried: usize,
    #[serde(default)]
    pub recovery_dispositions: BTreeMap<String, usize>,
    #[serde(default)]
    pub phase_counts: BTreeMap<String, usize>,
    #[serde(default)]
    pub oldest_in_flight: Vec<String>,
    #[serde(default)]
    pub exhausted_items: Vec<String>,
    #[serde(default)]
    pub updated_at_ms: u64,
}

impl LspWorkItemQueueSnapshot {
    pub fn render(&self) -> String {
        let phases = if self.phase_counts.is_empty() {
            "none".to_string()
        } else {
            self.phase_counts
                .iter()
                .map(|(phase, count)| format!("{phase}={count}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let recovery = if self.recovery_dispositions.is_empty() {
            "none".to_string()
        } else {
            self.recovery_dispositions
                .iter()
                .map(|(disposition, count)| format!("{disposition}={count}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let oldest = if self.oldest_in_flight.is_empty() {
            "none".to_string()
        } else {
            self.oldest_in_flight.join("; ")
        };
        let exhausted_items = if self.exhausted_items.is_empty() {
            "none".to_string()
        } else {
            self.exhausted_items.join("; ")
        };
        format!(
            "job={} total={} pending={} in_flight={} completed={} failed={} skipped={} exhausted={} resumed={} retried={} recovery=[{}] phases=[{}] oldest=[{}] exhausted_items=[{}]",
            self.job_id,
            self.total,
            self.pending,
            self.in_flight,
            self.completed,
            self.failed,
            self.skipped,
            self.exhausted,
            self.resumed,
            self.retried,
            recovery,
            phases,
            oldest,
            exhausted_items
        )
    }
}

pub(crate) struct LspWorkItemLedger {
    repo_root: PathBuf,
    job_id: String,
    store: Mutex<LspWorkItemStore>,
    last_flush: Mutex<Instant>,
    persist_lock: Arc<tokio::sync::Mutex<()>>,
    runnable_item_ids: BTreeSet<usize>,
    recovered_output: Mutex<(Vec<Edge>, Vec<Node>)>,
}

struct WorkItemFileLock {
    path: PathBuf,
    owner: String,
    acquired: bool,
}

impl WorkItemFileLock {
    fn acquire(repo_root: &Path) -> Self {
        let path = store_path(repo_root).with_extension("lock");
        let owner = std::process::id().to_string();
        if let Some(parent) = path.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            tracing::warn!(%error, "Failed to create LSP work-item lock directory");
        }

        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    if let Err(error) = writeln!(file, "{owner}") {
                        tracing::warn!(
                            %error,
                            path = %path.display(),
                            "Failed to write LSP work-item lock owner; continuing with process-local lock only"
                        );
                        let _ = std::fs::remove_file(&path);
                        return Self {
                            path,
                            owner,
                            acquired: false,
                        };
                    }
                    return Self {
                        path,
                        owner,
                        acquired: true,
                    };
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if work_item_lock_owner_is_dead(&path) {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        path = %path.display(),
                        "Failed to acquire LSP work-item file lock; continuing with process-local lock only"
                    );
                    return Self {
                        path,
                        owner,
                        acquired: false,
                    };
                }
            }
        }
    }
}

impl Drop for WorkItemFileLock {
    fn drop(&mut self) {
        if self.acquired && work_item_lock_is_owned_by(&self.path, &self.owner) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl LspWorkItemLedger {
    pub(crate) async fn begin(repo_root: &Path, seeds: &[LspWorkItemSeed]) -> Result<Arc<Self>> {
        let persisted = load_store(repo_root)?;
        let Some(recovery) = select_recovery_job(&persisted, seeds) else {
            return Self::begin_with_job_id(repo_root, new_job_id(), seeds).await;
        };
        let job_id = recovery.job_id;
        let prior_records = recovery.records;
        let duplicate_retained_keys = recovery.duplicate_keys;
        let source_snapshot = source_snapshot_identity(repo_root)?;
        let now = unix_millis();
        let current_item_keys = seeds
            .iter()
            .map(|seed| {
                (
                    seed.node.stable_id(),
                    canonical_operations_digest(&seed.requested_operations),
                )
            })
            .collect::<BTreeSet<_>>();
        let current_node_ids = seeds
            .iter()
            .map(|seed| seed.node.stable_id())
            .collect::<BTreeSet<_>>();
        let mut prior_by_key: BTreeMap<(String, String), Vec<LspWorkItemRecord>> = BTreeMap::new();
        for record in prior_records {
            prior_by_key
                .entry((
                    record.node_id.clone(),
                    canonical_operations_digest(&record.requested_operations),
                ))
                .or_default()
                .push(record);
        }
        // Tracks, per node ID, how many distinct-operations-digest keys for that
        // node remain in `prior_by_key`. Kept in sync as keys are removed below
        // so `changed_operations` stays an O(log n) lookup instead of rescanning
        // every remaining key on every seed with no exact match.
        let mut prior_node_id_counts: BTreeMap<String, usize> = BTreeMap::new();
        for (prior_node_id, _) in prior_by_key.keys() {
            *prior_node_id_counts
                .entry(prior_node_id.clone())
                .or_insert(0) += 1;
        }
        let mut store = LspWorkItemStore::default();
        let mut runnable_item_ids = BTreeSet::new();
        let mut recovered_edges = Vec::new();
        let mut recovered_nodes = Vec::new();
        let source_cache = SourceLineCache::default();

        for seed in seeds {
            let node_id = seed.node.stable_id();
            let current_identity = work_identity(repo_root, seed, &source_snapshot, &source_cache);
            let key = (node_id.clone(), current_identity.operations_digest.clone());
            let candidates = prior_by_key.remove(&key).unwrap_or_default();
            if !candidates.is_empty()
                && let Some(count) = prior_node_id_counts.get_mut(&node_id)
            {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    prior_node_id_counts.remove(&node_id);
                }
            }
            let (prior_record, disposition) = if duplicate_retained_keys.contains(&key) {
                (None, LspRecoveryDisposition::RejectedDuplicate)
            } else {
                match candidates.len() {
                    0 => {
                        let changed_operations = prior_node_id_counts.contains_key(&node_id);
                        (
                            None,
                            if changed_operations {
                                LspRecoveryDisposition::RerunOperations
                            } else {
                                LspRecoveryDisposition::New
                            },
                        )
                    }
                    1 => {
                        let prior = candidates.into_iter().next().expect("one candidate");
                        let disposition = match prior.recovery_disposition {
                            LspRecoveryDisposition::RejectedTampered => {
                                LspRecoveryDisposition::RejectedTampered
                            }
                            LspRecoveryDisposition::RerunSchema => {
                                LspRecoveryDisposition::RerunSchema
                            }
                            _ => identity_disposition(&prior.work_identity, &current_identity),
                        };
                        (Some(prior), disposition)
                    }
                    _ => (None, LspRecoveryDisposition::RejectedDuplicate),
                }
            };
            let is_exact = disposition == LspRecoveryDisposition::CarriedExact;
            let recovery_source_job_id = prior_record.as_ref().map(|record| record.job_id.clone());
            let mut record = if is_exact {
                prior_record.expect("exact disposition requires a prior record")
            } else {
                let mut record =
                    new_record(repo_root, &job_id, seed, current_identity.clone(), now);
                record.recovery_disposition = disposition;
                record.recovery_source_job_id = recovery_source_job_id;
                if disposition != LspRecoveryDisposition::New {
                    record.last_error =
                        Some(format!("recovery disposition: {}", disposition.as_str()));
                }
                record
            };
            record.item_id = seed.item_id;
            record.job_id = job_id.clone();
            record.repo = repo_root.display().to_string();
            record.root = seed.node.id.root.clone();
            record.file = seed.node.id.file.display().to_string();
            record.node_id = node_id;
            record.node_name = seed.node.id.name.clone();
            record.node_kind = seed.node.id.kind.to_string();
            record.input_hash = current_identity.digest.clone();
            record.work_identity = current_identity;
            if is_exact {
                record.recovery_disposition = LspRecoveryDisposition::CarriedExact;
                record.recovery_source_job_id = Some(job_id.clone());
            }
            record.requested_operations = seed.requested_operations.clone();
            record.schema_version = STORE_SCHEMA_VERSION;
            record.updated_at_ms = now;

            if !is_exact {
                runnable_item_ids.insert(seed.item_id);
                store
                    .records
                    .insert(record_key(&job_id, seed.item_id), record);
                continue;
            }

            match record.state {
                LspWorkItemState::Completed => {
                    record.recovery = LspWorkItemRecovery::CarriedCompleted;
                    recovered_edges.extend(record.output_edges.clone());
                    recovered_nodes.extend(record.output_nodes.clone());
                }
                LspWorkItemState::Skipped => {
                    record.recovery = LspWorkItemRecovery::CarriedSkipped;
                }
                LspWorkItemState::Exhausted => {
                    record.recovery = LspWorkItemRecovery::Exhausted;
                }
                LspWorkItemState::Pending
                | LspWorkItemState::InFlight
                | LspWorkItemState::Failed => {
                    if record.attempt_count >= MAX_ATTEMPTS {
                        let previous_error = record.last_error.take();
                        record.state = LspWorkItemState::Exhausted;
                        record.recovery = LspWorkItemRecovery::Exhausted;
                        record.output_edges.clear();
                        record.output_nodes.clear();
                        record.last_phase =
                            record.current_phase.take().or(record.last_phase.take());
                        record.completed_at_ms = Some(now);
                        record.last_error = Some(format!(
                            "retry budget exhausted after {} attempts; inspect phase {} and retry with narrower scope or fix the language server{}",
                            record.attempt_count,
                            record.last_phase.as_deref().unwrap_or("unknown"),
                            previous_error
                                .as_deref()
                                .map(|error| format!("; last server error: {error}"))
                                .unwrap_or_default()
                        ));
                    } else {
                        let previous_state = record.state;
                        let previous_error = record.last_error.take();
                        record.state = LspWorkItemState::Pending;
                        record.recovery = LspWorkItemRecovery::Retried;
                        record.attempt_count = record.attempt_count.saturating_add(1);
                        record.last_phase =
                            record.current_phase.take().or(record.last_phase.take());
                        record.started_at_ms = None;
                        record.completed_at_ms = None;
                        record.output_edges.clear();
                        record.output_nodes.clear();
                        record.last_error = Some(format!(
                            "resumed after {previous_state:?} at phase {}{}",
                            record.last_phase.as_deref().unwrap_or("unknown"),
                            previous_error
                                .as_deref()
                                .map(|error| format!("; prior error: {error}"))
                                .unwrap_or_default()
                        ));
                        runnable_item_ids.insert(seed.item_id);
                    }
                }
            }
            store
                .records
                .insert(record_key(&job_id, seed.item_id), record);
        }

        let remaining_records = prior_by_key.into_values().flatten().filter(|record| {
            !current_item_keys.contains(&(
                record.node_id.clone(),
                canonical_operations_digest(&record.requested_operations),
            ))
        });
        let mut next_item_id = seeds.len();
        for mut record in remaining_records {
            if current_node_ids.contains(&record.node_id) {
                // The graph input still exists, but this exact operation is no
                // longer in the current capability-aware plan. Retire the old
                // operation instead of converting it into required work.
                continue;
            }
            record.schema_version = STORE_SCHEMA_VERSION;
            let item_id = next_item_id;
            next_item_id += 1;
            record.item_id = item_id;
            record.state = LspWorkItemState::Skipped;
            record.recovery = LspWorkItemRecovery::CarriedSkipped;
            record.last_phase = record.current_phase.take().or(record.last_phase.take());
            record.completed_at_ms = Some(now);
            record.updated_at_ms = now;
            record.output_edges.clear();
            record.output_nodes.clear();
            record.last_error = Some(UNMATCHED_REQUIRED_WORK_ERROR.to_string());
            store.records.insert(record_key(&job_id, item_id), record);
        }

        let ledger = Arc::new(Self {
            repo_root: repo_root.to_path_buf(),
            job_id,
            store: Mutex::new(store),
            last_flush: Mutex::new(Instant::now()),
            persist_lock: store_lock(repo_root)?,
            runnable_item_ids,
            recovered_output: Mutex::new((recovered_edges, recovered_nodes)),
        });
        ledger.flush().await?;
        Ok(ledger)
    }

    pub(crate) async fn begin_with_job_id(
        repo_root: &Path,
        job_id: String,
        seeds: &[LspWorkItemSeed],
    ) -> Result<Arc<Self>> {
        let mut store = LspWorkItemStore::default();
        let now = unix_millis();
        let source_snapshot = source_snapshot_identity(repo_root)?;
        let source_cache = SourceLineCache::default();
        for seed in seeds {
            let identity = work_identity(repo_root, seed, &source_snapshot, &source_cache);
            let record = new_record(repo_root, &job_id, seed, identity, now);
            store
                .records
                .insert(record_key(&job_id, seed.item_id), record);
        }
        store.schema_version = STORE_SCHEMA_VERSION;
        let ledger = Arc::new(Self {
            repo_root: repo_root.to_path_buf(),
            job_id,
            store: Mutex::new(store),
            last_flush: Mutex::new(Instant::now()),
            persist_lock: store_lock(repo_root)?,
            runnable_item_ids: seeds.iter().map(|seed| seed.item_id).collect(),
            recovered_output: Mutex::new((Vec::new(), Vec::new())),
        });
        ledger.flush().await?;
        Ok(ledger)
    }

    pub(crate) fn should_run(&self, item_id: usize) -> bool {
        self.runnable_item_ids.contains(&item_id)
    }

    pub(crate) fn attempt_count(&self, item_id: usize) -> Option<u32> {
        self.store
            .lock()
            .unwrap()
            .records
            .get(&record_key(&self.job_id, item_id))
            .map(|record| record.attempt_count)
    }

    pub(crate) fn recovered_output(&self) -> (Vec<Edge>, Vec<Node>) {
        let mut recovered = self.recovered_output.lock().unwrap();
        (
            std::mem::take(&mut recovered.0),
            std::mem::take(&mut recovered.1),
        )
    }

    pub(crate) fn exhausted_count(&self) -> usize {
        self.store
            .lock()
            .map(|store| {
                store
                    .records
                    .values()
                    .filter(|record| record.state == LspWorkItemState::Exhausted)
                    .count()
            })
            .unwrap_or(1)
    }

    pub(crate) fn unmatched_required_count(&self) -> usize {
        self.store
            .lock()
            .map(|store| {
                store
                    .records
                    .values()
                    .filter(|record| {
                        record.state == LspWorkItemState::Skipped
                            && record.last_error.as_deref() == Some(UNMATCHED_REQUIRED_WORK_ERROR)
                    })
                    .count()
            })
            .unwrap_or(1)
    }

    pub(crate) async fn mark_phase(&self, item_id: usize, phase: &str) -> Result<()> {
        let now = unix_millis();
        self.update(item_id, |record| {
            record.state = LspWorkItemState::InFlight;
            record.started_at_ms.get_or_insert(now);
            record.last_phase = record.current_phase.take();
            record.current_phase = Some(phase.to_string());
            record.updated_at_ms = now;
        })?;
        self.maybe_flush().await
    }

    #[cfg(test)]
    pub(crate) async fn mark_completed(&self, item_id: usize) -> Result<()> {
        self.mark_completed_with_output(item_id, &[], &[], 0).await
    }

    pub(crate) async fn mark_completed_with_output(
        &self,
        item_id: usize,
        edges: &[Edge],
        nodes: &[Node],
        observed_result_count: u64,
    ) -> Result<()> {
        self.update(item_id, |record| {
            record.output_edges = edges.to_vec();
            record.output_nodes = nodes.to_vec();
            record.produced_result_ids = edges
                .iter()
                .map(Edge::stable_id)
                .chain(nodes.iter().map(Node::stable_id))
                .collect();
            record.observed_result_count = observed_result_count;
        })?;
        self.mark_terminal(item_id, LspWorkItemState::Completed, None)
            .await
    }

    pub(crate) async fn mark_failed(&self, item_id: usize, error: impl Into<String>) -> Result<()> {
        self.mark_terminal(item_id, LspWorkItemState::Failed, Some(error.into()))
            .await
    }

    #[allow(dead_code)]
    pub(crate) async fn mark_skipped(
        &self,
        item_id: usize,
        reason: impl Into<String>,
    ) -> Result<()> {
        self.mark_terminal(item_id, LspWorkItemState::Skipped, Some(reason.into()))
            .await
    }

    async fn mark_terminal(
        &self,
        item_id: usize,
        state: LspWorkItemState,
        error: Option<String>,
    ) -> Result<()> {
        let now = unix_millis();
        self.update(item_id, |record| {
            record.state = state;
            record.last_phase = record.current_phase.take().or(record.last_phase.take());
            record.updated_at_ms = now;
            record.completed_at_ms = Some(now);
            if let Some(error) = error {
                record.last_error = Some(error);
            }
            if state != LspWorkItemState::Completed {
                record.output_edges.clear();
                record.output_nodes.clear();
                record.observed_result_count = 0;
            }
        })?;
        self.maybe_flush().await
    }

    fn update(&self, item_id: usize, update: impl FnOnce(&mut LspWorkItemRecord)) -> Result<()> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("LSP work-item ledger lock poisoned"))?;
        let key = record_key(&self.job_id, item_id);
        let record = store
            .records
            .get_mut(&key)
            .ok_or_else(|| anyhow::anyhow!("LSP work item {key} is missing"))?;
        update(record);
        Ok(())
    }

    async fn maybe_flush(&self) -> Result<()> {
        let should_flush = {
            let mut last_flush = self
                .last_flush
                .lock()
                .map_err(|_| anyhow::anyhow!("LSP work-item flush clock lock poisoned"))?;
            if last_flush.elapsed() >= FLUSH_INTERVAL {
                *last_flush = Instant::now();
                true
            } else {
                false
            }
        };
        if should_flush {
            self.flush().await?;
        }
        Ok(())
    }

    pub(crate) async fn flush(&self) -> Result<()> {
        let _flush_guard = self.persist_lock.lock().await;
        let store = self
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("LSP work-item ledger lock poisoned"))?
            .clone();
        let repo_root = self.repo_root.clone();
        let job_id = self.job_id.clone();
        tokio::task::spawn_blocking(move || merge_and_write_store(&repo_root, &job_id, &store))
            .await
            .context("LSP work-item ledger writer task failed")??;
        Ok(())
    }

    #[cfg(test)]
    fn job_id(&self) -> &str {
        &self.job_id
    }

    #[cfg(test)]
    pub(crate) fn age_records_for_test(&self, age: Duration) {
        let age_ms = age.as_millis().try_into().unwrap_or(u64::MAX);
        let mut store = self.store.lock().unwrap();
        for record in store.records.values_mut() {
            record.created_at_ms = record.created_at_ms.saturating_sub(age_ms);
            record.updated_at_ms = record.updated_at_ms.saturating_sub(age_ms);
            record.started_at_ms = record
                .started_at_ms
                .map(|value| value.saturating_sub(age_ms));
            record.completed_at_ms = record
                .completed_at_ms
                .map(|value| value.saturating_sub(age_ms));
        }
    }
}

fn new_record(
    repo_root: &Path,
    job_id: &str,
    seed: &LspWorkItemSeed,
    work_identity: LspWorkIdentity,
    now: u64,
) -> LspWorkItemRecord {
    let node = &seed.node;
    let input_hash = work_identity.digest.clone();
    LspWorkItemRecord {
        schema_version: STORE_SCHEMA_VERSION,
        job_id: job_id.to_string(),
        item_id: seed.item_id,
        repo: repo_root.display().to_string(),
        root: node.id.root.clone(),
        file: node.id.file.display().to_string(),
        node_id: node.stable_id(),
        node_name: node.id.name.clone(),
        node_kind: node.id.kind.to_string(),
        input_hash,
        work_identity,
        recovery_disposition: LspRecoveryDisposition::New,
        recovery_source_job_id: None,
        integrity_digest: String::new(),
        requested_operations: seed.requested_operations.clone(),
        state: LspWorkItemState::Pending,
        attempt_count: seed.attempt_count,
        current_phase: None,
        last_phase: None,
        created_at_ms: now,
        updated_at_ms: now,
        started_at_ms: None,
        completed_at_ms: None,
        last_error: None,
        recovery: LspWorkItemRecovery::New,
        output_edges: Vec::new(),
        output_nodes: Vec::new(),
        produced_result_ids: BTreeSet::new(),
        observed_result_count: 0,
    }
}

fn canonical_operations_digest(requested_operations: &[String]) -> String {
    let mut operations = requested_operations.to_vec();
    operations.sort();
    operations.dedup();
    let mut hasher = blake3::Hasher::new();
    for operation in operations {
        hasher.update(operation.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex().to_string()
}

/// Per-file source line cache shared across a batch of position lookups.
///
/// `source_request_position` is invoked once per requested operation per node,
/// and a single file commonly holds many symbols, so without this cache a
/// K-symbol file is read from disk K times per batch (ledger identity build,
/// and again for each actual LSP request). Safe to share across concurrent
/// lookups: reads race harmlessly onto the same cached `Arc<Vec<String>>`.
#[derive(Default)]
pub(crate) struct SourceLineCache {
    lines_by_file: Mutex<HashMap<PathBuf, Arc<Vec<String>>>>,
}

impl SourceLineCache {
    fn lines(&self, path: &Path) -> Arc<Vec<String>> {
        if let Some(cached) = self
            .lines_by_file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(path)
        {
            return Arc::clone(cached);
        }
        let lines = std::fs::read_to_string(path)
            .map(|source| source.lines().map(str::to_owned).collect::<Vec<_>>())
            .unwrap_or_default();
        let lines = Arc::new(lines);
        self.lines_by_file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.to_path_buf(), Arc::clone(&lines));
        lines
    }
}

/// True if `byte` can continue an identifier (ASCII alnum or underscore).
///
/// Non-ASCII bytes (UTF-8 continuation bytes included) are treated as
/// boundaries; this only needs to reject accidental substring matches inside
/// a longer ASCII identifier, not fully tokenize the line.
fn is_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Finds `name` on `line` at an identifier boundary, skipping matches that are
/// a substring of a longer identifier (avoids matching inside e.g. `get_name`
/// when searching for `name`). Still line-local, so it does not distinguish a
/// real occurrence from one inside a same-line comment or string literal.
fn find_identifier_boundary(line: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let bytes = line.as_bytes();
    let mut search_start = 0;
    while let Some(offset) = line[search_start..].find(name) {
        let match_start = search_start + offset;
        let match_end = match_start + name.len();
        let before_ok = match_start == 0 || !is_identifier_continue(bytes[match_start - 1]);
        let after_ok = match_end == bytes.len() || !is_identifier_continue(bytes[match_end]);
        if before_ok && after_ok {
            return Some(match_start);
        }
        // Advance by one full char, not one byte: `match_start` is a valid
        // char boundary (from `str::find`), but a multi-byte character there
        // (e.g. a unicode identifier) would make `match_start + 1` land
        // mid-sequence and panic on the next slice.
        let matched_char_len = line[match_start..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
        search_start = match_start + matched_char_len;
        if search_start >= bytes.len() {
            break;
        }
    }
    None
}

fn request_anchor(repo_root: &Path, node: &Node, cache: &SourceLineCache) -> String {
    let relative_file = if node.id.file.is_absolute() {
        node.id
            .file
            .strip_prefix(repo_root)
            .unwrap_or(&node.id.file)
            .to_path_buf()
    } else {
        node.id.file.clone()
    };
    let (zero_based_line, zero_based_character) = source_request_position(repo_root, node, cache);
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{zero_based_line}\u{1f}{zero_based_character}",
        node.id.root,
        relative_file.to_string_lossy().replace('\\', "/"),
        node.language,
    )
}

pub(super) fn source_request_position(
    repo_root: &Path,
    node: &Node,
    cache: &SourceLineCache,
) -> (u32, u32) {
    let source_path = repo_root.join(&node.id.file);
    let zero_based_line = node.line_start.saturating_sub(1) as u32;
    let lines = cache.lines(&source_path);
    let zero_based_character = lines
        .get(zero_based_line as usize)
        .and_then(|line| {
            find_identifier_boundary(line, &node.id.name)
                .map(|byte| line[..byte].encode_utf16().count() as u32)
        })
        .unwrap_or(0);
    (zero_based_line, zero_based_character)
}

fn update_path_identity(hasher: &mut blake3::Hasher, path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect source snapshot path {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        hasher.update(b"symlink");
        let target = std::fs::read_link(path)
            .with_context(|| format!("failed to read source symlink {}", path.display()))?;
        hasher.update(target.to_string_lossy().as_bytes());
    } else if metadata.is_file() {
        hasher.update(b"file");
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read source snapshot file {}", path.display()))?;
        hasher.update(blake3::hash(&bytes).as_bytes());
    } else if metadata.is_dir() {
        hasher.update(b"directory");
        let mut entries = std::fs::read_dir(path)?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| {
                format!(
                    "failed to read source snapshot directory {}",
                    path.display()
                )
            })?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            hasher.update(entry.file_name().to_string_lossy().as_bytes());
            hasher.update(&[0]);
            update_path_identity(hasher, &entry.path())?;
            hasher.update(&[0]);
        }
    } else {
        hasher.update(b"missing");
    }
    Ok(())
}

fn non_git_source_snapshot(repo_root: &Path) -> Result<String> {
    fn visit(root: &Path, directory: &Path, rows: &mut Vec<PathBuf>) -> Result<()> {
        let mut entries = std::fs::read_dir(directory)
            .with_context(|| format!("failed to read source directory {}", directory.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap_or(&path);
            if relative == Path::new(".git")
                || relative.starts_with(".git/")
                || relative == Path::new(".oh/.cache")
                || relative.starts_with(".oh/.cache/")
            {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                visit(root, &path, rows)?;
            } else {
                rows.push(relative.to_path_buf());
            }
        }
        Ok(())
    }

    let mut rows = Vec::new();
    visit(repo_root, repo_root, &mut rows)?;
    rows.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rna-lsp-source-snapshot-non-git-v1");
    for relative in rows {
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update(&[0]);
        update_path_identity(&mut hasher, &repo_root.join(&relative))?;
        hasher.update(&[0]);
    }
    Ok(format!("content:{}", hasher.finalize().to_hex()))
}

/// Upper bound on how long the `git ls-files` enumeration in
/// `ignored_lsp_influence_identity` may run. This is a local filesystem
/// query, not a network or language-server call, so a few seconds is ample;
/// bounding it keeps a stuck or oversized Git invocation from hanging
/// recovery indefinitely.
const IGNORED_INPUT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Runs `command` to completion, killing it and returning an error if it has
/// not exited within `timeout`. Unlike `Command::output`, this never blocks
/// indefinitely on a hung child process.
///
/// Stdout/stderr are drained concurrently on dedicated threads for the same
/// reason `Command::output()` does it internally: a child that writes more
/// than the OS pipe buffer (a few tens of KB) will block on write() until
/// someone reads, so polling `try_wait()` without draining the pipes would
/// deadlock against exactly the timeout this function exists to enforce.
fn run_bounded(command: &mut Command, timeout: Duration) -> Result<std::process::Output> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn command")?;
    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
    let stdout_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().context("failed to poll spawned command")? {
            let stdout = stdout_reader.join().unwrap_or_default();
            let stderr = stderr_reader.join().unwrap_or_default();
            return Ok(std::process::Output {
                status,
                stdout,
                stderr,
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            anyhow::bail!("command timed out after {timeout:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Enumerates ignored files matching descriptor-declared influence patterns.
///
/// Runs `git ls-files` with `repo_root` as the working directory (not the
/// enclosing repository's workdir) so both pathspec resolution and returned
/// paths are scoped to `repo_root`: a nested startup root's identity is then
/// unaffected by ignored files elsewhere in the outer repository. Git ignore
/// rules still apply from the real repository root outward regardless of
/// `current_dir`, so `.gitignore` files above `repo_root` are still honored.
fn ignored_lsp_influence_identity(repo_root: &Path) -> Result<Option<String>> {
    let mut patterns = super::builtin_lsp_descriptors()
        .iter()
        .flat_map(|descriptor| descriptor.partition_influence_patterns())
        .collect::<Vec<_>>();
    patterns.sort_unstable();
    patterns.dedup();
    if patterns.is_empty() {
        return Ok(None);
    }

    let mut pathspecs = Vec::new();
    for pattern in &patterns {
        let normalized = pattern.replace('\\', "/");
        let pathspec = if normalized.contains('/') {
            format!(":(glob){normalized}")
        } else {
            format!(":(glob)**/{normalized}")
        };
        pathspecs.push(pathspec);
    }
    let mut command = Command::new("git");
    command
        .arg("-c")
        .arg("core.quotePath=false")
        .arg("ls-files")
        .arg("-z")
        .arg("--others")
        .arg("--ignored")
        .arg("--exclude-standard")
        .arg("--")
        .args(&pathspecs)
        .current_dir(repo_root);
    let output = match run_bounded(&mut command, IGNORED_INPUT_DISCOVERY_TIMEOUT) {
        Ok(output) if output.status.success() => output.stdout,
        Ok(output) => {
            tracing::warn!(
                repo_root = %repo_root.display(),
                status = ?output.status,
                stderr = %String::from_utf8_lossy(&output.stderr),
                "git ls-files exited non-zero enumerating ignored LSP inputs; falling back to full content snapshot"
            );
            return Ok(Some(format!(
                "full-fallback:{}",
                non_git_source_snapshot(repo_root)?
            )));
        }
        Err(error) => {
            tracing::warn!(
                repo_root = %repo_root.display(),
                %error,
                "failed to run git ls-files enumerating ignored LSP inputs; falling back to full content snapshot"
            );
            return Ok(Some(format!(
                "full-fallback:{}",
                non_git_source_snapshot(repo_root)?
            )));
        }
    };
    // Bounded: only descriptor-declared patterns are enumerated (never a bare
    // `--others --ignored` sweep of the whole ignored tree), and the result is
    // collapsed into one digest rather than an unbounded per-file list.
    let mut paths = String::from_utf8_lossy(&output)
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .filter(|path| {
            path != ".oh/.cache"
                && !path.starts_with(".oh/.cache/")
                && patterns
                    .iter()
                    .any(|pattern| super::partition_influence_pattern_matches(pattern, path))
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return Ok(None);
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rna-lsp-ignored-influences-v1");
    for relative in paths {
        hasher.update(relative.as_bytes());
        hasher.update(&[0]);
        update_path_identity(&mut hasher, &repo_root.join(&relative))?;
        hasher.update(&[0]);
    }
    Ok(Some(hasher.finalize().to_hex().to_string()))
}

/// Root-relative slash-separated path of `repo_root` within the repository
/// `workdir` that contains it, or `""` when they are the same directory.
fn root_relative_to_workdir(repo_root: &Path, workdir: &Path) -> String {
    repo_root
        .strip_prefix(workdir)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

/// Strips `root_relative` (a `root_relative_to_workdir` prefix) from a
/// workdir-relative Git status path, returning `None` for paths outside
/// `repo_root` so unrelated ancestor changes cannot affect its identity.
fn strip_to_repo_root(workdir_relative_path: &str, root_relative: &str) -> Option<String> {
    if root_relative.is_empty() {
        return Some(workdir_relative_path.to_string());
    }
    workdir_relative_path
        .strip_prefix(root_relative)
        .and_then(|rest| rest.strip_prefix('/'))
        .map(str::to_string)
}

fn source_snapshot_identity(repo_root: &Path) -> Result<String> {
    let repository = match git2::Repository::discover(repo_root) {
        Ok(repository) => repository,
        Err(_) => return non_git_source_snapshot(repo_root),
    };
    let workdir = repository
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("LSP source snapshot requires a non-bare Git repository"))?;
    let head = match repository.head().and_then(|head| head.peel_to_commit()) {
        Ok(head) => head,
        Err(_) => return non_git_source_snapshot(repo_root),
    };
    let tree = head.tree()?;
    // `repo_root` may be a subdirectory of the discovered repository (a
    // monorepo startup root). Scope both the tree and status identity to it,
    // so changes elsewhere in the repository do not invalidate this root's
    // recovery, and so `.oh/.cache/` exclusion matches nested roots too.
    let root_relative = root_relative_to_workdir(repo_root, workdir);
    let scoped_tree_id = if root_relative.is_empty() {
        tree.id().to_string()
    } else {
        match tree.get_path(Path::new(&root_relative)) {
            Ok(entry) => entry.id().to_string(),
            Err(_) => format!("missing:{root_relative}"),
        }
    };
    let ignored_influences = ignored_lsp_influence_identity(repo_root)?;
    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .include_unmodified(false)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);
    if !root_relative.is_empty() {
        options.pathspec(root_relative.as_str());
    }
    let mut changes = repository
        .statuses(Some(&mut options))?
        .iter()
        .filter_map(|entry| {
            entry
                .path()
                .ok()
                .and_then(|path| strip_to_repo_root(path, &root_relative))
                .map(|path| (path, entry.status().bits()))
        })
        .filter(|(path, _)| !path.starts_with(".oh/.cache/"))
        .collect::<Vec<_>>();
    changes.sort();
    if changes.is_empty() {
        return Ok(match ignored_influences {
            Some(influences) => format!("git-tree:{scoped_tree_id}:ignored:{influences}"),
            None => format!("git-tree:{scoped_tree_id}:clean"),
        });
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rna-lsp-source-snapshot-git-dirty-v2");
    hasher.update(scoped_tree_id.as_bytes());
    for (relative, status) in changes {
        hasher.update(relative.as_bytes());
        hasher.update(&[0]);
        hasher.update(&status.to_le_bytes());
        hasher.update(&[0]);
        let path = repo_root.join(&relative);
        if path.exists() || path.is_symlink() {
            update_path_identity(&mut hasher, &path)?;
        } else {
            hasher.update(b"deleted");
        }
        hasher.update(&[0]);
    }
    if let Some(influences) = ignored_influences {
        hasher.update(b"ignored-influences");
        hasher.update(&[0]);
        hasher.update(influences.as_bytes());
        hasher.update(&[0]);
    }
    Ok(format!("git-dirty:{}", hasher.finalize().to_hex()))
}

fn work_identity(
    repo_root: &Path,
    seed: &LspWorkItemSeed,
    source_snapshot: &str,
    cache: &SourceLineCache,
) -> LspWorkIdentity {
    let request_anchor = request_anchor(repo_root, &seed.node, cache);
    let operations_digest = canonical_operations_digest(&seed.requested_operations);
    let mut identity = LspWorkIdentity {
        schema_version: WORK_IDENTITY_SCHEMA_VERSION,
        source_snapshot: source_snapshot.to_string(),
        request_anchor,
        operations_digest,
        toolchain_contract: seed.toolchain_contract.clone(),
        planner_contract: PLANNER_CONTRACT_VERSION.to_string(),
        digest: String::new(),
    };
    identity.digest = work_identity_digest(&identity);
    identity
}

pub(crate) fn work_identity_digest(identity: &LspWorkIdentity) -> String {
    let mut hasher = blake3::Hasher::new();
    for component in [
        identity.schema_version.to_string(),
        identity.source_snapshot.clone(),
        identity.request_anchor.clone(),
        identity.operations_digest.clone(),
        identity.toolchain_contract.clone(),
        identity.planner_contract.clone(),
    ] {
        hasher.update(component.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex().to_string()
}

pub(crate) fn current_source_snapshot_identity(repo_root: &Path) -> Result<String> {
    source_snapshot_identity(repo_root)
}

pub(crate) fn current_request_anchor(
    repo_root: &Path,
    node: &Node,
    cache: &SourceLineCache,
) -> String {
    request_anchor(repo_root, node, cache)
}

pub(crate) const fn current_work_identity_schema_version() -> u32 {
    WORK_IDENTITY_SCHEMA_VERSION
}

pub(crate) fn current_planner_contract_version() -> &'static str {
    PLANNER_CONTRACT_VERSION
}

pub(crate) fn requested_operations_digest(requested_operations: &[String]) -> String {
    canonical_operations_digest(requested_operations)
}

#[cfg(test)]
pub(crate) fn build_work_identity(
    repo_root: &Path,
    node: &Node,
    requested_operations: &[String],
    toolchain_contract: &str,
) -> Result<LspWorkIdentity> {
    let source_snapshot = source_snapshot_identity(repo_root)?;
    let seed = LspWorkItemSeed {
        item_id: 0,
        node: node.clone(),
        requested_operations: requested_operations.to_vec(),
        attempt_count: 1,
        toolchain_contract: toolchain_contract.to_string(),
    };
    Ok(work_identity(
        repo_root,
        &seed,
        &source_snapshot,
        &SourceLineCache::default(),
    ))
}

fn identity_disposition(
    prior: &LspWorkIdentity,
    current: &LspWorkIdentity,
) -> LspRecoveryDisposition {
    if prior.schema_version != current.schema_version {
        LspRecoveryDisposition::RerunSchema
    } else if prior.source_snapshot != current.source_snapshot {
        LspRecoveryDisposition::RerunSourceSnapshot
    } else if prior.request_anchor != current.request_anchor {
        LspRecoveryDisposition::RerunRequestAnchor
    } else if prior.operations_digest != current.operations_digest {
        LspRecoveryDisposition::RerunOperations
    } else if prior.toolchain_contract != current.toolchain_contract {
        LspRecoveryDisposition::RerunToolchain
    } else if prior.planner_contract != current.planner_contract {
        // A `PLANNER_CONTRACT_VERSION` bump changes `digest` (it is one of the
        // hashed components) even though every other named field still
        // matches. Without this explicit check that would fall through to
        // the tamper branch below and mislabel a routine contract bump as
        // tampering. Rerun instead; the digest mismatch is expected.
        LspRecoveryDisposition::RerunPlannerContract
    } else if prior.digest != current.digest {
        LspRecoveryDisposition::RejectedTampered
    } else {
        LspRecoveryDisposition::CarriedExact
    }
}

fn record_integrity_digest(record: &LspWorkItemRecord) -> Result<String> {
    let mut canonical = record.clone();
    canonical.integrity_digest.clear();
    let bytes = serde_json::to_vec(&canonical)
        .context("failed to serialize canonical LSP work-item record")?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

struct RecoverySelection {
    job_id: String,
    records: Vec<LspWorkItemRecord>,
    duplicate_keys: BTreeSet<(String, String)>,
}

fn select_recovery_job(
    store: &LspWorkItemStore,
    seeds: &[LspWorkItemSeed],
) -> Option<RecoverySelection> {
    let seed_node_ids = seeds
        .iter()
        .map(|seed| seed.node.stable_id())
        .collect::<BTreeSet<_>>();
    let mut jobs: BTreeMap<&str, Vec<&LspWorkItemRecord>> = BTreeMap::new();
    for record in store.records.values() {
        jobs.entry(&record.job_id).or_default().push(record);
    }
    let eligible = jobs
        .into_iter()
        .filter_map(|(job_id, records)| {
            let has_unfinished = records.iter().any(|record| {
                matches!(
                    record.state,
                    LspWorkItemState::Pending
                        | LspWorkItemState::InFlight
                        | LspWorkItemState::Failed
                        | LspWorkItemState::Exhausted
                )
            });
            if !has_unfinished {
                return None;
            }
            let overlap = records
                .iter()
                .filter(|record| seed_node_ids.contains(&record.node_id))
                .count();
            if overlap == 0 {
                return None;
            }
            let updated_at = records
                .iter()
                .map(|record| record.updated_at_ms)
                .max()
                .unwrap_or_default();
            Some(((updated_at, overlap), job_id.to_string(), records))
        })
        .collect::<Vec<_>>();
    let mut jobs_by_key: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for (_, job_id, records) in &eligible {
        for record in records {
            if seed_node_ids.contains(&record.node_id) {
                jobs_by_key
                    .entry((
                        record.node_id.clone(),
                        canonical_operations_digest(&record.requested_operations),
                    ))
                    .or_default()
                    .insert(job_id.clone());
            }
        }
    }
    let duplicate_keys = jobs_by_key
        .into_iter()
        .filter_map(|(key, job_ids)| (job_ids.len() > 1).then_some(key))
        .collect();
    eligible
        .into_iter()
        .max_by_key(|(rank, _, _)| *rank)
        .map(|(_, job_id, records)| RecoverySelection {
            job_id,
            records: records.into_iter().cloned().collect(),
            duplicate_keys,
        })
}

pub fn load_queue_snapshots(
    repo_root: &Path,
    limit: usize,
) -> Result<Vec<LspWorkItemQueueSnapshot>> {
    let store = load_store(repo_root)?;
    Ok(snapshots_from_store(&store, limit))
}

/// Load durable work-item evidence updated during the current enrichment run.
///
/// The completeness report consumes these records immediately after a full
/// scan. Filtering at the persistence seam prevents an older successful job
/// for the same path from being mistaken for evidence produced by this scan.
pub(crate) fn load_records_since(
    repo_root: &Path,
    updated_since_ms: u64,
) -> Result<Vec<LspWorkItemRecord>> {
    let store = load_store(repo_root)?;
    let mut records = store
        .records
        .into_values()
        .filter(|record| record.updated_at_ms >= updated_since_ms)
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then_with(|| left.job_id.cmp(&right.job_id))
            .then_with(|| left.item_id.cmp(&right.item_id))
    });
    Ok(records)
}

pub(crate) fn load_all_records(repo_root: &Path) -> Result<Vec<LspWorkItemRecord>> {
    load_records_since(repo_root, 0)
}

/// Remove carried work for files that the structural-cache impact plan will
/// execute again. This operates only on the injected mutable copy; the base
/// archive remains immutable. Returning the removed producer IDs lets callers
/// audit that unchanged files were not accidentally invalidated.
pub(crate) fn purge_records_for_paths(
    repo_root: &Path,
    paths: &BTreeSet<String>,
) -> Result<Vec<String>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let _file_lock = WorkItemFileLock::acquire(repo_root);
    let mut store = load_store(repo_root)?;
    let removed = store
        .records
        .iter()
        .filter(|(_, record)| paths.contains(&record.file))
        .map(|(record_id, _)| record_id.clone())
        .collect::<Vec<_>>();
    store
        .records
        .retain(|_, record| !paths.contains(&record.file));
    write_store(repo_root, &store)?;
    Ok(removed)
}

pub fn load_queue_snapshots_since(
    repo_root: &Path,
    updated_since_ms: u64,
) -> Result<Vec<LspWorkItemQueueSnapshot>> {
    let store = load_store(repo_root)?;
    Ok(snapshots_from_store(&store, usize::MAX)
        .into_iter()
        .filter(|snapshot| snapshot.updated_at_ms >= updated_since_ms)
        .collect())
}

pub fn render_queue_snapshots_markdown(repo_root: &Path, limit: usize) -> String {
    let snapshots = match load_queue_snapshots(repo_root, limit) {
        Ok(snapshots) => snapshots,
        Err(error) => {
            tracing::warn!(
                %error,
                "Could not render persisted LSP work-item queue snapshots"
            );
            return String::new();
        }
    };
    if snapshots.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "\n\n## LSP Pass 1 Work Queues\n\n{} persisted queue(s)",
        snapshots.len()
    );
    for snapshot in snapshots {
        out.push_str("\n\n- ");
        out.push_str(&snapshot.render());
    }
    out
}

fn snapshots_from_store(store: &LspWorkItemStore, limit: usize) -> Vec<LspWorkItemQueueSnapshot> {
    let mut by_job: BTreeMap<&str, Vec<&LspWorkItemRecord>> = BTreeMap::new();
    for record in store.records.values() {
        by_job.entry(&record.job_id).or_default().push(record);
    }
    let now = unix_millis();
    let mut snapshots = by_job
        .into_iter()
        .map(|(job_id, records)| {
            let mut snapshot = LspWorkItemQueueSnapshot {
                schema_version: STORE_SCHEMA_VERSION,
                job_id: job_id.to_string(),
                repo: records
                    .first()
                    .map(|record| record.repo.clone())
                    .unwrap_or_default(),
                total: records.len(),
                ..Default::default()
            };
            let mut roots = BTreeSet::new();
            let mut oldest = Vec::new();
            for record in records {
                roots.insert(record.root.clone());
                snapshot.updated_at_ms = snapshot.updated_at_ms.max(record.updated_at_ms);
                *snapshot
                    .recovery_dispositions
                    .entry(record.recovery_disposition.as_str().to_string())
                    .or_insert(0) += 1;
                if record.recovery != LspWorkItemRecovery::New {
                    snapshot.resumed += 1;
                }
                if record.recovery == LspWorkItemRecovery::Retried {
                    snapshot.retried += 1;
                }
                match record.state {
                    LspWorkItemState::Pending => snapshot.pending += 1,
                    LspWorkItemState::InFlight => {
                        snapshot.in_flight += 1;
                        let phase = record.current_phase.as_deref().unwrap_or("unknown");
                        *snapshot.phase_counts.entry(phase.to_string()).or_insert(0) += 1;
                        oldest.push((
                            record.started_at_ms.unwrap_or(record.updated_at_ms),
                            format!(
                                "file={} node={} node_id={} phase={} attempt={} age_ms={}",
                                record.file,
                                record.node_name,
                                record.node_id,
                                phase,
                                record.attempt_count,
                                now.saturating_sub(
                                    record.started_at_ms.unwrap_or(record.updated_at_ms)
                                )
                            ),
                        ));
                    }
                    LspWorkItemState::Completed => snapshot.completed += 1,
                    LspWorkItemState::Failed => snapshot.failed += 1,
                    LspWorkItemState::Skipped => snapshot.skipped += 1,
                    LspWorkItemState::Exhausted => {
                        snapshot.exhausted += 1;
                        if snapshot.exhausted_items.len() < OLDEST_SAMPLE_LIMIT {
                            snapshot.exhausted_items.push(format!(
                                "file={} node={} attempt={} error={}",
                                record.file,
                                record.node_name,
                                record.attempt_count,
                                record.last_error.as_deref().unwrap_or("unknown")
                            ));
                        }
                    }
                }
            }
            oldest.sort_by_key(|(started_at, _)| *started_at);
            snapshot.oldest_in_flight = oldest
                .into_iter()
                .take(OLDEST_SAMPLE_LIMIT)
                .map(|(_, rendered)| rendered)
                .collect();
            snapshot.roots = roots.into_iter().collect();
            snapshot
        })
        .collect::<Vec<_>>();
    snapshots.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.updated_at_ms));
    snapshots.truncate(limit);
    snapshots
}

fn retain_recent_jobs(store: &mut LspWorkItemStore, terminal_limit: usize) {
    let mut jobs: BTreeMap<String, (u64, bool)> = BTreeMap::new();
    for record in store.records.values() {
        jobs.entry(record.job_id.clone())
            .and_modify(|(updated_at, active)| {
                *updated_at = (*updated_at).max(record.updated_at_ms);
                *active |= matches!(
                    record.state,
                    LspWorkItemState::Pending | LspWorkItemState::InFlight
                );
            })
            .or_insert((
                record.updated_at_ms,
                matches!(
                    record.state,
                    LspWorkItemState::Pending | LspWorkItemState::InFlight
                ),
            ));
    }
    let (mut active_jobs, mut terminal_jobs): (Vec<_>, Vec<_>) =
        jobs.into_iter().partition(|(_, (_, active))| *active);
    active_jobs.sort_by_key(|(_, (updated_at, _))| std::cmp::Reverse(*updated_at));
    terminal_jobs.sort_by_key(|(_, (updated_at, _))| std::cmp::Reverse(*updated_at));
    let mut retained = active_jobs
        .into_iter()
        .take(MAX_RETAINED_ACTIVE_JOBS)
        .map(|(job_id, _)| job_id)
        .collect::<BTreeSet<_>>();
    retained.extend(
        terminal_jobs
            .into_iter()
            .take(terminal_limit)
            .map(|(job_id, _)| job_id),
    );
    store
        .records
        .retain(|_, record| retained.contains(&record.job_id));
}

fn store_lock(repo_root: &Path) -> Result<Arc<tokio::sync::Mutex<()>>> {
    let locks = STORE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .map_err(|_| anyhow::anyhow!("LSP work-item store-lock registry poisoned"))?;
    Ok(Arc::clone(
        locks
            .entry(repo_root.to_path_buf())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
    ))
}

fn merge_and_write_store(
    repo_root: &Path,
    job_id: &str,
    current_job: &LspWorkItemStore,
) -> Result<()> {
    let _file_lock = WorkItemFileLock::acquire(repo_root);
    let mut persisted = load_store(repo_root)?;
    persisted
        .records
        .retain(|_, record| record.job_id != job_id);
    persisted.records.extend(current_job.records.clone());
    persisted.schema_version = STORE_SCHEMA_VERSION;
    retain_recent_jobs(&mut persisted, MAX_RETAINED_TERMINAL_JOBS);
    write_store(repo_root, &persisted)
}

fn work_item_lock_is_owned_by(path: &Path, owner: &str) -> bool {
    std::fs::read_to_string(path)
        .map(|content| content.trim() == owner)
        .unwrap_or(false)
}

fn work_item_lock_owner_is_dead(path: &Path) -> bool {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return work_item_lock_initialization_grace_elapsed(path),
    };
    let Ok(pid) = content.trim().parse::<u32>() else {
        return work_item_lock_initialization_grace_elapsed(path);
    };
    if pid == std::process::id() {
        return false;
    }
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| !status.success())
        .unwrap_or(true)
}

fn work_item_lock_initialization_grace_elapsed(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age >= LOCK_OWNER_INITIALIZATION_GRACE)
}

fn load_store(repo_root: &Path) -> Result<LspWorkItemStore> {
    let path = store_path(repo_root);
    if !path.exists() {
        return Ok(LspWorkItemStore::default());
    }
    let bytes = std::fs::read(&path)
        .with_context(|| format!("failed to read LSP work-item ledger {}", path.display()))?;
    let mut store: LspWorkItemStore = match serde_json::from_slice(&bytes) {
        Ok(store) => store,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "Ignoring malformed LSP work-item ledger"
            );
            return Ok(LspWorkItemStore::default());
        }
    };
    if store.schema_version > STORE_SCHEMA_VERSION {
        anyhow::bail!(
            "LSP work-item ledger schema {} is newer than supported schema {}",
            store.schema_version,
            STORE_SCHEMA_VERSION
        );
    }
    if store.schema_version < STORE_SCHEMA_VERSION
        || store
            .records
            .values()
            .any(|record| record.schema_version < STORE_SCHEMA_VERSION)
    {
        tracing::warn!(
            path = %path.display(),
            stored_schema = store.schema_version,
            current_schema = STORE_SCHEMA_VERSION,
            "Replaying LSP work items because persisted evidence predates stable work identities"
        );
        for record in store.records.values_mut() {
            record.recovery_disposition = LspRecoveryDisposition::RerunSchema;
            record.state = LspWorkItemState::Failed;
            record.output_edges.clear();
            record.output_nodes.clear();
            record.produced_result_ids.clear();
            record.observed_result_count = 0;
            record.last_error = Some(format!(
                "persisted work-item schema predates current schema {STORE_SCHEMA_VERSION}"
            ));
        }
        store.schema_version = STORE_SCHEMA_VERSION;
        return Ok(store);
    }
    // `integrity_digest` is an unkeyed BLAKE3 hash: it detects accidental
    // corruption and partial writes to `.oh/.cache`, not tampering by a
    // process that can already write there, since such a writer can
    // recompute a matching digest for any content it substitutes.
    for record in store.records.values_mut() {
        let observed = record_integrity_digest(record)?;
        if record.integrity_digest != observed {
            tracing::warn!(
                job_id = %record.job_id,
                item_id = record.item_id,
                "Rejecting tampered LSP work-item record and scheduling exact work again"
            );
            record.recovery_disposition = LspRecoveryDisposition::RejectedTampered;
            record.state = LspWorkItemState::Failed;
            record.output_edges.clear();
            record.output_nodes.clear();
            record.produced_result_ids.clear();
            record.observed_result_count = 0;
            record.last_error = Some("persisted work-item integrity digest mismatch".to_string());
        }
    }
    store.schema_version = STORE_SCHEMA_VERSION;
    Ok(store)
}

fn write_store(repo_root: &Path, store: &LspWorkItemStore) -> Result<()> {
    let path = store_path(repo_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create LSP work-item cache {}", parent.display())
        })?;
    }
    let mut sealed = store.clone();
    sealed.schema_version = STORE_SCHEMA_VERSION;
    for record in sealed.records.values_mut() {
        record.schema_version = STORE_SCHEMA_VERSION;
        record.integrity_digest = record_integrity_digest(record)?;
    }
    let bytes =
        serde_json::to_vec_pretty(&sealed).context("failed to serialize LSP work-item ledger")?;
    let temp_path = path.with_extension(format!(
        "json.tmp-{}-{}",
        std::process::id(),
        JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&temp_path, bytes).with_context(|| {
        format!(
            "failed to write LSP work-item ledger temp file {}",
            temp_path.display()
        )
    })?;
    std::fs::rename(&temp_path, &path).with_context(|| {
        format!(
            "failed to rename LSP work-item ledger temp file {} to {}",
            temp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn store_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".oh").join(".cache").join(STORE_FILE)
}

fn record_key(job_id: &str, item_id: usize) -> String {
    format!("{job_id}:{item_id}")
}

fn new_job_id() -> String {
    format!(
        "lsp-pass1-{}-{}-{}",
        unix_millis(),
        std::process::id(),
        JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use crate::extract::lsp::policy::LspQueryProfile;
    use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

    use super::*;

    fn node(name: &str) -> Node {
        Node {
            id: NodeId {
                root: "fixture".to_string(),
                file: PathBuf::from(format!("src/{name}.rs")),
                name: name.to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 1,
            signature: format!("fn {name}()"),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    #[test]
    fn request_position_is_source_derived_and_uses_lsp_utf16_columns() {
        let repo = tempfile::tempdir().unwrap();
        let mut source_node = node("item");
        source_node
            .metadata
            .insert("name_col".to_string(), "999-derived-and-wrong".to_string());
        source_node.signature = "derived signature without the identifier".to_string();
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(repo.path().join("src/item.rs"), "💡 fn item() {}\n").unwrap();

        assert_eq!(
            source_request_position(repo.path(), &source_node, &SourceLineCache::default()),
            (0, 6)
        );
    }

    #[test]
    fn find_identifier_boundary_skips_rejected_multibyte_match_without_panicking() {
        // "π" at byte 1 is a substring of the longer identifier "aπ" and must
        // be rejected; the retry step used to advance by one raw byte, which
        // landed inside "π"'s 2-byte UTF-8 encoding and panicked on the next
        // slice. No other "π" exists on this line, so this must return None,
        // not panic.
        assert_eq!(find_identifier_boundary("aπ = 1", "π"), None);
    }

    #[test]
    fn find_identifier_boundary_finds_valid_match_after_rejected_multibyte_match() {
        // First "π" (byte 1) is inside the longer identifier "xπfoo" and is
        // rejected; the standalone "π" later on the line must still be found
        // once the retry correctly steps past the rejected multi-byte match.
        let line = "xπfoo π bar";
        let found = find_identifier_boundary(line, "π").unwrap();
        assert_eq!(&line[found..found + "π".len()], "π");
        assert_eq!(line.as_bytes()[found - 1], b' ');
    }

    #[test]
    fn run_bounded_drains_output_larger_than_pipe_buffer_without_deadlock() {
        // A child writing more than the OS pipe buffer (tens of KB) blocks on
        // write() until someone reads. `run_bounded` only polled `try_wait`
        // without draining the pipes, so this used to hang until the timeout
        // fired instead of completing almost immediately.
        let mut command = Command::new("sh");
        command.arg("-c").arg("head -c 200000 /dev/zero");
        let started = Instant::now();
        let output = run_bounded(&mut command, Duration::from_secs(5)).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 200_000);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn run_bounded_times_out_and_kills_hung_child() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let started = Instant::now();
        let result = run_bounded(&mut command, Duration::from_millis(200));
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    fn seeds(count: usize) -> Vec<LspWorkItemSeed> {
        (0..count)
            .map(|item_id| LspWorkItemSeed {
                item_id,
                node: node(&format!("item_{item_id}")),
                requested_operations: vec!["textDocument/references".to_string()],
                attempt_count: 1,
                toolchain_contract: "fixture-toolchain-v1".to_string(),
            })
            .collect()
    }

    fn edge(from: &str, to: &str) -> Edge {
        Edge {
            from: node(from).id,
            to: node(to).id,
            kind: EdgeKind::Calls,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        }
    }

    #[tokio::test]
    async fn mixed_work_item_state_round_trips_and_reconstructs_snapshot() {
        let repo = tempfile::tempdir().unwrap();
        let ledger = LspWorkItemLedger::begin(repo.path(), &seeds(5))
            .await
            .unwrap();

        ledger.mark_phase(0, "sending_did_open").await.unwrap();
        ledger.mark_completed(1).await.unwrap();
        ledger
            .mark_failed(2, "language server disconnected")
            .await
            .unwrap();
        ledger.mark_skipped(3, "unsupported node").await.unwrap();
        ledger.flush().await.unwrap();

        let snapshots = load_queue_snapshots(repo.path(), 1).unwrap();
        let snapshot = &snapshots[0];
        assert_eq!(snapshot.job_id, ledger.job_id());
        assert_eq!(snapshot.total, 5);
        assert_eq!(snapshot.pending, 1);
        assert_eq!(snapshot.in_flight, 1);
        assert_eq!(snapshot.completed, 1);
        assert_eq!(snapshot.failed, 1);
        assert_eq!(snapshot.skipped, 1);
        assert_eq!(snapshot.phase_counts["sending_did_open"], 1);
        assert!(snapshot.oldest_in_flight[0].contains("node=item_0"));
        assert!(snapshot.render().contains("skipped=1"));
        assert_eq!(
            ledger.unmatched_required_count(),
            0,
            "an intentional runtime skip is not an unmatched required recovery record"
        );
        assert!(
            render_queue_snapshots_markdown(repo.path(), 1).contains("## LSP Pass 1 Work Queues")
        );
    }

    #[test]
    fn missing_and_older_schema_load_safely() {
        let missing = tempfile::tempdir().unwrap();
        assert!(load_queue_snapshots(missing.path(), 1).unwrap().is_empty());

        let older = tempfile::tempdir().unwrap();
        let path = store_path(older.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"records":{"legacy:0":{"job_id":"legacy","item_id":0}}}"#,
        )
        .unwrap();
        let snapshots = load_queue_snapshots(older.path(), 1).unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].failed, 1);
        assert_eq!(snapshots[0].recovery_dispositions["rerun_schema"], 1);
    }

    #[test]
    fn produced_result_ids_keep_the_existing_json_array_contract() {
        let record: LspWorkItemRecord = serde_json::from_value(serde_json::json!({
            "produced_result_ids": ["result-z", "result-a", "result-z"]
        }))
        .unwrap();
        assert_eq!(
            record.produced_result_ids,
            BTreeSet::from(["result-a".to_string(), "result-z".to_string()])
        );

        let serialized = serde_json::to_value(record).unwrap();
        assert_eq!(
            serialized["produced_result_ids"],
            serde_json::json!(["result-a", "result-z"])
        );
    }

    #[tokio::test]
    async fn retention_keeps_only_the_bounded_recent_job_set() {
        let repo = tempfile::tempdir().unwrap();
        for _ in 0..(MAX_RETAINED_TERMINAL_JOBS + 2) {
            let ledger = LspWorkItemLedger::begin(repo.path(), &seeds(1))
                .await
                .unwrap();
            ledger.mark_completed(0).await.unwrap();
            ledger.flush().await.unwrap();
        }

        assert_eq!(
            load_queue_snapshots(repo.path(), MAX_RETAINED_TERMINAL_JOBS + 10)
                .unwrap()
                .len(),
            MAX_RETAINED_TERMINAL_JOBS
        );
    }

    #[tokio::test]
    async fn concurrent_ledgers_merge_without_losing_jobs() {
        let repo = tempfile::tempdir().unwrap();
        let first =
            LspWorkItemLedger::begin_with_job_id(repo.path(), "first".to_string(), &seeds(1))
                .await
                .unwrap();
        let second =
            LspWorkItemLedger::begin_with_job_id(repo.path(), "second".to_string(), &seeds(1))
                .await
                .unwrap();
        first.mark_phase(0, "first_phase").await.unwrap();
        second.mark_phase(0, "second_phase").await.unwrap();

        let (first_flush, second_flush) = tokio::join!(first.flush(), second.flush());
        first_flush.unwrap();
        second_flush.unwrap();

        let snapshots = load_queue_snapshots(repo.path(), 10).unwrap();
        assert_eq!(snapshots.len(), 2);
        assert!(snapshots.iter().any(|snapshot| snapshot.job_id == "first"));
        assert!(snapshots.iter().any(|snapshot| snapshot.job_id == "second"));
    }

    #[tokio::test]
    async fn interrupted_queue_resumes_only_eligible_items_once() {
        let repo = tempfile::tempdir().unwrap();
        let initial_seeds = seeds(6);
        let ledger = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "resume-job".to_string(),
            &initial_seeds,
        )
        .await
        .unwrap();
        let recovered_edge = edge("item_0", "item_1");
        ledger
            .mark_completed_with_output(0, std::slice::from_ref(&recovered_edge), &[], 1)
            .await
            .unwrap();
        ledger.mark_skipped(1, "not supported").await.unwrap();
        ledger.mark_phase(2, "requesting_references").await.unwrap();
        ledger.mark_failed(3, "server disconnected").await.unwrap();
        ledger.mark_failed(4, "temporary timeout").await.unwrap();
        ledger.mark_completed(5).await.unwrap();
        {
            let mut store = ledger.store.lock().unwrap();
            store.records.get_mut("resume-job:3").unwrap().attempt_count = MAX_ATTEMPTS;
        }
        ledger.flush().await.unwrap();

        let resumed = LspWorkItemLedger::begin(repo.path(), &seeds(7))
            .await
            .unwrap();

        assert_eq!(resumed.job_id(), "resume-job");
        assert!(!resumed.should_run(0));
        assert!(!resumed.should_run(1));
        assert!(resumed.should_run(2));
        assert!(!resumed.should_run(3));
        assert!(resumed.should_run(4));
        assert!(!resumed.should_run(5));
        assert!(resumed.should_run(6));
        let (edges, nodes) = resumed.recovered_output();
        assert!(nodes.is_empty());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].stable_id(), recovered_edge.stable_id());

        let snapshot = &load_queue_snapshots(repo.path(), 1).unwrap()[0];
        assert_eq!(snapshot.total, 7);
        assert_eq!(snapshot.pending, 3);
        assert_eq!(snapshot.completed, 2);
        assert_eq!(snapshot.skipped, 1);
        assert_eq!(snapshot.exhausted, 1);
        assert_eq!(snapshot.resumed, 6);
        assert_eq!(snapshot.retried, 2);
        assert!(snapshot.exhausted_items[0].contains("retry with narrower scope"));
        assert!(snapshot.render().contains("exhausted_items=[file="));

        let store = resumed.store.lock().unwrap();
        assert_eq!(store.records["resume-job:2"].attempt_count, 2);
        assert_eq!(store.records["resume-job:3"].attempt_count, MAX_ATTEMPTS);
        assert_eq!(store.records["resume-job:4"].attempt_count, 2);
        assert!(
            store.records["resume-job:3"]
                .last_error
                .as_deref()
                .unwrap()
                .contains("retry budget exhausted")
        );
    }

    #[tokio::test]
    async fn schema_v1_completed_items_are_replayed_conservatively() {
        let repo = tempfile::tempdir().unwrap();
        let ledger = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "schema-v1-job".to_string(),
            &seeds(2),
        )
        .await
        .unwrap();
        ledger.mark_completed(0).await.unwrap();
        ledger.mark_phase(1, "requesting_references").await.unwrap();
        let mut legacy_store = load_store(repo.path()).unwrap();
        legacy_store.schema_version = 1;
        for record in legacy_store.records.values_mut() {
            record.schema_version = 1;
            record.input_hash.clear();
            record.output_edges.clear();
            record.output_nodes.clear();
        }
        std::fs::write(
            store_path(repo.path()),
            serde_json::to_vec_pretty(&legacy_store).unwrap(),
        )
        .unwrap();

        let resumed = LspWorkItemLedger::begin(repo.path(), &seeds(2))
            .await
            .unwrap();

        assert_eq!(resumed.job_id(), "schema-v1-job");
        assert!(resumed.should_run(0));
        assert!(resumed.should_run(1));
        let (edges, nodes) = resumed.recovered_output();
        assert!(edges.is_empty());
        assert!(nodes.is_empty());
        let store = resumed.store.lock().unwrap();
        assert!(
            store.records.values().all(|record| {
                record.recovery_disposition == LspRecoveryDisposition::RerunSchema
            })
        );
    }

    #[tokio::test]
    async fn changed_cross_file_config_snapshot_replays_instead_of_carrying_stale_output() {
        let repo = tempfile::tempdir().unwrap();
        let initial = seeds(2);
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(repo.path().join("src/item_0.rs"), "fn item_0() {}\n").unwrap();
        std::fs::write(
            repo.path().join("pyproject.toml"),
            "[tool.fixture]\nvalue = 1\n",
        )
        .unwrap();
        // Commit both files first so `source_snapshot_identity` takes the Git
        // snapshot path (a clean git-tree identity) rather than
        // `non_git_source_snapshot`, which hashes every file under the root
        // regardless of which one changed and so would pass this assertion
        // for any edit, not specifically a cross-file `pyproject.toml` change.
        let repository = git2::Repository::init(repo.path()).unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(Path::new("src/item_0.rs")).unwrap();
        index.add_path(Path::new("pyproject.toml")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repository.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("RNA test", "rna@example.com").unwrap();
        repository
            .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap();
        drop(tree);
        let ledger = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "changed-input-job".to_string(),
            &initial,
        )
        .await
        .unwrap();
        ledger
            .mark_completed_with_output(0, &[edge("item_0", "item_1")], &[], 1)
            .await
            .unwrap();
        ledger.mark_phase(1, "requesting_references").await.unwrap();
        ledger.flush().await.unwrap();

        std::fs::write(
            repo.path().join("pyproject.toml"),
            "[tool.fixture]\nvalue = 2\n",
        )
        .unwrap();
        let changed = seeds(2);
        let resumed = LspWorkItemLedger::begin(repo.path(), &changed)
            .await
            .unwrap();

        assert_eq!(resumed.job_id(), "changed-input-job");
        assert!(resumed.should_run(0));
        assert!(resumed.should_run(1));
        assert!(resumed.recovered_output().0.is_empty());
        let snapshot = &load_queue_snapshots(repo.path(), 1).unwrap()[0];
        assert_eq!(snapshot.total, changed.len());
        let store = resumed.store.lock().unwrap();
        assert_eq!(
            store.records["changed-input-job:0"].recovery_disposition,
            LspRecoveryDisposition::RerunSourceSnapshot
        );
    }

    #[test]
    fn unborn_git_repository_uses_content_snapshot() {
        let repo = tempfile::tempdir().unwrap();
        git2::Repository::init(repo.path()).unwrap();
        std::fs::write(repo.path().join("fixture.py"), "def fixture(): pass\n").unwrap();

        let before = source_snapshot_identity(repo.path()).unwrap();
        std::fs::write(repo.path().join("fixture.py"), "def fixture(): return 1\n").unwrap();
        let after = source_snapshot_identity(repo.path()).unwrap();

        assert!(before.starts_with("content:"));
        assert_ne!(before, after);
    }

    #[test]
    fn clean_git_snapshot_is_bound_to_the_tree_not_commit_metadata() {
        let repo = tempfile::tempdir().unwrap();
        let repository = git2::Repository::init(repo.path()).unwrap();
        std::fs::write(repo.path().join("fixture.py"), "def fixture(): return 1\n").unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(Path::new("fixture.py")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repository.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("RNA test", "rna@example.com").unwrap();
        let first_id = repository
            .commit(Some("HEAD"), &signature, &signature, "first", &tree, &[])
            .unwrap();
        drop(tree);

        let before = source_snapshot_identity(repo.path()).unwrap();
        let first = repository.find_commit(first_id).unwrap();
        let tree = repository.find_tree(first.tree_id()).unwrap();
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "metadata-only commit",
                &tree,
                &[&first],
            )
            .unwrap();
        drop(tree);
        drop(first);
        let after = source_snapshot_identity(repo.path()).unwrap();

        assert_eq!(before, after);
        assert!(before.starts_with("git-tree:"));
    }

    #[test]
    fn ignored_descriptor_influence_changes_git_source_snapshot() {
        let repo = tempfile::tempdir().unwrap();
        let repository = git2::Repository::init(repo.path()).unwrap();
        std::fs::write(
            repo.path().join("fixture.c"),
            "int fixture(void) { return 1; }\n",
        )
        .unwrap();
        std::fs::write(repo.path().join(".gitignore"), "build/\n").unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(Path::new("fixture.c")).unwrap();
        index.add_path(Path::new(".gitignore")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repository.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("RNA test", "rna@example.com").unwrap();
        repository
            .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap();
        drop(tree);

        std::fs::create_dir_all(repo.path().join("build")).unwrap();
        let compile_commands = repo.path().join("build/compile_commands.json");
        std::fs::write(&compile_commands, "[{\"command\":\"cc -O0 fixture.c\"}]\n").unwrap();
        let before = source_snapshot_identity(repo.path()).unwrap();

        std::fs::write(&compile_commands, "[{\"command\":\"cc -O2 fixture.c\"}]\n").unwrap();
        let after = source_snapshot_identity(repo.path()).unwrap();
        assert_ne!(before, after);
        assert!(after.contains(":ignored:"));

        std::fs::write(repo.path().join("build/unrelated.bin"), b"ignored noise").unwrap();
        assert_eq!(after, source_snapshot_identity(repo.path()).unwrap());
    }

    #[tokio::test]
    async fn reconstructed_node_representation_keeps_exact_source_work_identity() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(repo.path().join("src/item_0.rs"), "fn item_0() {}\n").unwrap();
        let initial = seeds(2);
        let ledger = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "reconstructed-node-job".to_string(),
            &initial,
        )
        .await
        .unwrap();
        ledger.mark_completed(0).await.unwrap();
        ledger.flush().await.unwrap();

        let mut reconstructed = seeds(2);
        reconstructed[0].node.line_end = 99;
        reconstructed[0].node.signature = "derived signature drift".to_string();
        reconstructed[0].node.body = "derived body drift".to_string();
        reconstructed[0]
            .node
            .metadata
            .insert("derived".to_string(), "changed".to_string());
        let resumed = LspWorkItemLedger::begin(repo.path(), &reconstructed)
            .await
            .unwrap();

        assert!(!resumed.should_run(0));
        assert!(resumed.should_run(1));
        let store = resumed.store.lock().unwrap();
        assert_eq!(
            store.records["reconstructed-node-job:0"].recovery_disposition,
            LspRecoveryDisposition::CarriedExact
        );
    }

    #[tokio::test]
    async fn changed_request_anchor_replays_with_explainable_disposition() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(repo.path().join("src/item_0.rs"), "fn item_0() {}\n").unwrap();
        let initial = seeds(2);
        let ledger = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "changed-anchor-job".to_string(),
            &initial,
        )
        .await
        .unwrap();
        ledger.mark_completed(0).await.unwrap();
        ledger.flush().await.unwrap();

        let mut changed = seeds(2);
        changed[0].node.line_start = 2;
        let resumed = LspWorkItemLedger::begin(repo.path(), &changed)
            .await
            .unwrap();

        assert!(resumed.should_run(0));
        let store = resumed.store.lock().unwrap();
        assert_eq!(
            store.records["changed-anchor-job:0"].recovery_disposition,
            LspRecoveryDisposition::RerunRequestAnchor
        );
    }

    #[tokio::test]
    async fn changed_operations_replay_with_explainable_disposition() {
        let repo = tempfile::tempdir().unwrap();
        let initial = seeds(2);
        let ledger = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "changed-operations-job".to_string(),
            &initial,
        )
        .await
        .unwrap();
        ledger.mark_completed(0).await.unwrap();
        ledger.flush().await.unwrap();

        let mut changed = seeds(2);
        changed[0].requested_operations = vec!["definitions".to_string()];
        let resumed = LspWorkItemLedger::begin(repo.path(), &changed)
            .await
            .unwrap();

        assert!(resumed.should_run(0));
        let store = resumed.store.lock().unwrap();
        assert_eq!(
            store.records["changed-operations-job:0"].recovery_disposition,
            LspRecoveryDisposition::RerunOperations
        );
    }

    #[tokio::test]
    async fn changed_toolchain_contract_replays_with_explainable_disposition() {
        let repo = tempfile::tempdir().unwrap();
        let initial = seeds(2);
        let ledger = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "changed-toolchain-job".to_string(),
            &initial,
        )
        .await
        .unwrap();
        ledger.mark_completed(0).await.unwrap();
        ledger.flush().await.unwrap();

        let mut changed = seeds(2);
        changed[0].toolchain_contract = "fixture-toolchain-v2".to_string();
        let resumed = LspWorkItemLedger::begin(repo.path(), &changed)
            .await
            .unwrap();

        assert!(resumed.should_run(0));
        let store = resumed.store.lock().unwrap();
        assert_eq!(
            store.records["changed-toolchain-job:0"].recovery_disposition,
            LspRecoveryDisposition::RerunToolchain
        );
    }

    #[test]
    fn planner_contract_bump_reruns_instead_of_tampering() {
        let mut prior = LspWorkIdentity {
            schema_version: WORK_IDENTITY_SCHEMA_VERSION,
            source_snapshot: "snapshot".to_string(),
            request_anchor: "anchor".to_string(),
            operations_digest: "operations".to_string(),
            toolchain_contract: "toolchain".to_string(),
            planner_contract: "lsp-pass1-work-planner-v0".to_string(),
            digest: String::new(),
        };
        prior.digest = work_identity_digest(&prior);
        let mut current = prior.clone();
        current.planner_contract = PLANNER_CONTRACT_VERSION.to_string();
        current.digest = work_identity_digest(&current);

        assert_ne!(prior.digest, current.digest);
        assert_eq!(
            identity_disposition(&prior, &current),
            LspRecoveryDisposition::RerunPlannerContract
        );
    }

    #[tokio::test]
    async fn tampered_completed_output_is_rejected_and_rerun() {
        let repo = tempfile::tempdir().unwrap();
        let initial = seeds(1);
        let ledger = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "tampered-output-job".to_string(),
            &initial,
        )
        .await
        .unwrap();
        ledger.mark_completed(0).await.unwrap();
        ledger.flush().await.unwrap();

        let path = store_path(repo.path());
        let mut stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        stored["records"]["tampered-output-job:0"]["observed_result_count"] =
            serde_json::json!(999);
        std::fs::write(&path, serde_json::to_vec_pretty(&stored).unwrap()).unwrap();

        let resumed = LspWorkItemLedger::begin(repo.path(), &seeds(1))
            .await
            .unwrap();
        assert!(resumed.should_run(0));
        assert!(resumed.recovered_output().0.is_empty());
        let store = resumed.store.lock().unwrap();
        assert_eq!(
            store.records["tampered-output-job:0"].recovery_disposition,
            LspRecoveryDisposition::RejectedTampered
        );
    }

    #[tokio::test]
    async fn duplicate_retained_identity_is_rejected_and_rerun_once() {
        let repo = tempfile::tempdir().unwrap();
        let initial = seeds(1);
        let ledger = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "duplicate-identity-job".to_string(),
            &initial,
        )
        .await
        .unwrap();
        ledger.mark_completed(0).await.unwrap();
        {
            let mut store = ledger.store.lock().unwrap();
            let mut duplicate = store.records["duplicate-identity-job:0"].clone();
            duplicate.item_id = 1;
            duplicate.state = LspWorkItemState::Pending;
            duplicate.integrity_digest.clear();
            store
                .records
                .insert("duplicate-identity-job:1".to_string(), duplicate);
        }
        ledger.flush().await.unwrap();

        let resumed = LspWorkItemLedger::begin(repo.path(), &seeds(1))
            .await
            .unwrap();
        assert!(resumed.should_run(0));
        assert_eq!(resumed.runnable_item_ids.len(), 1);
        let store = resumed.store.lock().unwrap();
        assert_eq!(
            store.records["duplicate-identity-job:0"].recovery_disposition,
            LspRecoveryDisposition::RejectedDuplicate
        );
    }

    #[tokio::test]
    async fn duplicate_identity_across_interrupted_jobs_is_rejected() {
        let repo = tempfile::tempdir().unwrap();
        let initial = seeds(2);
        let ledger = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "older-interrupted-job".to_string(),
            &initial,
        )
        .await
        .unwrap();
        ledger.mark_completed(0).await.unwrap();
        {
            let mut store = ledger.store.lock().unwrap();
            let older_records = store.records.values().cloned().collect::<Vec<_>>();
            for mut duplicate in older_records {
                duplicate.job_id = "newer-interrupted-job".to_string();
                duplicate.updated_at_ms += 1;
                duplicate.integrity_digest.clear();
                store.records.insert(
                    record_key("newer-interrupted-job", duplicate.item_id),
                    duplicate,
                );
            }
        }
        ledger.flush().await.unwrap();

        let resumed = LspWorkItemLedger::begin(repo.path(), &initial)
            .await
            .unwrap();

        assert!(resumed.should_run(0));
        assert!(resumed.should_run(1));
        let store = resumed.store.lock().unwrap();
        assert_eq!(
            store.records["newer-interrupted-job:0"].recovery_disposition,
            LspRecoveryDisposition::RejectedDuplicate
        );
    }

    #[tokio::test]
    async fn unmatched_required_work_remains_skipped_during_recovery() {
        let repo = tempfile::tempdir().unwrap();
        let mut initial = seeds(2);
        initial[0].requested_operations = vec!["document_symbols".to_string()];
        let ledger = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "removed-work-job".to_string(),
            &initial,
        )
        .await
        .unwrap();
        ledger.mark_completed(0).await.unwrap();
        ledger.mark_phase(1, "requesting_references").await.unwrap();
        ledger.flush().await.unwrap();

        let mut remaining = initial[1].clone();
        remaining.item_id = 0;
        let resumed = LspWorkItemLedger::begin(repo.path(), &[remaining])
            .await
            .unwrap();

        assert_eq!(resumed.job_id(), "removed-work-job");
        assert!(resumed.should_run(0));
        let snapshot = &load_queue_snapshots(repo.path(), 1).unwrap()[0];
        assert_eq!(snapshot.total, 2);
        assert_eq!(snapshot.pending, 1);
        assert_eq!(snapshot.skipped, 1);
        assert_eq!(resumed.unmatched_required_count(), 1);
        let store = resumed.store.lock().unwrap();
        let skipped = store
            .records
            .values()
            .find(|record| record.state == LspWorkItemState::Skipped)
            .expect("removed required work must remain visible as skipped");
        assert_eq!(skipped.requested_operations, ["document_symbols"]);
        assert!(
            skipped
                .last_error
                .as_deref()
                .unwrap()
                .contains("no longer present")
        );
        assert_eq!(
            store
                .records
                .get("removed-work-job:1")
                .map(|record| record.item_id),
            Some(1),
            "carried audit records must keep their persisted key and item ID aligned"
        );
    }

    #[tokio::test]
    async fn operation_removed_from_current_node_is_retired_not_failed() {
        let repo = tempfile::tempdir().unwrap();
        let mut fixture_seeds = seeds(1);
        let node = fixture_seeds.remove(0).node;
        let initial = vec![
            LspWorkItemSeed {
                item_id: 0,
                node: node.clone(),
                requested_operations: vec!["document_links".to_string()],
                attempt_count: 1,
                toolchain_contract: "fixture-toolchain-v1".to_string(),
            },
            LspWorkItemSeed {
                item_id: 1,
                node: node.clone(),
                requested_operations: vec!["references".to_string()],
                attempt_count: 1,
                toolchain_contract: "fixture-toolchain-v1".to_string(),
            },
        ];
        let ledger = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "changed-operation-job".to_string(),
            &initial,
        )
        .await
        .unwrap();
        ledger.mark_completed(0).await.unwrap();
        ledger.mark_phase(1, "requesting_references").await.unwrap();
        ledger.flush().await.unwrap();

        let current = [LspWorkItemSeed {
            item_id: 0,
            node,
            requested_operations: vec!["references".to_string()],
            attempt_count: 1,
            toolchain_contract: "fixture-toolchain-v1".to_string(),
        }];
        let resumed = LspWorkItemLedger::begin(repo.path(), &current)
            .await
            .unwrap();

        assert_eq!(resumed.job_id(), "changed-operation-job");
        assert!(resumed.should_run(0));
        assert_eq!(resumed.unmatched_required_count(), 0);
        let records = load_all_records(repo.path()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].requested_operations, ["references"]);
    }

    #[tokio::test]
    async fn persisted_lsp_output_is_not_new_work_while_source_work_retries() {
        let repo = tempfile::tempdir().unwrap();
        let source = node("source");
        let initial = [LspWorkItemSeed {
            item_id: 0,
            node: source.clone(),
            requested_operations: vec!["references".to_string()],
            attempt_count: 1,
            toolchain_contract: "fixture-toolchain-v1".to_string(),
        }];
        let ledger = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "persisted-lsp-output-job".to_string(),
            &initial,
        )
        .await
        .unwrap();
        ledger.mark_phase(0, "requesting_references").await.unwrap();
        ledger.flush().await.unwrap();

        let mut virtual_function = node("callee@lsp");
        virtual_function.source = ExtractionSource::Lsp;
        virtual_function
            .metadata
            .insert("virtual".to_string(), "true".to_string());
        let mut document_symbol_proof = node("proof");
        document_symbol_proof.id.kind = NodeKind::Other("lsp_document_symbol".to_string());
        document_symbol_proof.source = ExtractionSource::Lsp;
        let persisted_nodes = repo.path().join(".oh/.cache/persisted-nodes.json");
        std::fs::create_dir_all(persisted_nodes.parent().unwrap()).unwrap();
        std::fs::write(
            &persisted_nodes,
            serde_json::to_vec(&[source.clone(), virtual_function, document_symbol_proof]).unwrap(),
        )
        .unwrap();
        let reopened: Vec<Node> =
            serde_json::from_slice(&std::fs::read(persisted_nodes).unwrap()).unwrap();
        let profile = LspQueryProfile::new("rust", "fixture");
        let seeds = reopened
            .into_iter()
            .filter(|node| profile.accepts_declaration(node))
            .enumerate()
            .map(|(item_id, node)| LspWorkItemSeed {
                item_id,
                node,
                requested_operations: vec!["references".to_string()],
                attempt_count: 1,
                toolchain_contract: "fixture-toolchain-v1".to_string(),
            })
            .collect::<Vec<_>>();

        assert_eq!(seeds.len(), 1, "persisted LSP output must schedule no work");
        assert_eq!(seeds[0].node.stable_id(), source.stable_id());
        let resumed = LspWorkItemLedger::begin(repo.path(), &seeds).await.unwrap();
        assert!(resumed.should_run(0));
        let records = load_all_records(repo.path()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].recovery, LspWorkItemRecovery::Retried);
        assert!(
            records
                .iter()
                .all(|record| record.recovery != LspWorkItemRecovery::New)
        );
    }

    #[tokio::test]
    async fn interrupted_jobs_are_bounded_separately_from_terminal_history() {
        let repo = tempfile::tempdir().unwrap();
        for index in 0..(MAX_RETAINED_ACTIVE_JOBS + 2) {
            LspWorkItemLedger::begin_with_job_id(
                repo.path(),
                format!("interrupted-{index:03}"),
                &seeds(1),
            )
            .await
            .unwrap();
        }

        let snapshots = load_queue_snapshots(repo.path(), usize::MAX).unwrap();
        assert_eq!(snapshots.len(), MAX_RETAINED_ACTIVE_JOBS);
        assert!(snapshots.iter().all(|snapshot| snapshot.pending == 1));
    }

    #[test]
    fn stale_cross_process_lock_is_reclaimed() {
        let repo = tempfile::tempdir().unwrap();
        let path = store_path(repo.path()).with_extension("lock");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "999999999").unwrap();

        let lock = WorkItemFileLock::acquire(repo.path());
        assert!(lock.acquired);
        assert!(work_item_lock_is_owned_by(&path, &lock.owner));
        drop(lock);
        assert!(!path.exists());
    }

    #[test]
    fn job_id_process_helper() {
        let Ok(output) = std::env::var("RNA_JOB_ID_HELPER_OUTPUT") else {
            return;
        };
        std::fs::write(
            output,
            format!("{}\n{}\n", std::process::id(), new_job_id()),
        )
        .unwrap();
    }

    #[test]
    fn job_ids_are_unique_and_process_scoped_across_processes() {
        let repo = tempfile::tempdir().unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut children = Vec::new();
        let mut outputs = Vec::new();
        for index in 0..4 {
            let output = repo.path().join(format!("job-id-{index}"));
            let child = std::process::Command::new(&executable)
                .arg("--exact")
                .arg("extract::lsp::work_items::tests::job_id_process_helper")
                .arg("--nocapture")
                .env("RNA_JOB_ID_HELPER_OUTPUT", &output)
                .spawn()
                .unwrap();
            children.push(child);
            outputs.push(output);
        }

        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }

        let mut job_ids = std::collections::BTreeSet::new();
        for output in outputs {
            let content = std::fs::read_to_string(output).unwrap();
            let mut lines = content.lines();
            let pid = lines.next().unwrap();
            let job_id = lines.next().unwrap();
            assert!(job_id.contains(&format!("-{pid}-")), "{job_id}");
            assert!(job_ids.insert(job_id.to_string()), "duplicate {job_id}");
        }
        assert_eq!(job_ids.len(), 4);
    }

    #[test]
    fn lock_initialization_process_helper() {
        let Ok(repo) = std::env::var("RNA_LOCK_HELPER_REPO") else {
            return;
        };
        let ready = PathBuf::from(std::env::var("RNA_LOCK_HELPER_READY").unwrap());
        let release = PathBuf::from(std::env::var("RNA_LOCK_HELPER_RELEASE").unwrap());
        let lock_path = store_path(Path::new(&repo)).with_extension("lock");
        std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .unwrap();
        std::fs::write(&ready, b"ready").unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while !release.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(release.exists(), "parent never released helper");
        writeln!(file, "{}", std::process::id()).unwrap();
        file.flush().unwrap();
        thread::sleep(Duration::from_millis(100));
        std::fs::remove_file(lock_path).unwrap();
    }

    #[test]
    fn concurrent_process_does_not_reclaim_initializing_owner_file() {
        let repo = tempfile::tempdir().unwrap();
        let ready = repo.path().join("owner-ready");
        let release = repo.path().join("owner-release");
        let executable = std::env::current_exe().unwrap();
        let mut child = std::process::Command::new(executable)
            .arg("--exact")
            .arg("extract::lsp::work_items::tests::lock_initialization_process_helper")
            .arg("--nocapture")
            .env("RNA_LOCK_HELPER_REPO", repo.path())
            .env("RNA_LOCK_HELPER_READY", &ready)
            .env("RNA_LOCK_HELPER_RELEASE", &release)
            .spawn()
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists(), "helper did not create empty owner file");

        let repo_path = repo.path().to_path_buf();
        let contender = thread::spawn(move || WorkItemFileLock::acquire(&repo_path));
        thread::sleep(Duration::from_millis(100));
        assert!(
            !contender.is_finished(),
            "contender reclaimed a lock whose owner was still initializing"
        );

        std::fs::write(&release, b"release").unwrap();
        let child_status = child.wait().unwrap();
        if !child_status.success() {
            let _ = std::fs::remove_file(store_path(repo.path()).with_extension("lock"));
        }
        assert!(child_status.success());
        let lock = contender.join().unwrap();
        assert!(lock.acquired);
        assert!(work_item_lock_is_owned_by(&lock.path, &lock.owner));
        drop(lock);
    }
}
