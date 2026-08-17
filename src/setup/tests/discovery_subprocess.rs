use super::*;

#[test]
fn suggested_root_candidates_cover_home_environment_and_mounts() {
    let temporary = tempfile::tempdir().unwrap();
    let scratch = temporary.path().join("scratch");
    let missing = temporary.path().join("missing");
    let config = Config {
        local_user: "alice".into(),
        remote_user: "alice".into(),
        ssh_host: String::new(),
        state_path: PathBuf::from("/tmp/state.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: Vec::new(),
    };
    let roots = collect_suggested_roots(
        &config,
        false,
        Some(Path::new("/home/alice")),
        &[scratch.clone(), missing.clone()],
        &[],
    );
    assert!(roots.contains(&PathBuf::from("/home/alice")));
    // Candidates are verified by the killable probe, never statted inline:
    // even not-yet-created environment directories stay eligible.
    assert!(roots.contains(&scratch));
    assert!(roots.contains(&missing));
    assert!(collect_suggested_roots(&config, false, None, &[], &[]).is_empty());

    let mount = temporary.path().join("mount");
    let candidates = collect_suggested_roots(
        &config,
        true,
        Some(Path::new("/home/alice")),
        &[],
        std::slice::from_ref(&mount),
    );
    assert!(candidates.contains(&mount.join("alice")));
    let candidates = collect_suggested_roots(
        &config,
        false,
        Some(Path::new("/home/alice")),
        &[],
        std::slice::from_ref(&mount),
    );
    assert!(!candidates.contains(&mount.join("alice")));
}

#[test]
fn storage_mount_parsing_keeps_plausible_roots_and_drops_virtual_ones() {
    let mounts = "proc /proc proc rw 0 0\n\
                  tmpfs /run tmpfs rw 0 0\n\
                  sysfs /sys sysfs rw 0 0\n\
                  /dev/sda1 / ext4 rw 0 0\n\
                  /dev/sdb1 /storage1 ext4 rw 0 0\n\
                  server:/x /mnt/research nfs rw 0 0\n\
                  /dev/sdc1 /snap/core20 squashfs ro 0 0\n\
                  /dev/sdd1 /home/simon/radioactivity-2026 ext4 rw 0 0\n\
                  auto /nfs/auto autofs rw 0 0\n\
                  gvfsd /run/user/1000/gvfs fuse.gvfsd-fuse rw 0 0\n\
                  user@host: /ssh/mount fuse.sshfs rw 0 0\n\
                  /dev/sde1 /media/usb vfat rw 0 0\n\
                  /dev/sdf1 /var/lib/docker overlay rw 0 0\n\
                  /dev/sdg1 /usr ext4 rw 0 0\n";
    let roots = parse_storage_mounts(mounts);
    assert_eq!(
        roots,
        vec![
            PathBuf::from("/media/usb"),
            PathBuf::from("/mnt/research"),
            PathBuf::from("/ssh/mount"),
            PathBuf::from("/storage1"),
        ]
    );
    assert!(parse_storage_mounts("garbage without fields\n").is_empty());
}

#[test]
fn roots_worker_writes_only_existing_directories() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join("real");
    fs::create_dir(&directory).unwrap();
    let file = temporary.path().join("file.sbatch");
    fs::write(&file, "#!/bin/sh\n").unwrap();
    let output = temporary.path().join("roots.jsonl");
    run_roots_worker(&[
        output.to_str().unwrap().to_string(),
        directory.to_str().unwrap().to_string(),
        file.to_str().unwrap().to_string(),
        "/definitely/missing/root".to_string(),
    ])
    .unwrap();
    let text = fs::read_to_string(&output).unwrap();
    assert!(text.contains(&format!(
        "\"root\":\"{}\"",
        fs::canonicalize(&directory).unwrap().display()
    )));
    assert!(!text.contains("file.sbatch"));
    assert!(text.contains("\"complete\":true"));
    assert!(run_roots_worker(&[]).is_err());
}

#[test]
fn roots_probe_error_paths_fail_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("roots.jsonl");
    fs::write(&output, "").unwrap();
    assert!(
        probe_suggested_roots_with(
            &output,
            Some(PathBuf::from("true")),
            &[],
            Duration::from_secs(1)
        )
        .is_empty()
    );

    let output = temporary.path().join("no-exe.jsonl");
    assert!(
        probe_suggested_roots_with(
            &output,
            None,
            &[temporary.path().to_path_buf()],
            Duration::from_secs(1)
        )
        .is_empty()
    );
    assert!(!output.exists());

    let output = temporary.path().join("missing-exe.jsonl");
    assert!(
        probe_suggested_roots_with(
            &output,
            Some(PathBuf::from("/definitely/missing/binary")),
            &[],
            Duration::from_secs(1)
        )
        .is_empty()
    );
    assert!(!output.exists());
}

#[test]
fn roots_probe_reports_worker_roots_and_kills_slow_workers() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().unwrap();
    let writer = temporary.path().join("writer.sh");
    fs::write(
        &writer,
        "#!/bin/sh\nprintf '{\"root\":\"/fake/root\"}\\n' >> \"$2\"\n",
    )
    .unwrap();
    fs::set_permissions(&writer, fs::Permissions::from_mode(0o755)).unwrap();
    let output = temporary.path().join("roots.jsonl");
    assert_eq!(
        probe_suggested_roots_with(
            &output,
            Some(writer),
            &[PathBuf::from("/fake/root")],
            Duration::from_secs(1)
        ),
        vec![PathBuf::from("/fake/root")]
    );
    assert!(!output.exists());

    let slow = temporary.path().join("slow.sh");
    fs::write(&slow, "#!/bin/sh\nexec sleep 10\n").unwrap();
    fs::set_permissions(&slow, fs::Permissions::from_mode(0o755)).unwrap();
    let output = temporary.path().join("blocked.jsonl");
    assert!(
        probe_suggested_roots_with(&output, Some(slow), &[], Duration::from_millis(50)).is_empty()
    );
}

