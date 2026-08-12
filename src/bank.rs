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

include!("bank/catalog.rs");
include!("bank/ui.rs");

#[cfg(test)]
mod tests;
