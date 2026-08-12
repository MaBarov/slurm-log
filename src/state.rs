use anyhow::{Context, Result};
use fs2::FileExt;
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
            let baseline: HashSet<_> = state.baselined_clusters.iter().cloned().collect();
            let mut newly: HashSet<_> = complete_clusters
                .iter()
                .filter(|cluster| !baseline.contains(*cluster))
                .cloned()
                .collect();
            if state.tracking_schema != Some(SCHEMA) {
                newly.extend(complete_clusters.iter().cloned());
                state.tracking_schema = Some(SCHEMA);
                changed = true;
            }
            for job in jobs {
                let key = job.key();
                if !state.known.contains_key(&key) {
                    state.known.insert(key.clone(), now.clone());
                    changed = true;
                }
                if newly.contains(&job.cluster) && !job.active() && !state.opened.contains_key(&key)
                {
                    state.opened.insert(key.clone(), now.clone());
                    changed = true;
                }
                if job.interactive && !state.interactive_jobs.contains_key(&key) {
                    state.interactive_jobs.insert(key, now.clone());
                    changed = true;
                }
            }
            for cluster in complete_clusters {
                if !baseline.contains(cluster) {
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
    lock.lock_exclusive()?;
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
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, sync::Arc, thread};
    #[test]
    fn dismissal_hides_only_terminal_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let failed = Job {
            cluster: "cispa".into(),
            id: "1".into(),
            state: "FAILED".into(),
            ..Job::default()
        };
        let running = Job {
            cluster: "cispa".into(),
            id: "2".into(),
            state: "RUNNING".into(),
            ..Job::default()
        };
        assert_eq!(
            Ledger::dismiss(&path, &[failed.clone(), running]).unwrap(),
            1
        );
        let state = Ledger::load(&path).unwrap();
        assert!(state.dismissed.contains_key(&failed.key()));
        assert!(!state.dismissed.contains_key("cispa:2"));
    }

    #[test]
    fn explicitly_closed_monitor_can_suppress_an_active_job() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let running = Job {
            cluster: "cispa".into(),
            id: "2".into(),
            state: "RUNNING".into(),
            ..Job::default()
        };
        Ledger::suppress(&path, &running).unwrap();
        let state = Ledger::load(&path).unwrap();
        assert!(state.opened.contains_key(&running.key()));
        assert!(state.dismissed.contains_key(&running.key()));
    }

    #[test]
    fn sync_remembers_interactive_jobs_across_scheduler_sources() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let interactive = Job {
            cluster: "cispa".into(),
            id: "42".into(),
            state: "RUNNING".into(),
            interactive: true,
            ..Job::default()
        };
        let state =
            Ledger::sync(&path, std::slice::from_ref(&interactive), &HashSet::new()).unwrap();
        assert!(state.interactive_jobs.contains_key(&interactive.key()));
        assert!(
            Ledger::load(&path)
                .unwrap()
                .interactive_jobs
                .contains_key(&interactive.key())
        );
    }

    #[test]
    fn schema_migration_baselines_terminal_array_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let old = Job {
            cluster: "cispa".into(),
            id: "3202690_1".into(),
            state: "COMPLETED".into(),
            ..Job::default()
        };
        let state = Ledger::sync(
            &path,
            std::slice::from_ref(&old),
            &HashSet::from(["cispa".into()]),
        )
        .unwrap();
        assert_eq!(state.tracking_schema, Some(SCHEMA));
        assert!(state.opened.contains_key(&old.key()));
    }

    #[test]
    fn concurrent_updates_preserve_every_job_and_valid_json() {
        let directory = tempfile::tempdir().unwrap();
        let path = Arc::new(directory.path().join("state.json"));
        let workers: Vec<_> = (0..24)
            .map(|id| {
                let path = path.clone();
                thread::spawn(move || {
                    Ledger::mark_opened(
                        &path,
                        &Job {
                            cluster: "cispa".into(),
                            id: id.to_string(),
                            ..Job::default()
                        },
                    )
                    .unwrap();
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let bytes = fs::read(path.as_ref()).unwrap();
        let state: Ledger = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(state.known.len(), 24);
        assert_eq!(state.opened.len(), 24);
        assert!(bytes.ends_with(b"\n"));
        assert_eq!(
            fs::metadata(path.as_ref()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.with_extension("lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn corrupt_state_is_reported_by_read_only_load() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        fs::write(&path, b"not-json").unwrap();
        assert!(Ledger::load(&path).is_err());
    }

    #[test]
    fn oversized_state_is_rejected_before_reading() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_STATE_BYTES + 1).unwrap();
        assert!(Ledger::load(&path).is_err());
    }

    #[test]
    fn mutation_never_overwrites_a_corrupt_ledger() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        fs::write(&path, b"not-json").unwrap();
        assert!(Ledger::set_auto_add(&path, true).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"not-json");
    }

    #[test]
    #[ignore = "release-mode performance budget"]
    fn no_op_sync_of_twenty_thousand_jobs_avoids_rewrite_within_budget() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let jobs: Vec<_> = (0..20_000)
            .map(|id| Job {
                cluster: "cispa".into(),
                id: id.to_string(),
                state: "COMPLETED".into(),
                ..Job::default()
            })
            .collect();
        let complete = HashSet::from(["cispa".into()]);
        Ledger::sync(&path, &jobs, &complete).unwrap();
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        let started = std::time::Instant::now();
        Ledger::sync(&path, &jobs, &complete).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_millis(250));
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), before);
    }
}
