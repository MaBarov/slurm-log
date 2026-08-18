use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, OpenOptions},
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, BufReader, BufWriter, IsTerminal, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
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
    command::{remote_scheduler_command, ssh_with_input, text, text_with_input},
    config::{ClusterConfig, Config, SbatchBankConfig},
    model::{Job, valid_job_id},
};

mod scan_limits;

const MAX_SCRIPTS: usize = 20_000;
const MAX_DEPTH: usize = 3;
const MAX_SCRIPT_BYTES: u64 = 4 * 1024 * 1024;
const BANK_SCAN_TIME_LIMIT: Duration = Duration::from_secs(3);
const BANK_CACHE_TTL: Duration = Duration::from_secs(30);
const MAX_BANK_CACHE_BYTES: u64 = 64 * 1024 * 1024;
const BANK_CACHE_SCHEMA: u8 = 3;

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
    pub(crate) bytes: Vec<u8>,
    /// Unix epoch seconds when this bank's catalog entry was indexed.
    #[serde(default)]
    pub indexed_at: i64,
    /// Repository HEAD commit of the bank root, if it lives in a git worktree.
    #[serde(default)]
    pub repo_commit: Option<String>,
    /// Content fingerprint of the bank tree at index time.
    #[serde(default)]
    pub bank_fingerprint: u64,
}

#[cfg(test)]
fn scan(root: &Path) -> Result<(Vec<Script>, Vec<String>)> {
    scan_direct(root)
}

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
            if !opened.is_file() || opened.nlink() != 1 {
                warnings.push("ignored script changed while scanning".into());
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
                bytes,
                indexed_at: 0,
                repo_commit: None,
                bank_fingerprint: 0,
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
    if !value.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return false;
    }
    value
        .chars()
        .any(|character| character.is_control() || character == '\x1b')
}

#[derive(Deserialize, Serialize)]
struct ScanPayload {
    name: String,
    scripts: Vec<Script>,
    warnings: Vec<String>,
    error: Option<String>,
    #[serde(default)]
    indexed_at: i64,
    #[serde(default)]
    repo_commit: Option<String>,
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
            indexed_at: 0,
            repo_commit: None,
        },
        Err(error) => ScanPayload {
            name: fallback_name(root),
            scripts: Vec::new(),
            warnings: Vec::new(),
            error: Some(format!("{error:#}")),
            indexed_at: 0,
            repo_commit: None,
        },
    };
    let file = OpenOptions::new().write(true).truncate(true).open(output)?;
    let mut writer = BufWriter::with_capacity(256 * 1024, file);
    rmp_serde::encode::write(&mut writer, &payload)?;
    writer.flush()?;
    Ok(())
}

include!("bank/provenance.rs");
include!("bank/cache.rs");
include!("bank/catalog.rs");
include!("bank/index.rs");
include!("bank/ui.rs");

#[cfg(test)]
#[path = "bank/tests/edge.rs"]
mod edge_tests;
#[cfg(test)]
mod tests;
