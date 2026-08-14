use super::*;
use std::path::PathBuf;

#[test]
fn cancellation_scope_rejects_array_masters_and_ranges_but_accepts_one_task() {
    let unproven = CancelMetadata::from_scontrol(
        "JobId=700_3 UserId=offline(1000) JobName=train JobState=RUNNING",
    )
    .unwrap();
    assert!(unproven.prove_exact_cancel_scope("700_3").is_err());

    let master = CancelMetadata::from_scontrol(
        "JobId=700 UserId=offline(1000) JobName=train JobState=RUNNING ArrayJobId=700 ArrayTaskId=0-9",
    )
    .unwrap();
    assert!(master.prove_exact_cancel_scope("700").is_err());

    let range = CancelMetadata::from_scontrol(
        "JobId=700_3 UserId=offline(1000) JobName=train JobState=RUNNING ArrayJobId=700 ArrayTaskId=0-9",
    )
    .unwrap();
    assert!(range.prove_exact_cancel_scope("700_3").is_err());

    let task = CancelMetadata::from_scontrol(
        "JobId=700_3 UserId=offline(1000) JobName=train JobState=RUNNING ArrayJobId=700 ArrayTaskId=3",
    )
    .unwrap();
    task.prove_exact_cancel_scope("700_3").unwrap();

    let ordinary = CancelMetadata::from_scontrol(
        "JobId=701 UserId=offline(1000) JobName=train JobState=RUNNING",
    )
    .unwrap();
    ordinary.prove_exact_cancel_scope("701").unwrap();
}

#[test]
fn cancellation_rejects_an_invalid_job_id_before_any_scheduler_call() {
    let config = Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: String::new(),
        state_path: PathBuf::from("/tmp/state.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![crate::config::ClusterConfig {
            name: "local".into(),
            controller: None,
            transport: "local".into(),
            user: "offline".into(),
            ssh_host: String::new(),
            working_directory: PathBuf::from("/tmp"),
            accounting: true,
        }],
    };
    assert!(fresh_cancellable_job(&config, "local", "not-a-job-id").is_err());
}

#[test]
fn fresh_exact_authorization_rejects_an_id_reused_by_another_owner() {
    let live = "42|owner|RUNNING|owned|00:01|node|gpu|start|100|train.sbatch\n";
    assert_eq!(
        parse_exact_queued_response(live, "local", "42", "owner")
            .as_ref()
            .map(|job| job.name.as_str()),
        Some("owned")
    );
    // Same id, but a scheduler owner transition must be a hard miss even
    // before any scontrol/log/accounting follow-up is attempted.
    let reused = "42|other|RUNNING|foreign|00:01|node|gpu|start|100|train.sbatch\n";
    assert!(parse_exact_queued_response(reused, "local", "42", "owner").is_none());

    let terminal = "42|owner|COMPLETED|owned|00:01|end|0:0|1K|cpu=1|gpu|alpha\n";
    assert!(
        parse_exact_accounting_response(terminal, "alpha", "42", "owner", Some("alpha")).is_some()
    );
    let foreign_terminal = "42|other|COMPLETED|foreign|00:01|end|0:0|1K|cpu=1|gpu|alpha\n";
    assert!(
        parse_exact_accounting_response(foreign_terminal, "alpha", "42", "owner", Some("alpha"))
            .is_none()
    );
}
