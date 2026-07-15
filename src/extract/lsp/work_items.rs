use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::graph::Node;

const STORE_SCHEMA_VERSION: u32 = 1;
const STORE_FILE: &str = "lsp_pass1_work_items.json";
const MAX_RETAINED_ACTIVE_JOBS: usize = 32;
const MAX_RETAINED_TERMINAL_JOBS: usize = 16;
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
const OLDEST_SAMPLE_LIMIT: usize = 5;

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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    #[serde(default)]
    pub phase_counts: BTreeMap<String, usize>,
    #[serde(default)]
    pub oldest_in_flight: Vec<String>,
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
        format!(
            "job={} total={} pending={} in_flight={} completed={} failed={} skipped={} phases=[{}] oldest=[{}]",
            self.job_id,
            self.total,
            self.pending,
            self.in_flight,
            self.completed,
            self.failed,
            self.skipped,
            phases,
            oldest
        )
    }
}

pub(crate) struct LspWorkItemLedger {
    repo_root: PathBuf,
    job_id: String,
    store: Mutex<LspWorkItemStore>,
    last_flush: Mutex<Instant>,
    persist_lock: Arc<tokio::sync::Mutex<()>>,
}

impl LspWorkItemLedger {
    pub(crate) async fn begin(repo_root: &Path, seeds: &[LspWorkItemSeed]) -> Result<Arc<Self>> {
        let job_id = new_job_id();
        Self::begin_with_job_id(repo_root, job_id, seeds).await
    }

    async fn begin_with_job_id(
        repo_root: &Path,
        job_id: String,
        seeds: &[LspWorkItemSeed],
    ) -> Result<Arc<Self>> {
        let mut store = LspWorkItemStore::default();
        let now = unix_millis();
        for seed in seeds {
            let node = &seed.node;
            let record = LspWorkItemRecord {
                schema_version: STORE_SCHEMA_VERSION,
                job_id: job_id.clone(),
                item_id: seed.item_id,
                repo: repo_root.display().to_string(),
                root: node.id.root.clone(),
                file: node.id.file.display().to_string(),
                node_id: node.stable_id(),
                node_name: node.id.name.clone(),
                node_kind: node.id.kind.to_string(),
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
            };
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
        });
        ledger.flush().await?;
        Ok(ledger)
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

    pub(crate) async fn mark_completed(&self, item_id: usize) -> Result<()> {
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
            record.last_error = error;
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
    let mut persisted = load_store(repo_root)?;
    persisted
        .records
        .retain(|_, record| record.job_id != job_id);
    persisted.records.extend(current_job.records.clone());
    persisted.schema_version = STORE_SCHEMA_VERSION;
    retain_recent_jobs(&mut persisted, MAX_RETAINED_TERMINAL_JOBS);
    write_store(repo_root, &persisted)
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
        "lsp-pass1-{}-{}",
        unix_millis(),
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

    use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

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
}
