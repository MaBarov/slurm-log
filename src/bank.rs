use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::{self, OpenOptions},
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, BufReader, BufWriter, IsTerminal, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    style::{Attribute, Print, SetAttribute},
    terminal::{self, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use serde::{Deserialize, Serialize};

use crate::{
    command::{shell_quote, ssh, text, text_with_input},
    config::{Config, SbatchBankConfig},
    model::{Job, valid_job_id},
};

const MAX_SCRIPTS: usize = 20_000;
const MAX_DEPTH: usize = 3;
const MAX_SCRIPT_BYTES: u64 = 4 * 1024 * 1024;
const BANK_SCAN_TIME_LIMIT: Duration = Duration::from_secs(3);
const BANK_CACHE_TTL: Duration = Duration::from_secs(30);
const MAX_BANK_CACHE_BYTES: u64 = 64 * 1024 * 1024;
const BANK_CACHE_SCHEMA: u8 = 1;

fn ignored_directory(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".hg"
            | ".svn"
            | ".cache"
            | ".venv"
            | "venv"
            | "env"
            | "node_modules"
            | "target"
            | "build"
            | "dist"
            | "__pycache__"
            | ".mypy_cache"
            | ".pytest_cache"
            | ".ruff_cache"
    )
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Script {
    pub bank: String,
    pub relative: PathBuf,
    pub name: String,
    pub directives: Vec<String>,
    #[serde(default)]
    pub origin: Option<String>,
    bytes: Vec<u8>,
}

#[cfg(test)]
fn scan(root: &Path) -> Result<(Vec<Script>, Vec<String>)> {
    scan_direct(root)
}

fn scan_direct(root: &Path) -> Result<(Vec<Script>, Vec<String>)> {
    let root = root
        .canonicalize()
        .with_context(|| format!("open bank {}", root.display()))?;
    if !root.is_dir() {
        bail!("sbatch bank is not a directory: {}", root.display());
    }
    let mut stack = vec![(root.clone(), 0_usize)];
    let mut scripts = Vec::new();
    let mut warnings = Vec::new();
    while let Some((directory, depth)) = stack.pop() {
        let mut entries: Vec<_> = fs::read_dir(&directory)?.filter_map(Result::ok).collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries.into_iter().rev() {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                // The depth bound is an intentional scan policy, not an error.
                // Do not descend merely to emit hundreds of identical notices.
                if depth < MAX_DEPTH && !ignored_directory(&entry.file_name().to_string_lossy()) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !metadata.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("sbatch")
            {
                continue;
            }
            if scripts.len() == MAX_SCRIPTS {
                warnings.push(format!("bank limited to {MAX_SCRIPTS} scripts"));
                stack.clear();
                break;
            }
            if metadata.len() > MAX_SCRIPT_BYTES {
                warnings.push(format!("ignored oversized script: {}", path.display()));
                continue;
            }
            let canonical = path.canonicalize()?;
            if !canonical.starts_with(&root) {
                continue;
            }
            let bytes = fs::read(&canonical)?;
            let text = String::from_utf8_lossy(&bytes);
            let directives: Vec<_> = text
                .lines()
                .filter_map(|line| line.trim_start().strip_prefix("#SBATCH"))
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect();
            let fallback = canonical
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("job");
            let name = directive_job_name(&directives).unwrap_or_else(|| fallback.into());
            scripts.push(Script {
                bank: String::new(),
                relative: canonical.strip_prefix(&root)?.to_path_buf(),
                name,
                directives,
                origin: None,
                bytes,
            });
        }
    }
    scripts.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok((scripts, warnings))
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
    payload: ScanPayload,
}

#[derive(Serialize)]
struct BankCacheRef<'a> {
    schema: u8,
    root: &'a Path,
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

fn load_bank_cache(config: &Config, root: &Path) -> Option<ScanPayload> {
    let path = bank_cache_path(config, root);
    let metadata = fs::metadata(&path).ok()?;
    if metadata.len() > MAX_BANK_CACHE_BYTES
        || SystemTime::now()
            .duration_since(metadata.modified().ok()?)
            .ok()?
            > BANK_CACHE_TTL
        || fs::metadata(root)
            .and_then(|root| root.modified())
            .is_ok_and(|modified| modified > metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH))
    {
        return None;
    }
    let reader = BufReader::with_capacity(256 * 1024, fs::File::open(path).ok()?);
    let cache: BankCache = rmp_serde::from_read(reader).ok()?;
    (cache.schema == BANK_CACHE_SCHEMA && cache.root == root).then_some(cache.payload)
}

