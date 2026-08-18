use super::*;

fn job(id: &str, state: &str) -> Job {
    Job {
        cluster: "cispa".into(),
        id: id.into(),
        state: state.into(),
        ..Job::default()
    }
}
fn map(jobs: Vec<Job>) -> HashMap<(String, String), Job> {
    jobs.into_iter()
        .map(|job| ((job.cluster.clone(), job.id.clone()), job))
        .collect()
}
#[test]
fn auto_add_baselines_existing_running_jobs() {
    let observed = map(vec![job("1", "RUNNING"), job("2", "PENDING")]);
    assert!(monitor_additions(&observed, &observed).is_empty());
    let current = map(vec![
        job("1", "RUNNING"),
        job("2", "RUNNING"),
        job("3", "PENDING"),
    ]);
    let additions = monitor_additions(&observed, &current);
    assert_eq!(
        additions
            .iter()
            .map(|job| job.id.as_str())
            .collect::<HashSet<_>>(),
        HashSet::from(["2", "3"])
    );
    let mut interactive = job("4", "RUNNING");
    interactive.interactive = true;
    let current = map(vec![interactive]);
    assert!(monitor_additions(&HashMap::new(), &current).is_empty());
}

#[test]
fn monitor_state_reports_start_failure_and_new_active_jobs() {
    let mut state = MonitorState::new(vec![job("pending", "PENDING"), job("fail", "PENDING")]);
    let update = state.update(
        vec![
            job("pending", "RUNNING"),
            job("fail", "FAILED"),
            job("new", "PENDING"),
        ],
        &[],
    );
    assert_eq!(update.additions.len(), 2);
    assert!(
        update
            .events
            .iter()
            .any(|event| matches!(event, MonitorEvent::Started(job) if job.id == "pending"))
    );
    assert!(
        update
            .events
            .iter()
            .any(|event| matches!(event, MonitorEvent::Failed(job) if job.id == "fail"))
    );
}

#[test]
fn monitor_state_requires_two_clean_misses_before_pending_vanishes() {
    let mut state = MonitorState::new(vec![job("pending", "PENDING")]);
    assert!(state.update(Vec::new(), &[]).events.is_empty());
    let update = state.update(Vec::new(), &[]);
    assert!(matches!(
        update.events.as_slice(),
        [MonitorEvent::Vanished(job)] if job.id == "pending"
    ));
    assert!(state.update(Vec::new(), &[]).events.is_empty());
}

#[test]
fn monitor_state_does_not_treat_cluster_outage_as_job_vanishing() {
    let mut state = MonitorState::new(vec![job("pending", "PENDING")]);
    for _ in 0..3 {
        let update = state.update(Vec::new(), &["CISPA scheduler unavailable".into()]);
        assert!(update.events.is_empty());
    }
    // Seeing the pending job again resets its consecutive-miss counter.
    state.update(vec![job("pending", "PENDING")], &[]);
    assert!(state.update(Vec::new(), &[]).events.is_empty());
}

#[test]
fn one_or_zero_log_panes_are_safe_to_close_without_confirmation() {
    assert!(!confirmation_needed(0));
    assert!(!confirmation_needed(1));
    assert!(confirmation_needed(2));
}

#[test]
fn pane_labels_are_one_batched_tmux_transaction() {
    let mut named = job("42", "RUNNING");
    named.name = "training-run".into();
    let args = label_args("%7", &named);
    assert_eq!(args.iter().filter(|value| value.as_str() == ";").count(), 3);
    assert!(
        args.windows(2)
            .any(|pair| { pair[0] == "@slurm_log_job_name" && pair[1] == "training-run" })
    );
    assert_eq!(args.last().map(String::as_str), Some("cispa:42"));
    let unresolved = label_args("%8", &job("43", "RUNNING"));
    assert!(
        !unresolved
            .iter()
            .any(|value| value == "@slurm_log_job_name")
    );
}

#[test]
fn pane_job_names_sanitize_metadata() {
    assert_eq!(pane_job_name("  train\u{1b}[2J\nrun  "), "train[2Jrun");
    assert_eq!(pane_job_name("\n\t"), "Slurm job");
}

#[test]
fn persistent_status_names_the_focused_job() {
    let format = persistent_job_status_format();
    assert!(format.contains("#{@slurm_log_job_name}"));
    assert!(format.contains("#{@slurm_log_job_id}"));
    assert!(format.contains("Slurm job"));
    assert!(!format.contains("pane_title"));
    assert!(!format.contains("window_name"));
}

