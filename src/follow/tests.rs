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
fn filter_log_preserves_ansi_escapes_and_progress_carriage_returns() {
    let mut output = Vec::new();
    let raw_log = b"\x1b[32mProgress: 10%\r\x1b[32mProgress: 20%\r\x1b[32mProgress: 100%\x1b[0m\n";
    filter_log(&raw_log[..], true, false, &mut output).unwrap();
    assert_eq!(
        output,
        b"\x1b[32mProgress: 10%\r\x1b[32mProgress: 20%\r\x1b[32mProgress: 100%\x1b[0m\n"
    );
}

#[test]
fn warning_filtering_reliably_hides_all_python_warning_classes_and_multiline_continuations() {
    let sample_log = b"\
INFO:root:Starting training run
/path/to/torch/cuda/__init__.py:123: UserWarning: CUDA initialization warning
  warnings.warn(
    'multi-line warning message',
    UserWarning
  )
/path/to/transformers/modeling.py:50: FutureWarning: Model class is deprecated
  warnings.warn('deprecated')
/path/to/lib.py:10: DeprecationWarning: fromstring is deprecated
  img = torch.fromstring(...)
/path/to/regex.py:5: SyntaxWarning: invalid escape sequence
  re.compile('\\s')
/path/to/net.py:80: RuntimeWarning: divide by zero encountered in log
  return np.log(x)
/path/to/io.py:90: ResourceWarning: unclosed file <_io.TextIOWrapper>
  pass
/path/to/mod.py:12: ImportWarning: Module was renamed
  import old_module
There are modules in the model that are kept in float32
  module.linear
  module.conv
=== warnings summary ===
tests/test_model.py:42
  UserWarning: inner summary
-- Docs: https://pytest.org/warnings
WARNING:root:Application level warning that must be kept
Epoch 1/10 complete - loss: 0.42
";

    // When show_warnings is FALSE: All Python warning blocks are stripped; application logs remain
    let mut hidden_out = Vec::new();
    filter_log(&sample_log[..], false, false, &mut hidden_out).unwrap();
    let hidden_text = String::from_utf8(hidden_out).unwrap();

    assert!(hidden_text.contains("INFO:root:Starting training run"));
    assert!(hidden_text.contains("WARNING:root:Application level warning that must be kept"));
    assert!(hidden_text.contains("Epoch 1/10 complete - loss: 0.42"));

    // Verify all warning lines and their continuations are hidden
    assert!(!hidden_text.contains("UserWarning: CUDA initialization"));
    assert!(!hidden_text.contains("multi-line warning message"));
    assert!(!hidden_text.contains("FutureWarning: Model class is deprecated"));
    assert!(!hidden_text.contains("DeprecationWarning: fromstring is deprecated"));
    assert!(!hidden_text.contains("SyntaxWarning: invalid escape sequence"));
    assert!(!hidden_text.contains("RuntimeWarning: divide by zero"));
    assert!(!hidden_text.contains("ResourceWarning: unclosed file"));
    assert!(!hidden_text.contains("ImportWarning: Module was renamed"));
    assert!(!hidden_text.contains("There are modules in the model"));
    assert!(!hidden_text.contains("module.linear"));
    assert!(!hidden_text.contains("=== warnings summary ==="));
    assert!(!hidden_text.contains("inner summary"));

    // When show_warnings is TRUE: EVERYTHING is shown verbatim without loss
    let mut shown_out = Vec::new();
    filter_log(&sample_log[..], true, false, &mut shown_out).unwrap();
    let shown_text = String::from_utf8(shown_out).unwrap();

    assert_eq!(shown_text, String::from_utf8_lossy(sample_log));
}

#[test]
fn warning_filtering_never_hides_interleaved_exceptions_or_tracebacks() {
    let sample = b"\
/path/to/lib.py:1: UserWarning: warning 1
  warnings.warn('w1')
Traceback (most recent call last):
  File 'train.py', line 42, in <module>
    main()
  File 'train.py', line 20, in main
    raise RuntimeError('CUDA out of memory')
RuntimeError: CUDA out of memory
/path/to/lib.py:2: FutureWarning: warning 2
  warnings.warn('w2')
";

    let mut out = Vec::new();
    filter_log(&sample[..], false, false, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();

    assert!(!text.contains("UserWarning: warning 1"));
    assert!(!text.contains("FutureWarning: warning 2"));
    assert!(text.contains("Traceback (most recent call last):"));
    assert!(text.contains("  File 'train.py', line 42, in <module>"));
    assert!(text.contains("  File 'train.py', line 20, in main"));
    assert!(text.contains("RuntimeError: CUDA out of memory"));
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
