use super::*;

#[test]
fn interactive_allocations_have_no_usable_stdout() {
    for missing in [
        None,
        Some(""),
        Some("  "),
        Some("None"),
        Some("(null)"),
        Some("/dev/null"),
    ] {
        assert_eq!(usable_stdout(missing), None);
    }
    assert_eq!(
        usable_stdout(Some("/logs/slurm-42.out")),
        Some("/logs/slurm-42.out")
    );
}
use std::{os::unix::fs::PermissionsExt, path::PathBuf};
#[test]
fn arrays_expand_correctly() {
    assert_eq!(
        expand_path("/log/%x_%j_%A_%a.log", "train", "3202710", "3202690", "1"),
        "/log/train_3202710_3202690_1.log"
    );
    assert_eq!(expand_path("log-%", "job", "42", "42", "0"), "log-%");
}

#[test]
fn terminal_metadata_parsers_cover_active_accounting_steps_and_missing_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let config = Config {
        local_user: "owner".into(),
        remote_user: "owner".into(),
        ssh_host: String::new(),
        state_path: directory.path().join("state.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![crate::config::ClusterConfig {
            name: "local".into(),
            controller: None,
            transport: "local".into(),
            user: "owner".into(),
            ssh_host: String::new(),
            working_directory: directory.path().into(),
            accounting: true,
        }],
    };
    let active = active_terminal_metadata(
        &config,
        "local",
        "42",
        "JobId=42 UserId=owner(1000) JobName=train StdOut=/logs/%x-%j.out JobState=RUNNING",
    )
    .unwrap();
    assert_eq!(
        resolve_terminal_metadata(active),
        (Some("/logs/train-42.out".into()), "train".into())
    );
    let unnamed = active_terminal_metadata(
        &config,
        "local",
        "43",
        "JobId=43 UserId=owner(1000) StdOut=/dev/null",
    )
    .unwrap();
    assert_eq!(resolve_terminal_metadata(unnamed), (None, "job".into()));

    let accounting = accounting_terminal_metadata(
        &config,
        "local",
        "320_7",
        "320_7.batch|320_7.batch|owner|array task|/logs/%A_%a_%j_%%_%q.out|local\n",
    )
    .unwrap();
    assert_eq!(
        resolve_terminal_metadata(accounting),
        (
            Some("/logs/320_7_320_7_%_%q.out".into()),
            "array task".into()
        )
    );
    assert!(accounting_terminal_metadata(&config, "local", "320_7", "only|three|fields").is_err());
}

#[test]
fn terminal_metadata_rejects_reused_id_or_owner_transition() {
    let directory = tempfile::tempdir().unwrap();
    let config = Config {
        local_user: "owner".into(),
        remote_user: "owner".into(),
        ssh_host: String::new(),
        state_path: directory.path().join("state.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![crate::config::ClusterConfig {
            name: "local".into(),
            controller: None,
            transport: "local".into(),
            user: "owner".into(),
            ssh_host: String::new(),
            working_directory: directory.path().into(),
            accounting: true,
        }],
    };
    assert!(
        validate_control_identity(
            &config,
            "local",
            "42",
            "JobId=42 UserId=other(2000) StdOut=/safe/log"
        )
        .is_err()
    );
    assert!(
        accounting_terminal_metadata(&config, "local", "42", "42|42|other|reused|/safe/log|local")
            .is_err()
    );
}

#[test]
fn control_details_preserve_fallbacks_and_apply_available_tokens() {
    let mut job = Job {
        state: "RUNNING".into(),
        reason: "Resources".into(),
        exit_code: "old".into(),
        partition: "old".into(),
        ..Job::default()
    };
    apply_control_details(
        &mut job,
        "JobState=FAILED ExitCode=1:0 Partition=gpu Reason=NodeFail",
    );
    assert_eq!(job.state, "FAILED");
    assert_eq!(job.exit_code, "1:0");
    assert_eq!(job.partition, "gpu");
    assert_eq!(job.reason, "NodeFail");

    apply_control_details(&mut job, "JobId=42");
    assert_eq!(job.state, "FAILED");
    assert_eq!(job.reason, "NodeFail");
    assert!(job.exit_code.is_empty());
    assert!(job.partition.is_empty());
}
#[test]
fn queue_parser_rejects_steps() {
    let jobs = parse_queue(
        "1|RUNNING|ok|0:01|node\n1.batch|RUNNING|step|0:01|node\n",
        "cispa",
    );
    assert_eq!(jobs.len(), 1);
}

#[test]
fn queue_parser_classifies_shell_allocations_as_interactive() {
    let jobs = parse_queue(
        "41|RUNNING|batch|0:01|node|gpu|now|1|/work/train.sbatch\n42|RUNNING|named-shell|0:02|node|gpu|now|2|bash\n",
        "cispa",
    );
    assert!(!jobs[0].interactive);
    assert!(jobs[1].interactive);
    assert!(jobs[1].blocked_category());
    for command in [
        "/bin/zsh",
        "fish",
        "tcsh",
        "nu",
        "bash",
        "/bin/bash",
        "sh",
        "/bin/sh",
        "screen",
        "tmux",
    ] {
        assert!(interactive_command(command));
    }
    for command in [
        "/work/train.sbatch",
        "train.sbatch",
        "run.sh",
        "eval.slurm",
        "python",
        "python3",
        "/opt/venvs/e1/bin/python",
        "ipython",
        "julia",
        "node",
        "gdb",
    ] {
        assert!(!interactive_command(command));
    }
    assert_eq!(queue_cache_name("cispa"), "queue-v3-cispa");
    assert_ne!(queue_cache_name("cispa"), "queue-cispa");
    let mut accounting_row = vec![Job {
        cluster: "cispa".into(),
        id: "42".into(),
        state: "COMPLETED".into(),
        ..Job::default()
    }];
    let mut ledger = Ledger::default();
    ledger
        .interactive_jobs
        .insert("cispa:42".into(), "now".into());
    restore_interactive_classification(&mut accounting_row, &ledger);
    assert!(accounting_row[0].interactive);
    assert!(accounting_row[0].blocked_category());
}

