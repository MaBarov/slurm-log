fn has_explicit_clusters() -> bool {
    fs::read(config_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.get("clusters").cloned())
        .is_some_and(|clusters| clusters.as_array().is_some_and(|items| !items.is_empty()))
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

fn ignored_discovery_directory(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".cache"
            | ".cargo"
            | ".local"
            | ".venv"
            | "venv"
            | "__pycache__"
            | "node_modules"
            | "target"
            | "build"
    )
}

fn loose_bank_root(search_root: &Path, script_directory: &Path) -> PathBuf {
    // A broad workspace root is only a discovery boundary, not automatically
    // a bank. Group loose scripts by its first child so scanning `$HOME` does
    // not create a giant home-directory bank.
    script_directory
        .strip_prefix(search_root)
        .ok()
        .and_then(|relative| relative.components().next())
        .map(|component| search_root.join(component.as_os_str()))
        .unwrap_or_else(|| search_root.to_path_buf())
}

fn bank_kind(path: &Path) -> &'static str {
    if path.join(".git").exists() {
        "GIT"
    } else {
        "FOLDER"
    }
}

/// Find one bank per Git repository. Loose sbatch files are grouped under the
/// search root, so users enter workspace roots rather than every script folder.
fn discover_banks(roots: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    if cfg!(test) {
        return discover_banks_in_process(roots);
    }
    discover_banks_subprocess(roots)
}

fn discover_banks_in_process(roots: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    let (sender, receiver) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let owned_roots = roots.to_vec();
    if thread::Builder::new()
        .name("sbatch-bank-discovery".into())
        .spawn(move || discover_banks_worker(owned_roots, sender, worker_stop))
        .is_err()
    {
        return (Vec::new(), true);
    }

    collect_discovery(receiver, stop, DISCOVERY_TIME_LIMIT)
}

fn read_discovery_output(path: &Path) -> (Vec<PathBuf>, bool) {
    let Ok(file) = fs::File::open(path) else {
        return (Vec::new(), true);
    };
    let mut banks = BTreeSet::new();
    let mut truncated = true;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(bank) = value.get("bank").and_then(|value| value.as_str()) {
            banks.insert(PathBuf::from(bank));
        }
        if value.get("complete").and_then(|value| value.as_bool()) == Some(true) {
            truncated = value
                .get("truncated")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
        }
    }
    (banks.into_iter().collect(), truncated)
}

fn discover_banks_subprocess(roots: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    discover_banks_subprocess_with(
        &worker_output_path("discovery"),
        std::env::current_exe().ok(),
        roots,
        DISCOVERY_TIME_LIMIT,
    )
}

fn discover_banks_subprocess_with(
    output: &Path,
    executable: Option<PathBuf>,
    roots: &[PathBuf],
    time_limit: Duration,
) -> (Vec<PathBuf>, bool) {
    let ran = run_worker(output, executable, "setup-discover-worker", roots, time_limit);
    if matches!(ran, WorkerRun::NotStarted) {
        return (Vec::new(), true);
    }
    let (banks, worker_truncated) = read_discovery_output(output);
    let _ = fs::remove_file(output);
    (banks, matches!(ran, WorkerRun::TimedOut) || worker_truncated)
}

pub fn run_discovery_worker(arguments: &[String]) -> Result<()> {
    let (output, roots) = arguments
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("discovery output path required"))?;
    let output = Path::new(output);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(output)?;
    let (sender, receiver) = mpsc::channel();
    let writer = thread::spawn(move || {
        let mut file = file;
        while let Ok(event) = receiver.recv() {
            let value = match event {
                DiscoveryEvent::Bank(path) => json!({ "bank": path }),
                DiscoveryEvent::Complete { truncated } => {
                    json!({ "complete": true, "truncated": truncated })
                }
            };
            if serde_json::to_writer(&mut file, &value).is_err()
                || writeln!(file).is_err()
                || file.flush().is_err()
            {
                break;
            }
        }
    });
    discover_banks_worker(
        roots.iter().map(PathBuf::from).collect(),
        sender,
        Arc::new(AtomicBool::new(false)),
    );
    let _ = writer.join();
    Ok(())
}

