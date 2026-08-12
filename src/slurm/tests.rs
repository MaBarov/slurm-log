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
    let active = active_terminal_metadata(
        "42",
        "JobId=42 JobName=train StdOut=/logs/%x-%j.out JobState=RUNNING",
    );
    assert_eq!(
        resolve_terminal_metadata(active),
        (Some("/logs/train-42.out".into()), "train".into())
    );
    let unnamed = active_terminal_metadata("43", "JobId=43 StdOut=/dev/null");
    assert_eq!(resolve_terminal_metadata(unnamed), (None, "job".into()));

    let accounting = accounting_terminal_metadata(
        "320_7.batch|320_7.batch|array task|/logs/%A_%a_%j_%%_%q.out\n",
    )
    .unwrap();
    assert_eq!(
        resolve_terminal_metadata(accounting),
        (
            Some("/logs/320_7_320_7_%_%q.out".into()),
            "array task".into()
        )
    );
    assert!(accounting_terminal_metadata("only|three|fields").is_none());
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
    for command in ["/bin/zsh", "fish", "tcsh", "nu"] {
        assert!(interactive_command(command));
    }
    assert!(!interactive_command("python"));
    assert_eq!(queue_cache_name("cispa"), "queue-v2-cispa");
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
    assert!(elapsed < Duration::from_millis(500));
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
fn visibility_matches_live_archive_and_dismiss_rules() {
    let running = Job {
        cluster: "cispa".into(),
        id: "1".into(),
        state: "RUNNING".into(),
        ..Job::default()
    };
    let failed = Job {
        cluster: "cispa".into(),
        id: "2".into(),
        state: "FAILED".into(),
        ..Job::default()
    };
    let mut ledger = Ledger::default();
    ledger.opened.insert(failed.key(), "now".into());
    assert_eq!(
        visible_jobs(vec![running.clone(), failed.clone()], &ledger, 0, false),
        vec![running.clone()]
    );
    assert_eq!(
        visible_jobs(vec![failed.clone()], &ledger, 2, false),
        vec![failed.clone()]
    );
    ledger.dismissed.insert(failed.key(), "now".into());
    assert!(visible_jobs(vec![failed.clone()], &ledger, 0, false).is_empty());
    assert_eq!(
        visible_jobs(vec![failed.clone()], &ledger, 2, false),
        vec![failed]
    );
    ledger.dismissed.insert(running.key(), "now".into());
    assert!(visible_jobs(vec![running.clone()], &ledger, 0, false).is_empty());
    assert_eq!(
        visible_jobs(vec![running.clone()], &ledger, 2, false),
        vec![running]
    );
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
    assert!(second.try_lock_exclusive().is_err());
    drop(first);
    assert!(second.try_lock_exclusive().is_ok());
}

#[test]
fn blocked_jobs_are_hidden_until_requested() {
    let blocked = Job {
        cluster: "sprint".into(),
        id: "3".into(),
        state: "PENDING".into(),
        reason: "DependencyNeverSatisfied".into(),
        ..Job::default()
    };
    assert!(visible_jobs(vec![blocked.clone()], &Ledger::default(), 0, false).is_empty());
    assert_eq!(
        visible_jobs(vec![blocked.clone()], &Ledger::default(), 0, true),
        vec![blocked]
    );
    let interactive = Job {
        cluster: "cispa".into(),
        id: "4".into(),
        state: "RUNNING".into(),
        interactive: true,
        ..Job::default()
    };
    assert!(visible_jobs(vec![interactive.clone()], &Ledger::default(), 0, false).is_empty());
    assert_eq!(
        visible_jobs(vec![interactive.clone()], &Ledger::default(), 0, true),
        vec![interactive]
    );
}

#[test]
fn shared_job_cache_round_trips_and_invalidates() {
    let directory = tempfile::tempdir().unwrap();
    let config = Config {
        local_user: "local".into(),
        remote_user: "remote".into(),
        ssh_host: "host".into(),
        state_path: directory.path().join("state.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![crate::config::ClusterConfig {
            name: "cispa".into(),
            transport: "ssh".into(),
            user: "remote".into(),
            ssh_host: "host".into(),
            working_directory: PathBuf::from("/tmp"),
            accounting: false,
        }],
    };
    let path = cache_path(&config, "recent");
    let jobs = vec![Job {
        cluster: "cispa".into(),
        id: "42".into(),
        ..Job::default()
    }];
    store_jobs(&path, &jobs);
    assert_eq!(cached_jobs(&path, Duration::from_secs(3)), Some(jobs));
    assert_eq!(
        recent(&config, "cispa", false).unwrap(),
        Vec::<Job>::new(),
        "accounting-disabled clusters must return before invoking SSH or sacct"
    );
    assert!(accounting_warnings(&config, &["cispa"], false).is_empty());
    let warnings = accounting_warnings(&config, &["cispa"], true);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("completed jobs unavailable"));
    assert!(warnings[0].contains("only active squeue jobs"));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    invalidate_caches(&config);
    assert!(!path.exists());
}

