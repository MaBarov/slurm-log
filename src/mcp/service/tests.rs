use std::{collections::HashMap, path::PathBuf, sync::Mutex};

use super::*;
use crate::{
    config::{ClusterConfig, Config},
    mcp::McpServer,
};

fn server(state: PathBuf) -> McpServer {
    let config = Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: String::new(),
        state_path: state,
        executable: PathBuf::from("/bin/false"),
        sbatch_banks: Vec::new(),
        clusters: vec![ClusterConfig {
            name: "alpha".into(),
            transport: "local".into(),
            user: "offline".into(),
            ssh_host: String::new(),
            working_directory: PathBuf::from("/tmp"),
            accounting: false,
        }],
    };
    McpServer {
        tools: std::sync::Arc::new(super::super::schema::tools(&config)),
        config: std::sync::Arc::new(config),
        previews: std::sync::Arc::new(Mutex::new(HashMap::new())),
        subscriptions: std::sync::Arc::new(Mutex::new(HashMap::new())),
    }
}

#[test]
fn expired_preview_is_consumed_before_any_submission_attempt() {
    let directory = tempfile::tempdir().unwrap();
    let server = server(directory.path().join("state.json"));
    server.previews.lock().unwrap().insert(
        "expired-token".into(),
        Preview {
            created: Instant::now() - PREVIEW_TTL - Duration::from_secs(1),
            cluster: "alpha".into(),
            script: "Bank/train.sbatch".into(),
            digest: "a".repeat(64),
            directives: Vec::new(),
            working_directory: "/tmp".into(),
            job_name: "train".into(),
        },
    );
    let args = JsonObject::from_iter([(
        "preview_token".into(),
        Value::String("expired-token".into()),
    )]);
    assert!(
        server
            .submit_job(&args, "unit client")
            .unwrap_err()
            .to_string()
            .contains("expired")
    );
    assert!(
        !server
            .previews
            .lock()
            .unwrap()
            .contains_key("expired-token")
    );
}

#[test]
fn cluster_is_never_inferred_for_exact_job_tools() {
    let directory = tempfile::tempdir().unwrap();
    let server = server(directory.path().join("state.json"));
    let args = JsonObject::from_iter([("job_id".into(), Value::String("123".into()))]);
    assert!(
        exact_job(&server.config, &args)
            .unwrap_err()
            .to_string()
            .contains("cluster")
    );
}