fn collect_discovery(
    receiver: mpsc::Receiver<DiscoveryEvent>,
    stop: Arc<AtomicBool>,
    time_limit: Duration,
) -> (Vec<PathBuf>, bool) {
    let mut banks = BTreeSet::new();
    let started = Instant::now();
    let truncated = loop {
        let Some(remaining) = time_limit.checked_sub(started.elapsed()) else {
            stop.store(true, Ordering::Relaxed);
            break true;
        };
        match receiver.recv_timeout(remaining) {
            Ok(DiscoveryEvent::Bank(path)) => {
                banks.insert(path);
            }
            Ok(DiscoveryEvent::Complete { truncated }) => break truncated,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                stop.store(true, Ordering::Relaxed);
                break true;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break true,
        }
    };
    (banks.into_iter().collect(), truncated)
}

enum DiscoveryEvent {
    Bank(PathBuf),
    Complete { truncated: bool },
}

fn discover_banks_worker(
    roots: Vec<PathBuf>,
    sender: mpsc::Sender<DiscoveryEvent>,
    stop: Arc<AtomicBool>,
) {
    let mut banks = BTreeSet::new();
    let mut canonical_roots = BTreeSet::new();
    let mut queue = VecDeque::new();
    let mut visited = 0usize;

    for requested_root in &roots {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(root) = fs::canonicalize(requested_root) else {
            continue;
        };
        if !canonical_roots.insert(root.clone()) {
            continue;
        }
        if root.is_file() {
            if root
                .extension()
                .is_some_and(|extension| extension == "sbatch")
                && let Some(parent) = root.parent()
            {
                let path = parent.to_path_buf();
                if banks.insert(path.clone()) && sender.send(DiscoveryEvent::Bank(path)).is_err() {
                    return;
                }
            }
            continue;
        }
        // Interleave broad roots rather than allowing the first large mount to
        // consume the entire budget before later roots are inspected.
        queue.push_back((root.clone(), root, None, 0usize));
    }

    // Never adopt a repository above a requested root: discovery must stay
    // inside the scope the user explicitly selected. Breadth-first traversal
    // also finds repository roots early and avoids diving into one huge tree.
    while let Some((search_root, directory, inherited_repository, depth)) = queue.pop_front() {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if visited >= DISCOVERY_DIRECTORY_LIMIT {
            let _ = sender.send(DiscoveryEvent::Complete { truncated: true });
            return;
        }
        visited += 1;
        let repository = if directory.join(".git").exists() {
            Some(directory.clone())
        } else {
            inherited_repository
        };
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "sbatch")
            {
                let bank = repository
                    .clone()
                    .unwrap_or_else(|| loose_bank_root(&search_root, &directory));
                if banks.insert(bank.clone()) && sender.send(DiscoveryEvent::Bank(bank)).is_err() {
                    return;
                }
            } else if kind.is_dir() && depth < DISCOVERY_DEPTH_LIMIT {
                let name = entry.file_name();
                if !ignored_discovery_directory(&name.to_string_lossy()) {
                    queue.push_back((search_root.clone(), path, repository.clone(), depth + 1));
                }
            }
        }
    }
    let _ = sender.send(DiscoveryEvent::Complete { truncated: false });
}

fn parse_selection(input: &str, count: usize) -> Result<Vec<usize>> {
    let input = input.trim();
    if input.is_empty() || input.eq_ignore_ascii_case("all") {
        return Ok((0..count).collect());
    }
    if input.eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }
    let mut selected = BTreeSet::new();
    for part in input
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let (start, end) = if let Some((start, end)) = part.split_once('-') {
            (start, end)
        } else {
            (part, part)
        };
        let start: usize = start.parse().context("bank selection must use numbers")?;
        let end: usize = end.parse().context("bank selection must use numbers")?;
        if start == 0 || end < start || end > count {
            bail!("bank selection {part} is outside 1-{count}");
        }
        selected.extend((start - 1)..end);
    }
    Ok(selected.into_iter().collect())
}
