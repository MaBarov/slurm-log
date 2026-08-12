use super::*;

use crate::config::ClusterConfig;
use std::path::PathBuf;
fn job(id: &str, name: &str) -> Job {
    Job {
        cluster: "cispa".into(),
        id: id.into(),
        state: "COMPLETED".into(),
        name: name.into(),
        ..Job::default()
    }
}

fn multi_cluster_config() -> Config {
    Config {
        local_user: "alice".into(),
        remote_user: "alice".into(),
        ssh_host: String::new(),
        state_path: PathBuf::from("/tmp/slurm-log-ui-test.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: ["sprint", "cispa"]
            .into_iter()
            .map(|name| ClusterConfig {
                name: name.into(),
                transport: "local".into(),
                user: "alice".into(),
                ssh_host: String::new(),
                working_directory: PathBuf::from("/tmp"),
                accounting: true,
            })
            .collect(),
    }
}

#[test]
fn cluster_cycle_includes_all_and_wraps_in_both_directions() {
    let config = multi_cluster_config();
    assert_eq!(cycle_cluster(&config, "both", false), "sprint");
    assert_eq!(cycle_cluster(&config, "sprint", false), "cispa");
    assert_eq!(cycle_cluster(&config, "cispa", false), "all");
    assert_eq!(cycle_cluster(&config, "all", true), "cispa");
}
#[test]
fn groups_collapse_and_expand_by_job_name() {
    let jobs = vec![job("12", "train"), job("11", "train"), job("10", "eval")];
    let indices = vec![0, 1, 2];
    let collapsed = grouped_rows(&jobs, &indices, &HashSet::new());
    assert_eq!(collapsed.len(), 2);
    assert!(
        collapsed
            .iter()
            .any(|row| row.name == "train" && row.job.is_none() && row.members.len() == 2)
    );
    let expanded = grouped_rows(&jobs, &indices, &HashSet::from(["train".into()]));
    assert_eq!(expanded.len(), 4);
    assert!(
        expanded
            .iter()
            .any(|row| row.name == "train" && row.expanded)
    );
    assert_eq!(expanded.iter().filter(|row| row.nested).count(), 2);
    assert!(
        expanded
            .iter()
            .filter(|row| row.nested)
            .all(|row| row.job.is_some())
    );
}

#[test]
fn popup_styles_keep_color_and_focus_in_one_incremental_line() {
    assert_eq!(
        popup_styled("job".into(), Some(32), false),
        "\x1b[32mjob\x1b[0m"
    );
    assert_eq!(
        popup_styled("job".into(), Some(33), true),
        "\x1b[7m\x1b[33mjob\x1b[0m"
    );
}

#[test]
fn compact_rows_overwrite_the_width_without_clear_sequences() {
    assert_eq!(fit_popup_line("job", 6), "job   ");
    assert_eq!(fit_popup_line("archive", 4), "arch");
    assert_eq!(fit_popup_line("", 3), "   ");
    assert_eq!(clip_line("abcdef", 5), "abcd");
    let (primary, secondary) = compact_commands(true);
    assert!(primary.contains("Enter apply"));
    assert!(primary.contains("? help"));
    assert!(secondary.contains("b blocked"));
    assert!(secondary.contains("A auto"));
}

#[test]
fn pending_picker_name_does_not_append_scheduler_explanation() {
    let pending = Job {
        state: "PENDING".into(),
        name: "training".into(),
        reason: "Resources".into(),
        start_time: "2026-08-11T18:00:00".into(),
        ..Job::default()
    };
    assert_eq!(display_name(&pending), "training");
    assert!(pending.insight().contains("estimated start"));
}

#[test]
fn field_search_is_case_insensitive_without_joining_fields() {
    let mut value = job("42", "MixedCaseExperiment");
    value.partition = "GPU-Long".into();
    assert!(job_matches(&value, "mixedcase"));
    assert!(job_matches(&value, "gpu-long"));
    assert!(!job_matches(&value, "not-present"));
}

#[test]
#[ignore = "release-mode performance budget"]
fn searches_one_hundred_thousand_jobs_within_budget() {
    let jobs: Vec<_> = (0..100_000)
        .map(|id| job(&id.to_string(), &format!("experiment-{}", id % 500)))
        .collect();
    let started = Instant::now();
    let matches = jobs
        .iter()
        .filter(|job| job_matches(job, "experiment-499"))
        .count();
    assert_eq!(matches, 200);
    assert!(started.elapsed() < Duration::from_millis(150));
}

#[test]
fn compact_header_points_to_static_shortcuts_page() {
    let (header, primary, secondary) = header_lines(false, 2, true, false, false, "cispa");
    assert!(header.contains("ARCHIVE"));
    assert!(header.contains("cluster cispa"));
    assert!(primary.contains("CLUSTER Tab/Shift-Tab"));
    assert!(primary.contains("HELP ?"));
    assert!(secondary.contains("b blocked"));
    assert!(secondary.contains("A auto-add"));
    assert!(secondary.contains("d dismiss"));
}

#[test]
fn toggle_notices_are_explicit_and_expire_after_about_1500ms() {
    let before = Instant::now();
    let mut notice = None;
    set_notice(&mut notice, "Blocked and interactive jobs shown");
    let (message, expires) = notice.unwrap();
    assert_eq!(message, "Blocked and interactive jobs shown");
    let lifetime = expires.saturating_duration_since(before);
    assert!(lifetime >= Duration::from_millis(1_500));
    assert!(lifetime < Duration::from_millis(1_550));
    assert!(footer_text(4, 1, "", "", &message).contains(&message));
}

#[test]
fn blocked_count_summary_is_quiet_and_actionable() {
    assert_eq!(blocked_summary(3, false), " | blocked: 3 (b to show)");
    assert_eq!(blocked_summary(3, true), " | blocked: 3 (shown)");
}

#[test]
fn stop_confirmation_ignores_non_decision_events() {
    assert_eq!(cancel_confirmation_choice(KeyCode::Char('y')), Some(true));
    assert_eq!(cancel_confirmation_choice(KeyCode::Esc), Some(false));
    assert_eq!(cancel_confirmation_choice(KeyCode::Char('a')), None);
}

#[test]
fn group_rows_are_quiet_and_do_not_use_the_old_group_badge() {
    let row = Row {
        name: "training".into(),
        job: None,
        members: vec![0, 1, 2],
        nested: false,
        expanded: true,
    };
    let text = group_row_text(&row, false, false);
    assert_eq!(text, "    ▾    3 runs  ·  training");
    assert!(!text.contains("GROUP"));
}

#[test]
fn help_reference_covers_picker_and_workspace_controls() {
    let help = help_lines().join("\n").to_ascii_lowercase();
    for topic in [
        "navigation",
        "selection",
        "views",
        "workspace",
        "auto-add",
        "right-click",
    ] {
        assert!(help.contains(topic), "missing help topic: {topic}");
    }
}

#[test]
fn help_reference_aligns_every_command_description() {
    for line in help_lines().iter().filter(|line| line.starts_with("  ")) {
        assert_eq!(
            line.chars().nth(19),
            Some(' '),
            "missing key/description gutter: {line:?}"
        );
        assert!(
            line.chars()
                .nth(20)
                .is_some_and(|character| character != ' '),
            "description does not begin in column 21: {line:?}"
        );
    }
}

#[test]
fn help_reference_does_not_merge_distinct_actions() {
    let help = help_lines().join("\n");
    for command in [
        "  b                 Toggle blocked and interactive jobs",
        "  r                 Refresh the scheduler now",
        "  w                 Toggle scheduler notices",
        "  W                 Include warnings in opened log panes",
        "  x                 Close the current pane",
        "  z                 Zoom / restore the current pane",
    ] {
        assert!(help.contains(command), "missing command: {command}");
    }
}

#[test]
fn grouping_order_is_deterministic_for_equal_statuses() {
    let jobs = vec![job("10", "zeta"), job("10", "alpha"), job("10", "middle")];
    let indices = vec![0, 1, 2];
    let first: Vec<_> = grouped_rows(&jobs, &indices, &HashSet::new())
        .into_iter()
        .map(|row| row.name)
        .collect();
    let second: Vec<_> = grouped_rows(&jobs, &indices, &HashSet::new())
        .into_iter()
        .map(|row| row.name)
        .collect();
    assert_eq!(first, second);
    assert_eq!(first, ["alpha", "middle", "zeta"]);
}

#[test]
#[ignore = "release-mode performance budget"]
fn groups_twenty_thousand_archive_jobs_within_budget() {
    let jobs: Vec<_> = (0..20_000)
        .map(|id| job(&id.to_string(), &format!("experiment-{}", id % 200)))
        .collect();
    let indices: Vec<_> = (0..jobs.len()).collect();
    let started = Instant::now();
    let rows = grouped_rows(&jobs, &indices, &HashSet::new());
    assert_eq!(rows.len(), 200);
    assert!(started.elapsed() < Duration::from_millis(100));
}
