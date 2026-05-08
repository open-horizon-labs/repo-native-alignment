//! Durable enrichment job ledger and scoped scan controls.
//!
//! This is intentionally a small control-plane primitive, not a worker scheduler.
//! It gives enrichment work invocation identity, restart-visible lifecycle history,
//! and one active-job key per repo/capability/scope so foreground/background work
//! cannot emit indistinguishable progress for the same capability.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
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

const JOB_LEASE_SECONDS: u64 = 60 * 60;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_at: Option<u64>,
    pub completed_at: Option<u64>,
    pub failure: Option<String>,
    pub superseded_by: Option<String>,
    pub owner_id: Option<String>,
    pub lease_expires_at: Option<u64>,
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
    ) -> Result<JobStart> {
        let repo = normalize_repo(repo_root);
        let key = JobKey {
            repo: repo.clone(),
            capability,
            scope: scope.stable_key(),
        };

        let now = unix_now();
        let owner_id = process_owner_id();
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
            last_progress_at: Some(now),
            completed_at: None,
            failure: None,
            superseded_by: None,
            owner_id: Some(owner_id.clone()),
            lease_expires_at: Some(now + JOB_LEASE_SECONDS),
            schema_version: SCHEMA_VERSION,
        };

        {
            let _guard = self.io_lock.lock().unwrap();
            let _file_lock = JobFileLock::acquire(repo_root);
            let mut store = read_store(repo_root)?;
            let mut active = self.active.lock().unwrap();

            if let Some(existing_job_id) = active.get(&key).cloned() {
                if store_job_is_active(&store, &existing_job_id, now) {
                    store.events.push(EnrichmentJobEvent {
                        job_id: existing_job_id.clone(),
                        timestamp: now,
                        state: EnrichmentJobState::Running,
                        phase: Some("joined".to_string()),
                        message: Some("overlapping request joined existing active job".to_string()),
                        counters: EnrichmentCounters::empty(),
                    });
                    write_store(repo_root, &store)?;
                    return Ok(JobStart::Joined { existing_job_id });
                }
                active.remove(&key);
            }

            if let Some(existing_job_id) = store
                .jobs
                .iter()
                .find(|existing| {
                    matching_job_key(existing, &job, &key) && persisted_job_is_live(existing, now)
                })
                .map(|existing| existing.job_id.clone())
            {
                store.events.push(EnrichmentJobEvent {
                    job_id: existing_job_id.clone(),
                    timestamp: now,
                    state: EnrichmentJobState::Running,
                    phase: Some("joined".to_string()),
                    message: Some("overlapping request joined live persisted job".to_string()),
                    counters: EnrichmentCounters::empty(),
                });
                write_store(repo_root, &store)?;
                return Ok(JobStart::Joined { existing_job_id });
            }

            let mut superseded_events = Vec::new();
            for existing in store
                .jobs
                .iter_mut()
                .filter(|existing| matching_job_key(existing, &job, &key))
            {
                existing.state = EnrichmentJobState::Superseded;
                existing.phase = Some("superseded".to_string());
                existing.updated_at = now;
                existing.last_progress_at = Some(now);
                existing.completed_at = Some(now);
                existing.superseded_by = Some(job.job_id.clone());
                existing.owner_id = None;
                existing.lease_expires_at = None;
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
            write_store(repo_root, &store)?;
            active.insert(key, job.job_id.clone());
        }
        Ok(JobStart::Started(Box::new(job)))
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

    pub fn mark_timed_out(&self, repo_root: &Path, job_id: &str, detail: impl Into<String>) {
        self.update_job(
            repo_root,
            job_id,
            JobUpdate {
                state: EnrichmentJobState::Failed,
                phase: Some("timed_out".to_string()),
                counters: None,
                failure: Some(detail.into()),
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
        let mut store = {
            let _guard = self.io_lock.lock().unwrap();
            let _file_lock = JobFileLock::acquire(repo_root);
            let mut store = match read_store(repo_root) {
                Ok(store) => store,
                Err(e) => {
                    tracing::warn!("Failed to read enrichment job ledger: {}", e);
                    return Vec::new();
                }
            };
            if recover_stale_jobs(&mut store, unix_now())
                && let Err(e) = write_store(repo_root, &store)
            {
                tracing::warn!("Failed to persist stale enrichment job recovery: {}", e);
            }
            store
        };
        store
            .jobs
            .retain(|job| job.schema_version == SCHEMA_VERSION);
        store
            .jobs
            .sort_by_key(|job| std::cmp::Reverse(job.updated_at));
        store.jobs.truncate(limit);
        store.jobs
    }

    pub fn events_for_job(&self, repo_root: &Path, job_id: &str) -> Vec<EnrichmentJobEvent> {
        match read_store(repo_root) {
            Ok(store) => store
                .events
                .into_iter()
                .filter(|event| event.job_id == job_id)
                .collect(),
            Err(e) => {
                tracing::warn!("Failed to read enrichment job ledger events: {}", e);
                Vec::new()
            }
        }
    }

    fn update_job(&self, repo_root: &Path, job_id: &str, update: JobUpdate) {
        let _guard = self.io_lock.lock().unwrap();
        let _file_lock = JobFileLock::acquire(repo_root);
        let mut store = match read_store(repo_root) {
            Ok(store) => store,
            Err(e) => {
                tracing::warn!("Failed to read enrichment job ledger for update: {}", e);
                return;
            }
        };
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
            job.last_progress_at = Some(now);
            if update.state.is_terminal() {
                job.lease_expires_at = None;
                job.owner_id = None;
            } else {
                job.owner_id = Some(process_owner_id());
                job.lease_expires_at = Some(now + JOB_LEASE_SECONDS);
            }
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
        if let Err(e) = write_store(repo_root, &store) {
            tracing::warn!(
                "Failed to persist enrichment job update for {}: {}",
                job_id,
                e
            );
            return;
        }

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

fn matching_job_key(
    job: &EnrichmentJobRecord,
    new_job: &EnrichmentJobRecord,
    key: &JobKey,
) -> bool {
    job.schema_version == SCHEMA_VERSION
        && !job.state.is_terminal()
        && job.repo == new_job.repo
        && job.capability == new_job.capability
        && job.scope.stable_key() == key.scope
}

fn store_job_is_active(store: &EnrichmentJobStore, job_id: &str, now: u64) -> bool {
    store
        .jobs
        .iter()
        .find(|job| job.job_id == job_id)
        .is_some_and(|job| persisted_job_is_live(job, now))
}

fn persisted_job_is_live(job: &EnrichmentJobRecord, now: u64) -> bool {
    job.schema_version == SCHEMA_VERSION
        && !job.state.is_terminal()
        && job
            .lease_expires_at
            .is_some_and(|expires_at| expires_at >= now)
        && job.owner_id.as_deref().is_some_and(process_owner_is_alive)
}

fn recover_stale_jobs(store: &mut EnrichmentJobStore, now: u64) -> bool {
    let mut changed = false;
    let mut events = Vec::new();
    for job in store.jobs.iter_mut() {
        if job.schema_version != SCHEMA_VERSION || job.state.is_terminal() {
            continue;
        }
        if persisted_job_is_live(job, now) {
            continue;
        }
        job.state = EnrichmentJobState::Cancelled;
        job.phase = Some("stale_recovered".to_string());
        job.updated_at = now;
        job.last_progress_at = Some(now);
        job.completed_at = Some(now);
        job.failure = Some(
            "previous enrichment process exited or its lease expired before completion".to_string(),
        );
        job.owner_id = None;
        job.lease_expires_at = None;
        events.push(EnrichmentJobEvent {
            job_id: job.job_id.clone(),
            timestamp: now,
            state: EnrichmentJobState::Cancelled,
            phase: job.phase.clone(),
            message: job.failure.clone(),
            counters: job.counters.clone(),
        });
        changed = true;
    }
    store.events.extend(events);
    changed
}

fn process_owner_id() -> String {
    std::process::id().to_string()
}

fn process_owner_is_alive(owner: &str) -> bool {
    let Ok(pid) = owner.parse::<u32>() else {
        return false;
    };
    process_is_alive(pid)
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
    owner: String,
    acquired: bool,
}

impl JobFileLock {
    fn acquire(repo_root: &Path) -> Self {
        let path = ledger_path(repo_root).with_extension("lock");
        let owner = std::process::id().to_string();
        if let Some(parent) = path.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            tracing::warn!("Failed to create enrichment job lock directory: {}", e);
        }

        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    if let Err(e) = writeln!(file, "{}", owner) {
                        tracing::warn!(
                            "Failed to write enrichment job lock owner {}; continuing with process-local lock only: {}",
                            path.display(),
                            e
                        );
                        let _ = fs::remove_file(&path);
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
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_owner_is_dead(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to acquire enrichment job file lock {}; continuing with process-local lock only: {}",
                        path.display(),
                        e
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

impl Drop for JobFileLock {
    fn drop(&mut self) {
        if self.acquired && lock_is_owned_by(&self.path, &self.owner) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn lock_is_owned_by(path: &Path, owner: &str) -> bool {
    fs::read_to_string(path)
        .map(|content| content.trim() == owner)
        .unwrap_or(false)
}

fn lock_owner_is_dead(path: &Path) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return true;
    };
    !process_owner_is_alive(content.trim())
}

fn process_is_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn read_store(repo_root: &Path) -> Result<EnrichmentJobStore> {
    let path = ledger_path(repo_root);
    if !path.exists() {
        return Ok(EnrichmentJobStore::default());
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read enrichment job ledger {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse enrichment job ledger {}", path.display()))
}

fn write_store(repo_root: &Path, store: &EnrichmentJobStore) -> Result<()> {
    let path = ledger_path(repo_root);
    let parent = path.parent().with_context(|| {
        format!(
            "enrichment job ledger path has no parent: {}",
            path.display()
        )
    })?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create enrichment job ledger directory {}",
            parent.display()
        )
    })?;
    let tmp_path = path.with_extension("tmp");
    let json =
        serde_json::to_string_pretty(store).context("failed to serialize enrichment job ledger")?;
    fs::write(&tmp_path, json).with_context(|| {
        format!(
            "failed to write enrichment job ledger temp file {}",
            tmp_path.display()
        )
    })?;
    fs::rename(&tmp_path, &path).with_context(|| {
        let _ = fs::remove_file(&tmp_path);
        format!(
            "failed to rename enrichment job ledger temp file {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn job_transitions_are_persisted() {
        let tmp = TempDir::new().unwrap();
        let ledger = EnrichmentJobLedger::default();
        let job = match ledger
            .begin_job(
                tmp.path(),
                EnrichmentCapability::CallReferences,
                EnrichmentScope::Repo,
                EnrichmentTrigger::ForegroundScan,
                None,
            )
            .unwrap()
        {
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
        let first = match ledger
            .begin_job(
                tmp.path(),
                EnrichmentCapability::Embeddings,
                EnrichmentScope::Repo,
                EnrichmentTrigger::BackgroundScan,
                None,
            )
            .unwrap()
        {
            JobStart::Started(job) => job,
            JobStart::Joined { .. } => panic!("first job should start"),
        };
        ledger.mark_running(tmp.path(), &first.job_id, "embedding");

        let second = ledger
            .begin_job(
                tmp.path(),
                EnrichmentCapability::Embeddings,
                EnrichmentScope::Repo,
                EnrichmentTrigger::ForegroundScan,
                None,
            )
            .unwrap();

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
        let first = match ledger
            .begin_job(
                tmp.path(),
                EnrichmentCapability::Embeddings,
                EnrichmentScope::Repo,
                EnrichmentTrigger::BackgroundScan,
                None,
            )
            .unwrap()
        {
            JobStart::Started(job) => job,
            JobStart::Joined { .. } => panic!("first job should start"),
        };
        ledger.mark_failed(tmp.path(), &first.job_id, "boom");

        let second = ledger
            .begin_job(
                tmp.path(),
                EnrichmentCapability::Embeddings,
                EnrichmentScope::Repo,
                EnrichmentTrigger::ForegroundScan,
                None,
            )
            .unwrap();

        assert!(matches!(second, JobStart::Started(_)));
        assert_eq!(ledger.recent_jobs(tmp.path(), 10).len(), 2);
    }

    #[test]
    fn timed_out_job_records_terminal_failure_and_progress_time() {
        let tmp = TempDir::new().unwrap();
        let ledger = EnrichmentJobLedger::default();
        let job = match ledger
            .begin_job(
                tmp.path(),
                EnrichmentCapability::CallReferences,
                EnrichmentScope::Repo,
                EnrichmentTrigger::Explicit,
                None,
            )
            .unwrap()
        {
            JobStart::Started(job) => job,
            JobStart::Joined { .. } => panic!("first job should start"),
        };

        ledger.mark_running(tmp.path(), &job.job_id, "lsp");
        ledger.mark_progress(tmp.path(), &job.job_id, "lsp_edges", 3, Some(10));
        ledger.mark_timed_out(tmp.path(), &job.job_id, "budget exceeded");

        let jobs = ledger.recent_jobs(tmp.path(), 10);
        assert_eq!(jobs.len(), 1);
        let persisted = &jobs[0];
        assert_eq!(persisted.state, EnrichmentJobState::Failed);
        assert_eq!(persisted.phase.as_deref(), Some("timed_out"));
        assert_eq!(persisted.failure.as_deref(), Some("budget exceeded"));
        assert!(persisted.completed_at.is_some());
        assert!(persisted.last_progress_at.is_some());
        assert!(persisted.owner_id.is_none());
        assert!(persisted.lease_expires_at.is_none());
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
    fn new_ledger_joins_live_persisted_job() {
        let tmp = TempDir::new().unwrap();
        let ledger = EnrichmentJobLedger::default();
        let first = match ledger
            .begin_job(
                tmp.path(),
                EnrichmentCapability::CallReferences,
                EnrichmentScope::Repo,
                EnrichmentTrigger::ForegroundScan,
                None,
            )
            .unwrap()
        {
            JobStart::Started(job) => job,
            JobStart::Joined { .. } => panic!("first job should start"),
        };
        ledger.mark_running(tmp.path(), &first.job_id, "lsp");

        let restarted_ledger = EnrichmentJobLedger::default();
        let second = restarted_ledger
            .begin_job(
                tmp.path(),
                EnrichmentCapability::CallReferences,
                EnrichmentScope::Repo,
                EnrichmentTrigger::ForegroundScan,
                None,
            )
            .unwrap();

        assert_eq!(
            second,
            JobStart::Joined {
                existing_job_id: first.job_id.clone()
            }
        );
        let jobs = restarted_ledger.recent_jobs(tmp.path(), 10);
        let first_after = jobs.iter().find(|job| job.job_id == first.job_id).unwrap();
        assert_eq!(first_after.state, EnrichmentJobState::Running);
        assert!(first_after.lease_expires_at.is_some());
    }

    #[test]
    fn new_job_supersedes_stale_non_terminal_job_from_store() {
        let tmp = TempDir::new().unwrap();
        let ledger = EnrichmentJobLedger::default();
        let first = match ledger
            .begin_job(
                tmp.path(),
                EnrichmentCapability::CallReferences,
                EnrichmentScope::Repo,
                EnrichmentTrigger::ForegroundScan,
                None,
            )
            .unwrap()
        {
            JobStart::Started(job) => job,
            JobStart::Joined { .. } => panic!("first job should start"),
        };
        ledger.mark_running(tmp.path(), &first.job_id, "lsp");
        let mut store = read_store(tmp.path()).unwrap();
        let stale = store
            .jobs
            .iter_mut()
            .find(|job| job.job_id == first.job_id)
            .unwrap();
        stale.owner_id = None;
        stale.lease_expires_at = Some(unix_now().saturating_sub(1));
        write_store(tmp.path(), &store).unwrap();

        let restarted_ledger = EnrichmentJobLedger::default();
        let second = match restarted_ledger
            .begin_job(
                tmp.path(),
                EnrichmentCapability::CallReferences,
                EnrichmentScope::Repo,
                EnrichmentTrigger::ForegroundScan,
                None,
            )
            .unwrap()
        {
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

    #[test]
    fn recent_jobs_recovers_dead_running_job_as_cancelled() {
        let tmp = TempDir::new().unwrap();
        let ledger = EnrichmentJobLedger::default();
        let first = match ledger
            .begin_job(
                tmp.path(),
                EnrichmentCapability::CallReferences,
                EnrichmentScope::Repo,
                EnrichmentTrigger::ForegroundScan,
                None,
            )
            .unwrap()
        {
            JobStart::Started(job) => job,
            JobStart::Joined { .. } => panic!("first job should start"),
        };
        ledger.mark_running(tmp.path(), &first.job_id, "lsp");
        let mut store = read_store(tmp.path()).unwrap();
        let stale = store
            .jobs
            .iter_mut()
            .find(|job| job.job_id == first.job_id)
            .unwrap();
        stale.owner_id = None;
        stale.lease_expires_at = Some(unix_now().saturating_sub(1));
        write_store(tmp.path(), &store).unwrap();

        let jobs = ledger.recent_jobs(tmp.path(), 10);
        let recovered = jobs.iter().find(|job| job.job_id == first.job_id).unwrap();
        assert_eq!(recovered.state, EnrichmentJobState::Cancelled);
        assert_eq!(recovered.phase.as_deref(), Some("stale_recovered"));
        assert!(
            recovered
                .failure
                .as_deref()
                .unwrap()
                .contains("lease expired")
        );
        assert!(recovered.completed_at.is_some());
        assert!(recovered.owner_id.is_none());
        assert!(recovered.lease_expires_at.is_none());
    }
}