#[test]
fn python_compute_jobs_appear_in_default_live_view() {
    // A python job submitted via sbatch or srun must NOT be hidden as blocked
    let raw_queue =
        "463107|RUNNING|python|0:42|sprint1|all|2026-08-17T15:37:15|1|/opt/venvs/e1/bin/python\n";
    let jobs = parse_queue(raw_queue, "sprint");
    assert_eq!(jobs.len(), 1);
    let job = &jobs[0];

    assert!(
        !job.interactive,
        "Python compute job must NOT be classified as an interactive shell"
    );
    assert!(
        !job.blocked_category(),
        "Python compute job must NOT be in blocked category"
    );

    let ledger = Ledger::default();
    let default_view = visible_jobs(vec![job.clone()], &ledger, HistoryMode::Live, false);
    assert_eq!(
        default_view.len(),
        1,
        "Python compute job must be visible in default live view"
    );
}

#[test]
fn deduplication_is_single_pass_and_cluster_scoped() {
    let mut jobs = Vec::new();
    let mut seen = HashSet::new();
    extend_unique(
        &mut jobs,
        &mut seen,
        [
            Job {
                cluster: "sprint".into(),
                id: "7".into(),
                ..Job::default()
            },
            Job {
                cluster: "sprint".into(),
                id: "7".into(),
                ..Job::default()
            },
            Job {
                cluster: "cispa".into(),
                id: "7".into(),
                ..Job::default()
            },
        ],
    );
    assert_eq!(jobs.len(), 2);
    assert_eq!(seen.len(), 2);
}

#[test]
fn archive_horizon_is_bounded_and_date_based() {
    assert_eq!(validated_archive_days(None), 365);
    assert_eq!(validated_archive_days(Some("30")), 30);
    assert_eq!(validated_archive_days(Some("0")), 365);
    assert_eq!(validated_archive_days(Some("999999")), 365);
    let epoch = OffsetDateTime::from_unix_timestamp(0).unwrap();
    assert_eq!(archive_start_at(epoch, 365), "1969-01-01");
}

#[test]
#[ignore = "release-mode performance budget"]
fn parses_one_hundred_thousand_accounting_rows_within_budget() {
    let mut input = String::with_capacity(14 * 1024 * 1024);
    for id in 1..=100_000 {
        use std::fmt::Write as _;
        writeln!(
            input,
            "{id}|COMPLETED|training|01:02:03|2026-08-11T17:00:00+02:00|0:0|4G|cpu=8,mem=16G|gpu"
        )
        .unwrap();
    }
    let started = std::time::Instant::now();
    let jobs = parse_recent(&input, "cispa");
    let elapsed = started.elapsed();
    assert_eq!(jobs.len(), 100_000);
    assert!(elapsed < Duration::from_millis(if cfg!(coverage) { 2000 } else { 500 }));
    eprintln!("parse 100k accounting rows: {elapsed:?}");
}

#[test]
fn extended_scheduler_fields_are_preserved() {
    let queued = parse_queue(
        "7|PENDING|train|0:00|Resources|gpu|2026-08-11T18:00:00|991\n",
        "cispa",
    );
    assert_eq!(queued[0].partition, "gpu");
    assert_eq!(queued[0].priority, "991");
    assert!(queued[0].insight().contains("estimated start"));

    let recent = parse_recent(
        "8|OUT_OF_MEMORY|train|1:00|2026-08-11T17:00:00+02:00|0:9|63G|gres/gpu=4|gpu\n",
        "cispa",
    );
    assert_eq!(recent[0].exit_code, "0:9");
    assert_eq!(recent[0].alloc_tres, "gres/gpu=4");
    assert_eq!(recent[0].insight(), "exit 0:9 · peak memory 63G");
}

#[test]
fn scheduler_query_lock_is_private_and_cross_process_exclusive() {
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("queue-cache.json");
    let first = query_lock(&cache).unwrap();
    let lock_path = cache.with_extension("query.lock");
    assert_eq!(
        fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let second = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    assert!(
        rustix::fs::flock(
            &second,
            rustix::fs::FlockOperation::NonBlockingLockExclusive
        )
        .is_err()
    );
    let _ = rustix::fs::flock(&first, rustix::fs::FlockOperation::Unlock);
    drop(first);
    assert!(
        rustix::fs::flock(
            &second,
            rustix::fs::FlockOperation::NonBlockingLockExclusive
        )
        .is_ok()
    );
}

mod history;

mod cache;
mod controller;

#[path = "tests/identity.rs"]
mod identity;
