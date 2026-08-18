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
    let state = Ledger::sync(&path, std::slice::from_ref(&interactive), &HashSet::new()).unwrap();
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
fn defaults_noop_updates_and_read_markers_are_consistent() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("nested/state.json");
    assert_eq!(Ledger::load(&path).unwrap(), Ledger::default());

    Ledger::set_auto_add(&path, false).unwrap();
    assert!(!path.exists(), "a no-op must not create the ledger");
    Ledger::set_auto_add(&path, true).unwrap();
    Ledger::set_auto_add(&path, true).unwrap();
    assert!(Ledger::load(&path).unwrap().auto_add_default);

    Ledger::set_log_warnings(&path, true).unwrap();
    assert!(Ledger::load(&path).unwrap().log_warnings_default);
    Ledger::set_log_warnings(&path, false).unwrap();
    assert!(!Ledger::load(&path).unwrap().log_warnings_default);
    for cluster in ["cispa", "sprint"] {
        Ledger::mark_opened(
            &path,
            &Job {
                cluster: cluster.into(),
                id: "42".into(),
                ..Job::default()
            },
        )
        .unwrap();
    }
    assert_eq!(Ledger::set_read(&path, "42", false).unwrap(), 2);
    assert!(Ledger::load(&path).unwrap().opened.is_empty());
    assert_eq!(Ledger::set_read(&path, "42", true).unwrap(), 2);
    assert_eq!(Ledger::load(&path).unwrap().opened.len(), 2);
    assert_eq!(Ledger::set_read(&path, "missing", true).unwrap(), 0);
}

#[test]
fn update_rejects_oversized_existing_state_without_replacing_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_STATE_BYTES + 1).unwrap();
    assert!(Ledger::set_auto_add(&path, true).is_err());
    assert_eq!(fs::metadata(&path).unwrap().len(), MAX_STATE_BYTES + 1);
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
    let elapsed = started.elapsed();
    assert!(elapsed < std::time::Duration::from_millis(if cfg!(coverage) { 1000 } else { 250 }));
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), before);
    eprintln!("no-op sync 20k jobs: {elapsed:?}");
}
