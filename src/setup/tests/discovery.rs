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

#[test]
fn discovery_output_is_deduplicated_and_fail_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let missing = temporary.path().join("missing");
    assert_eq!(read_discovery_output(&missing), (Vec::new(), true));

    let output = temporary.path().join("output.jsonl");
    fs::write(
        &output,
        "not-json\n{\"bank\":\"/b\"}\n{\"bank\":\"/a\"}\n{\"bank\":\"/b\"}\n{\"complete\":true}\n",
    )
    .unwrap();
    assert_eq!(
        read_discovery_output(&output),
        (vec![PathBuf::from("/a"), PathBuf::from("/b")], true)
    );
    fs::write(&output, "{\"complete\":true,\"truncated\":false}\n").unwrap();
    assert_eq!(read_discovery_output(&output), (Vec::new(), false));
}

#[test]
fn discovery_channels_cover_completion_disconnect_and_early_stop() {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(DiscoveryEvent::Bank(PathBuf::from("/b")))
        .unwrap();
    sender
        .send(DiscoveryEvent::Bank(PathBuf::from("/a")))
        .unwrap();
    sender
        .send(DiscoveryEvent::Complete { truncated: true })
        .unwrap();
    let (banks, truncated) = collect_discovery(
        receiver,
        Arc::new(AtomicBool::new(false)),
        Duration::from_secs(1),
    );
    assert_eq!(banks, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    assert!(truncated);

    let (sender, receiver) = mpsc::channel::<DiscoveryEvent>();
    drop(sender);
    assert_eq!(
        collect_discovery(
            receiver,
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(1)
        ),
        (Vec::new(), true)
    );

    let (sender, receiver) = mpsc::channel();
    let stopped = Arc::new(AtomicBool::new(true));
    discover_banks_worker(vec![PathBuf::from("/does-not-matter")], sender, stopped);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn discovery_handles_file_roots_duplicates_symlinks_and_closed_receivers() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let script = temporary.path().join("job.sbatch");
    let ordinary = temporary.path().join("notes.txt");
    fs::write(&script, "#!/bin/sh").unwrap();
    fs::write(&ordinary, "text").unwrap();
    symlink(temporary.path(), temporary.path().join("loop")).unwrap();
    fs::create_dir(temporary.path().join("target")).unwrap();
    fs::write(temporary.path().join("target/ignored.sbatch"), "").unwrap();

    let (sender, receiver) = mpsc::channel();
    discover_banks_worker(
        vec![script.clone(), ordinary, script],
        sender,
        Arc::new(AtomicBool::new(false)),
    );
    let events: Vec<_> = receiver.into_iter().collect();
    assert!(matches!(
        events.last(),
        Some(DiscoveryEvent::Complete { truncated: false })
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, DiscoveryEvent::Bank(_)))
            .count(),
        1
    );

    let (sender, receiver) = mpsc::channel();
    drop(receiver);
    discover_banks_worker(
        vec![temporary.path().to_path_buf()],
        sender,
        Arc::new(AtomicBool::new(false)),
    );
}

#[test]
fn discovery_worker_rejects_missing_or_unwritable_output() {
    assert!(run_discovery_worker(&[]).is_err());
    assert!(
        run_discovery_worker(&["/definitely/missing/output.jsonl".into(), "/tmp".into()]).is_err()
    );
}

#[test]
fn workspace_suggestions_and_path_helpers_are_stable() {
    let config = Config {
        local_user: "coverage-user".into(),
        remote_user: "coverage-user".into(),
        ssh_host: String::new(),
        state_path: PathBuf::from("/tmp/state.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: Vec::new(),
    };
    // Keep this unit test hermetic: production setup probes common cluster
    // mount roots, which may be automounted or temporarily unavailable.
    let suggestions = suggested_workspace_roots_with_host_storage(&config, false);
    assert_eq!(
        suggestions.iter().collect::<BTreeSet<_>>().len(),
        suggestions.len()
    );
    assert_eq!(
        expand_home("/absolute/path"),
        PathBuf::from("/absolute/path")
    );
    assert_eq!(
        loose_bank_root(Path::new("/a"), Path::new("/outside")),
        PathBuf::from("/a")
    );
    assert!(ignored_discovery_directory("target"));
    assert!(ignored_discovery_directory("node_modules"));
    assert!(!ignored_discovery_directory("experiments"));
}
