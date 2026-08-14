use std::{collections::HashMap, path::PathBuf, sync::Mutex};

use rmcp::model::JsonObject;
use serde_json::Value;

use super::super::helpers::{declared_results_for_job, exact_job, sha256};
use super::super::present::{fallback_text, match_field, script_stale};
use super::*;
use crate::{
    config::{ClusterConfig, Config, SbatchBankConfig},
    mcp::McpServer,
};

fn server_config(state: PathBuf) -> Config {
    Config {
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
    }
}

fn server(state: PathBuf) -> McpServer {
    let config = server_config(state);
    McpServer {
        tools: std::sync::Arc::new(super::super::schema::tools(&config)),
        config: std::sync::Arc::new(config),
        previews: std::sync::Arc::new(Mutex::new(HashMap::new())),
        subscriptions: std::sync::Arc::new(Mutex::new(HashMap::new())),
        work: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
    }
}

fn job(cluster: &str, id: &str, name: &str) -> crate::model::Job {
    crate::model::Job {
        cluster: cluster.into(),
        id: id.into(),
        name: name.into(),
        ..Default::default()
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
        declared_results: Vec::new(),
        bytes: Vec::new(),
    };
    assert_eq!(match_field(&script, "train.sbatch"), Some("script_id"));
    assert_eq!(match_field(&script, "lora"), Some("job_name"));
    assert_eq!(match_field(&script, "wdr"), Some("directives"));
    assert_eq!(match_field(&script, "absent"), None);
}

#[test]
fn fallback_text_reports_an_unavailable_catalog_and_match_counts() {
    let unavailable = fallback_text(
        "slurm_list_scripts",
        &json!({
            "ok":true, "total":0,
            "catalog":{"available":false,"indexed_at":"2026-08-13T00:00:00Z"}
        }),
    );
    assert!(unavailable.contains("catalog unavailable"), "{unavailable}");

    let searched = fallback_text(
        "slurm_search_log",
        &json!({"ok":true, "match_count":7, "matches":[{"text":"a"}]}),
    );
    assert!(searched.contains("7 match(es)"), "{searched}");

    let warned = fallback_text(
        "slurm_list_jobs",
        &json!({"ok":true, "warnings":["degraded"], "jobs":[{"name":"x"}]}),
    );
    assert!(warned.contains("1 warning(s)"), "{warned}");
}

#[test]
fn fallback_samples_skip_empty_names_and_report_overflow() {
    let no_names = fallback_text(
        "slurm_list_jobs",
        &json!({"ok":true, "jobs":[{"name":""},{"name":""}]}),
    );
    assert!(!no_names.contains("job:"), "{no_names}");

    let many = fallback_text(
        "slurm_list_clusters",
        &json!({
            "ok":true,
            "clusters":[
                {"name":"a"},{"name":"b"},{"name":"c"},{"name":"d"},{"name":"e"},{"name":"f"}
            ]
        }),
    );
    assert!(many.contains("cluster: a, b, c, d, e, …"), "{many}");

    let findings = fallback_text(
        "slurm_diagnose_job",
        &json!({"ok":true, "findings":[{"classification":"cuda_out_of_memory"}]}),
    );
    assert!(
        findings.contains("finding: cuda_out_of_memory"),
        "{findings}"
    );
}

#[test]
fn script_stale_requires_a_valid_indexed_time_and_a_present_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("run.sbatch");
    let script = crate::bank::Script {
        bank: "Bank".into(),
        relative: PathBuf::from("run.sbatch"),
        name: "run".into(),
        directives: Vec::new(),
        origin: None,
        declared_results: Vec::new(),
        bytes: Vec::new(),
    };
    let meta = |indexed_at: Option<String>| crate::bank::BankMeta {
        name: "Bank".into(),
        path: directory.path().to_path_buf(),
        available: true,
        script_count: 1,
        indexed_at,
        generation: None,
        repo_head: None,
        dirty: None,
    };

    let none = meta(None);
    assert!(!script_stale(&script, Some(&&none)));
    let malformed = meta(Some("not-a-date".into()));
    assert!(!script_stale(&script, Some(&&malformed)));
    let missing = meta(Some("2026-08-13T00:00:00Z".into()));
    assert!(!script_stale(&script, Some(&&missing)));

    std::fs::write(&path, b"#!/bin/sh\n").unwrap();
    assert!(
        script_stale(&script, Some(&&missing)),
        "modified after indexing"
    );
    let future = meta(Some("2099-01-01T00:00:00Z".into()));
    assert!(
        !script_stale(&script, Some(&&future)),
        "indexed after modification"
    );
    assert!(
        !script_stale(&script, None),
        "absent bank metadata is not stale"
    );
}

#[test]
fn declared_results_resolve_by_producer_hash_then_name_and_fail_closed() {
    use std::fs;
    let directory = tempfile::tempdir().unwrap();
    let bank = directory.path().join("bank");
    fs::create_dir(&bank).unwrap();
    let bytes = b"#!/bin/sh\n#SBATCH --job-name=train\n#SLURM_LOG-RESULT: cpu_gate.json\n";
    fs::write(bank.join("train.sbatch"), bytes).unwrap();
    let state = directory.path().join("state.json");
    let mut config = server_config(state.clone());
    config.sbatch_banks = vec![SbatchBankConfig {
        path: bank.clone(),
        name: Some("Bank".into()),
    }];

    let by_name = declared_results_for_job(&config, "alpha", &job("alpha", "7", "train")).unwrap();
    assert_eq!(by_name, ["cpu_gate.json"]);

    crate::state::Ledger::mark_submitted(&state, "alpha", "9", &sha256(bytes)).unwrap();
    let by_hash =
        declared_results_for_job(&config, "alpha", &job("alpha", "9", "renamed")).unwrap();
    assert_eq!(by_hash, ["cpu_gate.json"]);

    assert!(
        declared_results_for_job(&config, "alpha", &job("alpha", "5", "unrecorded")).is_err(),
        "unknown jobs must fail closed instead of inheriting another job's declarations"
    );
}

#[test]
fn declared_results_reject_duplicate_job_names_across_scripts() {
    use std::fs;
    let directory = tempfile::tempdir().unwrap();
    let bank = directory.path().join("bank");
    fs::create_dir(&bank).unwrap();
    fs::write(
        bank.join("first.sbatch"),
        b"#!/bin/sh\n#SBATCH --job-name=shared-name\n#SLURM_LOG-RESULT: first.json\n",
    )
    .unwrap();
    fs::write(
        bank.join("second.sbatch"),
        b"#!/bin/sh\n#SBATCH --job-name=shared-name\n#SLURM_LOG-RESULT: second.json\n",
    )
    .unwrap();
    let state = directory.path().join("state.json");
    let mut config = server_config(state.clone());
    config.sbatch_banks = vec![SbatchBankConfig {
        path: bank.clone(),
        name: Some("Bank".into()),
    }];

    let error =
        declared_results_for_job(&config, "alpha", &job("alpha", "7", "shared-name")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("multiple configured scripts share"),
        "{error:#}"
    );
}