#[test]
fn suggested_workspace_roots_verify_candidates_in_process() {
    let config = Config {
        local_user: "alice".into(),
        remote_user: "alice".into(),
        ssh_host: String::new(),
        state_path: PathBuf::from("/tmp/state.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: Vec::new(),
    };
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && home.is_dir()
    {
        assert!(suggested_workspace_roots(&config).contains(&home));
    }
    assert!(
        suggested_workspace_roots(&config)
            .iter()
            .all(|path| path.is_dir())
    );
}

#[test]
fn discovery_subprocess_error_paths_fail_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("output.jsonl");
    fs::write(&output, "").unwrap();
    assert_eq!(
        discover_banks_subprocess_with(
            &output,
            Some(PathBuf::from("true")),
            &[],
            Duration::from_secs(1)
        ),
        (Vec::new(), true)
    );

    let output = temporary.path().join("no-exe.jsonl");
    assert_eq!(
        discover_banks_subprocess_with(&output, None, &[], Duration::from_secs(1)),
        (Vec::new(), true)
    );
    assert!(!output.exists());

    let output = temporary.path().join("missing-exe.jsonl");
    assert_eq!(
        discover_banks_subprocess_with(
            &output,
            Some(PathBuf::from("/definitely/missing/binary")),
            &[],
            Duration::from_secs(1)
        ),
        (Vec::new(), true)
    );
    assert!(!output.exists());
}

#[test]
fn discovery_subprocess_timeout_kills_a_blocked_worker() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().unwrap();
    let slow = temporary.path().join("slow.sh");
    fs::write(&slow, "#!/bin/sh\nexec sleep 10\n").unwrap();
    fs::set_permissions(&slow, fs::Permissions::from_mode(0o755)).unwrap();
    let output = temporary.path().join("blocked.jsonl");
    let (banks, truncated) =
        discover_banks_subprocess_with(&output, Some(slow), &[], Duration::from_millis(50));
    assert!(truncated);
    assert!(banks.is_empty());
}

#[test]
fn discovery_directory_limit_truncates_an_overwide_scan() {
    let temporary = tempfile::tempdir().unwrap();
    let wide = temporary.path().join("wide");
    fs::create_dir(&wide).unwrap();
    for index in 0..DISCOVERY_DIRECTORY_LIMIT {
        fs::create_dir(wide.join(format!("d{index}"))).unwrap();
    }
    let (sender, receiver) = mpsc::channel();
    discover_banks_worker(vec![wide], sender, Arc::new(AtomicBool::new(false)));
    let events: Vec<_> = receiver.into_iter().collect();
    assert!(matches!(
        events.last(),
        Some(DiscoveryEvent::Complete { truncated: true })
    ));
}

#[test]
fn discovery_skips_symlinked_entries_without_following_them() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("job.sbatch"), "#!/bin/sh\n").unwrap();
    let target = temporary.path().join("target");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("hidden.sbatch"), "#!/bin/sh\n").unwrap();
    symlink(&target, root.join("linked")).unwrap();

    let (sender, receiver) = mpsc::channel();
    discover_banks_worker(vec![root.clone()], sender, Arc::new(AtomicBool::new(false)));
    let events: Vec<_> = receiver.into_iter().collect();
    let banks: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            DiscoveryEvent::Bank(path) => Some(path.clone()),
            DiscoveryEvent::Complete { .. } => None,
        })
        .collect();
    assert_eq!(banks, vec![root]);
    assert!(matches!(
        events.last(),
        Some(DiscoveryEvent::Complete { truncated: false })
    ));
}

#[test]
fn discovery_cancellation_stops_a_wide_directory_scan() {
    let temporary = tempfile::tempdir().unwrap();
    let wide = temporary.path().join("wide");
    fs::create_dir(&wide).unwrap();
    for index in 0..20_000 {
        fs::create_dir(wide.join(format!("d{index}"))).unwrap();
    }
    let (sender, receiver) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let stop = Arc::clone(&stop);
        thread::spawn(move || discover_banks_worker(vec![wide], sender, stop))
    };
    let (_, truncated) = collect_discovery(receiver, Arc::clone(&stop), Duration::from_millis(1));
    assert!(truncated);
    worker.join().unwrap();
}

#[test]
fn discovery_cancellation_stops_between_directory_iterations() {
    let temporary = tempfile::tempdir().unwrap();
    let signal = temporary.path().join("signal");
    fs::create_dir(&signal).unwrap();
    fs::write(signal.join("job.sbatch"), "#!/bin/sh\n").unwrap();
    let mut roots = vec![signal];
    for index in 0..100 {
        let empty = temporary.path().join(format!("empty{index}"));
        fs::create_dir(&empty).unwrap();
        roots.push(empty);
    }
    let (sender, receiver) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let stop = Arc::clone(&stop);
        thread::spawn(move || discover_banks_worker(roots, sender, stop))
    };
    while let Ok(event) = receiver.recv() {
        if matches!(event, DiscoveryEvent::Bank(_)) {
            stop.store(true, Ordering::Relaxed);
            break;
        }
    }
    // Keep draining so the worker never observes a closed channel; the empty
    // roots that follow have no entries, so the worker can only exit through
    // the stop flag between directory iterations.
    for _ in receiver {}
    worker.join().unwrap();
}
