use std::{collections::HashMap, path::PathBuf, sync::Mutex};

use super::*;
use crate::{
    config::{ClusterConfig, Config, SbatchBankConfig},
    mcp::McpServer,
};

fn server(state: PathBuf) -> McpServer {
    let bank = state.parent().unwrap().join("bank");
    std::fs::create_dir_all(&bank).unwrap();
    std::fs::write(
        bank.join("train.sbatch"),
        "#!/bin/sh\n#SBATCH --job-name=train\n",
    )
    .unwrap();
    let config = Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: String::new(),
        state_path: state,
        executable: PathBuf::from("/bin/false"),
        sbatch_banks: vec![SbatchBankConfig {
            path: bank,
            name: None,
        }],
        clusters: vec![ClusterConfig {
            name: "alpha".into(),
            controller: None,
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
        work: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
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
            catalog_generation: String::new(),
            repo_commit: None,
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

#[test]
fn mcp_service_dispatch_and_inspection_helpers_cover_all_branches() {
    let directory = tempfile::tempdir().unwrap();
    let server = server(directory.path().join("state.json"));
    // Unknown tool dispatch
    let unknown_request: CallToolRequestParams =
        serde_json::from_value(json!({"name": "unknown_slurm_tool"})).unwrap();
    let result = server.dispatch_tool(unknown_request, "test-client");
    assert!(result.is_error.unwrap_or(false));

    // List clusters
    let list_clusters_request: CallToolRequestParams =
        serde_json::from_value(json!({"name": "slurm_list_clusters", "arguments": {}})).unwrap();
    let result = server.dispatch_tool(list_clusters_request, "test-client");
    assert!(!result.is_error.unwrap_or(false));

    // Workspace context
    let workspace_result = server.workspace_context();
    assert!(workspace_result.is_ok());

    // List scripts
    let list_scripts_result = server.list_scripts(&JsonObject::new());
    assert!(list_scripts_result.is_ok());

    // List jobs with search and state filter
    let filter_args = JsonObject::from_iter([
        ("cluster".into(), Value::String("alpha".into())),
        ("search".into(), Value::String("train".into())),
        (
            "states".into(),
            Value::Array(vec![Value::String("RUNNING".into())]),
        ),
    ]);
    let _ = server.list_jobs(&filter_args);

    // Inspect job missing arguments
    let missing_inspect =
        JsonObject::from_iter([("cluster".into(), Value::String("alpha".into()))]);
    assert!(server.inspect_job(&missing_inspect).is_err());
}