fn store_bank_cache(config: &Config, root: &Path, payload: &ScanPayload) {
    if payload.error.is_some() {
        return;
    }
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

#[derive(Clone, Debug)]
struct LoadedBank {
    name: String,
    first: usize,
    last: usize,
}

fn inferred_name(bank: &SbatchBankConfig) -> Result<String> {
    if let Some(name) = bank
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        return Ok(name.to_string());
    }
    let canonical = bank
        .path
        .canonicalize()
        .with_context(|| format!("open bank {}", bank.path.display()))?;
    if let Some(repository) = canonical
        .ancestors()
        .find(|directory| directory.join(".git").exists())
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
    {
        return Ok(repository.to_string());
    }
    Ok(fallback_name(&canonical))
}

fn fallback_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Sbatch Bank")
        .to_string()
}

fn scan_all(config: &Config) -> Result<(Vec<LoadedBank>, Vec<Script>, Vec<String>)> {
    scan_all_inner(config, false)
}

fn scan_all_fresh(config: &Config) -> Result<(Vec<LoadedBank>, Vec<Script>, Vec<String>)> {
    scan_all_inner(config, true)
}

fn scan_all_inner(
    config: &Config,
    force: bool,
) -> Result<(Vec<LoadedBank>, Vec<Script>, Vec<String>)> {
    if config.sbatch_banks.is_empty() {
        bail!("no sbatch bank configured; run slurm-log setup");
    }
    let mut used = BTreeMap::<String, usize>::new();
    let mut banks = Vec::with_capacity(config.sbatch_banks.len());
    let mut scripts = Vec::new();
    let mut warnings = Vec::new();
    let started = Instant::now();
    for configured in &config.sbatch_banks {
        let payload = if cfg!(test) {
            let (scripts, warnings) = scan_direct(&configured.path)?;
            ScanPayload {
                name: inferred_name(configured)?,
                scripts,
                warnings,
                error: None,
            }
        } else if !force && let Some(payload) = load_bank_cache(config, &configured.path) {
            payload
        } else if let Some(remaining) = BANK_SCAN_TIME_LIMIT.checked_sub(started.elapsed()) {
            match scan_isolated(&configured.path, remaining) {
                Ok(payload) => {
                    store_bank_cache(config, &configured.path, &payload);
                    payload
                }
                Err(error) => ScanPayload {
                    name: fallback_name(&configured.path),
                    scripts: Vec::new(),
                    warnings: Vec::new(),
                    error: Some(format!("{error:#}")),
                },
            }
        } else {
            ScanPayload {
                name: fallback_name(&configured.path),
                scripts: Vec::new(),
                warnings: Vec::new(),
                error: Some("skipped after the 3s total bank-scan limit".into()),
            }
        };
        let base = configured
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .unwrap_or(payload.name);
        let count = used.entry(base.clone()).or_default();
        *count += 1;
        let name = if *count == 1 {
            base
        } else {
            format!("{base} ({count})")
        };
        let first = scripts.len();
        let mut found = payload.scripts;
        let remaining = MAX_SCRIPTS.saturating_sub(first);
        if found.len() > remaining {
            found.truncate(remaining);
            warnings.push(format!(
                "all banks combined are limited to {MAX_SCRIPTS} scripts"
            ));
        }
        for script in &mut found {
            script.bank.clone_from(&name);
            script.origin = infer_script_origin(script, config);
        }
        scripts.append(&mut found);
        warnings.extend(
            payload
                .warnings
                .into_iter()
                .map(|warning| format!("{name}: {warning}")),
        );
        if let Some(error) = payload.error {
            warnings.push(format!("{name}: unavailable ({error})"));
        }
        banks.push(LoadedBank {
            name,
            first,
            last: scripts.len(),
        });
        if scripts.len() == MAX_SCRIPTS {
            break;
        }
    }
    Ok((banks, scripts, warnings))
}

