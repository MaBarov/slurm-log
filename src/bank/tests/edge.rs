use super::*;
use crate::config::ClusterConfig;

fn config(banks: Vec<SbatchBankConfig>) -> Config {
    Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: String::new(),
        state_path: PathBuf::from("/tmp/slurm-log-bank-edge.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: banks,
        clusters: vec![ClusterConfig {
            name: "local".into(),
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
fn scan_rejects_files_skips_ignored_trees_and_warns_for_oversized_scripts() {
    let directory = tempfile::tempdir().unwrap();
    let plain = directory.path().join("plain.sbatch");
    fs::write(&plain, b"#!/bin/sh\n").unwrap();
    assert!(scan_direct(&plain).is_err());

    let root = directory.path().join("bank");
    fs::create_dir_all(root.join(".git/deep")).unwrap();
    fs::write(root.join(".git/deep/hidden.sbatch"), b"#!/bin/sh\n").unwrap();
    let huge = root.join("huge.sbatch");
    let file = fs::File::create(&huge).unwrap();
    file.set_len(MAX_SCRIPT_BYTES + 1).unwrap();
    fs::write(root.join("fallback.sbatch"), b"#SBATCH\n").unwrap();

    let (scripts, warnings) = scan_direct(&root).unwrap();
    assert_eq!(scripts.len(), 1);
    assert_eq!(scripts[0].name, "fallback");
    assert!(scripts[0].directives.is_empty());
    assert!(warnings.iter().any(|warning| warning.contains("oversized")));
    assert!(ignored_directory("target"));
    assert!(!ignored_directory("experiments"));
}

#[test]
fn cache_rejects_error_payload_oversize_corruption_and_wrong_schema() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("bank");
    fs::create_dir(&root).unwrap();
    let mut config = config(Vec::new());
    config.state_path = directory.path().join("state/state.json");
    let failed = ScanPayload {
        name: "bank".into(),
        scripts: Vec::new(),
        warnings: Vec::new(),
        error: Some("failed".into()),
    };
    store_bank_cache(&config, &root, &failed);
    assert!(!bank_cache_path(&config, &root).exists());

    let path = bank_cache_path(&config, &root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_BANK_CACHE_BYTES + 1).unwrap();
    assert!(load_bank_cache(&config, &root).is_none());
    fs::write(&path, b"not-msgpack").unwrap();
    assert!(load_bank_cache(&config, &root).is_none());

    let wrong = BankCache {
        schema: BANK_CACHE_SCHEMA + 1,
        root: root.clone(),
        fingerprint: 0,
        payload: ScanPayload {
            name: "bank".into(),
            scripts: Vec::new(),
            warnings: Vec::new(),
            error: None,
        },
    };
    fs::write(&path, rmp_serde::to_vec(&wrong).unwrap()).unwrap();
    assert!(load_bank_cache(&config, &root).is_none());
}

#[test]
fn cache_write_failures_leave_no_partial_payload() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("bank");
    fs::create_dir(&root).unwrap();
    let payload = ScanPayload {
        name: "bank".into(),
        scripts: Vec::new(),
        warnings: Vec::new(),
        error: None,
    };

    let blocked_parent = directory.path().join("blocked-parent");
    fs::write(&blocked_parent, b"not a directory").unwrap();
    let mut blocked = config(Vec::new());
    blocked.state_path = blocked_parent.join("state.json");
    store_bank_cache(&blocked, &root, &payload);

    let mut unwritable = config(Vec::new());
    unwritable.state_path = directory.path().join("state/state.json");
    let cache = bank_cache_path(&unwritable, &root);
    fs::create_dir_all(cache.parent().unwrap()).unwrap();
    let temporary = cache.with_extension(format!("tmp.{}", std::process::id()));
    fs::create_dir(&temporary).unwrap();
    store_bank_cache(&unwritable, &root, &payload);
    assert!(!cache.exists());
}

#[test]
fn catalog_helpers_cover_empty_names_origins_directives_and_cluster_support() {
    assert_eq!(fallback_name(Path::new("/")), "Sbatch Bank");
    assert!(scan_all(&config(Vec::new())).is_err());
    assert_eq!(
        directive_job_name(&["-J=name".into()]).as_deref(),
        Some("name")
    );
    assert!(directive_job_name(&["--time=1".into()]).is_none());
    assert!(token_matches_cluster("sprint", "sprint"));
    assert!(token_matches_cluster("sprint12", "sprint"));
    assert!(!token_matches_cluster("sprintx", "sprint"));

    let shared = Script {
        bank: "bank".into(),
        relative: PathBuf::from("run.sbatch"),
        name: "run".into(),
        directives: Vec::new(),
        origin: None,
        bytes: Vec::new(),
    };
    assert!(supports_cluster(&shared, "local"));
    let local = Script {
        origin: Some("local".into()),
        ..shared
    };
    assert!(supports_cluster(&local, "local"));
    assert!(!supports_cluster(&local, "remote"));
}

#[test]
fn routing_directives_require_one_matching_controller() {
    let mut configured = config(Vec::new());
    configured.clusters[0].controller = Some("controller-a".into());
    let target = &configured.clusters[0];
    let script = |directive: &str| Script {
        bank: "bank".into(),
        relative: PathBuf::from("run.sbatch"),
        name: "run".into(),
        directives: vec![directive.into()],
        origin: None,
        bytes: Vec::new(),
    };

    for directive in [
        "--clusters=controller-a",
        "--cluster controller-a",
        "-Mcontroller-a",
        "-M=controller-a",
    ] {
        validate_script_controller(&script(directive), target).unwrap();
    }
    for directive in [
        "--clusters",
        "--cluster controller-a controller-b",
        "-Mcontroller-b",
    ] {
        assert!(validate_script_controller(&script(directive), target).is_err());
    }
}

#[test]
fn scan_worker_validates_arguments_and_serializes_failures() {
    assert!(run_scan_worker(&[]).is_err());
    assert!(run_scan_worker(&["output".into()]).is_err());
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("payload.msgpack");
    fs::write(&output, b"").unwrap();
    run_scan_worker(&[
        output.display().to_string(),
        directory.path().join("missing").display().to_string(),
    ])
    .unwrap();
    let payload: ScanPayload = rmp_serde::from_slice(&fs::read(output).unwrap()).unwrap();
    assert!(payload.error.unwrap().contains("configured sbatch bank"));
}

#[test]
fn cancellation_rejects_invalid_active_ids_without_invoking_scheduler() {
    let job = Job {
        cluster: "local".into(),
        id: "bad id".into(),
        state: "RUNNING".into(),
        ..Job::default()
    };
    assert!(cancel(&config(Vec::new()), &[job]).is_err());
}

#[test]
fn multi_bank_catalog_names_duplicates_and_infers_script_origins() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("project");
    let first = repository.join("jobs");
    let second = directory.path().join("loose");
    fs::create_dir_all(repository.join(".git")).unwrap();
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    fs::write(first.join("local_train.sbatch"), b"#SBATCH -J first\n").unwrap();
    fs::write(second.join("eval.sbatch"), b"#SBATCH --job-name=second\n").unwrap();
    let banks = vec![
        SbatchBankConfig {
            path: first.clone(),
            name: Some("shared".into()),
        },
        SbatchBankConfig {
            path: second.clone(),
            name: Some("shared".into()),
        },
    ];
    let configured = config(banks);
    let (loaded, scripts, warnings) = scan_all_fresh(&configured).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(loaded[0].name, "shared");
    assert_eq!(loaded[1].name, "shared (2)");
    assert_eq!((loaded[0].first, loaded[0].last), (0, 1));
    assert_eq!(scripts[0].origin.as_deref(), Some("local"));
    assert_eq!(scripts[1].bank, "shared (2)");
    assert_eq!(
        inferred_name(&SbatchBankConfig {
            path: first,
            name: None
        })
        .unwrap(),
        "project"
    );
    assert!(
        !inferred_name(&SbatchBankConfig {
            path: second,
            name: None
        })
        .unwrap()
        .is_empty()
    );
}

#[test]
fn scan_skips_symlinks_and_canceling_no_active_jobs_is_a_noop() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("real.sbatch"), b"#!/bin/sh\n").unwrap();
    fs::write(directory.path().join("notes.txt"), b"ignored\n").unwrap();
    symlink(
        directory.path().join("real.sbatch"),
        directory.path().join("linked.sbatch"),
    )
    .unwrap();
    let (scripts, warnings) = scan(directory.path()).unwrap();
    assert_eq!(scripts.len(), 1);
    assert!(warnings.is_empty());

    let finished = Job {
        cluster: "local".into(),
        id: "42".into(),
        state: "COMPLETED".into(),
        ..Job::default()
    };
    assert!(cancel(&config(Vec::new()), &[finished]).unwrap().is_empty());
    assert_eq!(
        directive_job_name(&["-J spaced".into()]).as_deref(),
        Some("spaced")
    );
}

#[test]
fn configured_bank_scan_rejects_hard_linked_scripts() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("bank");
    let outside = directory.path().join("outside.sbatch");
    fs::create_dir(&root).unwrap();
    fs::write(&outside, b"#!/bin/sh\n#SBATCH --job-name=foreign\n").unwrap();
    fs::hard_link(&outside, root.join("linked.sbatch")).unwrap();
    fs::write(
        root.join("safe.sbatch"),
        b"#!/bin/sh\n#SBATCH --job-name=safe\n",
    )
    .unwrap();

    let (scripts, warnings) = scan_direct(&root).unwrap();
    assert_eq!(scripts.len(), 1);
    assert_eq!(scripts[0].relative, PathBuf::from("safe.sbatch"));
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("changed while scanning"))
    );
}
