// sbatch bank scanning: SecureDir walk, directives parsing, cache, and the
// isolated worker used by `slurm-log bank-scan-worker`.
//
// This file is an include! stream into bank.rs.  Keep `//` comments only, no
// top-level `use` items (the parent supplies them), and no test module.

fn scan_direct(root: &Path) -> Result<(Vec<Script>, Vec<String>)> {
    let root = crate::secure_open::SecureDir::open_root(root)
        .context("securely open configured sbatch bank")?;
    let mut stack = vec![(PathBuf::new(), root, 0_usize)];
    let mut scripts = Vec::new();
    let mut warnings = Vec::new();
    let mut payload_budget = scan_limits::PayloadBudget::new(MAX_BANK_CACHE_BYTES);
    while let Some((relative, directory, depth)) = stack.pop() {
        let mut entries: Vec<_> = fs::read_dir(directory.proc_path())?
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries.into_iter().rev() {
            let name = entry.file_name();
            let child = relative.join(&name);
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                // The depth bound is an intentional scan policy, not an error.
                // Do not descend merely to emit hundreds of identical notices.
                if depth < MAX_DEPTH
                    && !ignored_directory(&name.to_string_lossy())
                    && let Ok(next) = directory.open_directory(Path::new(&name))
                {
                    stack.push((child, next, depth + 1));
                }
                continue;
            }
            if !file_type.is_file()
                || child.extension().and_then(|value| value.to_str()) != Some("sbatch")
            {
                continue;
            }
            if scripts.len() == MAX_SCRIPTS {
                warnings.push(format!("bank limited to {MAX_SCRIPTS} scripts"));
                stack.clear();
                break;
            }
            let file = match directory.open_file(Path::new(&name)) {
                Ok(file) => file,
                Err(_) => {
                    warnings.push("ignored script changed while scanning".into());
                    continue;
                }
            };
            let opened = file.metadata()?;
            if opened.len() > MAX_SCRIPT_BYTES {
                warnings.push("ignored oversized script".into());
                continue;
            }
            let mut bytes = Vec::with_capacity(opened.len() as usize);
            file.take(MAX_SCRIPT_BYTES.saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_SCRIPT_BYTES {
                warnings.push("ignored oversized script".into());
                continue;
            }
            if !payload_budget.accept(bytes.len() as u64) {
                warnings.push("bank limited to 64 MiB of script data".into());
                stack.clear();
                break;
            }
            let text = String::from_utf8_lossy(&bytes);
            let directives = sbatch_directives(&text);
            let fallback = child
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("job");
            let name = directive_job_name(&directives).unwrap_or_else(|| fallback.into());
            if has_terminal_control(&name)
                || directives
                    .iter()
                    .any(|directive| has_terminal_control(directive))
                || child
                    .components()
                    .any(|component| has_terminal_control(&component.as_os_str().to_string_lossy()))
            {
                warnings.push("ignored script with terminal control characters".into());
                continue;
            }
            scripts.push(Script {
                bank: String::new(),
                relative: child,
                name,
                directives,
                origin: None,
                declared_results: parse_declared_results(&bytes),
                bytes,
            });
        }
    }
    scripts.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok((scripts, warnings))
}

fn sbatch_directives(text: &str) -> Vec<String> {
    let mut directives = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        if line.is_empty() {
            continue;
        }
        if let Some(directive) = line.strip_prefix("#SBATCH") {
            let directive = directive.trim();
            if !directive.is_empty() {
                directives.push(directive.to_string());
            }
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        break;
    }
    directives
}

fn has_terminal_control(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_control() || character == '\x1b')
}

/// Marker a batch script uses to declare the result files it may produce,
/// relative to the cluster working directory. Anything else the script
/// writes is not a declared result and cannot be read through the MCP
/// declared-result surface.
const DECLARED_RESULT_MARKER: &str = "#SLURM_LOG-RESULT:";

/// Extract basename globs declared with `#SLURM_LOG-RESULT: <glob>` comment
/// lines. Patterns are restricted to a single path component of at most 128
/// glob characters; slashes, whitespace, control characters, and duplicate
/// declarations are rejected or ignored.
fn parse_declared_results(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut results = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix(DECLARED_RESULT_MARKER) else {
            continue;
        };
        let pattern = rest.trim();
        if pattern.is_empty()
            || pattern.len() > 128
            || pattern.contains('/')
            || pattern.contains('\\')
            || !pattern
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._?*-".contains(&byte))
            || results.iter().any(|existing| existing == pattern)
        {
            continue;
        }
        results.push(pattern.to_string());
    }
    results
}

#[derive(Deserialize, Serialize)]
struct ScanPayload {
    name: String,
    scripts: Vec<Script>,
    warnings: Vec<String>,
    error: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct BankCache {
    schema: u8,
    root: PathBuf,
    fingerprint: u64,
    payload: ScanPayload,
}

#[derive(Serialize)]
struct BankCacheRef<'a> {
    schema: u8,
    root: &'a Path,
    fingerprint: u64,
    payload: &'a ScanPayload,
}

fn bank_cache_path(config: &Config, root: &Path) -> PathBuf {
    let mut hash = DefaultHasher::new();
    root.hash(&mut hash);
    config
        .state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("bank-{:016x}.msgpack", hash.finish()))
}

