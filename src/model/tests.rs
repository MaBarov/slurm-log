use super::*;

#[test]
fn job_ids() {
    assert!(valid_job_id("3202710"));
    assert!(valid_job_id("3202690_1"));
    assert!(!valid_job_id("3202690_1_2"));
    assert!(!valid_job_id("abc"));
    for invalid in ["", "_", "1_", "_1", "1-2", " 1", "1\n", "١٢٣"] {
        assert!(!valid_job_id(invalid), "accepted invalid ID {invalid:?}");
    }
    for valid in ["0", "00001", "1_0", "999999999999999999999999"] {
        assert!(valid_job_id(valid), "rejected valid ID {valid:?}");
    }
    assert_eq!(token("JobId=42 UserId=owner(1000)", "JobId="), Some("42"));
    assert_eq!(
        token("JobId=42 UserId=owner(1000)", "UserId="),
        Some("owner(1000)")
    );
    assert_eq!(token("JobId=42", "Missing="), None);
}

#[test]
fn terminal_text_escapes_controls() {
    assert_eq!(
        terminal_text("name\x1b]52;c;bad\x07\r\n"),
        "name\\x1b]52;c;bad\\u{7}\\r\\n"
    );
}

#[test]
fn state_classification_handles_slurm_suffixes() {
    for state in [
        "FAILED",
        "FAILED+",
        "TIMEOUT",
        "OUT_OF_MEMORY",
        "NODE_FAIL",
        "CANCELLED by 1",
    ] {
        assert!(
            Job {
                state: state.into(),
                ..Job::default()
            }
            .failed()
        );
    }
    assert!(
        Job {
            state: "RUNNING+".into(),
            ..Job::default()
        }
        .running()
    );
    assert!(
        Job {
            state: "PENDING".into(),
            ..Job::default()
        }
        .pending()
    );
    assert!(
        !Job {
            state: "COMPLETED".into(),
            ..Job::default()
        }
        .active()
    );
}

#[test]
fn insights_explain_pending_and_failed_jobs() {
    let pending = Job {
        state: "PENDING".into(),
        reason: "Resources".into(),
        start_time: "2026-08-11T18:00:00".into(),
        priority: "1234".into(),
        ..Job::default()
    };
    let insight = pending.insight();
    assert!(insight.contains("waiting for requested compute resources"));
    assert!(insight.contains("estimated start"));
    assert!(insight.contains("priority 1234"));

    let failed = Job {
        state: "OUT_OF_MEMORY".into(),
        exit_code: "0:9".into(),
        max_rss: "63G".into(),
        ..Job::default()
    };
    assert_eq!(failed.insight(), "exit 0:9 · peak memory 63G");
}

#[test]
fn insights_cover_every_scheduler_reason_and_empty_metadata() {
    let cases = [
        ("Priority", "higher-priority"),
        ("(DependencyNeverSatisfied,foo)", "can never"),
        ("Dependency", "a dependency"),
        ("QOSMaxJobsPerUserLimit", "rate limit"),
        ("QOSMaxCpuPerUserLimit", "quota limit"),
        ("AssocMaxJobsLimit", "rate limit"),
        ("AssocMaxCpuLimit", "quota limit"),
        ("JobArrayTaskLimit", "array task"),
        ("PartitionMaxJobsPerUserLimit", "partition job limit"),
        ("PartitionNodeLimit", "partition limit"),
        ("ReqNodeNotAvail", "node(s) unavailable"),
        ("BeginTime", "begin time"),
        ("Reservation", "reservation"),
        ("Licenses", "license"),
        ("JobHoldAdmin", "administrator or user"),
        ("BadConstraints", "node constraints"),
        ("None", "pending"),
        ("", "pending"),
        ("UnusualReason", "UnusualReason"),
    ];
    for (reason, expected) in cases {
        let job = Job {
            state: "PENDING".into(),
            reason: reason.into(),
            start_time: "N/A".into(),
            ..Job::default()
        };
        assert!(job.insight().contains(expected), "reason={reason}");
    }
    let unknown = Job {
        state: "PENDING".into(),
        start_time: "Unknown".into(),
        ..Job::default()
    };
    assert_eq!(unknown.insight(), "pending");
}

