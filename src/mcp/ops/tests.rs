use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::*;

fn object(value: Value) -> JsonObject {
    value.as_object().unwrap().clone()
}

#[test]
fn wait_conditions_default_and_validate() {
    let flags = wait_until(&JsonObject::new()).unwrap();
    assert!(flags.state_change);
    assert!(!flags.completion);
    assert!(!flags.log_change);

    let flags = wait_until(&object(json!({"until":["completion","log_change"]}))).unwrap();
    assert!(!flags.state_change);
    assert!(flags.completion);
    assert!(flags.log_change);

    assert!(wait_until(&object(json!({"until":["fly"]}))).is_err());
    assert!(wait_until(&object(json!({"until":7}))).is_err());
}

#[test]
fn resource_directives_never_confuse_similar_flags() {
    let resources = script_resources(&[
        "--gres=gpu:1".into(),
        "--mem=32G".into(),
        "--cpus-per-task=4".into(),
        "--time=01:00:00".into(),
    ]);
    assert_eq!(resources["gres"], "gpu:1");
    assert_eq!(resources["mem"], "32G");
    assert_eq!(resources["cpus-per-task"], "4");
    assert_eq!(resources["time"], "01:00:00");

    let resources = script_resources(&["--mem-per-cpu=8G".into(), "--gpus 2".into()]);
    assert!(resources.get("mem").is_none());
    assert_eq!(resources["gpus"], "2");

    assert_eq!(script_resources(&[]), json!({}));
    assert_eq!(directive_value("--mem", "--mem"), Some(""));
    assert_eq!(directive_value("--nodes=2", "--nodes"), Some("2"));
    assert_eq!(directive_value("--nodes 2", "--nodes"), Some("2"));
    assert_eq!(directive_value("--nodelist=x", "--nodes"), None);
}

#[test]
fn probe_status_formats_expected_strings() {
    assert_eq!(probe_status(&Ok(true)), "ok");
    assert_eq!(probe_status(&Ok(false)), "error");
    assert_eq!(
        probe_status(&Err(anyhow::anyhow!("timeout expired"))),
        "error: timeout expired"
    );
}

#[test]
fn mcp_ops_doctor_and_refresh_bank_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    let config = Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: "offline.invalid".into(),
        state_path: directory.path().join("state.json"),
        executable: std::path::PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![crate::config::ClusterConfig {
            name: "local".into(),
            controller: None,
            transport: "local".into(),
            user: "offline".into(),
            ssh_host: String::new(),
            working_directory: directory.path().to_path_buf(),
            accounting: false,
        }],
    };
    let server = McpServer::new(config);
    let doctor_result = server.doctor().unwrap();
    assert!(doctor_result["ok"].as_bool().unwrap());
    assert!(doctor_result["scheduler_health"]["clusters"].is_array());
    assert!(doctor_result["bank_health"].is_object());

    let refresh_result = server.refresh_bank().unwrap();
    assert!(refresh_result["ok"].as_bool().unwrap());
    assert!(refresh_result["total"].as_u64().is_some());
}

#[test]
fn adopt_job_validates_hash_and_matching_name() {
    let directory = tempfile::tempdir().unwrap();
    let config = Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: "offline.invalid".into(),
        state_path: directory.path().join("state.json"),
        executable: std::path::PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![crate::config::ClusterConfig {
            name: "local".into(),
            controller: None,
            transport: "local".into(),
            user: "offline".into(),
            ssh_host: String::new(),
            working_directory: directory.path().to_path_buf(),
            accounting: false,
        }],
    };
    let server = McpServer::new(config);
    let invalid_sha = object(json!({
        "cluster": "local",
        "job_id": "42",
        "expected_job_name": "train",
        "batch_script_sha256": "bad_hex"
    }));
    assert!(server.adopt_job(&invalid_sha, "test-client").is_err());
}

#[test]
fn mcp_ops_tools_handle_arguments_and_fallbacks() {
    let directory = tempfile::tempdir().unwrap();
    let config = Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: "offline.invalid".into(),
        state_path: directory.path().join("state.json"),
        executable: std::path::PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![
            crate::config::ClusterConfig {
                name: "local".into(),
                controller: None,
                transport: "local".into(),
                user: "offline".into(),
                ssh_host: String::new(),
                working_directory: directory.path().to_path_buf(),
                accounting: true,
            },
            crate::config::ClusterConfig {
                name: "remote".into(),
                controller: None,
                transport: "ssh".into(),
                user: "offline".into(),
                ssh_host: "remote.invalid".into(),
                working_directory: directory.path().to_path_buf(),
                accounting: false,
            },
        ],
    };
    let server = McpServer::new(config.clone());

    // Missing / invalid job arguments
    assert!(
        server
            .explain_pending(&object(json!({"cluster":"local"})))
            .is_err()
    );
    assert!(
        server
            .explain_pending(&object(json!({"cluster":"nonexistent","job_id":"1"})))
            .is_err()
    );

    assert!(
        server
            .wait_job(&object(json!({"cluster":"local"})))
            .is_err()
    );
    assert!(
        server
            .wait_job(&object(
                json!({"cluster":"local","job_id":"999","timeout_seconds":1})
            ))
            .is_err()
    );

    let adopt_missing = object(json!({
        "cluster": "local",
        "job_id": "999",
        "expected_job_name": "train"
    }));
    assert!(server.adopt_job(&adopt_missing, "test-client").is_err());

    let adopt_with_sha = object(json!({
        "cluster": "local",
        "job_id": "999",
        "expected_job_name": "train",
        "batch_script_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    }));
    assert!(server.adopt_job(&adopt_with_sha, "test-client").is_err());

    // Partitions parsing
    let _ = partitions(&config, "local");
    let _ = check_squeue(&config, &config.clusters[0]);
    let _ = check_sacct(&config, &config.clusters[0]);
    let _ = check_sinfo(&config, &config.clusters[0]);
}
