use super::*;
use crate::config::ClusterConfig;
use std::os::unix::fs::{PermissionsExt, symlink};

fn test_config(banks: Vec<SbatchBankConfig>) -> Config {
    Config {
        local_user: "alice".into(),
        remote_user: "alice".into(),
        ssh_host: "cluster".into(),
        state_path: PathBuf::from("/tmp/slurm-log-bank-test.json"),
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
fn extracts_job_names() {
    assert_eq!(
        directive_job_name(&["--job-name=train".into()]),
        Some("train".into())
    );
    assert_eq!(directive_job_name(&["-J eval".into()]), Some("eval".into()));
    assert_eq!(
        directive_job_name(&["--job-name eval".into()]),
        Some("eval".into())
    );
}

#[test]
fn scanner_uses_only_the_effective_sbatch_preamble_and_rejects_controls() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("effective.sbatch"),
        b"#!/bin/sh\n#SBATCH --job-name=effective\necho start\n#SBATCH --job-name=ignored\n",
    )
    .unwrap();
    fs::write(
        root.path().join("unsafe.sbatch"),
        b"#!/bin/sh\n#SBATCH --job-name=bad\x1b]52;c;clipboard\x07\n",
    )
    .unwrap();
    let (scripts, warnings) = scan(root.path()).unwrap();
    assert_eq!(scripts.len(), 1);
    assert_eq!(scripts[0].name, "effective");
    assert_eq!(scripts[0].directives, ["--job-name=effective"]);
    assert_eq!(
        warnings,
        ["ignored script with terminal control characters"]
    );
}

#[test]
fn recursive_scan_is_sorted_and_ignores_symlinks() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("group/nested")).unwrap();
    fs::write(root.path().join("z.sbatch"), b"#!/bin/sh\n#SBATCH -J top\n").unwrap();
    fs::write(
        root.path().join("group/nested/a.sbatch"),
        b"#SBATCH --job-name=deep\n",
    )
    .unwrap();
    fs::write(root.path().join("ignored.sh"), b"#SBATCH -J ignored\n").unwrap();
    symlink(
        root.path().join("z.sbatch"),
        root.path().join("link.sbatch"),
    )
    .unwrap();
    let (scripts, warnings) = scan(root.path()).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(scripts.len(), 2);
    assert_eq!(scripts[0].relative, PathBuf::from("group/nested/a.sbatch"));
    assert_eq!(scripts[0].name, "deep");
    assert_eq!(scripts[1].name, "top");
}

#[test]
fn depth_limit_is_quiet_and_does_not_scan_deeper_scripts() {
    let root = tempfile::tempdir().unwrap();
    let deep = root.path().join("one/two/three/four");
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("too-deep.sbatch"), b"#!/bin/sh\n").unwrap();
    let (scripts, warnings) = scan(root.path()).unwrap();
    assert!(scripts.is_empty());
    assert!(warnings.is_empty());
}

#[test]
fn bank_name_prefers_custom_then_git_then_directory() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("useful-repo");
    let nested = repository.join("cluster/scripts");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir(repository.join(".git")).unwrap();
    let unnamed = root.path().join("plain-bank");
    fs::create_dir(&unnamed).unwrap();
    assert_eq!(
        inferred_name(&SbatchBankConfig {
            path: nested.clone(),
            name: Some("My Runs".into()),
        })
        .unwrap(),
        "My Runs"
    );
    assert_eq!(
        inferred_name(&SbatchBankConfig {
            path: nested,
            name: None,
        })
        .unwrap(),
        "useful-repo"
    );
    assert_eq!(fallback_name(&unnamed), "plain-bank");
}

#[test]
fn duplicate_inferred_names_are_disambiguated_and_scoped() {
    let root = tempfile::tempdir().unwrap();
    let left = root.path().join("left/shared");
    let right = root.path().join("right/shared");
    fs::create_dir_all(&left).unwrap();
    fs::create_dir_all(&right).unwrap();
    fs::write(left.join("same.sbatch"), b"#!/bin/sh\n").unwrap();
    fs::write(right.join("same.sbatch"), b"#!/bin/sh\n").unwrap();
    let config = test_config(vec![
        SbatchBankConfig {
            path: left,
            name: None,
        },
        SbatchBankConfig {
            path: right,
            name: None,
        },
    ]);
    let (banks, scripts, warnings) = scan_all(&config).unwrap();
    assert!(warnings.is_empty());
    let base = inferred_name(&config.sbatch_banks[0]).unwrap();
    assert_eq!(banks[0].name, base);
    assert_eq!(banks[1].name, format!("{base} (2)"));
    assert_eq!(scripts[0].bank, base);
    assert_eq!(scripts[1].bank, format!("{base} (2)"));
}

