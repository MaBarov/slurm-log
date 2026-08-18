use std::path::PathBuf;

use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::*;
use crate::config::{ClusterConfig, Config};

fn object(value: Value) -> JsonObject {
    value.as_object().unwrap().clone()
}

fn config() -> Config {
    Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: String::new(),
        state_path: PathBuf::from("/tmp/state"),
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

#[test]
fn argument_helpers_cover_defaults_errors_and_cursors() {
    let args = object(json!({
        "cluster":"alpha", "job_id":"12_3", "flag":true,
        "number":7, "states":["running", "pending"], "cursor":"jobs:4"
    }));
    assert_eq!(exact_job(&config(), &args).unwrap(), ("alpha", "12_3"));
    assert_eq!(required_string(&args, "cluster").unwrap(), "alpha");
    assert_eq!(optional_string(&args, "missing"), None);
    assert_eq!(optional_bool(&args, "flag"), Some(true));
    assert_eq!(optional_usize(&args, "missing", 9).unwrap(), 9);
    assert_eq!(optional_usize(&args, "number", 9).unwrap(), 7);
    assert_eq!(
        optional_strings(&args, "states").unwrap(),
        ["RUNNING", "PENDING"]
    );
    assert_eq!(
        optional_strings(&args, "missing").unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(page(&args, "jobs", 20, 100).unwrap(), (4, 20));

    assert!(
        exact_job(
            &config(),
            &object(json!({"cluster":"alpha","job_id":"bad/id"}))
        )
        .is_err()
    );
    assert!(
        exact_job(
            &config(),
            &object(json!({"cluster":"missing","job_id":"1"}))
        )
        .is_err()
    );
    assert!(required_string(&JsonObject::new(), "cluster").is_err());
    assert!(optional_usize(&object(json!({"number":-1})), "number", 1).is_err());
    assert!(optional_strings(&object(json!({"states":7})), "states").is_err());
    assert!(optional_strings(&object(json!({"states":[7]})), "states").is_err());
    assert!(page(&object(json!({"cursor":"wrong"})), "jobs", 20, 100).is_err());
}

#[test]
fn histories_scripts_hashes_and_tokens_are_exact() {
    for value in ["live", "2h", "12h", "1d", "1w", "all"] {
        history_mode(value).unwrap();
    }
    assert!(history_mode("forever").is_err());

    let script = bank::Script {
        bank: "Bank".into(),
        relative: PathBuf::from("train.sbatch"),
        name: "train".into(),
        directives: vec!["--job-name=train".into()],
        origin: Some("alpha".into()),
        bytes: b"#!/bin/sh\n".to_vec(),
        bank_fingerprint: 0,
        indexed_at: 0,
        repo_commit: None,
    };
    assert_eq!(script_id(&script), "Bank/train.sbatch");
    assert_eq!(
        exact_script(std::slice::from_ref(&script), "Bank/train.sbatch", "alpha")
            .unwrap()
            .name,
        "train"
    );
    assert!(exact_script(std::slice::from_ref(&script), "Bank/train.sbatch", "beta").is_err());
    assert!(exact_script(&[script.clone(), script], "Bank/train.sbatch", "alpha").is_err());
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );

    let first = preview_token().unwrap();
    assert_eq!(first.len(), 64);
    assert_ne!(first, preview_token().unwrap());
}

#[test]
fn bounded_and_sanitized_text_handles_unicode_signals_and_osc() {
    assert_eq!(bounded_text("short", 8), ("short".into(), false));
    assert_eq!(bounded_text("aébc", 3), ("bc".into(), true));
    assert_eq!(bounded_line("short", 8), "short");
    assert_eq!(bounded_line("aébc", 2), "a");
    assert!(signal_exit_code("1:9"));
    assert!(!signal_exit_code("0:0"));
    assert!(!signal_exit_code("invalid"));
    assert_eq!(sanitize(b"a\x1b]title\x07b\x1bXc\x01\n\t"), "abXc\n\t");
    assert_eq!(sanitize(b"a\x1b]title\x1b\\b"), "ab");
    assert_eq!(sanitize(b"1234567812345678"), "1234567812345678");
    assert_eq!(
        sanitize(b"1234567\x1b[32mgreen\x1b[0m890"),
        "1234567green890"
    );
    assert_eq!(sanitize(b"\x00\x01\x02clean text\x03\x04"), "clean text");
    assert_eq!(sanitize("こんにちは世界\n".as_bytes()), "こんにちは世界\n");
}
