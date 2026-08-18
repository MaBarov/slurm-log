use super::*;

#[test]
fn home_expansion_worker_failures_and_zero_deadline_are_bounded() {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    assert_eq!(expand_home("~"), home.clone().unwrap_or_else(|| "~".into()));
    assert_eq!(
        expand_home("~/jobs"),
        home.map_or_else(|| PathBuf::from("~/jobs"), |path| path.join("jobs"))
    );

    let (sender, receiver) = mpsc::channel();
    discover_banks_worker(
        vec![PathBuf::from("/definitely/missing/slurm-log-root")],
        sender,
        Arc::new(AtomicBool::new(false)),
    );
    assert!(matches!(
        receiver.into_iter().last(),
        Some(DiscoveryEvent::Complete { truncated: false })
    ));

    let (sender, receiver) = mpsc::channel::<DiscoveryEvent>();
    drop(sender);
    let stopped = Arc::new(AtomicBool::new(false));
    assert_eq!(
        collect_discovery(receiver, Arc::clone(&stopped), Duration::ZERO),
        (Vec::new(), true)
    );
    assert!(stopped.load(Ordering::Relaxed));
}

#[test]
fn closed_file_receiver_and_unused_browser_keys_fail_safely() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("job.sbatch");
    fs::write(&script, "#!/bin/sh").unwrap();
    let (sender, receiver) = mpsc::channel();
    drop(receiver);
    discover_banks_worker(vec![script], sender, Arc::new(AtomicBool::new(false)));

    let mut current = None;
    let mut focus = 0;
    assert_eq!(
        apply_browser_key(KeyCode::Tab, &mut current, &mut focus, 0),
        BrowserKey::Continue
    );
    assert_eq!(
        apply_browser_key(KeyCode::Esc, &mut current, &mut focus, 0),
        BrowserKey::Cancel
    );
}

#[test]
fn wildcard_absolute_includes_and_ssh_include_failures_are_safe() {
    assert!(!wildcard_match("abc", "abd"));
    assert!(wildcard_match("ab*", "ab"));
    let directory = tempfile::tempdir().unwrap();
    let absolute = directory.path().join("absolute.conf");
    fs::write(&absolute, "Host absolute\n").unwrap();
    assert_eq!(
        include_paths(Path::new("config"), absolute.to_str().unwrap()),
        [absolute]
    );
    assert!(include_paths(Path::new("config"), "/").is_empty());

    let ssh = directory.path().join(".ssh");
    fs::create_dir_all(ssh.join("parts")).unwrap();
    fs::create_dir(ssh.join("directory.conf")).unwrap();
    fs::write(ssh.join("parts/one.conf"), "Host included\n").unwrap();
    let big = fs::File::create(ssh.join("big.conf")).unwrap();
    big.set_len(1024 * 1024 + 1).unwrap();
    fs::write(
        ssh.join("config"),
        "Host root\nInclude config parts/*.conf missing.conf directory.conf big.conf\n",
    )
    .unwrap();
    assert_eq!(
        ssh_config_aliases_from(directory.path()),
        vec!["included".to_string(), "root".to_string()]
    );
}

#[test]
fn unreadable_discovery_directories_and_broken_output_sinks_do_not_hang() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let locked = directory.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let (sender, receiver) = mpsc::channel();
    discover_banks_worker(
        vec![locked.clone()],
        sender,
        Arc::new(AtomicBool::new(false)),
    );
    assert!(matches!(
        receiver.into_iter().last(),
        Some(DiscoveryEvent::Complete { truncated: false })
    ));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();

    if Path::new("/dev/full").exists() {
        run_discovery_worker(&["/dev/full".into(), directory.path().display().to_string()])
            .unwrap();
    }
}
