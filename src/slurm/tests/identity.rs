use super::*;

#[test]
fn cancellation_scope_rejects_array_masters_and_ranges_but_accepts_one_task() {
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

#[test]
fn validate_control_identity_accepts_array_task_output() {
    let directory = tempfile::tempdir().unwrap();
    let config = Config {
        local_user: "owner".into(),
        remote_user: "owner".into(),
        ssh_host: String::new(),
        state_path: directory.path().join("state.json"),
        executable: std::path::PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![crate::config::ClusterConfig {
            name: "cispa".into(),
            controller: None,
            transport: "local".into(),
            user: "owner".into(),
            ssh_host: String::new(),
            working_directory: directory.path().into(),
            accounting: true,
        }],
    };

    // Real Slurm scontrol output for array task 3233555_22:
    // JobId is reported as the base array master ID 3233555, with ArrayJobId=3233555 and ArrayTaskId=22
    let scontrol_output = "JobId=3233555 UserId=owner(1000) JobName=eval JobState=RUNNING ArrayJobId=3233555 ArrayTaskId=22 NodeList=gpu-01";

    // Expected behavior: validate_control_identity should recognize task 3233555_22 from ArrayJobId/ArrayTaskId
    assert!(
        validate_control_identity(&config, "cispa", "3233555_22", scontrol_output).is_ok(),
        "Expected scontrol array task output to match sub-job ID 3233555_22"
    );
}

#[test]
fn exact_authorization_parses_missing_job_errors() {
    let err_msg = "slurm_load_jobs error: Invalid job id specified";
    assert!(err_msg.contains("Invalid job id"));
    assert!(err_msg.contains("slurm_load_jobs error"));
}
