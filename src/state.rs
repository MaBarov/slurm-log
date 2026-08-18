use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

use crate::model::Job;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const SCHEMA: u32 = 2;
const MAX_STATE_BYTES: u64 = 64 * 1024 * 1024;

fn now_string() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
}

#[derive(Clone, Default, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ledger {
    #[serde(default)]
    pub known: HashMap<String, String>,
    #[serde(default)]
    pub opened: HashMap<String, String>,
    #[serde(default)]
    pub dismissed: HashMap<String, String>,
    #[serde(default)]
    pub baselined_clusters: Vec<String>,
    pub tracking_schema: Option<u32>,
    #[serde(default)]
    pub auto_add_default: bool,
    #[serde(default)]
    pub log_warnings_default: bool,
    #[serde(default)]
    pub interactive_jobs: HashMap<String, String>,
}

impl Ledger {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        if fs::metadata(path)?.len() > MAX_STATE_BYTES {
            anyhow::bail!("state ledger exceeds 64 MiB safety limit");
        }
        serde_json::from_slice(&fs::read(path)?).context("parse state ledger")
    }

    pub fn sync(path: &Path, jobs: &[Job], complete_clusters: &HashSet<String>) -> Result<Self> {
        update(path, |state| {
            let mut changed = false;
            let now = now_string();
            let migrating = state.tracking_schema != Some(SCHEMA);
            if migrating {
                state.tracking_schema = Some(SCHEMA);
                changed = true;
            }
            let mut key = String::new();
            for job in jobs {
                job.write_key(&mut key);
                if !state.known.contains_key(&key) {
                    state.known.insert(key.clone(), now.clone());
                    changed = true;
                }
                let newly_complete_cluster = complete_clusters.contains(&job.cluster)
                    && (migrating || !state.baselined_clusters.contains(&job.cluster));
                if newly_complete_cluster && !job.active() && !state.opened.contains_key(&key) {
                    state.opened.insert(key.clone(), now.clone());
                    changed = true;
                }
                if job.interactive && !state.interactive_jobs.contains_key(&key) {
                    state.interactive_jobs.insert(key.clone(), now.clone());
                    changed = true;
                }
            }
            for cluster in complete_clusters {
                if !state.baselined_clusters.contains(cluster) {
                    state.baselined_clusters.push(cluster.clone());
                    changed = true;
                }
            }
            changed
        })
    }

    pub fn mark_opened(path: &Path, job: &Job) -> Result<()> {
        update(path, |state| {
            let now = now_string();
            let key = job.key();
            state
                .known
                .entry(key.clone())
                .or_insert_with(|| now.clone());
            state.opened.insert(key, now);
            true
        })
        .map(|_| ())
    }

    pub fn dismiss(path: &Path, jobs: &[Job]) -> Result<usize> {
        let terminal: Vec<_> = jobs
            .iter()
            .filter(|job| !job.active() && job.state != "OPEN")
            .collect();
        let count = terminal.len();
        update(path, |state| {
            let now = now_string();
            for job in terminal {
                let key = job.key();
                state
                    .known
                    .entry(key.clone())
                    .or_insert_with(|| now.clone());
                state.opened.insert(key.clone(), now.clone());
                state.dismissed.insert(key, now.clone());
            }
            count > 0
        })?;
        Ok(count)
    }

    /// Hide a deliberately closed monitor without changing or cancelling the
    /// scheduler job. Unlike the picker's `d` action, this also accepts active
    /// interactive allocations. Archive mode remains the escape hatch.
    pub fn suppress(path: &Path, job: &Job) -> Result<()> {
        update(path, |state| {
            let now = now_string();
            let key = job.key();
            state
                .known
                .entry(key.clone())
                .or_insert_with(|| now.clone());
            state.opened.insert(key.clone(), now.clone());
            state.dismissed.insert(key, now);
            true
        })
        .map(|_| ())
    }

    pub fn set_auto_add(path: &Path, enabled: bool) -> Result<()> {
        update(path, |state| {
            if state.auto_add_default == enabled {
                return false;
            }
            state.auto_add_default = enabled;
            true
        })
        .map(|_| ())
    }

    pub fn set_log_warnings(path: &Path, enabled: bool) -> Result<()> {
        update(path, |state| {
            if state.log_warnings_default == enabled {
                return false;
            }
            state.log_warnings_default = enabled;
            true
        })
        .map(|_| ())
    }

    pub fn set_read(path: &Path, job_id: &str, read: bool) -> Result<usize> {
        let mut changed = 0;
        update(path, |state| {
            let now = now_string();
            let suffix = format!(":{job_id}");
            let keys: Vec<_> = state
                .known
                .keys()
                .filter(|key| key.ends_with(&suffix))
                .cloned()
                .collect();
            for key in keys {
                if read {
                    state.opened.insert(key, now.clone());
                } else {
                    state.opened.remove(&key);
                }
                changed += 1;
            }
            changed > 0
        })?;
        Ok(changed)
    }
}

fn update(path: &Path, mutate: impl FnOnce(&mut Ledger) -> bool) -> Result<Ledger> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)?;
    // Parse once and retain the original bytes. Most refreshes discover no
    // state change; comparing the serialized result avoids a full atomic file
    // rewrite and its filesystem synchronization cost on that hot path.
    if fs::metadata(path).is_ok_and(|metadata| metadata.len() > MAX_STATE_BYTES) {
        anyhow::bail!("state ledger exceeds 64 MiB safety limit");
    }
    let existing = fs::read(path).unwrap_or_default();
    let mut state: Ledger = if existing.is_empty() {
        Ledger::default()
    } else {
        serde_json::from_slice(&existing).context("parse state ledger before update")?
    };
    if !mutate(&mut state) {
        return Ok(state);
    }
    let encoded = serde_json::to_vec(&state)?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(&encoded)?;
    file.write_all(b"\n")?;
    fs::rename(tmp, path)?;
    Ok(state)
}

#[cfg(test)]
#[path = "state/tests.rs"]
mod tests;
