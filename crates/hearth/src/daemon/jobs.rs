//! Jobs: every change runs as one, with live progress events and a final status.
//!
//! Changes to the same app never overlap (a per-app lock), finished jobs are written to SQLite
//! as an audit log, and the most recent jobs are kept in memory for the CLI/UI to follow.

use super::State;
use super::db::now;
use hearth_core::model::{Job, JobStatus};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex};

const KEEP_IN_MEMORY: usize = 100;
const MAX_EVENTS: usize = 5000;

#[derive(Default)]
pub struct Jobs {
    recent: Mutex<VecDeque<Arc<Mutex<Job>>>>,
    app_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// How a job ended badly.
#[derive(Debug, Clone)]
pub struct JobError {
    pub status: JobStatus,
    pub message: String,
}

impl JobError {
    pub fn failed(message: impl Into<String>) -> Self {
        JobError {
            status: JobStatus::Failed,
            message: message.into(),
        }
    }
    pub fn rolled_back(message: impl Into<String>) -> Self {
        JobError {
            status: JobStatus::RolledBack,
            message: message.into(),
        }
    }
    pub fn rollback_failed(message: impl Into<String>) -> Self {
        JobError {
            status: JobStatus::RollbackFailed,
            message: message.into(),
        }
    }
}

impl From<String> for JobError {
    fn from(s: String) -> Self {
        JobError::failed(s)
    }
}

/// Handed to the code running a job, to report progress.
#[derive(Clone)]
pub struct JobHandle {
    job: Arc<Mutex<Job>>,
}

impl JobHandle {
    pub fn log(&self, line: impl Into<String>) {
        let line = line.into();
        tracing::debug!(target: "job", "{line}");
        if let Ok(mut j) = self.job.lock() {
            if j.events.len() >= MAX_EVENTS {
                j.events.remove(0);
                j.events_from += 1;
            }
            j.events.push(line);
        }
    }
}

fn new_job(id: &str, kind: &str, app: &str) -> Job {
    Job {
        id: id.to_string(),
        kind: kind.to_string(),
        app: app.to_string(),
        status: JobStatus::Running,
        started_at: now(),
        finished_at: None,
        events: Vec::new(),
        events_from: 0,
        result: None,
        error: None,
    }
}

impl Jobs {
    fn lock_for(&self, app: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.app_locks.lock().unwrap_or_else(|p| p.into_inner());
        locks.entry(app.to_string()).or_default().clone()
    }

    /// Starts `work` as a job and returns its id at once.
    pub fn start<F, Fut>(&self, state: &State, kind: &str, app: &str, actor: &str, work: F) -> String
    where
        F: FnOnce(JobHandle) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Value, JobError>> + Send + 'static,
    {
        let id = random_id();
        let job = Arc::new(Mutex::new(new_job(&id, kind, app)));
        {
            let mut recent = self.recent.lock().unwrap_or_else(|p| p.into_inner());
            recent.push_back(job.clone());
            while recent.len() > KEEP_IN_MEMORY {
                recent.pop_front();
            }
        }
        let handle = JobHandle { job: job.clone() };
        let lock = self.lock_for(app);
        let state = state.clone();
        let (kind, app, actor) = (kind.to_string(), app.to_string(), actor.to_string());
        tracing::info!(job = %id, kind, app, actor, "job started");
        tokio::spawn(async move {
            let guard = match lock.try_lock() {
                Ok(g) => g,
                Err(_) => {
                    handle.log(format!("waiting for another change to {app} to finish…"));
                    lock.lock().await
                }
            };
            let outcome = work(handle.clone()).await;
            drop(guard);
            let snapshot = {
                let mut j = job.lock().unwrap_or_else(|p| p.into_inner());
                j.finished_at = Some(now());
                match outcome {
                    Ok(v) => {
                        j.status = JobStatus::Succeeded;
                        j.result = Some(v);
                    }
                    Err(e) => {
                        j.status = e.status;
                        j.error = Some(e.message);
                    }
                }
                j.clone()
            };
            tracing::info!(job = %snapshot.id, kind, app, status = ?snapshot.status, error = ?snapshot.error, "job finished");
            state.with_db(|db| {
                if let Err(e) = db.save_job(&snapshot, &actor) {
                    tracing::warn!("cannot save job: {e}");
                }
            });
        });
        id
    }

    /// The job, with events from index `after` on.
    pub fn get(&self, id: &str, after: usize) -> Option<Job> {
        let recent = self.recent.lock().unwrap_or_else(|p| p.into_inner());
        let job = recent
            .iter()
            .find(|j| j.lock().map(|j| j.id == id).unwrap_or(false))?;
        let j = job.lock().ok()?;
        let mut out = j.clone();
        let skip = after.saturating_sub(j.events_from).min(j.events.len());
        out.events = j.events[skip..].to_vec();
        out.events_from = j.events_from + skip;
        Some(out)
    }

    pub fn list(&self) -> Vec<Job> {
        let recent = self.recent.lock().unwrap_or_else(|p| p.into_inner());
        recent
            .iter()
            .rev()
            .filter_map(|j| {
                j.lock().ok().map(|j| Job {
                    events: Vec::new(),
                    ..j.clone()
                })
            })
            .collect()
    }
}

fn random_id() -> String {
    let mut b = [0u8; 6];
    let _ = getrandom::fill(&mut b);
    hex::encode(b)
}
