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
