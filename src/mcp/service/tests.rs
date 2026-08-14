use std::{collections::HashMap, path::PathBuf, sync::Mutex};

use rmcp::model::JsonObject;
use serde_json::Value;

use super::super::helpers::exact_job;
use super::super::present::{fallback_text, match_field};
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
            catalog_generation: "0".repeat(16),
            overrides: None,
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
fn fallback_text_reports_counts_warnings_cursors_and_samples() {
    let value = json!({
        "ok": true, "total": 220, "warnings": [],
        "next_cursor": "j:50",
        "jobs": [{"name":"train"},{"name":"eval"}],
    });
    let text = fallback_text("slurm_list_jobs", &value);
    assert!(text.contains("220 result(s)"), "{text}");
    assert!(text.contains("next cursor j:50"), "{text}");
    assert!(text.contains("train"), "{text}");
    assert!(text.contains("eval"), "{text}");

    let error = fallback_text(
        "slurm_list_scripts",
        &json!({"ok":false,"error":"catalog unavailable"}),
    );
    assert_eq!(error, "slurm_list_scripts: catalog unavailable");

    let zero = fallback_text(
        "slurm_list_scripts",
        &json!({
            "ok":true, "total":0, "warnings":[],
            "catalog":{"available":true,"indexed_at":"2026-08-13T00:00:00Z"}
        }),
    );
    assert!(zero.contains("catalog healthy"), "{zero}");
    assert!(zero.contains("refresh"), "{zero}");
}

#[test]
fn match_field_prefers_id_then_name_then_directives() {
    let script = crate::bank::Script {
        bank: "Bank".into(),
        relative: PathBuf::from("train.sbatch"),
        name: "train-lora".into(),
        directives: vec!["--partition=wdr".into()],
        origin: None,
        bytes: Vec::new(),
    };
    assert_eq!(match_field(&script, "train.sbatch"), Some("script_id"));
    assert_eq!(match_field(&script, "lora"), Some("job_name"));
    assert_eq!(match_field(&script, "wdr"), Some("directives"));
    assert_eq!(match_field(&script, "absent"), None);
}
