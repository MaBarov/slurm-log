use super::*;

#[test]
fn warning_filter_does_not_hide_exceptions() {
    let input = b"FutureWarning: old api\n  warnings.warn(x)\nTraceback (most recent call last):\nValueError: boom\n";
    let mut output = Vec::new();
    filter_log(&input[..], false, false, &mut output).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("ValueError: boom"));
    assert!(!text.contains("FutureWarning:"));
    assert!(!text.contains("warnings.warn"));
}

#[test]
fn warning_toggle_shows_warning_records() {
    let input = b"FutureWarning: old api\nValueError: boom\n";
    let mut output = Vec::new();
    filter_log(&input[..], true, false, &mut output).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("FutureWarning:"));
    assert!(text.contains("ValueError: boom"));
}

#[test]
fn warning_filter_removes_pytest_summaries_and_library_continuations() {
    let input = b"=== warnings summary ===\nignored detail\n-- Docs: https://pytest.org\nkept after summary\nThere are modules in float32 kept in float32\n  continuation\nordinary line\n";
    let mut output = Vec::new();
    filter_log(&input[..], false, false, &mut output).unwrap();
    assert_eq!(
        String::from_utf8(output).unwrap(),
        "kept after summary\nordinary line\n"
    );
}

#[test]
fn terminal_output_uses_crlf_to_avoid_staircase_logs() {
    let mut output = Vec::new();
    filter_log(&b"first\nsecond\n"[..], true, true, &mut output).unwrap();
    assert_eq!(output, b"first\r\nsecond\r\n");
}

#[test]
fn enter_accepts_canonical_and_raw_terminal_endings() {
    assert!(enter_byte(b'\n'));
    assert!(enter_byte(b'\r'));
    assert!(!enter_byte(b' '));
}

#[test]
fn fallback_enter_reader_accepts_both_endings_and_eof() {
    read_until_enter(&b"ignored\nremaining"[..]);
    read_until_enter(&b"ignored\rremaining"[..]);
    read_until_enter(&b"no terminator"[..]);
}

#[test]
fn interactive_snapshots_update_missing_counts_without_network_access() {
    let mut current = Job {
        id: "42".into(),
        state: "PENDING".into(),
        ..Job::default()
    };
    let mut missing = 3;
    apply_monitor_snapshot(
        &mut current,
        "42",
        &mut missing,
        Ok((
            vec![Job {
                id: "42".into(),
                state: "RUNNING".into(),
                ..Job::default()
            }],
            crate::state::Ledger::default(),
            Vec::new(),
        )),
    );
    assert!(current.running());
    assert_eq!(missing, 0);

    apply_monitor_snapshot(
        &mut current,
        "42",
        &mut missing,
        Ok((Vec::new(), crate::state::Ledger::default(), Vec::new())),
    );
    assert_eq!(missing, 1);
    apply_monitor_snapshot(
        &mut current,
        "42",
        &mut missing,
        Err(anyhow::anyhow!("offline scheduler outage")),
    );
    assert_eq!(missing, 0);
}

#[test]
fn close_prompt_accepts_only_enter_key_events() {
    assert!(close_key(KeyCode::Enter));
    assert!(!close_key(KeyCode::Char(' ')));
    assert!(!close_key(KeyCode::Esc));
}

#[test]
fn long_lived_followers_never_consume_query_mux_channels() {
    assert!(FOLLOWER_SSH_OPTIONS.contains(&"ControlMaster=no"));
    assert!(FOLLOWER_SSH_OPTIONS.contains(&"ControlPath=none"));
    assert!(!FOLLOWER_SSH_OPTIONS.contains(&"ControlMaster=auto"));
}

#[test]
fn interactive_monitor_explains_missing_log_and_safe_close() {
    let frame = interactive_frame(
        &Job {
            cluster: "cispa".into(),
            id: "42".into(),
            name: "shell".into(),
            state: "RUNNING".into(),
            elapsed: "00:12".into(),
            partition: "gpu".into(),
            reason: "node-a".into(),
            interactive: true,
            ..Job::default()
        },
        false,
    );
    assert!(frame.contains("INTERACTIVE ALLOCATION  cispa:42  shell"));
    assert!(frame.contains("BatchFlag=0"));
    assert!(frame.contains("another PTY cannot be mirrored"));
    assert!(frame.contains("allocation keeps running"));
    assert!(frame.contains("Ctrl-b i details"));
    assert!(!frame.replace("\r\n", "").contains('\n'));
}