fn infer_script_origin(script: &Script, config: &Config) -> Option<String> {
    let text = script.relative.to_string_lossy().to_lowercase();
    let tokens: Vec<_> = text
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    let mut matches = config.clusters.iter().filter(|cluster| {
        let name = cluster.name.to_lowercase();
        let host = cluster
            .ssh_host
            .split(['.', '@'])
            .next()
            .unwrap_or("")
            .to_lowercase();
        tokens.iter().any(|token| {
            token_matches_cluster(token, &name)
                || (!host.is_empty() && token_matches_cluster(token, &host))
                || (!cluster.remote() && *token == "local")
        })
    });
    let first = matches.next()?.name.clone();
    matches.next().is_none().then_some(first)
}

fn token_matches_cluster(token: &str, cluster: &str) -> bool {
    token == cluster
        || token.strip_prefix(cluster).is_some_and(|suffix| {
            !suffix.is_empty() && suffix.chars().all(|value| value.is_ascii_digit())
        })
}

pub fn supports_cluster(script: &Script, cluster: &str) -> bool {
    script
        .origin
        .as_deref()
        .is_none_or(|origin| origin == cluster)
}

pub fn configured_scripts(config: &Config) -> Result<(Vec<Script>, Vec<String>)> {
    let (_, scripts, warnings) = scan_all(config)?;
    Ok((scripts, warnings))
}

fn directive_job_name(directives: &[String]) -> Option<String> {
    directives.iter().find_map(|line| {
        line.strip_prefix("--job-name=")
            .or_else(|| line.strip_prefix("-J="))
            .map(str::to_string)
            .or_else(|| line.strip_prefix("-J ").map(str::to_string))
    })
}

pub fn submit(config: &Config, script: &Script, cluster: &str) -> Result<Job> {
    let target = config.cluster(cluster)?;
    let output = if target.remote() {
        let remote = format!(
            "cd {} && exec sbatch --parsable",
            shell_quote(&target.working_directory.display().to_string())
        );
        text_with_input(
            "ssh",
            &[
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=8",
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPersist=120",
                "-o",
                "ControlPath=~/.ssh/slurm-log-%C",
                &target.ssh_host,
                &remote,
            ],
            &script.bytes,
            None,
        )?
    } else {
        text_with_input(
            "sbatch",
            &["--parsable"],
            &script.bytes,
            Some(&target.working_directory),
        )?
    };
    let id = output.trim().split(';').next().unwrap_or("");
    if !valid_job_id(id) {
        bail!("sbatch returned an invalid job ID: {:?}", output.trim());
    }
    crate::slurm::invalidate_caches(config);
    Ok(Job {
        cluster: cluster.into(),
        id: id.into(),
        state: "PENDING".into(),
        name: script.name.clone(),
        ..Job::default()
    })
}

pub fn cancel(config: &Config, jobs: &[Job]) -> Result<Vec<String>> {
    let mut grouped: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for job in jobs.iter().filter(|job| job.active()) {
        if !valid_job_id(&job.id) {
            bail!("invalid job ID {}", job.id);
        }
        grouped.entry(&job.cluster).or_default().push(&job.id);
    }
    let mut failures = Vec::new();
    for (cluster, ids) in grouped {
        let target = config.cluster(cluster)?;
        let result = if target.remote() {
            let command = std::iter::once("scancel".to_string())
                .chain(ids.iter().map(|id| shell_quote(id)))
                .collect::<Vec<_>>()
                .join(" ");
            ssh(&target.ssh_host, &command).map(|_| ())
        } else {
            text("scancel", &ids).map(|_| ())
        };
        if let Err(error) = result {
            failures.push(format!("{cluster}: {error:#}"));
        }
    }
    crate::slurm::invalidate_caches(config);
    Ok(failures)
}