#[test]
fn bank_tree_indents_folders_and_their_files_by_level() {
    assert_eq!(row_indent(&BankRow::Bank(0, true, 1)), 0);
    assert_eq!(
        row_indent(&BankRow::Directory(0, PathBuf::from("jobs"), 1, true)),
        2
    );
    assert_eq!(row_indent(&BankRow::File(0, 1)), 2);
    assert_eq!(row_indent(&BankRow::File(0, 2)), 4);
}

#[test]
fn script_origin_uses_cluster_prefixes_and_keeps_ambiguous_files_shared() {
    let mut config = test_config(Vec::new());
    config.clusters[0].name = "sprint".into();
    config.clusters.push(ClusterConfig {
        name: "cispa".into(),
        controller: None,
        transport: "ssh".into(),
        user: "alice".into(),
        ssh_host: "cispa.example".into(),
        working_directory: PathBuf::from("/work"),
        accounting: true,
    });
    let script = |path: &str| Script {
        bank: "bank".into(),
        relative: PathBuf::from(path),
        name: "job".into(),
        directives: Vec::new(),
        origin: None,
        bytes: Vec::new(),
    };
    assert_eq!(
        infer_script_origin(&script("cluster/cispa_train.sbatch"), &config).as_deref(),
        Some("cispa")
    );
    assert_eq!(
        infer_script_origin(&script("cluster/sprint1_eval.sbatch"), &config).as_deref(),
        Some("sprint")
    );
    assert_eq!(
        infer_script_origin(&script("cluster/eval.sbatch"), &config),
        None
    );
}

#[test]
fn submit_confirmation_uses_crlf_for_every_terminal_line() {
    let config = test_config(Vec::new());
    let script = Script {
        bank: "bank".into(),
        relative: PathBuf::from("train.sbatch"),
        name: "train".into(),
        directives: vec!["--gpus=2".into(), "--time=1:00:00".into()],
        origin: None,
        bytes: Vec::new(),
    };
    let text = submit_confirmation(&script, &config.clusters[0]);
    assert!(text.as_bytes().iter().enumerate().all(
            |(index, byte)| *byte != b'\n' || index > 0 && text.as_bytes()[index - 1] == b'\r'
        ));
    assert!(text.contains("submit and open its pane"));
    assert_eq!(confirmation_choice(KeyCode::Char('y')), Some(true));
    assert_eq!(confirmation_choice(KeyCode::Esc), Some(false));
    assert_eq!(confirmation_choice(KeyCode::Char('a')), None);
}

#[test]
fn selected_submission_target_is_obvious_in_cluster_tabs() {
    let mut config = test_config(Vec::new());
    config.clusters.push(ClusterConfig {
        name: "remote".into(),
        controller: None,
        transport: "ssh".into(),
        user: "alice".into(),
        ssh_host: "remote".into(),
        working_directory: PathBuf::from("/work"),
        accounting: true,
    });
    assert_eq!(cluster_tabs(&config, 0), "[local]  remote");
    assert_eq!(cluster_tabs(&config, 1), "local  [remote]");
}

#[test]
fn bank_rows_cover_nested_expansion_cluster_filtering_and_search() {
    let script = |path: &str, origin: Option<&str>| Script {
        bank: "bank".into(),
        relative: PathBuf::from(path),
        name: path.into(),
        directives: Vec::new(),
        origin: origin.map(str::to_string),
        bytes: Vec::new(),
    };
    let scripts = vec![
        script("one/two/train.sbatch", None),
        script("one/eval.sbatch", Some("local")),
        script("top.sbatch", None),
        script("remote-only.sbatch", Some("remote")),
    ];
    let banks = [
        LoadedBank {
            name: "bank".into(),
            first: 0,
            last: 3,
        },
        LoadedBank {
            name: "remote".into(),
            first: 3,
            last: 4,
        },
    ];

    let closed = rows(&banks, &scripts, &HashSet::new(), "", "local");
    assert_eq!(closed.len(), 1);
    assert!(matches!(closed[0], BankRow::Bank(0, false, 3)));

    let bank_open = HashSet::from([Expanded::Bank(0)]);
    let first_level = rows(&banks, &scripts, &bank_open, "", "local");
    assert!(first_level.iter().any(|row| {
        matches!(row, BankRow::Directory(0, path, 2, false) if path == Path::new("one"))
    }));
    assert!(
        first_level
            .iter()
            .any(|row| matches!(row, BankRow::File(2, 1)))
    );
    assert!(
        !first_level
            .iter()
            .any(|row| matches!(row, BankRow::File(0, _)))
    );

    let fully_open = HashSet::from([
        Expanded::Bank(0),
        Expanded::Directory(0, PathBuf::from("one")),
        Expanded::Directory(0, PathBuf::from("one/two")),
    ]);
    let nested = rows(&banks, &scripts, &fully_open, "", "local");
    assert!(nested.iter().any(|row| {
        matches!(row, BankRow::Directory(0, path, 3, true) if path == Path::new("one/two"))
    }));
    assert!(nested.iter().any(|row| matches!(row, BankRow::File(0, 3))));
    assert!(nested.iter().any(|row| matches!(row, BankRow::File(1, 2))));

    let found = rows(&banks, &scripts, &HashSet::new(), "TWO/TRAIN", "local");
    assert_eq!(found.len(), 1);
    assert!(matches!(found[0], BankRow::File(0, 1)));
    assert!(rows(&banks, &scripts, &HashSet::new(), "eval", "remote").is_empty());
}

