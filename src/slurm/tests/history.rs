use super::super::*;

fn completed(id: &str, age_seconds: i64) -> Job {
    Job {
        cluster: "alpha".into(),
        id: id.into(),
        state: "COMPLETED".into(),
        ended: (OffsetDateTime::now_utc() - TimeDuration::seconds(age_seconds))
            .format(&Rfc3339)
            .unwrap(),
        ..Job::default()
    }
}

fn visible_ids(jobs: &[Job], mode: HistoryMode) -> Vec<String> {
    visible_jobs(jobs.to_vec(), &Ledger::default(), mode, true)
        .into_iter()
        .map(|job| job.id)
        .collect()
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
        visible_jobs(
            vec![running.clone(), failed.clone()],
            &ledger,
            HistoryMode::Live,
            false,
        ),
        vec![running.clone()]
    );
    assert_eq!(
        visible_jobs(vec![failed.clone()], &ledger, HistoryMode::All, false),
        vec![failed.clone()]
    );
    ledger.dismissed.insert(failed.key(), "now".into());
    assert!(visible_jobs(vec![failed.clone()], &ledger, HistoryMode::Live, false).is_empty());
    assert_eq!(
        visible_jobs(vec![failed], &ledger, HistoryMode::All, false).len(),
        1
    );
    ledger.dismissed.insert(running.key(), "now".into());
    assert!(visible_jobs(vec![running.clone()], &ledger, HistoryMode::Live, false).is_empty());
    assert_eq!(
        visible_jobs(vec![running], &ledger, HistoryMode::All, false).len(),
        1
    );
}

#[test]
fn blocked_jobs_are_hidden_until_requested() {
    for job in [
        Job {
            cluster: "sprint".into(),
            id: "3".into(),
            state: "PENDING".into(),
            reason: "DependencyNeverSatisfied".into(),
            ..Job::default()
        },
        Job {
            cluster: "cispa".into(),
            id: "4".into(),
            state: "RUNNING".into(),
            interactive: true,
            ..Job::default()
        },
    ] {
        assert!(
            visible_jobs(
                vec![job.clone()],
                &Ledger::default(),
                HistoryMode::Live,
                false,
            )
            .is_empty()
        );
        assert_eq!(
            visible_jobs(
                vec![job.clone()],
                &Ledger::default(),
                HistoryMode::Live,
                true,
            ),
            vec![job]
        );
    }
}

#[test]
fn history_cycle_and_labels_cover_every_requested_window() {
    let sequence = [
        HistoryMode::Live,
        HistoryMode::Hours2,
        HistoryMode::Hours12,
        HistoryMode::Day1,
        HistoryMode::Week1,
        HistoryMode::All,
        HistoryMode::Live,
    ];
    for pair in sequence.windows(2) {
        assert_eq!(pair[0].next(), pair[1]);
    }
    assert_eq!(HistoryMode::Hours2.label(), "LAST 2h");
    assert_eq!(HistoryMode::Hours12.label(), "LAST 12h");
    assert_eq!(HistoryMode::Day1.label(), "LAST 1d");
    assert_eq!(HistoryMode::Week1.label(), "LAST 1w");
    assert_eq!(HistoryMode::All.label(), "ALL HISTORY");
    assert!(!HistoryMode::Live.scheduler_archive());
    assert!(HistoryMode::Hours2.scheduler_archive());
}