#[test]
fn reconciliation_removes_old_panes_before_additions() {
    let current = vec![
        Pane {
            id: "%1".into(),
            cluster: "cispa".into(),
            job_id: "old".into(),
        },
        Pane {
            id: "%2".into(),
            cluster: "cispa".into(),
            job_id: "keep".into(),
        },
    ];
    let desired = HashSet::from([
        ("cispa".into(), "keep".into()),
        ("cispa".into(), "new".into()),
    ]);
    let (remove_first, anchor) = obsolete_panes(&current, &desired);
    assert_eq!(
        remove_first
            .iter()
            .map(|pane| pane.id.as_str())
            .collect::<Vec<_>>(),
        ["%1"]
    );
    assert!(anchor.is_none());
}

#[test]
fn reconciliation_keeps_one_anchor_for_a_total_replacement() {
    let current = vec![Pane {
        id: "%1".into(),
        cluster: "cispa".into(),
        job_id: "old".into(),
    }];
    let desired = HashSet::from([("cispa".into(), "new".into())]);
    let (remove_first, anchor) = obsolete_panes(&current, &desired);
    assert!(remove_first.is_empty());
    assert_eq!(anchor.map(|pane| pane.id.as_str()), Some("%1"));
}

#[test]
fn empty_workspace_split_arguments_and_startup_errors_are_deterministic() {
    let config = Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: String::new(),
        state_path: "/tmp/slurm-log-tmux-test.json".into(),
        executable: "slurm-log".into(),
        sbatch_banks: Vec::new(),
        clusters: Vec::new(),
    };
    assert_eq!(open(&config, &[], 50, false).unwrap(), 0);
    assert!(
        reconcile(&config, "session", &[], 50, false)
            .unwrap_err()
            .to_string()
            .contains("at least one log")
    );
    let args = split_watcher_args(&config, "session", &job("42", "RUNNING"), 77, true);
    assert_eq!(
        &args[..7],
        [
            "split-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            "session"
        ]
    );
    assert!(args.iter().any(|value| value == "--show-log-warnings"));
    assert!(args.iter().any(|value| value == "77"));
    assert!(
        first_watcher_error(b"")
            .to_string()
            .contains("respawn failed")
    );
    assert!(
        first_watcher_error(b"broken\n")
            .to_string()
            .contains("broken")
    );
    assert!(
        split_watcher_error(b"", 1, 3)
            .to_string()
            .contains("split failed")
    );
    assert!(
        split_watcher_error(b"too small", 2, 3)
            .to_string()
            .contains("too small")
    );
}

#[test]
fn watcher_command_constructs_proper_follower_invocation() {
    let config = Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: String::new(),
        state_path: "/tmp/slurm-log-tmux-test.json".into(),
        executable: "slurm-log".into(),
        sbatch_banks: Vec::new(),
        clusters: Vec::new(),
    };
    let j = job("99", "RUNNING");
    let cmd = watcher(&config, &j, 100, true);
    assert_eq!(cmd[0], "slurm-log");
    assert!(cmd.iter().any(|arg| arg == "--pane-follow"));
    assert!(cmd.iter().any(|arg| arg == "cispa"));
    assert!(cmd.iter().any(|arg| arg == "99"));
    assert!(cmd.iter().any(|arg| arg == "100"));
    assert!(cmd.iter().any(|arg| arg == "--show-log-warnings"));

    let quiet_cmd = watcher(&config, &j, 50, false);
    assert!(!quiet_cmd.iter().any(|arg| arg == "--show-log-warnings"));
    assert!(quiet_cmd.iter().any(|arg| arg == "50"));
}

#[test]
fn obsolete_panes_handles_partial_and_empty_transitions() {
    let current = vec![
        Pane {
            id: "%1".into(),
            cluster: "cispa".into(),
            job_id: "10".into(),
        },
        Pane {
            id: "%2".into(),
            cluster: "sprint".into(),
            job_id: "20".into(),
        },
    ];

    // When desired set matches all current, nothing to remove
    let all_kept = HashSet::from([
        ("cispa".into(), "10".into()),
        ("sprint".into(), "20".into()),
    ]);
    let (remove, anchor) = obsolete_panes(&current, &all_kept);
    assert!(remove.is_empty());
    assert!(anchor.is_none());

    // When one cluster job is dropped, only that pane is removed
    let partial = HashSet::from([("cispa".into(), "10".into())]);
    let (remove, anchor) = obsolete_panes(&current, &partial);
    assert_eq!(remove.len(), 1);
    assert_eq!(remove[0].id, "%2");
    assert!(anchor.is_none());
}
