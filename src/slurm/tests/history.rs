use super::super::*;

fn completed(id: &str, age_seconds: i64) -> Job {
    Job {
        cluster: "alpha".into(),
        id: id.into(),
        state: "COMPLETED".into(),
        ended: (OffsetDateTime::now_utc() - TimeDuration::seconds(age_seconds))
            .format(&Rfc3339)
            .unwrap(),
        ..Job::default()
    }
}

fn visible_ids(jobs: &[Job], mode: HistoryMode) -> Vec<String> {
    visible_jobs(jobs.to_vec(), &Ledger::default(), mode, true)
        .into_iter()
        .map(|job| job.id)
        .collect()
}

#[test]
fn visibility_matches_live_archive_and_dismiss_rules() {
    let running = Job {
        cluster: "cispa".into(),
        id: "1".into(),
        state: "RUNNING".into(),
        ..Job::default()
    };
    let failed = Job {
        cluster: "cispa".into(),
        id: "2".into(),
        state: "FAILED".into(),
        ..Job::default()
    };
    let mut ledger = Ledger::default();
    ledger.opened.insert(failed.key(), "now".into());
    assert_eq!(
        visible_jobs(
            vec![running.clone(), failed.clone()],
            &ledger,
            HistoryMode::Live,
            false,
        ),
        vec![running.clone()]
    );
    assert_eq!(
        visible_jobs(vec![failed.clone()], &ledger, HistoryMode::All, false),
        vec![failed.clone()]
    );
    ledger.dismissed.insert(failed.key(), "now".into());
    assert!(visible_jobs(vec![failed.clone()], &ledger, HistoryMode::Live, false).is_empty());
    assert_eq!(
        visible_jobs(vec![failed], &ledger, HistoryMode::All, false).len(),
        1
    );
    ledger.dismissed.insert(running.key(), "now".into());
    assert!(visible_jobs(vec![running.clone()], &ledger, HistoryMode::Live, false).is_empty());
    assert_eq!(
        visible_jobs(vec![running], &ledger, HistoryMode::All, false).len(),
        1
    );
}

#[test]
fn blocked_jobs_are_hidden_until_requested() {
    for job in [
        Job {
            cluster: "sprint".into(),
            id: "3".into(),
            state: "PENDING".into(),
            reason: "DependencyNeverSatisfied".into(),
            ..Job::default()
        },
        Job {
            cluster: "cispa".into(),
            id: "4".into(),
            state: "RUNNING".into(),
            interactive: true,
            ..Job::default()
        },
    ] {
        assert!(
            visible_jobs(
                vec![job.clone()],
                &Ledger::default(),
                HistoryMode::Live,
                false,
            )
            .is_empty()
        );
        assert_eq!(
            visible_jobs(
                vec![job.clone()],
                &Ledger::default(),
                HistoryMode::Live,
                true,
            ),
            vec![job]
        );
    }
}

#[test]
fn history_cycle_and_labels_cover_every_requested_window() {
    let sequence = [
        HistoryMode::Live,
        HistoryMode::Hours2,
        HistoryMode::Hours12,
        HistoryMode::Day1,
        HistoryMode::Week1,
        HistoryMode::All,
        HistoryMode::Live,
    ];
    for pair in sequence.windows(2) {
        assert_eq!(pair[0].next(), pair[1]);
    }
    assert_eq!(HistoryMode::Hours2.label(), "LAST 2h");
    assert_eq!(HistoryMode::Hours12.label(), "LAST 12h");
    assert_eq!(HistoryMode::Day1.label(), "LAST 1d");
    assert_eq!(HistoryMode::Week1.label(), "LAST 1w");
    assert_eq!(HistoryMode::All.label(), "ALL HISTORY");
    assert!(!HistoryMode::Live.scheduler_archive());
    assert!(HistoryMode::Hours2.scheduler_archive());
}

#[test]
fn history_windows_apply_exact_terminal_horizons_and_keep_active_jobs() {
    let jobs = vec![
        completed("one-hour", 60 * 60),
        completed("three-hours", 3 * 60 * 60),
        completed("eighteen-hours", 18 * 60 * 60),
        completed("two-days", 2 * 24 * 60 * 60),
        completed("eight-days", 8 * 24 * 60 * 60),
        Job {
            cluster: "alpha".into(),
            id: "running".into(),
            state: "RUNNING".into(),
            ..Job::default()
        },
    ];
    assert_eq!(
        visible_ids(&jobs, HistoryMode::Hours2),
        ["one-hour", "running"]
    );
    assert_eq!(
        visible_ids(&jobs, HistoryMode::Hours12),
        ["one-hour", "three-hours", "running"]
    );
    assert_eq!(
        visible_ids(&jobs, HistoryMode::Day1),
        ["one-hour", "three-hours", "eighteen-hours", "running"]
    );
    assert_eq!(
        visible_ids(&jobs, HistoryMode::Week1),
        [
            "one-hour",
            "three-hours",
            "eighteen-hours",
            "two-days",
            "running",
        ]
    );
    assert_eq!(visible_ids(&jobs, HistoryMode::All).len(), jobs.len());
}

#[test]
fn history_is_read_only_and_still_shows_previously_dismissed_jobs() {
    let job = completed("dismissed", 60 * 60);
    let mut ledger = Ledger::default();
    ledger.opened.insert(job.key(), "now".into());
    ledger.dismissed.insert(job.key(), "now".into());
    assert!(visible_jobs(vec![job.clone()], &ledger, HistoryMode::Live, false).is_empty());
    assert_eq!(
        visible_jobs(vec![job.clone()], &ledger, HistoryMode::Hours2, false),
        vec![job.clone()]
    );
    assert_eq!(
        visible_jobs(vec![job.clone()], &ledger, HistoryMode::All, false),
        vec![job]
    );
}
