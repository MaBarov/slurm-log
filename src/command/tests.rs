use super::*;

#[test]
fn shell_quote_round_trips_adversarial_values() {
    for value in [
        "simple",
        "with spaces",
        "single'quote",
        "$(touch /tmp/never)",
        "; rm -rf nope",
        "line1\nline2",
        "unicode-λ",
    ] {
        let script = format!("printf %s {}", shell_quote(value));
        let output = text("sh", &["-c", &script]).unwrap();
        assert_eq!(output, value);
    }
}

#[test]
fn remote_scheduler_wrapper_uses_fixed_paths_and_quotes_arguments() {
    let command = remote_scheduler_command(
        "sbatch",
        &[
            "--parsable",
            "--clusters",
            "controller-a",
            "$(not-expanded)",
        ],
        Some(Path::new("/work space")),
    );
    assert!(
        command.starts_with("/usr/bin/env -i PATH=/usr/local/bin:/usr/bin:/bin HOME=/ /bin/sh -c ")
    );
    assert!(command.contains("/bin/sh"));
    assert!(!command.contains("${PATH"));
    assert!(!command.contains("$PATH"));
    assert!(command.contains("'$(not-expanded)'"));
}

#[test]
fn failed_commands_return_stderr() {
    let error = text("sh", &["-c", "printf denied >&2; exit 7"]).unwrap_err();
    assert!(format!("{error:#}").contains("denied"));
}

#[test]
fn multiplexed_ssh_args_carry_the_control_path_and_plain_fallback_does_not() {
    let multiplexed = ssh_args(true).join(" ");
    assert!(multiplexed.contains("ControlPath=~/.ssh/slurm-log-%C"));
    assert!(multiplexed.contains("ControlMaster=auto"));
    let plain = ssh_args(false).join(" ");
    assert!(!plain.contains("ControlPath"));
    assert!(!plain.contains("ControlMaster"));
}

#[test]
fn oversized_stdout_and_stderr_are_drained_then_rejected() {
    for script in [
        "i=0; while [ $i -lt 200 ]; do printf 0123456789; i=$((i+1)); done",
        "i=0; while [ $i -lt 200 ]; do printf 0123456789 >&2; i=$((i+1)); done",
    ] {
        let error = output_with_limit("sh", &["-c", script], 1024).unwrap_err();
        assert!(format!("{error:#}").contains("safety limit"));
    }
}

#[test]
fn bounded_input_command_preserves_bytes_and_working_directory() {
    let directory = tempfile::tempdir().unwrap();
    let output = text_with_input(
        "sh",
        &["-c", "printf '%s|' \"$PWD\"; cat"],
        b"exact\0bytes\n",
        Some(directory.path()),
    )
    .unwrap();
    assert_eq!(
        output.as_bytes(),
        [
            directory.path().as_os_str().as_encoded_bytes(),
            b"|exact\0bytes\n"
        ]
        .concat()
    );
}

#[test]
fn bounded_input_command_reports_failures_and_output_overflow() {
    let error = text_with_input_limit(
        "sh",
        &["-c", "cat >/dev/null; printf rejected >&2; exit 9"],
        b"input",
        None,
        1024,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("rejected"));

    let error = text_with_input_limit(
        "sh",
        &["-c", "cat >/dev/null; printf 0123456789"],
        b"input",
        None,
        4,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("safety limit"));
}

#[test]
fn deadline_kills_descendants_that_hold_the_output_pipe() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("descendant.pid");
    let started = Instant::now();
    let output = output_with_limit_and_timeout(
        "sh",
        &[
            "-c",
            "sleep 30 & printf '%s' \"$!\" > \"$1\"; printf ready",
            "sh",
            pid_file.to_str().unwrap(),
        ],
        1024,
        Duration::from_millis(250),
    )
    .unwrap();
    assert_eq!(output.stdout, b"ready");
    assert!(started.elapsed() < Duration::from_secs(2));
    let pid = std::fs::read_to_string(pid_file).unwrap();
    let process = Path::new("/proc").join(pid.trim());
    for _ in 0..20 {
        if !process.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!process.exists(), "background descendant survived cleanup");
}