#[test]
fn ended_interactive_monitor_waits_for_enter() {
    let frame = interactive_frame(
        &Job {
            interactive: true,
            ..Job::default()
        },
        true,
    );
    assert!(frame.contains("allocation has ended"));
    assert!(frame.contains("Press Enter"));
}

#[test]
fn pending_scheduler_lag_promises_automatic_log_attachment() {
    let frame = interactive_frame(
        &Job {
            cluster: "local".into(),
            id: "42".into(),
            name: "train".into(),
            state: "PENDING".into(),
            ..Job::default()
        },
        false,
    );
    assert!(frame.contains("WAITING FOR LOG  local:42  train"));
    assert!(frame.contains("will attach automatically"));
    assert!(!frame.contains("INTERACTIVE ALLOCATION"));
}

#[test]
fn completion_messages_distinguish_failure_follower_loss_and_success() {
    let job = Job {
        id: "42".into(),
        ..Job::default()
    };
    let failed = Job {
        state: "OUT_OF_MEMORY".into(),
        exit_code: "0:9".into(),
        max_rss: "16G".into(),
        ..Job::default()
    };
    let message = completion_message(&job, &failed, false, 1);
    assert!(message.contains("failed: OUT_OF_MEMORY"));
    assert!(message.contains("peak memory 16G"));

    let failed_without_insight = Job {
        state: "FAILED".into(),
        ..Job::default()
    };
    assert_eq!(
        completion_message(&job, &failed_without_insight, false, 1),
        "Job 42 failed: FAILED"
    );
    assert_eq!(
        completion_message(&job, &Job::default(), true, -15),
        "Job 42 log follower stopped (status -15)"
    );
    assert_eq!(
        completion_message(&job, &Job::default(), false, 0),
        "Job 42 finished"
    );
}

#[test]
fn queue_observation_requires_two_absences_and_detects_pending_start() {
    let pending = Job {
        id: "42".into(),
        state: "PENDING".into(),
        ..Job::default()
    };
    let running = Job {
        id: "42".into(),
        state: "RUNNING".into(),
        ..Job::default()
    };
    assert_eq!(observe_queue(&pending, &[running], 1), (0, true, false));
    assert_eq!(observe_queue(&pending, &[], 0), (1, false, false));
    assert_eq!(observe_queue(&pending, &[], 1), (2, false, true));
    assert_eq!(
        observe_queue(&pending, &[], u8::MAX),
        (u8::MAX, false, true)
    );
}

#[test]
fn supervision_handles_start_outage_disappearance_and_child_kill_offline() {
    let directory = tempfile::tempdir().unwrap();
    let config = Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: String::new(),
        state_path: directory.path().join("state.json"),
        executable: "slurm-log".into(),
        sbatch_banks: Vec::new(),
        clusters: Vec::new(),
    };
    let child = Command::new("sh").args(["-c", "sleep 1"]).spawn().unwrap();
    let display = thread::spawn(|| {});
    let pending = Job {
        id: "42".into(),
        state: "PENDING".into(),
        ..Job::default()
    };
    let mut calls = 0;
    let code = supervise_follower(
        &config,
        &pending,
        false,
        child,
        display,
        Arc::new(AtomicBool::new(false)),
        Duration::from_millis(1),
        || {
            calls += 1;
            match calls {
                1 => Ok(vec![Job {
                    id: "42".into(),
                    state: "RUNNING".into(),
                    ..Job::default()
                }]),
                2 => Err(anyhow::anyhow!("scheduler outage")),
                _ => Ok(Vec::new()),
            }
        },
    )
    .unwrap();
    assert_eq!(code, -15);
    assert!(calls >= 4);
}

#[test]
fn supervision_returns_shell_interrupt_status_without_polling_scheduler() {
    let directory = tempfile::tempdir().unwrap();
    let config = Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: String::new(),
        state_path: directory.path().join("state.json"),
        executable: "slurm-log".into(),
        sbatch_banks: Vec::new(),
        clusters: Vec::new(),
    };
    let child = Command::new("sh").args(["-c", "sleep 1"]).spawn().unwrap();
    let interrupted = Arc::new(AtomicBool::new(true));
    let code = supervise_follower(
        &config,
        &Job::default(),
        false,
        child,
        thread::spawn(|| {}),
        interrupted,
        Duration::from_secs(1),
        || panic!("interrupted follower queried scheduler"),
    )
    .unwrap();
    assert_eq!(code, 130);
}

#[test]
fn monitor_render_skips_unchanged_frames() {
    let mut previous = String::from("unchanged");
    render_monitor("unchanged", &mut previous).unwrap();
    assert_eq!(previous, "unchanged");
}
