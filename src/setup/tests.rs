use super::*;

#[test]
fn selection_accepts_ranges_and_rejects_invalid_indices() {
    assert_eq!(parse_selection("1,3-4", 5).unwrap(), vec![0, 2, 3]);
    assert_eq!(parse_selection("all", 3).unwrap(), vec![0, 1, 2]);
    assert!(parse_selection("0", 3).is_err());
    assert!(parse_selection("2-5", 3).is_err());
}

#[test]
fn discovery_groups_scripts_by_repository_and_search_root() {
    let temporary = tempfile::tempdir().unwrap();
    let repository = temporary.path().join("project");
    fs::create_dir_all(repository.join(".git")).unwrap();
    fs::create_dir_all(repository.join("cluster/nested")).unwrap();
    fs::write(repository.join("cluster/a.sbatch"), "#!/bin/sh").unwrap();
    fs::write(repository.join("cluster/nested/b.sbatch"), "#!/bin/sh").unwrap();
    let loose = temporary.path().join("loose/jobs");
    fs::create_dir_all(&loose).unwrap();
    fs::write(loose.join("c.sbatch"), "#!/bin/sh").unwrap();

    let (banks, truncated) = discover_banks(&[repository.clone(), loose.clone()]);
    assert!(!truncated);
    assert_eq!(banks, vec![loose, repository]);
    assert_eq!(bank_kind(&banks[0]), "FOLDER");
    assert_eq!(bank_kind(&banks[1]), "GIT");
}

#[test]
fn broad_roots_do_not_become_giant_loose_script_banks() {
    let temporary = tempfile::tempdir().unwrap();
    for directory in ["experiment-a", "experiment-b/sbatch"] {
        let directory = temporary.path().join(directory);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("job.sbatch"), "#!/bin/sh").unwrap();
    }
    let (banks, truncated) = discover_banks(&[temporary.path().into()]);
    assert!(!truncated);
    assert_eq!(
        banks,
        vec![
            temporary.path().join("experiment-a"),
            temporary.path().join("experiment-b")
        ]
    );
    assert!(!banks.contains(&temporary.path().to_path_buf()));
    assert_eq!(bank_kind(&banks[0]), "FOLDER");
}

#[test]
fn automatic_discovery_never_descends_beyond_three_levels() {
    let temporary = tempfile::tempdir().unwrap();
    let shallow = temporary.path().join("shallow/one/two");
    let deep = temporary.path().join("deep/one/two/three");
    fs::create_dir_all(&shallow).unwrap();
    fs::create_dir_all(&deep).unwrap();
    fs::write(shallow.join("found.sbatch"), "#!/bin/sh").unwrap();
    fs::write(deep.join("ignored.sbatch"), "#!/bin/sh").unwrap();

    let (banks, truncated) = discover_banks(&[temporary.path().into()]);
    assert!(!truncated);
    assert_eq!(banks, vec![temporary.path().join("shallow")]);
}

#[test]
fn discovery_worker_streams_safe_results_to_its_output_file() {
    let temporary = tempfile::tempdir().unwrap();
    let repository = temporary.path().join("project");
    fs::create_dir_all(repository.join(".git")).unwrap();
    fs::write(repository.join("job.sbatch"), "#!/bin/sh").unwrap();
    let output = temporary.path().join("results.jsonl");
    fs::write(&output, "").unwrap();

    run_discovery_worker(&[
        output.display().to_string(),
        temporary.path().display().to_string(),
    ])
    .unwrap();
    let (banks, truncated) = read_discovery_output(&output);
    assert!(!truncated);
    assert_eq!(banks, vec![repository]);
}

#[test]
fn folder_browser_names_cannot_inject_terminal_controls() {
    assert_eq!(
        safe_terminal_name(std::ffi::OsStr::new("bad\u{1b}[2J\nname")),
        "bad�[2J�name"
    );
}

#[test]
fn discovery_deadline_does_not_wait_for_a_blocked_worker() {
    let (sender, receiver) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let keep_open = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        drop(sender);
    });
    let started = Instant::now();
    let (banks, truncated) =
        collect_discovery(receiver, Arc::clone(&stop), Duration::from_millis(10));
    assert!(truncated);
    assert!(banks.is_empty());
    assert!(stop.load(Ordering::Relaxed));
    assert!(started.elapsed() < Duration::from_millis(100));
    keep_open.join().unwrap();
}

#[test]
fn ssh_config_parser_keeps_only_selectable_literal_aliases() {
    let aliases = ssh_aliases_from_text(
        "Host *\nHost cispa sprint *.internal !blocked\nHOST gpu-box # comment\n",
    );
    assert_eq!(aliases, vec!["cispa", "sprint", "gpu-box"]);
    assert!(wildcard_match("*.conf", "cluster.conf"));
    assert!(!wildcard_match("*.conf", "cluster.txt"));
}

#[test]
fn ssh_probe_values_become_safe_editable_defaults() {
    let probe = parse_ssh_probe(
        "SLURM_LOG_USER=alice\nSLURM_LOG_HOME=/remote/home/alice\nSLURM_LOG_CLUSTER=gpu lab\nSLURM_LOG_ACCOUNTING=yes\n",
    );
    assert_eq!(probe.user.as_deref(), Some("alice"));
    assert_eq!(probe.home.as_deref(), Some("/remote/home/alice"));
    assert_eq!(probe.accounting, Some(true));
    assert_eq!(safe_cluster_name("gpu lab", "remote"), "gpu-lab");
    assert_eq!(safe_cluster_name("all", "remote-alias"), "remote-alias");
}

#[test]
fn suggested_workspace_root_text_round_trips_paths_with_spaces() {
    let roots = vec![
        PathBuf::from("/home/alice"),
        PathBuf::from("/work/my project"),
    ];
    let displayed = display_workspace_roots(&roots);
    let parsed: Vec<PathBuf> = shell_words::split(&displayed)
        .unwrap()
        .into_iter()
        .map(PathBuf::from)
        .collect();
    assert_eq!(parsed, roots);
}