#[derive(Clone)]
enum BankRow {
    Bank(usize, bool, usize),
    Directory(usize, PathBuf, usize, bool),
    File(usize, usize),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Expanded {
    Bank(usize),
    Directory(usize, PathBuf),
}

fn row_indent(row: &BankRow) -> usize {
    match row {
        BankRow::Bank(_, _, _) => 0,
        BankRow::Directory(_, _, depth, _) | BankRow::File(_, depth) => depth * 2,
    }
}

fn cluster_tabs(config: &Config, selected: usize) -> String {
    config
        .clusters
        .iter()
        .enumerate()
        .map(|(index, cluster)| {
            if index == selected {
                format!("[{}]", cluster.name)
            } else {
                cluster.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("  ")
}

fn rows(
    banks: &[LoadedBank],
    scripts: &[Script],
    expanded: &HashSet<Expanded>,
    query: &str,
    cluster: &str,
) -> Vec<BankRow> {
    if !query.is_empty() {
        let needle = query.to_lowercase();
        return scripts
            .iter()
            .enumerate()
            .filter(|(_, script)| {
                supports_cluster(script, cluster)
                    && format!("{}/{}", script.bank, script.relative.display())
                        .to_lowercase()
                        .contains(&needle)
            })
            .map(|(index, _)| BankRow::File(index, 1))
            .collect();
    }
    let mut result = Vec::new();
    for (bank_index, bank) in banks.iter().enumerate() {
        let eligible: Vec<_> = (bank.first..bank.last)
            .filter(|&index| supports_cluster(&scripts[index], cluster))
            .collect();
        if eligible.is_empty() {
            continue;
        }
        let bank_open = expanded.contains(&Expanded::Bank(bank_index));
        result.push(BankRow::Bank(bank_index, bank_open, eligible.len()));
        if !bank_open {
            continue;
        }
        let mut directories = BTreeSet::new();
        let mut children: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
        for index in eligible {
            let script = &scripts[index];
            let direct_parent = script.relative.parent().unwrap_or_else(|| Path::new(""));
            children
                .entry(direct_parent.to_path_buf())
                .or_default()
                .push(index);
            let mut parent = script.relative.parent();
            while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
                directories.insert(path.to_path_buf());
                parent = path.parent();
            }
        }
        let visible = |path: &Path| {
            path.ancestors()
                .skip(1)
                .filter(|ancestor| !ancestor.as_os_str().is_empty())
                .all(|ancestor| {
                    expanded.contains(&Expanded::Directory(bank_index, ancestor.to_path_buf()))
                })
        };
        for directory in directories {
            let key = Expanded::Directory(bank_index, directory.clone());
            if visible(&directory) {
                result.push(BankRow::Directory(
                    bank_index,
                    directory.clone(),
                    directory.components().count() + 1,
                    expanded.contains(&key),
                ));
            }
            if expanded.contains(&key) && visible(&directory) {
                for &index in children.get(&directory).into_iter().flatten() {
                    result.push(BankRow::File(index, directory.components().count() + 1));
                }
            }
        }
        for &index in children.get(Path::new("")).into_iter().flatten() {
            result.push(BankRow::File(index, 1));
        }
    }
    result
}

pub fn run(config: &Config) -> Result<Option<Job>> {
    let (mut banks, mut scripts, mut warnings) = scan_all(config)?;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        for script in scripts {
            println!("{}/{}", script.bank, script.relative.display());
        }
        return Ok(None);
    }
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = terminal::disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
        }
    }
    let _guard = Guard;
    let mut expanded = HashSet::new();
    let mut focus = 0_usize;
    let mut query = String::new();
    let mut searching = false;
    let mut selected_cluster = 0_usize;
    let mut previous_frame = Vec::new();
    let mut current = Vec::new();
    let mut visible_scripts = 0_usize;
    let mut view_dirty = true;
    loop {
        if view_dirty {
            let cluster = &config.clusters[selected_cluster].name;
            current = rows(&banks, &scripts, &expanded, &query, cluster);
            visible_scripts = scripts
                .iter()
                .filter(|script| supports_cluster(script, cluster))
                .count();
            view_dirty = false;
        }
        focus = focus.min(current.len().saturating_sub(1));
        draw_bank(
            &banks,
            &scripts,
            &current,
            focus,
            &query,
            &warnings,
            config,
            selected_cluster,
            visible_scripts,
            &mut previous_frame,
        )?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if searching {
            match key.code {
                KeyCode::Esc => {
                    searching = false;
                    query.clear();
                    view_dirty = true;
                }
                KeyCode::Enter => searching = false,
                KeyCode::Backspace => {
                    query.pop();
                    view_dirty = true;
                }
                KeyCode::Char(character) if !character.is_control() => {
                    query.push(character);
                    view_dirty = true;
                }
                _ => {}
            }
            continue;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | 's') if query.is_empty() => return Ok(None),
            KeyCode::Esc => {
                query.clear();
                view_dirty = true;
            }
            KeyCode::Up | KeyCode::Char('k') => focus = focus.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                focus = (focus + 1).min(current.len().saturating_sub(1))
            }
            KeyCode::Tab => {
                selected_cluster = (selected_cluster + 1) % config.clusters.len();
                view_dirty = true;
                previous_frame.clear();
            }
            KeyCode::BackTab => {
                selected_cluster = selected_cluster
                    .checked_sub(1)
                    .unwrap_or(config.clusters.len() - 1);
                view_dirty = true;
                previous_frame.clear();
            }
            KeyCode::Char('r') => {
                (banks, scripts, warnings) = scan_all_fresh(config)?;
                expanded.extend((0..banks.len()).map(Expanded::Bank));
                view_dirty = true;
            }
            KeyCode::Char('/') => {
                query.clear();
                searching = true;
                view_dirty = true;
            }
            KeyCode::Enter | KeyCode::Right if !current.is_empty() => match &current[focus] {
                BankRow::Bank(index, _, _) => {
                    let key = Expanded::Bank(*index);
                    if !expanded.remove(&key) {
                        expanded.insert(key);
                    }
                    view_dirty = true;
                }
                BankRow::Directory(bank, path, _, _) => {
                    let key = Expanded::Directory(*bank, path.clone());
                    if !expanded.remove(&key) {
                        expanded.insert(key);
                    }
                    view_dirty = true;
                }
                BankRow::File(index, _) => {
                    match submit_flow(config, &scripts[*index], selected_cluster) {
                        Ok(Some(job)) => return Ok(Some(job)),
                        Ok(None) => {}
                        Err(error) => warnings.push(format!("submission failed: {error:#}")),
                    }
                    previous_frame.clear();
                }
            },
            KeyCode::Left if !current.is_empty() => match &current[focus] {
                BankRow::Bank(index, _, _) => {
                    view_dirty = expanded.remove(&Expanded::Bank(*index));
                }
                BankRow::Directory(bank, path, _, _) => {
                    view_dirty = expanded.remove(&Expanded::Directory(*bank, path.clone()));
                }
                BankRow::File(_, _) => {}
            },
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_bank(
    banks: &[LoadedBank],
    scripts: &[Script],
    rows: &[BankRow],
    focus: usize,
    query: &str,
    warnings: &[String],
    config: &Config,
    selected_cluster: usize,
    visible_scripts: usize,
    previous: &mut Vec<String>,
) -> Result<()> {
    let (width, height) = terminal::size()?;
    let mut frame = vec![String::new(); height as usize];
    if height == 0 {
        return Ok(());
    }
    let tabs = cluster_tabs(config, selected_cluster);
    frame[0] = format!("SBATCH BANK  ·  SUBMIT TO  {tabs}");
    if height > 1 {
        frame[1] =
            "Tab / Shift-Tab target  ·  ↑↓ move  ·  Enter run  ·  ←→ folders  ·  / search  ·  Esc return"
                .into();
    }
    let top = focus.saturating_sub(height.saturating_sub(5) as usize);
    for (screen, (position, row)) in rows
        .iter()
        .enumerate()
        .skip(top)
        .take(height.saturating_sub(4) as usize)
        .enumerate()
    {
        let text = match row {
            BankRow::Bank(index, expanded, visible) => {
                format!(
                    "{} {}  ({} scripts here / {} total)",
                    if *expanded { "▾" } else { "▸" },
                    banks[*index].name,
                    visible,
                    banks[*index].last - banks[*index].first
                )
            }
            BankRow::Directory(_, path, _, expanded) => {
                format!(
                    "{} {}/",
                    if *expanded { "▾" } else { "▸" },
                    path.file_name().unwrap_or_default().to_string_lossy()
                )
            }
            BankRow::File(index, _) => {
                format!(
                    "{}{}  [{}]",
                    if query.is_empty() {
                        String::new()
                    } else {
                        format!("{}/", scripts[*index].bank)
                    },
                    scripts[*index]
                        .relative
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                    scripts[*index].name
                ) + &format!(
                    "  @{}",
                    scripts[*index].origin.as_deref().unwrap_or("shared")
                )
            }
        };
        frame[screen + 2] = format!(
            "{}{}{}",
            if position == focus { "> " } else { "  " },
            " ".repeat(row_indent(row)),
            text
        );
    }
    frame[height as usize - 1] = format!(
        "{} shown / {} scripts · submit target={} · search={query:?}{}",
        visible_scripts,
        scripts.len(),
        config.clusters[selected_cluster].name,
        if warnings.is_empty() {
            String::new()
        } else {
            format!(" · ⚠ {}", warnings.len())
        }
    );
    let mut out = Vec::new();
    for (line, content) in frame.iter().enumerate() {
        if previous.get(line) == Some(content) {
            continue;
        }
        let fitted: String = content.chars().take(width as usize).collect();
        execute!(
            out,
            cursor::MoveTo(0, line as u16),
            SetAttribute(Attribute::Reset),
            Print(fitted),
            terminal::Clear(ClearType::UntilNewLine)
        )?;
    }
    io::stdout().write_all(&out)?;
    io::stdout().flush()?;
    *previous = frame;
    Ok(())
}

fn submit_flow(config: &Config, script: &Script, selected_cluster: usize) -> Result<Option<Job>> {
    let cluster = &config.clusters[selected_cluster];
    execute!(
        io::stdout(),
        cursor::MoveTo(0, 0),
        terminal::Clear(ClearType::All),
        Print(submit_confirmation(script, cluster))
    )?;
    io::stdout().flush()?;
    let confirmed = loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if let Some(choice) = confirmation_choice(key.code) {
                    break choice;
                }
            }
            _ => {}
        }
    };
    if !confirmed {
        return Ok(None);
    }
    Ok(Some(submit(config, script, &cluster.name)?))
}

fn confirmation_choice(code: KeyCode) -> Option<bool> {
    match code {
        KeyCode::Char('y' | 'Y') => Some(true),
        KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => Some(false),
        _ => None,
    }
}

fn submit_confirmation(script: &Script, cluster: &crate::config::ClusterConfig) -> String {
    let arguments = script
        .directives
        .iter()
        .map(|directive| format!("    {directive}\r\n"))
        .collect::<String>();
    format!(
        "SUBMIT?\r\n  Script: {}/{}\r\n  Name: {}\r\n  Cluster: {}\r\n  Directory: {}\r\n  Arguments:\r\n{}\r\nPress y to submit and open its pane · n/Esc cancels",
        script.bank,
        script.relative.display(),
        script.name,
        cluster.name,
        cluster.working_directory.display(),
        arguments
    )
}

#[cfg(test)]
mod tests {
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
        assert_eq!(cached.scripts.len(), 20_000);
        assert!(started.elapsed() < Duration::from_millis(150));
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
        let started = std::time::Instant::now();
        assert_eq!(rows(&banks, &scripts, &expanded, "", "local").len(), 20_101);
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
    }
}