#[test]
fn history_windows_apply_exact_terminal_horizons_and_keep_active_jobs() {
    let jobs = vec![
        completed("one-hour", 60 * 60),
        completed("three-hours", 3 * 60 * 60),
        completed("eighteen-hours", 18 * 60 * 60),
        completed("two-days", 2 * 24 * 60 * 60),
        completed("eight-days", 8 * 24 * 60 * 60),
        Job {
            cluster: "alpha".into(),
            id: "running".into(),
            state: "RUNNING".into(),
            ..Job::default()
        },
    ];
    assert_eq!(
        visible_ids(&jobs, HistoryMode::Hours2),
        ["one-hour", "running"]
    );
    assert_eq!(
        visible_ids(&jobs, HistoryMode::Hours12),
        ["one-hour", "three-hours", "running"]
    );
    assert_eq!(
        visible_ids(&jobs, HistoryMode::Day1),
        ["one-hour", "three-hours", "eighteen-hours", "running"]
    );
    assert_eq!(
        visible_ids(&jobs, HistoryMode::Week1),
        [
            "one-hour",
            "three-hours",
            "eighteen-hours",
            "two-days",
            "running",
        ]
    );
    assert_eq!(visible_ids(&jobs, HistoryMode::All).len(), jobs.len());
}

#[test]
fn history_is_read_only_and_still_shows_previously_dismissed_jobs() {
    let job = completed("dismissed", 60 * 60);
    let mut ledger = Ledger::default();
    ledger.opened.insert(job.key(), "now".into());
    ledger.dismissed.insert(job.key(), "now".into());
    assert!(visible_jobs(vec![job.clone()], &ledger, HistoryMode::Live, false).is_empty());
    assert_eq!(
        visible_jobs(vec![job.clone()], &ledger, HistoryMode::Hours2, false),
        vec![job.clone()]
    );
    assert_eq!(
        visible_jobs(vec![job.clone()], &ledger, HistoryMode::All, false),
        vec![job]
    );
}

#[test]
fn pending_reasons_distinguish_blocked_dependency_failures_from_actionable_waits() {
    // 1. Blocked category: Dead dependencies that will never run
    let dead_dep = Job {
        cluster: "sprint".into(),
        id: "101".into(),
        state: "PENDING".into(),
        reason: "DependencyNeverSatisfied".into(),
        ..Job::default()
    };
    assert!(dead_dep.blocked_category());
    assert_eq!(
        dead_dep.insight(),
        "dependency can never be satisfied"
    );

    // 2. Non-blocked category: Actionable pending reasons that should remain visible in default view
    for (reason, expected_explanation) in [
        ("Priority", "waiting behind higher-priority jobs"),
        ("Resources", "waiting for requested resources"),
        ("Dependency", "waiting for a dependency"),
        ("QOSMaxJobsPerUserLimit", "waiting on an account or QOS limit"),
        ("AssocMaxJobsLimit", "waiting on an account or QOS limit"),
        ("ReqNodeNotAvail, UnavailableNodes:node01", "requested node is unavailable"),
        ("BeginTime", "waiting for its requested begin time"),
        ("Reservation", "waiting for a reservation"),
        ("Licenses", "waiting for a license"),
        ("None", "pending"),
        ("", "pending"),
    ] {
        let pending_job = Job {
            cluster: "sprint".into(),
            id: "102".into(),
            state: "PENDING".into(),
            reason: reason.into(),
            ..Job::default()
        };
        assert!(
            !pending_job.blocked_category(),
            "Reason '{reason}' should NOT be categorized as blocked"
        );
        assert!(
            pending_job.insight().starts_with(expected_explanation),
            "Reason '{reason}' insight should start with '{expected_explanation}', got '{}'",
            pending_job.insight()
        );

        // Actionable pending jobs MUST be visible in default live view
        let default_view = visible_jobs(
            vec![pending_job.clone()],
            &Ledger::default(),
            HistoryMode::Live,
            false,
        );
        assert_eq!(
            default_view,
            vec![pending_job],
            "Actionable pending job with reason '{reason}' must be visible in default view"
        );
    }
}