#[test]
fn malformed_scheduler_output_is_ignored_without_panicking() {
    let input = "\n|||\nabc|RUNNING|name|1:00|node\n1.batch|RUNNING|step|1:00|node\n\
                     42|RUNNING|valid|00:01|node\n999999999999999999999999|FAILED|huge|x|\n";
    let jobs = parse_queue(input, "cispa");
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].id, "42");
}

#[test]
fn parsers_survive_deterministic_hostile_corpus() {
    let mut seed = 0x9e37_79b9_u32;
    for length in [0, 1, 2, 7, 31, 255, 4096, 65_535] {
        let mut input = String::with_capacity(length);
        for _ in 0..length {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            input.push(char::from_u32(1 + seed % 0x7e).unwrap());
        }
        let queued = parse_queue(&input, "cispa");
        let recent = parse_recent(&input, "cispa");
        assert!(queued.iter().all(|job| valid_job_id(&job.id)));
        assert!(recent.iter().all(|job| valid_job_id(&job.id)));
    }
}

#[test]
fn corrupt_or_stale_cache_is_a_miss() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache.json");
    fs::write(&path, b"not-json").unwrap();
    assert!(cached_jobs(&path, Duration::from_secs(60)).is_none());
    assert!(cached_jobs(&path, Duration::ZERO).is_none());
    fs::write(&path, [0xdd, 0xff, 0xff, 0xff, 0xff]).unwrap();
    assert!(cached_jobs(&path, Duration::from_secs(60)).is_none());
}

#[test]
fn messagepack_cache_length_guard_covers_all_sequence_headers() {
    assert_eq!(msgpack_sequence_len(&[0x90]), Some(0));
    assert_eq!(msgpack_sequence_len(&[0x9f]), Some(15));
    assert_eq!(msgpack_sequence_len(&[0xdc, 0x01, 0x00]), Some(256));
    assert_eq!(msgpack_sequence_len(&[0xdd, 0, 1, 0, 0]), Some(65_536));
    assert_eq!(msgpack_sequence_len(&[]), None);
    assert_eq!(msgpack_sequence_len(&[0xdc, 0]), None);
    assert_eq!(msgpack_sequence_len(b"not messagepack"), None);
}

#[test]
fn query_dimensions_are_strictly_bounded() {
    assert!(validate_query("both", "all").is_ok());
    assert!(validate_query("../../tmp", "all").is_err());
    assert!(validate_query("cispa", "arbitrary").is_err());
}

#[test]
fn oversized_cache_is_rejected_before_reading() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache.json");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_CACHE_BYTES + 1).unwrap();
    assert!(cached_jobs(&path, Duration::from_secs(60)).is_none());
}

#[test]
fn cache_lock_creates_parents_and_failed_writes_leave_no_file() {
    let directory = tempfile::tempdir().unwrap();
    let nested = directory.path().join("new/cache.json");
    let lock = query_lock(&nested).unwrap();
    assert!(nested.with_extension("query.lock").exists());
    drop(lock);

    let blocked_parent = directory.path().join("blocked");
    fs::write(&blocked_parent, b"not a directory").unwrap();
    let blocked = blocked_parent.join("cache.json");
    store_jobs(&blocked, &[Job::default()]);
    assert!(!blocked.exists());
    assert!(query_lock(&blocked).is_err());
}

#[test]
#[ignore = "release-mode performance budget"]
fn decodes_fifty_thousand_cached_jobs_within_budget() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("archive-cache.json");
    let jobs: Vec<_> = (0..50_000)
        .map(|id| Job {
            cluster: "cispa".into(),
            id: id.to_string(),
            state: "COMPLETED".into(),
            name: "archive-training".into(),
            elapsed: "01:23:45".into(),
            ended: "2026-08-12T00:00:00+02:00".into(),
            alloc_tres: "cpu=8,mem=32G,gres/gpu=2".into(),
            ..Job::default()
        })
        .collect();
    store_jobs(&path, &jobs);
    let started = std::time::Instant::now();
    let decoded = cached_jobs(&path, Duration::from_secs(60)).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(decoded.len(), jobs.len());
    assert!(elapsed < Duration::from_millis(250));
    eprintln!("decode 50k cached jobs: {elapsed:?}");
}
