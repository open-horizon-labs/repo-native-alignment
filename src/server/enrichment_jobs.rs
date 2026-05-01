//! Durable enrichment job ledger and scoped scan controls.
//!
//! This is intentionally a small control-plane primitive, not a worker scheduler.
//! It gives enrichment work invocation identity, restart-visible lifecycle history,
//! and one active-job key per repo/capability/scope so foreground/background work
//! cannot emit indistinguishable progress for the same capability.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::graph::store::SCHEMA_VERSION;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspEnrichmentMode {
    Run,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingEnrichmentMode {
    Run,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanEnrichmentOptions {
    pub lsp: LspEnrichmentMode,
    pub embeddings: EmbeddingEnrichmentMode,
}

impl ScanEnrichmentOptions {
    pub const fn all() -> Self {
        Self {
            lsp: LspEnrichmentMode::Run,
            embeddings: EmbeddingEnrichmentMode::Run,
        }
    }

    pub const fn extract_only() -> Self {
        Self {
            lsp: LspEnrichmentMode::Skip,
            embeddings: EmbeddingEnrichmentMode::Skip,
        }
    }

    pub fn without_lsp(mut self) -> Self {
        self.lsp = LspEnrichmentMode::Skip;
        self
    }

    pub fn without_embeddings(mut self) -> Self {
        self.embeddings = EmbeddingEnrichmentMode::Skip;
        self
    }

    pub const fn runs_lsp(self) -> bool {
        matches!(self.lsp, LspEnrichmentMode::Run)
    }

    pub const fn runs_embeddings(self) -> bool {
        matches!(self.embeddings, EmbeddingEnrichmentMode::Run)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentCapability {
    ExtractedGraph,
    Embeddings,
    CallReferences,
}

impl EnrichmentCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExtractedGraph => "extracted_graph",
            Self::Embeddings => "embeddings",
            Self::CallReferences => "call_references",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum EnrichmentScope {
    Repo,
    Root(String),
    ChangedFiles,
    Explicit(String),
}

impl EnrichmentScope {
    pub fn stable_key(&self) -> String {
        match self {
            Self::Repo => "repo".to_string(),
            Self::Root(root) => format!("root:{root}"),
            Self::ChangedFiles => "changed_files".to_string(),
            Self::Explicit(value) => format!("explicit:{value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentTrigger {
    Startup,
    ForegroundScan,
    BackgroundScan,
    IncrementalRefresh,
    Explicit,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentJobState {
    Queued,
    Running,
    Persisting,
    Completed,
    Failed,
    Cancelled,
    Superseded,
}

impl EnrichmentJobState {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Superseded
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnrichmentCounters {
    pub current: usize,
    pub total: Option<usize>,
    pub node_count: Option<usize>,
    pub edge_count: Option<usize>,
}

impl EnrichmentCounters {
    fn empty() -> Self {
        Self {
            current: 0,
            total: None,
            node_count: None,
            edge_count: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnrichmentJobRecord {
    pub job_id: String,
    pub repo: String,
    pub root: Option<String>,
    pub capability: EnrichmentCapability,
    pub scope: EnrichmentScope,
    pub trigger: EnrichmentTrigger,
    pub state: EnrichmentJobState,
    pub phase: Option<String>,
    pub counters: EnrichmentCounters,
    pub created_at: u64,
    pub updated_at: u64,
    pub completed_at: Option<u64>,
    pub failure: Option<String>,
    pub superseded_by: Option<String>,
    pub schema_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnrichmentJobEvent {
    pub job_id: String,
    pub timestamp: u64,
    pub state: EnrichmentJobState,
    pub phase: Option<String>,
    pub message: Option<String>,
    pub counters: EnrichmentCounters,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct EnrichmentJobStore {
    pub jobs: Vec<EnrichmentJobRecord>,
    pub events: Vec<EnrichmentJobEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStart {
    Started(Box<EnrichmentJobRecord>),
    Joined { existing_job_id: String },
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct JobKey {
    repo: String,
    capability: EnrichmentCapability,
    scope: String,
}

struct JobUpdate {
    state: EnrichmentJobState,
    phase: Option<String>,
    counters: Option<EnrichmentCounters>,
    failure: Option<String>,
    superseded_by: Option<String>,
}

#[derive(Debug, Default)]
pub struct EnrichmentJobLedger {
    active: Mutex<HashMap<JobKey, String>>,
    io_lock: Mutex<()>,
}

impl EnrichmentJobLedger {
    pub fn begin_job(
        &self,
        repo_root: &Path,
        capability: EnrichmentCapability,
        scope: EnrichmentScope,
        trigger: EnrichmentTrigger,
        root: Option<String>,
    ) -> JobStart {
        let repo = normalize_repo(repo_root);
        let key = JobKey {
            repo: repo.clone(),
            capability,
            scope: scope.stable_key(),
        };


        let now = unix_now();
        let job = EnrichmentJobRecord {
            job_id: format!("{}-{}", capability.as_str(), uuid::Uuid::new_v4()),
            repo,
            root,
            capability,
            scope,
            trigger,
            state: EnrichmentJobState::Queued,
            phase: Some("created".to_string()),
            counters: EnrichmentCounters::empty(),
            created_at: now,
            updated_at: now,
            completed_at: None,
            failure: None,
            superseded_by: None,
            schema_version: SCHEMA_VERSION,
        };

        {
            let _guard = self.io_lock.lock().unwrap();
            let _file_lock = JobFileLock::acquire(repo_root);
            let mut store = read_store(repo_root);
            let mut active = self.active.lock().unwrap();

            if let Some(existing_job_id) = active.get(&key).cloned() {
                if store_job_is_active(&store, &existing_job_id) {
                    store.events.push(EnrichmentJobEvent {
                        job_id: existing_job_id.clone(),
                        timestamp: now,
                        state: EnrichmentJobState::Running,
                        phase: Some("joined".to_string()),
                        message: Some("overlapping request joined existing active job".to_string()),
                        counters: EnrichmentCounters::empty(),
                    });
                    write_store(repo_root, &store);
                    return JobStart::Joined { existing_job_id };
                }
                active.remove(&key);
            }

            let mut superseded_events = Vec::new();
            for existing in store.jobs.iter_mut().filter(|existing| {
                existing.schema_version == SCHEMA_VERSION
                    && !existing.state.is_terminal()
                    && existing.repo == job.repo
                    && existing.capability == job.capability
                    && existing.scope.stable_key() == key.scope
            }) {
                existing.state = EnrichmentJobState::Superseded;
                existing.phase = Some("superseded".to_string());
                existing.updated_at = now;
                existing.completed_at = Some(now);
                existing.superseded_by = Some(job.job_id.clone());
                superseded_events.push(EnrichmentJobEvent {
                    job_id: existing.job_id.clone(),
                    timestamp: now,
                    state: EnrichmentJobState::Superseded,
                    phase: existing.phase.clone(),
                    message: Some(job.job_id.clone()),
                    counters: existing.counters.clone(),
                });
            }
            store.events.extend(superseded_events);
            store.jobs.push(job.clone());
            store.events.push(EnrichmentJobEvent {
                job_id: job.job_id.clone(),
                timestamp: now,
                state: EnrichmentJobState::Queued,
                phase: job.phase.clone(),
                message: Some("job created".to_string()),
                counters: job.counters.clone(),
            });
            active.insert(key, job.job_id.clone());
            write_store(repo_root, &store);
        }
        JobStart::Started(Box::new(job))
    }

    pub fn mark_running(&self, repo_root: &Path, job_id: &str, phase: impl Into<String>) {
        self.update_job(
            repo_root,
            job_id,
            JobUpdate {
                state: EnrichmentJobState::Running,
                phase: Some(phase.into()),
                counters: None,
                failure: None,
                superseded_by: None,
            },
        );
    }

    pub fn mark_progress(
        &self,
        repo_root: &Path,
        job_id: &str,
        phase: impl Into<String>,
        current: usize,
        total: Option<usize>,
    ) {
        self.update_job(
            repo_root,
            job_id,
            JobUpdate {
                state: EnrichmentJobState::Running,
                phase: Some(phase.into()),
                counters: Some(EnrichmentCounters {
                    current,
                    total,
                    node_count: None,
                    edge_count: None,
                }),
                failure: None,
                superseded_by: None,
            },
        );
    }

    pub fn mark_persisting(
        &self,
        repo_root: &Path,
        job_id: &str,
        node_count: usize,
        edge_count: usize,
    ) {
        self.update_job(
            repo_root,
            job_id,
            JobUpdate {
                state: EnrichmentJobState::Persisting,
                phase: Some("persisting".to_string()),
                counters: Some(EnrichmentCounters {
                    current: edge_count,
                    total: None,
                    node_count: Some(node_count),
                    edge_count: Some(edge_count),
                }),
                failure: None,
                superseded_by: None,
            },
        );
    }

    pub fn mark_completed(
        &self,
        repo_root: &Path,
        job_id: &str,
        node_count: usize,
        edge_count: usize,
    ) {
        self.update_job(
            repo_root,
            job_id,
            JobUpdate {
                state: EnrichmentJobState::Completed,
                phase: Some("completed".to_string()),
                counters: Some(EnrichmentCounters {
                    current: edge_count,
                    total: None,
                    node_count: Some(node_count),
                    edge_count: Some(edge_count),
                }),
                failure: None,
                superseded_by: None,
            },
        );
    }

    pub fn mark_failed(&self, repo_root: &Path, job_id: &str, error: impl Into<String>) {
        self.update_job(
            repo_root,
            job_id,
            JobUpdate {
                state: EnrichmentJobState::Failed,
                phase: Some("failed".to_string()),
                counters: None,
                failure: Some(error.into()),
                superseded_by: None,
            },
        );
    }

    pub fn mark_superseded(
        &self,
        repo_root: &Path,
        job_id: &str,
        superseded_by: impl Into<String>,
    ) {
        self.update_job(
            repo_root,
            job_id,
            JobUpdate {
                state: EnrichmentJobState::Superseded,
                phase: Some("superseded".to_string()),
                counters: None,
                failure: None,
                superseded_by: Some(superseded_by.into()),
            },
        );
    }

    pub fn recent_jobs(&self, repo_root: &Path, limit: usize) -> Vec<EnrichmentJobRecord> {
        let mut jobs = read_store(repo_root).jobs;
        jobs.retain(|job| job.schema_version == SCHEMA_VERSION);
        jobs.sort_by_key(|job| std::cmp::Reverse(job.updated_at));
        jobs.truncate(limit);
        jobs
    }

    pub fn events_for_job(&self, repo_root: &Path, job_id: &str) -> Vec<EnrichmentJobEvent> {
        read_store(repo_root)
            .events
            .into_iter()
            .filter(|event| event.job_id == job_id)
            .collect()
    }


    fn update_job(&self, repo_root: &Path, job_id: &str, update: JobUpdate) {
        let _guard = self.io_lock.lock().unwrap();
        let _file_lock = JobFileLock::acquire(repo_root);
        let mut store = read_store(repo_root);
        let now = unix_now();
        let mut event_counters = EnrichmentCounters::empty();
        let mut key_to_clear = None;

        if let Some(job) = store.jobs.iter_mut().find(|job| job.job_id == job_id) {
            job.state = update.state;
            job.phase = update.phase.clone();
            if let Some(counters) = update.counters {
                job.counters = counters;
            }
            event_counters = job.counters.clone();
            if let Some(failure) = update.failure.clone() {
                job.failure = Some(failure);
            }
            if let Some(superseded_by) = update.superseded_by.clone() {
                job.superseded_by = Some(superseded_by);
            }
            job.updated_at = now;
            if update.state.is_terminal() {
                job.completed_at = Some(now);
                key_to_clear = Some(JobKey {
                    repo: job.repo.clone(),
                    capability: job.capability,
                    scope: job.scope.stable_key(),
                });
            }
        }

        store.events.push(EnrichmentJobEvent {
            job_id: job_id.to_string(),
            timestamp: now,
            state: update.state,
            phase: update.phase,
            message: update.failure.or(update.superseded_by),
            counters: event_counters,
        });
        write_store(repo_root, &store);

        if let Some(key) = key_to_clear {
            let mut active = self.active.lock().unwrap();
            if active
                .get(&key)
                .is_some_and(|active_id| active_id == job_id)
            {
                active.remove(&key);
            }
        }
    }
}

fn store_job_is_active(store: &EnrichmentJobStore, job_id: &str) -> bool {
    store
        .jobs
        .iter()
        .find(|job| job.job_id == job_id)
        .is_some_and(|job| job.schema_version == SCHEMA_VERSION && !job.state.is_terminal())
}

fn normalize_repo(repo_root: &Path) -> String {
    repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf())
        .display()
        .to_string()
}

fn ledger_path(repo_root: &Path) -> PathBuf {
    repo_root
        .join(".oh")
        .join(".cache")
        .join("enrichment_jobs.json")
}

struct JobFileLock {
    path: PathBuf,
    acquired: bool,
}

impl JobFileLock {
    fn acquire(repo_root: &Path) -> Self {
        let path = ledger_path(repo_root).with_extension("lock");
        if let Some(parent) = path.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            tracing::warn!("Failed to create enrichment job lock directory: {}", e);
        }

        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => {
                    return Self {
                        path,
                        acquired: true,
                    };
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to acquire enrichment job file lock {}; continuing with process-local lock only: {}",
                        path.display(),
                        e
                    );
                    return Self {
                        path,
                        acquired: false,
                    };
                }
            }
        }
    }
}

impl Drop for JobFileLock {
    fn drop(&mut self) {
        if self.acquired {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn lock_is_stale(path: &Path) -> bool {
    path.metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|elapsed| elapsed > Duration::from_secs(300))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn read_store(repo_root: &Path) -> EnrichmentJobStore {
    let path = ledger_path(repo_root);
    let Ok(content) = std::fs::read_to_string(path) else {
        return EnrichmentJobStore::default();
    };
    serde_json::from_str(&content).unwrap_or_default()
}

fn write_store(repo_root: &Path, store: &EnrichmentJobStore) {
    let path = ledger_path(repo_root);
    let Some(parent) = path.parent() else {
        tracing::warn!(
            "Enrichment job ledger path has no parent: {}",
            path.display()
        );
        return;
    };
    if let Err(e) = std::fs::create_dir_all(parent) {
        tracing::warn!("Failed to create enrichment job ledger directory: {}", e);
        return;
    }
    let tmp_path = path.with_extension("tmp");
    match serde_json::to_string_pretty(store) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&tmp_path, json) {
                tracing::warn!("Failed to write enrichment job ledger temp file: {}", e);
                return;
            }
            if let Err(e) = std::fs::rename(&tmp_path, &path) {
                tracing::warn!("Failed to rename enrichment job ledger into place: {}", e);
                let _ = std::fs::remove_file(&tmp_path);
            }
        }
        Err(e) => tracing::warn!("Failed to serialize enrichment job ledger: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn job_transitions_are_persisted() {
        let tmp = TempDir::new().unwrap();
        let ledger = EnrichmentJobLedger::default();
        let job = match ledger.begin_job(
            tmp.path(),
            EnrichmentCapability::CallReferences,
            EnrichmentScope::Repo,
            EnrichmentTrigger::ForegroundScan,
            None,
        ) {
            JobStart::Started(job) => job,
            JobStart::Joined { .. } => panic!("first job should start"),
        };

        ledger.mark_running(tmp.path(), &job.job_id, "lsp");
        ledger.mark_persisting(tmp.path(), &job.job_id, 7, 11);
        ledger.mark_completed(tmp.path(), &job.job_id, 7, 11);

        let jobs = ledger.recent_jobs(tmp.path(), 10);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].state, EnrichmentJobState::Completed);
        assert_eq!(jobs[0].counters.node_count, Some(7));
        assert_eq!(jobs[0].counters.edge_count, Some(11));
        assert!(jobs[0].completed_at.is_some());

        let events = ledger.events_for_job(tmp.path(), &job.job_id);
        assert_eq!(events.len(), 4);
        assert_eq!(events.last().unwrap().state, EnrichmentJobState::Completed);
    }

    #[test]
    fn overlapping_same_key_joins_active_job() {
        let tmp = TempDir::new().unwrap();
        let ledger = EnrichmentJobLedger::default();
        let first = match ledger.begin_job(
            tmp.path(),
            EnrichmentCapability::Embeddings,
            EnrichmentScope::Repo,
            EnrichmentTrigger::BackgroundScan,
            None,
        ) {
            JobStart::Started(job) => job,
            JobStart::Joined { .. } => panic!("first job should start"),
        };
        ledger.mark_running(tmp.path(), &first.job_id, "embedding");

        let second = ledger.begin_job(
            tmp.path(),
            EnrichmentCapability::Embeddings,
            EnrichmentScope::Repo,
            EnrichmentTrigger::ForegroundScan,
            None,
        );

        assert_eq!(
            second,
            JobStart::Joined {
                existing_job_id: first.job_id.clone()
            }
        );
        assert_eq!(ledger.recent_jobs(tmp.path(), 10).len(), 1);
    }

    #[test]
    fn terminal_job_releases_active_key() {
        let tmp = TempDir::new().unwrap();
        let ledger = EnrichmentJobLedger::default();
        let first = match ledger.begin_job(
            tmp.path(),
            EnrichmentCapability::Embeddings,
            EnrichmentScope::Repo,
            EnrichmentTrigger::BackgroundScan,
            None,
        ) {
            JobStart::Started(job) => job,
            JobStart::Joined { .. } => panic!("first job should start"),
        };
        ledger.mark_failed(tmp.path(), &first.job_id, "boom");

        let second = ledger.begin_job(
            tmp.path(),
            EnrichmentCapability::Embeddings,
            EnrichmentScope::Repo,
            EnrichmentTrigger::ForegroundScan,
            None,
        );

        assert!(matches!(second, JobStart::Started(_)));
        assert_eq!(ledger.recent_jobs(tmp.path(), 10).len(), 2);
    }

    #[test]
    fn scan_enrichment_options_preserve_structured_modes() {
        let all = ScanEnrichmentOptions::all();
        assert!(all.runs_lsp());
        assert!(all.runs_embeddings());

        let extract_only = ScanEnrichmentOptions::extract_only();
        assert!(!extract_only.runs_lsp());
        assert!(!extract_only.runs_embeddings());

        let no_lsp = ScanEnrichmentOptions::all().without_lsp();
        assert!(!no_lsp.runs_lsp());
        assert!(no_lsp.runs_embeddings());

        let no_embed = ScanEnrichmentOptions::all().without_embeddings();
        assert!(no_embed.runs_lsp());
        assert!(!no_embed.runs_embeddings());
    }

    #[test]
    fn new_job_supersedes_stale_non_terminal_job_from_store() {
        let tmp = TempDir::new().unwrap();
        let ledger = EnrichmentJobLedger::default();
        let first = match ledger.begin_job(
            tmp.path(),
            EnrichmentCapability::CallReferences,
            EnrichmentScope::Repo,
            EnrichmentTrigger::ForegroundScan,
            None,
        ) {
            JobStart::Started(job) => job,
            JobStart::Joined { .. } => panic!("first job should start"),
        };
        ledger.mark_running(tmp.path(), &first.job_id, "lsp");

        let restarted_ledger = EnrichmentJobLedger::default();
        let second = match restarted_ledger.begin_job(
            tmp.path(),
            EnrichmentCapability::CallReferences,
            EnrichmentScope::Repo,
            EnrichmentTrigger::ForegroundScan,
            None,
        ) {
            JobStart::Started(job) => job,
            JobStart::Joined { .. } => panic!("stale persisted job should be superseded"),
        };

        let jobs = restarted_ledger.recent_jobs(tmp.path(), 10);
        let first_after = jobs.iter().find(|job| job.job_id == first.job_id).unwrap();
        assert_eq!(first_after.state, EnrichmentJobState::Superseded);
        assert_eq!(
            first_after.superseded_by.as_deref(),
            Some(second.job_id.as_str())
        );
        assert!(jobs.iter().any(|job| job.job_id == second.job_id));
    }
}
