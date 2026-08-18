use std::{fs, os::unix::fs::symlink, path::PathBuf};

use super::*;
use crate::config::{ClusterConfig, Config};

fn config(path: PathBuf) -> Config {
    Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: String::new(),
        state_path: path,
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

fn entry(_config: &Config, id: &str, name: &str) -> AdoptionEntry {
    AdoptionEntry {
        adopted_at: "2026-08-14T00:00:00Z".into(),
        cluster: "alpha".into(),
        job_id: id.into(),
        job_name: name.into(),
        observed_state: "PENDING".into(),
        batch_script_sha256: Some("a".repeat(64)),
        externally_submitted: true,
        source: "manual submission outside MCP".into(),
    }
}

#[test]
fn adoption_ledger_roundtrips_and_filters_by_cluster_and_job() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path().join("state.json"));
    append_adoption(&config, &entry(&config, "12", "one")).unwrap();
    append_adoption(&config, &entry(&config, "13", "two")).unwrap();

    let found = adoption_entry(&config, "alpha", "12").unwrap();
    assert_eq!(found["job_name"], "one");
    assert_eq!(found["externally_submitted"], true);
    assert_eq!(found["batch_script_sha256"], "a".repeat(64));
    assert!(adoption_entry(&config, "alpha", "99").is_none());
    assert!(adoption_entry(&config, "beta", "12").is_none());
    assert_eq!(
        fs::metadata(adoption_path(&config))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn adoption_ledger_rotates_and_rejects_symlinks() {
    let directory = tempfile::tempdir().unwrap();
    let rotating = config(directory.path().join("state.json"));
    let path = adoption_path(&rotating);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = fs::File::create(&path).unwrap();
    file.set_len(ADOPTION_MAX_BYTES).unwrap();
    append_adoption(&rotating, &entry(&rotating, "1", "rotated")).unwrap();
    assert!(path.with_extension("jsonl.1").exists());
    assert!(fs::metadata(&path).unwrap().len() < ADOPTION_MAX_BYTES);

    let directory = tempfile::tempdir().unwrap();
    let rejecting = config(directory.path().join("state.json"));
    let path = adoption_path(&rejecting);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let target = directory.path().join("unrelated");
    fs::write(&target, b"keep").unwrap();
    symlink(&target, &path).unwrap();
    assert!(append_adoption(&rejecting, &entry(&rejecting, "2", "no")).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"keep");
}

#[test]
fn invalid_batch_hashes_are_rejected_before_any_scheduler_call() {
    assert!(adoption_sha(&"a".repeat(64)).is_ok());
    assert!(adoption_sha(&"a".repeat(64).to_uppercase()).is_ok());
    assert!(adoption_sha("xyz").is_err());
    assert!(adoption_sha(&"g".repeat(64)).is_err());
    assert!(adoption_sha(&"a".repeat(65)).is_err());
}
