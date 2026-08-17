use super::*;
use crate::config::ClusterConfig;

fn config(banks: Vec<SbatchBankConfig>) -> Config {
    Config {
        local_user: "alice".into(),
        remote_user: "alice".into(),
        ssh_host: "cluster".into(),
        state_path: PathBuf::from("/tmp/slurm-log-bank-coverage.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: banks,
        clusters: vec![ClusterConfig {
            name: "local".into(),
            controller: None,
            transport: "local".into(),
            user: "alice".into(),
            ssh_host: String::new(),
            working_directory: PathBuf::from("/tmp"),
            accounting: false,
        }],
    }
}

#[test]
fn git_provenance_reads_head_and_dirtiness_from_a_repository() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let run = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&["init", "-q"]);
    run(&["commit", "-q", "--allow-empty", "-m", "init"]);
    let (head, dirty) = git_provenance(&repo);
    assert!(head.is_some(), "expected a resolved HEAD");
    assert_eq!(dirty, Some(false));
    fs::write(repo.join("untracked.sbatch"), b"#!/bin/sh\n").unwrap();
    assert_eq!(git_provenance(&repo).1, Some(true));
}

#[test]
fn script_origin_is_shared_when_multiple_clusters_match() {
    let mut config = config(Vec::new());
    config.clusters[0].name = "node".into();
    config.clusters.push(ClusterConfig {
        name: "node1".into(),
        controller: None,
        transport: "ssh".into(),
        user: "alice".into(),
        ssh_host: "node1.example".into(),
        working_directory: PathBuf::from("/work"),
        accounting: true,
    });
    let script = Script {
        bank: "bank".into(),
        relative: PathBuf::from("node12/train.sbatch"),
        name: "job".into(),
        directives: Vec::new(),
        origin: None,
        declared_results: Vec::new(),
        bytes: Vec::new(),
    };
    assert_eq!(infer_script_origin(&script, &config), None);
}

/// Fall back to the directory name only when no ancestor carries a `.git`
/// directory. A host may have a stray `/tmp/.git`, so locate a base directory
/// with a clean ancestor chain (falling back to `/tmp`) before asserting.
#[test]
fn inferred_name_falls_back_to_directory_name_without_git() {
    let directory = tempdir_without_git_ancestor();
    let bank = directory.path().join("plain-bank");
    fs::create_dir(&bank).unwrap();
    let configured = SbatchBankConfig {
        path: bank,
        name: None,
    };
    assert_eq!(inferred_name(&configured).unwrap(), "plain-bank");
}

fn tempdir_without_git_ancestor() -> tempfile::TempDir {
    for base in ["/dev/shm", "/tmp"] {
        if let Ok(directory) = tempfile::Builder::new().tempdir_in(base)
            && git_root(directory.path()).is_none()
        {
            return directory;
        }
    }
    panic!("no writable temporary directory without a .git ancestor is available");
}

#[test]
fn scan_limits_total_script_data_to_64_mib() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("bank");
    fs::create_dir(&root).unwrap();
    let full_files = (MAX_BANK_CACHE_BYTES / MAX_SCRIPT_BYTES) as usize;
    for index in 0..full_files {
        let path = root.join(format!("large-{index:02}.sbatch"));
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_SCRIPT_BYTES).unwrap();
    }
    fs::write(root.join("zz-tiny.sbatch"), b"#!/bin/sh\n").unwrap();
    let (scripts, warnings) = scan_direct(&root).unwrap();
    assert_eq!(scripts.len(), full_files);
    assert!(warnings.iter().any(|warning| warning.contains("64 MiB")));
}

#[test]
fn tree_fingerprint_is_none_for_non_directory_or_symlink() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("plain.txt");
    fs::write(&file, b"x").unwrap();
    assert!(bank_tree_fingerprint(&file).is_none());

    let link = directory.path().join("linked");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert!(bank_tree_fingerprint(&link).is_none());
}

#[test]
fn catalog_reports_indexed_at_for_a_cached_non_git_bank() {
    let directory = tempdir_without_git_ancestor();
    let root = directory.path().join("bank");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("job.sbatch"), b"#!/bin/sh\n").unwrap();
    let mut config = config(vec![SbatchBankConfig {
        path: root.clone(),
        name: None,
    }]);
    config.state_path = directory.path().join("state/state.json");
    let payload = ScanPayload {
        name: "bank".into(),
        scripts: Vec::new(),
        warnings: Vec::new(),
        error: None,
    };
    store_bank_cache(&config, &root, &payload);
    let (_, _, meta) = catalog(&config, false).unwrap();
    assert!(meta.indexed_at.is_some());
}

fn write_many_scripts(root: &Path, count: usize) {
    for index in 0..count {
        fs::write(root.join(format!("job-{index:06}.sbatch")), b"#!/bin/sh\n").unwrap();
    }
}

#[test]
#[ignore = "release-mode performance budget"]
fn script_limit_is_reported_and_caps_scan_and_fingerprint() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("bank");
    fs::create_dir(&root).unwrap();
    write_many_scripts(&root, MAX_SCRIPTS + 1);
    let (scripts, warnings) = scan_direct(&root).unwrap();
    assert_eq!(scripts.len(), MAX_SCRIPTS);
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("bank limited"))
    );
    assert!(bank_tree_fingerprint(&root).is_some());
    let mut config = config(vec![SbatchBankConfig {
        path: root,
        name: None,
    }]);
    config.state_path = directory.path().join("state/state.json");
    let (scripts, _, _) = catalog(&config, false).unwrap();
    assert_eq!(scripts.len(), MAX_SCRIPTS);
}

#[test]
#[ignore = "release-mode performance budget"]
fn combined_bank_script_limit_truncates_the_second_bank() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    write_many_scripts(&first, 1);
    write_many_scripts(&second, MAX_SCRIPTS);
    let mut config = config(vec![
        SbatchBankConfig {
            path: first,
            name: None,
        },
        SbatchBankConfig {
            path: second,
            name: None,
        },
    ]);
    config.state_path = directory.path().join("state/state.json");
    let (scripts, warnings, _) = catalog(&config, false).unwrap();
    assert_eq!(scripts.len(), MAX_SCRIPTS);
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("all banks combined are limited"))
    );
}

#[test]
fn store_bank_cache_is_noop_when_tree_fingerprint_is_none() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("not-a-directory");
    fs::write(&file, b"x").unwrap();
    let config = config(Vec::new());
    let payload = ScanPayload {
        name: "bank".into(),
        scripts: Vec::new(),
        warnings: Vec::new(),
        error: None,
    };
    store_bank_cache(&config, &file, &payload);
    assert!(load_bank_cache(&config, &file).is_none());
}