fn bank_tree_fingerprint(root: &Path) -> Option<u64> {
    let metadata = fs::symlink_metadata(root).ok()?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return None;
    }
    let mut hasher = DefaultHasher::new();
    let mut directories = vec![(root.to_path_buf(), PathBuf::new(), 0_usize)];
    let mut scripts = 0_usize;
    while let Some((directory, relative, depth)) = directories.pop() {
        let mut entries: Vec<_> = fs::read_dir(&directory)
            .ok()?
            .collect::<io::Result<_>>()
            .ok()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries.into_iter().rev() {
            let file_type = entry.file_type().ok()?;
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            let child = relative.join(entry.file_name());
            if file_type.is_dir() {
                if depth < MAX_DEPTH && !ignored_directory(&entry.file_name().to_string_lossy()) {
                    directories.push((path, child, depth + 1));
                }
                continue;
            }
            if !file_type.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("sbatch")
            {
                continue;
            }
            let metadata = entry.metadata().ok()?;
            child.hash(&mut hasher);
            metadata.len().hash(&mut hasher);
            metadata.dev().hash(&mut hasher);
            metadata.ino().hash(&mut hasher);
            metadata.mtime().hash(&mut hasher);
            metadata.mtime_nsec().hash(&mut hasher);
            metadata.ctime().hash(&mut hasher);
            metadata.ctime_nsec().hash(&mut hasher);
            scripts += 1;
            if scripts > MAX_SCRIPTS {
                return Some(hasher.finish());
            }
        }
    }
    Some(hasher.finish())
}

fn load_bank_cache(config: &Config, root: &Path) -> Option<ScanPayload> {
    let path = bank_cache_path(config, root);
    let metadata = fs::metadata(&path).ok()?;
    if metadata.len() > MAX_BANK_CACHE_BYTES
        || SystemTime::now()
            .duration_since(metadata.modified().ok()?)
            .ok()?
            > BANK_CACHE_TTL
    {
        return None;
    }
    let reader = BufReader::with_capacity(256 * 1024, fs::File::open(path).ok()?);
    let cache: BankCache = rmp_serde::from_read(reader).ok()?;
    (cache.schema == BANK_CACHE_SCHEMA
        && cache.root == root
        && Some(cache.fingerprint) == bank_tree_fingerprint(root))
    .then_some(cache.payload)
}

fn store_bank_cache(config: &Config, root: &Path, payload: &ScanPayload) {
    if payload.error.is_some() {
        return;
    }
    let Some(fingerprint) = bank_tree_fingerprint(root) else {
        return;
    };
    let path = bank_cache_path(config, root);
    let Some(parent) = path.parent() else { return };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let result = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .and_then(|file| {
            let mut writer = BufWriter::with_capacity(256 * 1024, file);
            rmp_serde::encode::write(
                &mut writer,
                &BankCacheRef {
                    schema: BANK_CACHE_SCHEMA,
                    root,
                    fingerprint,
                    payload,
                },
            )
            .map_err(io::Error::other)?;
            writer.flush()
        });
    if result.is_ok() {
        let _ = fs::rename(&temporary, path);
    } else {
        let _ = fs::remove_file(temporary);
    }
}

fn scan_output_path() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "slurm-log-bank-scan-{}-{nonce}.msgpack",
        std::process::id()
    ))
}

fn scan_isolated(root: &Path, time_limit: Duration) -> Result<ScanPayload> {
    let output = scan_output_path();
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&output)?;
    drop(file);
    let executable = std::env::current_exe()?;
    let mut child = Command::new(executable)
        .arg("bank-scan-worker")
        .arg(&output)
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("start isolated sbatch bank scan")?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    let _ = fs::remove_file(&output);
                    bail!("sbatch bank scanner exited with {status}");
                }
                break;
            }
            Ok(None) if started.elapsed() < time_limit => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = fs::remove_file(&output);
                bail!("bank scan exceeded {:.1}s", time_limit.as_secs_f32());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = fs::remove_file(&output);
                return Err(error).context("check isolated sbatch bank scan");
            }
        }
    }
    let file = fs::File::open(&output)?;
    let reader = BufReader::with_capacity(256 * 1024, file);
    let payload = rmp_serde::from_read(reader).context("read isolated sbatch bank scan")?;
    let _ = fs::remove_file(&output);
    Ok(payload)
}

pub fn run_scan_worker(arguments: &[String]) -> Result<()> {
    let output = arguments
        .first()
        .ok_or_else(|| anyhow::anyhow!("bank scan output path required"))?;
    let root = arguments
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("bank scan root required"))?;
    let root = Path::new(root);
    let payload = match scan_direct(root) {
        Ok((scripts, warnings)) => ScanPayload {
            name: inferred_name(&SbatchBankConfig {
                path: root.to_path_buf(),
                name: None,
            })
            .unwrap_or_else(|_| fallback_name(root)),
            scripts,
            warnings,
            error: None,
        },
        Err(error) => ScanPayload {
            name: fallback_name(root),
            scripts: Vec::new(),
            warnings: Vec::new(),
            error: Some(format!("{error:#}")),
        },
    };
    let file = OpenOptions::new().write(true).truncate(true).open(output)?;
    let mut writer = BufWriter::with_capacity(256 * 1024, file);
    rmp_serde::encode::write(&mut writer, &payload)?;
    writer.flush()?;
    Ok(())
}