#[test]
fn private_bank_cache_round_trips_without_changing_payload() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("bank");
    fs::create_dir(&root).unwrap();
    let mut config = test_config(Vec::new());
    config.state_path = directory.path().join("state/state.json");
    let payload = ScanPayload {
        name: "bank".into(),
        scripts: vec![Script {
            bank: String::new(),
            relative: PathBuf::from("run.sbatch"),
            name: "run".into(),
            directives: vec!["--gpus=1".into()],
            origin: None,
            bytes: b"#!/bin/sh\n".to_vec(),
        }],
        warnings: Vec::new(),
        error: None,
    };
    store_bank_cache(&config, &root, &payload);
    let cached = load_bank_cache(&config, &root).unwrap();
    assert_eq!(cached.name, "bank");
    assert_eq!(cached.scripts[0].bytes, b"#!/bin/sh\n");
    assert_eq!(
        fs::metadata(bank_cache_path(&config, &root))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn bank_cache_invalidates_nested_additions_changes_and_removals() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("bank");
    fs::create_dir_all(root.join("nested")).unwrap();
    fs::write(root.join("run.sbatch"), b"#!/bin/sh\n").unwrap();
    let mut config = test_config(Vec::new());
    config.state_path = directory.path().join("state/state.json");
    let cache_current = || {
        let (scripts, warnings) = scan_direct(&root).unwrap();
        store_bank_cache(
            &config,
            &root,
            &ScanPayload {
                name: "bank".into(),
                scripts,
                warnings,
                error: None,
            },
        );
        assert!(load_bank_cache(&config, &root).is_some());
    };

    cache_current();
    fs::write(root.join("nested/added.sbatch"), b"#!/bin/sh\n").unwrap();
    assert!(load_bank_cache(&config, &root).is_none());

    cache_current();
    fs::write(root.join("run.sbatch"), b"#!/bin/sh\ntrue\n").unwrap();
    assert!(load_bank_cache(&config, &root).is_none());

    cache_current();
    fs::remove_file(root.join("nested/added.sbatch")).unwrap();
    assert!(load_bank_cache(&config, &root).is_none());
}

#[test]
#[ignore = "release-mode performance budget"]
fn loads_twenty_thousand_cached_scripts_within_budget() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("bank");
    fs::create_dir(&root).unwrap();
    let mut config = test_config(Vec::new());
    config.state_path = directory.path().join("state/state.json");
    let payload = ScanPayload {
        name: "bank".into(),
        scripts: (0..20_000)
            .map(|id| Script {
                bank: String::new(),
                relative: PathBuf::from(format!("jobs/{id}.sbatch")),
                name: format!("job-{id}"),
                directives: vec!["--time=1:00:00".into()],
                origin: None,
                bytes: b"#!/bin/sh\n#SBATCH --time=1:00:00\n".to_vec(),
            })
            .collect(),
        warnings: Vec::new(),
        error: None,
    };
    store_bank_cache(&config, &root, &payload);
    let started = Instant::now();
    let cached = load_bank_cache(&config, &root).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(cached.scripts.len(), 20_000);
    assert!(elapsed < Duration::from_millis(150));
    eprintln!("load 20k cached scripts: {elapsed:?}");
}

#[test]
#[ignore = "release-mode performance budget"]
fn builds_twenty_thousand_bank_rows_within_budget() {
    let scripts: Vec<_> = (0..20_000)
        .map(|index| Script {
            bank: "bank".into(),
            relative: PathBuf::from(format!("group-{}/job-{index}.sbatch", index % 100)),
            name: format!("job-{index}"),
            directives: Vec::new(),
            origin: None,
            bytes: Vec::new(),
        })
        .collect();
    let mut expanded: HashSet<_> = (0..100)
        .map(|index| Expanded::Directory(0, PathBuf::from(format!("group-{index}"))))
        .collect();
    expanded.insert(Expanded::Bank(0));
    let banks = [LoadedBank {
        name: "bank".into(),
        first: 0,
        last: scripts.len(),
    }];
    let index_started = std::time::Instant::now();
    let index = BankIndex::new(&banks, &scripts, ["local"]);
    let index_elapsed = index_started.elapsed();
    let started = std::time::Instant::now();
    assert_eq!(index.rows(&scripts, &expanded, "", "local").len(), 20_101);
    let elapsed = started.elapsed();
    assert!(index_elapsed < std::time::Duration::from_millis(100));
    assert!(elapsed < std::time::Duration::from_millis(20));
    eprintln!("index 20k scripts once: {index_elapsed:?}; rebuild rows: {elapsed:?}");
}
