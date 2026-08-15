use super::*;
use std::path::PathBuf;

#[test]
fn exact_authorization_parsers_accept_matching_rows() {
    let queued = parse_exact_queued_response(
        "123|alice|RUNNING|ok|0:01|node|gpu|now|1|bash\n",
        "cispa",
        "123",
        "alice",
    );
    assert_eq!(queued.map(|job| job.id), Some("123".to_string()));

    let recent = parse_exact_accounting_response(
        "8|alice|OUT_OF_MEMORY|train|1:00|2026-08-11T17:00:00+02:00|0:9|63G|gres/gpu=4|gpu|cispa\n",
        "cispa",
        "8",
        "alice",
        Some("cispa"),
    );
    assert_eq!(recent.map(|job| job.id), Some("8".to_string()));
}

#[test]
fn exact_authorization_parsers_reject_foreign_rows() {
    assert!(
        parse_exact_queued_response(
            "123|bob|RUNNING|ok|0:01|node|gpu|now|1|bash\n",
            "cispa",
            "123",
            "alice",
        )
        .is_none()
    );
    assert!(
        parse_exact_queued_response(
            "123.batch|alice|RUNNING|step|0:01|node|gpu|now|1|bash\n",
            "cispa",
            "123",
            "alice",
        )
        .is_none()
    );
    assert!(
        parse_exact_accounting_response(
            "8|alice|OUT_OF_MEMORY|train|1:00|2026-08-11T17:00:00+02:00|0:9|63G|gres/gpu=4|gpu|other\n",
            "cispa",
            "8",
            "alice",
            Some("cispa"),
        )
        .is_none()
    );
}

#[test]
fn terminal_path_authorized_rejects_a_mismatched_authorization() {
    let directory = tempfile::tempdir().unwrap();
    let config = Config {
        local_user: "owner".into(),
        remote_user: "owner".into(),
        ssh_host: String::new(),
        state_path: directory.path().join("state.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: Vec::new(),
    };
    let authorized = Job {
        cluster: "other".into(),
        id: "42".into(),
        ..Job::default()
    };
    assert!(terminal_path_authorized(&config, "local", "42", &authorized).is_err());
}

#[test]
fn control_identity_rejects_mismatched_job_and_cluster() {
    let directory = tempfile::tempdir().unwrap();
    let config = Config {
        local_user: "owner".into(),
        remote_user: "owner".into(),
        ssh_host: String::new(),
        state_path: directory.path().join("state.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![crate::config::ClusterConfig {
            name: "local".into(),
            controller: Some("ctld".into()),
            transport: "local".into(),
            user: "owner".into(),
            ssh_host: String::new(),
            working_directory: directory.path().into(),
            accounting: true,
        }],
    };
    assert!(
        validate_control_identity(&config, "local", "42", "JobId=41 UserId=owner(1000)").is_err()
    );
    assert!(
        validate_control_identity(
            &config,
            "local",
            "42",
            "JobId=42 UserId=owner(1000) ClusterName=other"
        )
        .is_err()
    );
    assert!(
        validate_control_identity(
            &config,
            "local",
            "42",
            "JobId=42 UserId=owner(1000) ClusterName=ctld"
        )
        .is_ok()
    );
}
