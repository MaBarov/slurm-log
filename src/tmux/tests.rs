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