#[test]
fn job_helpers_cover_keys_blocking_and_empty_insights() {
    let mut job = Job {
        cluster: "alpha".into(),
        id: "42".into(),
        state: "COMPLETED".into(),
        ..Job::default()
    };
    assert_eq!(job.key(), "alpha:42");
    let mut reusable = String::from("old allocation");
    job.write_key(&mut reusable);
    assert_eq!(reusable, "alpha:42");
    assert_eq!(job.insight(), "");
    assert!(!job.blocked_category());
    job.interactive = true;
    assert!(job.blocked_category());

    job.state = "FAILED".into();
    job.exit_code = "0:0".into();
    assert_eq!(job.insight(), "");
    job.max_rss = "1G".into();
    assert_eq!(job.insight(), "peak memory 1G");
}

#[test]
fn pending_tags_and_state_display_format_cleanly() {
    let resources = Job {
        state: "PENDING".into(),
        reason: "Resources".into(),
        ..Job::default()
    };
    assert_eq!(resources.pending_tag(), "Resources");
    assert_eq!(resources.state_display(), "PENDING (Resources)");

    let rate_limit = Job {
        state: "PENDING".into(),
        reason: "QOSMaxJobsPerUserLimit".into(),
        ..Job::default()
    };
    assert_eq!(rate_limit.pending_tag(), "Rate Limit");
    assert_eq!(rate_limit.state_display(), "PENDING (Rate Limit)");

    let quota_limit = Job {
        state: "PENDING".into(),
        reason: "AssocMaxCpuLimit".into(),
        ..Job::default()
    };
    assert_eq!(quota_limit.pending_tag(), "Quota Limit");
    assert_eq!(quota_limit.state_display(), "PENDING (Quota Limit)");

    let running = Job {
        state: "RUNNING".into(),
        ..Job::default()
    };
    assert_eq!(running.pending_tag(), "");
    assert_eq!(running.state_display(), "RUNNING");
}

#[test]
fn state_filter_cycles_and_matches_accurately() {
    assert_eq!(StateFilter::All.next(), StateFilter::Running);
    assert_eq!(StateFilter::Running.next(), StateFilter::Pending);
    assert_eq!(StateFilter::Pending.next(), StateFilter::Failed);
    assert_eq!(StateFilter::Failed.next(), StateFilter::All);

    assert_eq!(StateFilter::All.label(), "ALL");
    assert_eq!(StateFilter::Running.label(), "RUNNING");
    assert_eq!(StateFilter::Pending.label(), "PENDING");
    assert_eq!(StateFilter::Failed.label(), "FAILED");

    assert!(StateFilter::All.notice().contains("ALL"));
    assert!(StateFilter::Running.notice().contains("RUNNING"));
    assert!(StateFilter::Pending.notice().contains("PENDING"));
    assert!(StateFilter::Failed.notice().contains("FAILED"));

    let running = Job {
        state: "RUNNING".into(),
        ..Job::default()
    };
    let pending = Job {
        state: "PENDING".into(),
        ..Job::default()
    };
    let failed = Job {
        state: "FAILED".into(),
        ..Job::default()
    };
    let completed = Job {
        state: "COMPLETED".into(),
        ..Job::default()
    };

    assert!(StateFilter::All.matches(&running));
    assert!(StateFilter::All.matches(&pending));
    assert!(StateFilter::All.matches(&failed));
    assert!(StateFilter::All.matches(&completed));

    assert!(StateFilter::Running.matches(&running));
    assert!(!StateFilter::Running.matches(&pending));
    assert!(!StateFilter::Running.matches(&failed));

    assert!(!StateFilter::Pending.matches(&running));
    assert!(StateFilter::Pending.matches(&pending));
    assert!(!StateFilter::Pending.matches(&failed));

    assert!(!StateFilter::Failed.matches(&running));
    assert!(!StateFilter::Failed.matches(&pending));
    assert!(StateFilter::Failed.matches(&failed));
    assert!(!StateFilter::Failed.matches(&completed));
}