#[test]
fn heterogeneous_queue_correctly_partitions_live_and_blocked_jobs() {
    let batch_running = Job {
        cluster: "sprint".into(),
        id: "1".into(),
        state: "RUNNING".into(),
        name: "train_bert".into(),
        ..Job::default()
    };
    let batch_pending = Job {
        cluster: "sprint".into(),
        id: "2".into(),
        state: "PENDING".into(),
        name: "eval_bert".into(),
        reason: "Resources".into(),
        ..Job::default()
    };
    let dead_dep = Job {
        cluster: "cispa".into(),
        id: "3".into(),
        state: "PENDING".into(),
        name: "dead_chain".into(),
        reason: "DependencyNeverSatisfied".into(),
        ..Job::default()
    };
    let interactive_python = Job {
        cluster: "sprint".into(),
        id: "4".into(),
        state: "RUNNING".into(),
        name: "python".into(),
        interactive: true,
        ..Job::default()
    };
    let interactive_bash = Job {
        cluster: "cispa".into(),
        id: "5".into(),
        state: "RUNNING".into(),
        name: "bash".into(),
        interactive: true,
        ..Job::default()
    };

    let all_queue = vec![
        batch_running.clone(),
        batch_pending.clone(),
        dead_dep.clone(),
        interactive_python.clone(),
        interactive_bash.clone(),
    ];

    let ledger = Ledger::default();

    // 1. Default live view (show_blocked = false): only non-blocked batch jobs appear
    let live_default = visible_jobs(all_queue.clone(), &ledger, HistoryMode::Live, false);
    assert_eq!(live_default, vec![batch_running.clone(), batch_pending.clone()]);

    // 2. Blocked count calculation
    let eligible = visible_jobs(all_queue.clone(), &ledger, HistoryMode::Live, true);
    let blocked_count = eligible.iter().filter(|j| j.blocked_category()).count();
    assert_eq!(blocked_count, 3, "Expected 3 blocked jobs (1 dead dep + 2 interactive)");

    // 3. Blocked view (show_blocked = true, e.g. pressing 'b'): all 5 jobs appear
    let live_blocked = visible_jobs(all_queue.clone(), &ledger, HistoryMode::Live, true);
    assert_eq!(live_blocked.len(), 5);

    // 4. Suppressing an interactive allocation hides it from live blocked view
    let mut suppressed_ledger = Ledger::default();
    suppressed_ledger.dismissed.insert(interactive_python.key(), "now".into());

    let suppressed_live = visible_jobs(all_queue.clone(), &suppressed_ledger, HistoryMode::Live, true);
    assert_eq!(suppressed_live.len(), 4);
    assert!(!suppressed_live.iter().any(|j| j.id == interactive_python.id));

    // 5. In All/Archive history mode, suppressed interactive job still reappears in historical accounting
    let archive_view = visible_jobs(all_queue, &suppressed_ledger, HistoryMode::All, true);
    assert_eq!(archive_view.len(), 5);
    assert!(archive_view.iter().any(|j| j.id == interactive_python.id));
}

#[test]
fn interactive_interpreters_and_shells_across_various_paths_are_recognized() {
    // Verify various interactive command forms encountered in HPC environments
    for cmd in [
        "python",
        "python3",
        "/usr/bin/python3",
        "/storage1/mansur/venvs/e1/bin/python",
        "/opt/conda/bin/python3.10",
        "ipython",
        "/home/user/.local/bin/ipython",
        "bash",
        "/bin/bash",
        "sh",
        "/bin/sh",
        "zsh",
        "/bin/zsh",
        "fish",
        "tcsh",
        "csh",
        "julia",
        "/opt/julia/bin/julia",
        "R",
        "gdb",
        "cuda-gdb",
        "node",
        "matlab",
    ] {
        assert!(
            interactive_command(cmd),
            "Command '{cmd}' should be recognized as an interactive allocation command"
        );
    }

    // Verify batch commands and scripts are NOT treated as interactive
    for non_interactive in [
        "train.sbatch",
        "/path/to/experiment.sbatch",
        "slurm_script",
        "run.sh",
        "python train.py",
        "/usr/bin/python train_classifier.py --epochs 100",
        "bash submit.sh",
        "srun-worker",
    ] {
        assert!(
            !interactive_command(non_interactive),
            "Command '{non_interactive}' must NOT be recognized as an interactive shell"
        );
    }
}
