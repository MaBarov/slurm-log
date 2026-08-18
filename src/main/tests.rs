use super::{help_text, open_pane_job, render_watch};

#[test]
fn cli_help_is_scannable_and_documents_public_workflows() {
    let help = help_text();
    for section in [
        "USAGE",
        "START HERE",
        "VIEWS",
        "JOB & SCRIPT COMMANDS",
        "WORKSPACE & CACHE",
        "INSTALLATION",
        "OPTIONS",
        "PICKER ESSENTIALS",
        "EXAMPLES",
    ] {
        assert!(
            help.lines().any(|line| line == section),
            "missing {section}"
        );
    }
    for workflow in [
        "slurm-log setup",
        "slurm-log JOB_ID",
        "details JOB_ID",
        "submit SCRIPT --cluster C",
        "cancel JOB_ID... --cluster C",
        "daemon start|status|stop",
        "update --binary FILE",
        "uninstall --purge",
        "--cluster NAME|all",
        "--show-log-warnings",
        "-h, --help",
        "-V, --version",
    ] {
        assert!(help.contains(workflow), "missing workflow: {workflow}");
    }
    assert!(help.ends_with('\n'));
    assert!(
        help.lines().all(|line| line.chars().count() <= 88),
        "help should fit comfortably in a standard terminal"
    );
}

#[test]
fn cli_help_hides_internal_worker_commands() {
    let help = help_text();
    for internal in [
        "setup-discover-worker",
        "bank-scan-worker",
        "pick-add",
        "toggle-details",
        "auto-monitor",
        "pane-follow",
        "initial-state",
        "suppress",
        "close-pane",
    ] {
        assert!(
            !help.contains(internal),
            "leaked internal command: {internal}"
        );
    }
}

#[test]
fn synthetic_open_panes_and_watch_frames_are_renderable() {
    let job = open_pane_job("cispa".into(), "42".into());
    assert_eq!(job.cluster, "cispa");
    assert_eq!(job.id, "42");
    assert_eq!(job.state, "OPEN");

    render_watch(&[job], &["scheduler temporarily unavailable".into()]);
}
