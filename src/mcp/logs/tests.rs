use super::*;

#[test]
fn sanitizer_handles_invalid_utf8_ansi_and_controls() {
    let value = sanitize(b"ok\x1b[31mred\x1b[0m\0\xff\nnext\tline");
    assert_eq!(value, "okred�\nnext\tline");
}

#[test]
fn cursors_reject_wrong_shapes_and_tail_is_line_bounded() {
    let generation = "a".repeat(64);
    assert_eq!(parse_cursor(&make_cursor(&generation, 9)).unwrap().1, 9);
    assert!(parse_cursor("v1:no:9").is_err());
    assert_eq!(tail_lines(b"one\ntwo\nthree\n", 2), b"two\nthree\n");
}

#[test]
fn exception_mode_keeps_complete_short_tracebacks() {
    let text = "noise\nTraceback (most recent call last):\n  File x\nValueError: bad\n\nnoise";
    let value = filter_text(text, "exceptions");
    assert!(value.contains("Traceback"));
    assert!(value.contains("ValueError"));
    assert!(!value.starts_with("noise"));
}

fn classes(values: &[Value]) -> Vec<&str> {
    values
        .iter()
        .filter_map(|value| value["classification"].as_str())
        .collect()
}

#[test]
fn diagnosis_classifies_every_supported_failure_family() {
    let log = LogData {
        status: "available".into(),
        bytes: b"evidence".to_vec(),
        ..LogData::default()
    };
    let text = concat!(
        "Traceback (most recent call last):\n",
        "thread 'main' panicked at source\n",
        "CUDA out of memory\n",
        "NCCL error\n",
        "AssertionError\n",
        "loss NaN\n",
        "node failure\n"
    );
    let job = crate::model::Job {
        state: "OUT_OF_MEMORY".into(),
        exit_code: "1:9".into(),
        ..crate::model::Job::default()
    };
    let found = findings(Some(&job), None, &log, text);
    let found = classes(&found);
    for expected in [
        "python_traceback",
        "rust_panic",
        "cuda_out_of_memory",
        "nccl_error",
        "assertion_failure",
        "nan_or_inf",
        "signal",
        "slurm_out_of_memory",
        "node_failure",
    ] {
        assert!(found.contains(&expected), "missing {expected}");
    }

    let timeout = crate::model::Job {
        state: "TIMEOUT".into(),
        ..crate::model::Job::default()
    };
    assert!(classes(&findings(Some(&timeout), None, &log, "")).contains(&"slurm_timeout"));
}

#[test]
fn diagnosis_distinguishes_pending_missing_and_silent_logs() {
    let pending = crate::model::Job {
        state: "PENDING".into(),
        reason: "DependencyNeverSatisfied".into(),
        ..crate::model::Job::default()
    };
    let pending_log = LogData {
        status: "pending_log".into(),
        ..LogData::default()
    };
    let found = findings(Some(&pending), None, &pending_log, "");
    let found = classes(&found);
    assert!(found.contains(&"pending_cause"));
    assert!(found.contains(&"dependency_failure"));
    assert!(found.contains(&"pending_log"));

    let no_stdout = LogData {
        status: "no_stdout".into(),
        ..LogData::default()
    };
    assert!(classes(&findings(None, None, &no_stdout, "")).contains(&"no_stdout"));
    let silent = LogData {
        status: "available".into(),
        ..LogData::default()
    };
    let found = findings(None, None, &silent, "");
    assert!(classes(&found).contains(&"no_recent_output"));
    assert!(
        found[0]["practical_check"]
            .as_str()
            .unwrap()
            .contains("without assuming buffering")
    );
}
