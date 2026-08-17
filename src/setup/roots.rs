fn suggested_workspace_roots(current: &Config) -> Vec<PathBuf> {
    // Hermetic integration builds must not trigger host automounters while
    // exercising setup with synthetic users. Production still probes the
    // live mount table for conventional per-user storage roots.
    let candidates =
        suggested_workspace_roots_with_host_storage(current, !cfg!(slurm_log_test_build));
    if candidates.is_empty() {
        return Vec::new();
    }
    if cfg!(test) {
        return candidates
            .into_iter()
            .filter(|candidate| candidate.is_dir())
            .collect();
    }
    // Existence probes run in a killable worker because a stale network mount
    // can block a stat indefinitely; the parent enforces the discovery budget.
    probe_suggested_roots_with(
        &worker_output_path("roots"),
        std::env::current_exe().ok(),
        &candidates,
        DISCOVERY_TIME_LIMIT,
    )
}

fn suggested_workspace_roots_with_host_storage(
    current: &Config,
    probe_host_storage: bool,
) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let environment_dirs: Vec<PathBuf> = ["SCRATCH", "WORK", "PROJECT_DIR", "PROJECTS"]
        .into_iter()
        .filter_map(|variable| std::env::var_os(variable).map(PathBuf::from))
        .collect();
    let mounts = if probe_host_storage {
        plausible_storage_mounts()
    } else {
        Vec::new()
    };
    collect_suggested_roots(
        current,
        probe_host_storage,
        home.as_deref(),
        &environment_dirs,
        &mounts,
    )
}

fn collect_suggested_roots(
    current: &Config,
    probe_host_storage: bool,
    home: Option<&Path>,
    environment_dirs: &[PathBuf],
    storage_mounts: &[PathBuf],
) -> Vec<PathBuf> {
    let mut candidates = BTreeSet::new();
    if let Some(path) = home {
        candidates.insert(path.to_path_buf());
    }
    candidates.extend(environment_dirs.iter().cloned());
    let mut identities = BTreeSet::from([current.local_user.as_str()]);
    if let Some(name) = home.and_then(Path::file_name).and_then(|v| v.to_str()) {
        identities.insert(name);
    }
    if probe_host_storage {
        for identity in &identities {
            for mount in storage_mounts {
                candidates.insert(mount.join(identity));
            }
        }
    }
    candidates.into_iter().collect()
}

/// Mount points that plausibly hold per-user project storage, read from the
/// live kernel mount table so setup works on any system without hardcoded
/// cluster paths. Virtual, snapshot, and automount filesystems are excluded
/// because statting them can trigger mounts or hang on stale servers.
fn plausible_storage_mounts() -> Vec<PathBuf> {
    let Ok(text) = fs::read_to_string("/proc/mounts") else {
        return Vec::new();
    };
    parse_storage_mounts(&text)
}

fn parse_storage_mounts(mounts: &str) -> Vec<PathBuf> {
    const STORAGE_FILESYSTEMS: &[&str] = &[
        "ext2", "ext3", "ext4", "xfs", "btrfs", "zfs", "reiserfs", "jfs", "ntfs", "ntfs3",
        "exfat", "vfat", "hfs", "hfsplus", "nfs", "nfs4", "cifs", "smb3", "lustre", "ceph",
        "9p", "fuse",
    ];
    const MAX_SUGGESTED_MOUNTS: usize = 16;
    let mut roots = BTreeSet::new();
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let Some(mount) = fields.nth(1) else {
            continue;
        };
        let Some(filesystem) = fields.next() else {
            continue;
        };
        if mount.contains('\\') {
            continue;
        }
        if !STORAGE_FILESYSTEMS.contains(&filesystem) && !filesystem.starts_with("fuse.ssh") {
            continue;
        }
        let path = Path::new(mount);
        if path == Path::new("/") {
            continue;
        }
        if path
            .strip_prefix("/")
            .ok()
            .is_none_or(|rest| rest.components().count() > 2)
        {
            continue;
        }
        if path.starts_with("/proc")
            || path.starts_with("/sys")
            || path.starts_with("/dev")
            || path.starts_with("/run")
            || path.starts_with("/snap")
            || path.starts_with("/boot")
            || path.starts_with("/efi")
            || path.starts_with("/var")
            || path.starts_with("/etc")
            || path.starts_with("/usr")
        {
            continue;
        }
        roots.insert(path.to_path_buf());
        if roots.len() >= MAX_SUGGESTED_MOUNTS {
            break;
        }
    }
    roots.into_iter().collect()
}

fn display_workspace_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|path| shell_words::quote(&path.display().to_string()).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn worker_output_path(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "slurm-log-{label}-{}-{nonce}.jsonl",
        std::process::id()
    ))
}

enum WorkerRun {
    Completed,
    TimedOut,
    NotStarted,
}

/// Spawn `executable` as an internal worker (`mode`) that appends JSON lines
/// to `output`, enforcing the shared safety budget so a worker blocked on a
/// stale mount or filesystem walk is killed instead of wedging setup.
fn run_worker(
    output: &Path,
    executable: Option<PathBuf>,
    mode: &str,
    arguments: &[PathBuf],
    time_limit: Duration,
) -> WorkerRun {
    let Ok(file) = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(output)
    else {
        return WorkerRun::NotStarted;
    };
    drop(file);
    let Some(executable) = executable else {
        let _ = fs::remove_file(output);
        return WorkerRun::NotStarted;
    };
    let mut command = Command::new(executable);
    command
        .arg(mode)
        .arg(output)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = command.spawn() else {
        let _ = fs::remove_file(output);
        return WorkerRun::NotStarted;
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return WorkerRun::Completed,
            Ok(None) if started.elapsed() < time_limit => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return WorkerRun::TimedOut;
            }
        }
    }
}

fn probe_suggested_roots_with(
    output: &Path,
    executable: Option<PathBuf>,
    candidates: &[PathBuf],
    time_limit: Duration,
) -> Vec<PathBuf> {
    if matches!(
        run_worker(
            output,
            executable,
            "setup-roots-worker",
            candidates,
            time_limit
        ),
        WorkerRun::NotStarted
    ) {
        return Vec::new();
    }
    let roots = read_roots_output(output);
    let _ = fs::remove_file(output);
    roots
}

fn read_roots_output(path: &Path) -> Vec<PathBuf> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let mut roots = BTreeSet::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(root) = value.get("root").and_then(|value| value.as_str()) {
            roots.insert(PathBuf::from(root));
        }
    }
    roots.into_iter().collect()
}

pub fn run_roots_worker(arguments: &[String]) -> Result<()> {
    let (output, candidates) = arguments
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("roots output path required"))?;
    let output = Path::new(output);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(output)?;
    for candidate in candidates {
        let Ok(path) = fs::canonicalize(Path::new(candidate)) else {
            continue;
        };
        if !path.is_dir() {
            continue;
        }
        if serde_json::to_writer(&mut file, &json!({ "root": path })).is_err()
            || writeln!(file).is_err()
            || file.flush().is_err()
        {
            break;
        }
    }
    let _ = serde_json::to_writer(&mut file, &json!({ "complete": true }));
    let _ = writeln!(file);
    Ok(())
}
