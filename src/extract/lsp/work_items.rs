use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::graph::{Edge, Node};

const STORE_SCHEMA_VERSION: u32 = 2;
const STORE_FILE: &str = "lsp_pass1_work_items.json";
const MAX_RETAINED_ACTIVE_JOBS: usize = 32;
const MAX_RETAINED_TERMINAL_JOBS: usize = 16;
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
const OLDEST_SAMPLE_LIMIT: usize = 5;
const LOCK_OWNER_INITIALIZATION_GRACE: Duration = Duration::from_secs(2);
const MAX_ATTEMPTS: u32 = 3;

static JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static STORE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

#[derive(Debug, Clone)]
pub(crate) struct LspWorkItemSeed {
    pub item_id: usize,
    pub node: Node,
    pub requested_operations: Vec<String>,
    pub attempt_count: u32,
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
            "job={} total={} pending={} in_flight={} completed={} failed={} skipped={} exhausted={} resumed={} retried={} phases=[{}] oldest=[{}] exhausted_items=[{}]",
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
        let Some((job_id, prior_records)) = select_recovery_job(&persisted, seeds) else {
            return Self::begin_with_job_id(repo_root, new_job_id(), seeds).await;
        };
        let now = unix_millis();
        let current_item_keys = seeds
            .iter()
            .map(|seed| (seed.node.stable_id(), seed.requested_operations.clone()))
            .collect::<BTreeSet<_>>();
        let mut prior_by_key = prior_records
            .into_iter()
            .map(|record| {
                (
                    recovery_key(
                        &record.node_id,
                        &record.input_hash,
                        &record.requested_operations,
                    ),
                    record,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut store = LspWorkItemStore::default();
        let mut runnable_item_ids = BTreeSet::new();
        let mut recovered_edges = Vec::new();
        let mut recovered_nodes = Vec::new();

        for seed in seeds {
            let node_id = seed.node.stable_id();
            let input_hash = node_input_hash(&seed.node);
            let key = recovery_key(&node_id, &input_hash, &seed.requested_operations);
            let prior_record = prior_by_key.remove(&key);
            let is_new = prior_record.is_none();
            let mut record =
                prior_record.unwrap_or_else(|| new_record(repo_root, &job_id, seed, now));
            record.item_id = seed.item_id;
            record.job_id = job_id.clone();
            record.repo = repo_root.display().to_string();
            record.root = seed.node.id.root.clone();
            record.file = seed.node.id.file.display().to_string();
            record.node_id = node_id;
            record.node_name = seed.node.id.name.clone();
            record.node_kind = seed.node.id.kind.to_string();
            record.input_hash = input_hash;
            record.requested_operations = seed.requested_operations.clone();
            record.schema_version = STORE_SCHEMA_VERSION;
            record.updated_at_ms = now;

            if is_new {
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

        let remaining_records = prior_by_key.into_values().filter(|record| {
            !current_item_keys
                .contains(&(record.node_id.clone(), record.requested_operations.clone()))
        });
        for (next_item_id, mut record) in (seeds.len()..).zip(remaining_records) {
            record.schema_version = STORE_SCHEMA_VERSION;
            record.item_id = next_item_id;
            record.state = LspWorkItemState::Skipped;
            record.recovery = LspWorkItemRecovery::CarriedSkipped;
            record.last_phase = record.current_phase.take().or(record.last_phase.take());
            record.completed_at_ms = Some(now);
            record.updated_at_ms = now;
            record.output_edges.clear();
            record.output_nodes.clear();
            record.last_error = Some(
                "persisted work item is no longer present in the enrichable node set; skipped"
                    .to_string(),
            );
            store
                .records
                .insert(record_key(&job_id, next_item_id), record);
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
        for seed in seeds {
            let record = new_record(repo_root, &job_id, seed, now);
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
        self.mark_completed_with_output(item_id, &[], &[]).await
    }

    pub(crate) async fn mark_completed_with_output(
        &self,
        item_id: usize,
        edges: &[Edge],
        nodes: &[Node],
    ) -> Result<()> {
        self.update(item_id, |record| {
            record.output_edges = edges.to_vec();
            record.output_nodes = nodes.to_vec();
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
    now: u64,
) -> LspWorkItemRecord {
    let node = &seed.node;
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
        input_hash: node_input_hash(node),
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
    }
}

fn node_input_hash(node: &Node) -> String {
    let mut hasher = blake3::Hasher::new();
    for value in [
        node.stable_id(),
        node.language.clone(),
        node.line_start.to_string(),
        node.line_end.to_string(),
        node.signature.clone(),
        node.body.clone(),
    ] {
        hasher.update(value.as_bytes());
        hasher.update(&[0]);
    }
    for (key, value) in &node.metadata {
        hasher.update(key.as_bytes());
        hasher.update(&[0]);
        hasher.update(value.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex().to_string()
}

fn recovery_key(node_id: &str, input_hash: &str, requested_operations: &[String]) -> String {
    let mut operations = requested_operations.to_vec();
    operations.sort();
    format!(
        "{node_id}\u{1f}{input_hash}\u{1f}{}",
        operations.join("\u{1f}")
    )
}

fn select_recovery_job(
    store: &LspWorkItemStore,
    seeds: &[LspWorkItemSeed],
) -> Option<(String, Vec<LspWorkItemRecord>)> {
    let seed_keys = seeds
        .iter()
        .map(|seed| {
            recovery_key(
                &seed.node.stable_id(),
                &node_input_hash(&seed.node),
                &seed.requested_operations,
            )
        })
        .collect::<BTreeSet<_>>();
    let mut jobs: BTreeMap<&str, Vec<&LspWorkItemRecord>> = BTreeMap::new();
    for record in store.records.values() {
        jobs.entry(&record.job_id).or_default().push(record);
    }
    jobs.into_iter()
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
                .filter(|record| {
                    seed_keys.contains(&recovery_key(
                        &record.node_id,
                        &record.input_hash,
                        &record.requested_operations,
                    ))
                })
                .count();
            if overlap == 0 {
                return None;
            }
            let updated_at = records
                .iter()
                .map(|record| record.updated_at_ms)
                .max()
                .unwrap_or_default();
            Some((
                (updated_at, overlap),
                job_id.to_string(),
                records.into_iter().cloned().collect::<Vec<_>>(),
            ))
        })
        .max_by_key(|(rank, _, _)| *rank)
        .map(|(_, job_id, records)| (job_id, records))
}

pub fn load_queue_snapshots(
    repo_root: &Path,
    limit: usize,
) -> Result<Vec<LspWorkItemQueueSnapshot>> {
    let store = load_store(repo_root)?;
    Ok(snapshots_from_store(&store, limit))
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
    store.schema_version = STORE_SCHEMA_VERSION;
    for record in store.records.values_mut() {
        if record.schema_version == 0 {
            record.schema_version = STORE_SCHEMA_VERSION;
        }
    }
    Ok(store)
}

fn write_store(repo_root: &Path, store: &LspWorkItemStore) -> Result<()> {
    let path = store_path(repo_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create LSP work-item cache {}", parent.display())
        })?;
    }
    let bytes =
        serde_json::to_vec_pretty(store).context("failed to serialize LSP work-item ledger")?;
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
    use std::collections::BTreeMap;
    use std::path::PathBuf;

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

    fn seeds(count: usize) -> Vec<LspWorkItemSeed> {
        (0..count)
            .map(|item_id| LspWorkItemSeed {
                item_id,
                node: node(&format!("item_{item_id}")),
                requested_operations: vec!["textDocument/references".to_string()],
                attempt_count: 1,
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
        assert_eq!(snapshots[0].job_id, "legacy");
        assert_eq!(snapshots[0].pending, 1);
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
            .mark_completed_with_output(0, std::slice::from_ref(&recovered_edge), &[])
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
        {
            let mut store = ledger.store.lock().unwrap();
            store.schema_version = 1;
            for record in store.records.values_mut() {
                record.schema_version = 1;
                record.input_hash.clear();
                record.output_edges.clear();
                record.output_nodes.clear();
            }
        }
        ledger.flush().await.unwrap();

        let resumed = LspWorkItemLedger::begin(repo.path(), &seeds(2))
            .await
            .unwrap();

        assert_ne!(resumed.job_id(), "schema-v1-job");
        assert!(resumed.should_run(0));
        assert!(resumed.should_run(1));
        let (edges, nodes) = resumed.recovered_output();
        assert!(edges.is_empty());
        assert!(nodes.is_empty());
    }

    #[tokio::test]
    async fn changed_node_input_replays_instead_of_carrying_stale_output() {
        let repo = tempfile::tempdir().unwrap();
        let initial = seeds(2);
        let ledger = LspWorkItemLedger::begin_with_job_id(
            repo.path(),
            "changed-input-job".to_string(),
            &initial,
        )
        .await
        .unwrap();
        ledger
            .mark_completed_with_output(0, &[edge("item_0", "item_1")], &[])
            .await
            .unwrap();
        ledger.mark_phase(1, "requesting_references").await.unwrap();
        ledger.flush().await.unwrap();

        let mut changed = seeds(2);
        changed[0].node.body = "let changed = true;".to_string();
        let resumed = LspWorkItemLedger::begin(repo.path(), &changed)
            .await
            .unwrap();

        assert_eq!(resumed.job_id(), "changed-input-job");
        assert!(resumed.should_run(0));
        assert!(resumed.should_run(1));
        assert!(resumed.recovered_output().0.is_empty());
        let snapshot = &load_queue_snapshots(repo.path(), 1).unwrap()[0];
        assert_eq!(snapshot.total, changed.len());
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
