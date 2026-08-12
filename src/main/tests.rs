use super::help_text;

#[test]
fn cli_help_is_scannable_and_documents_public_workflows() {
    let help = help_text();
    for section in [
        "USAGE",
        "START HERE",
        "VIEWS",
        "JOB & SCRIPT COMMANDS",
        "WORKSPACE & CACHE",
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
    ] {
        assert!(
            !help.contains(internal),
            "leaked internal command: {internal}"
        );
    }
}
