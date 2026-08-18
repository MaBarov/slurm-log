//! Provenance ledger for jobs submitted outside MCP.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;

use crate::config::Config;

const ADOPTION_MAX_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Serialize)]
pub(super) struct AdoptionEntry {
    pub(super) adopted_at: String,
    pub(super) cluster: String,
    pub(super) job_id: String,
    pub(super) job_name: String,
    pub(super) observed_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) batch_script_sha256: Option<String>,
    pub(super) externally_submitted: bool,
    pub(super) source: String,
}

fn adoption_path(config: &Config) -> PathBuf {
    config
        .state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("mcp-adopted-jobs.jsonl")
}

fn reject_adoption_symlink(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!("refusing symlinked adoption ledger path {}", path.display());
    }
    Ok(())
}

pub(super) fn append_adoption(config: &Config, entry: &AdoptionEntry) -> Result<()> {
    let path = adoption_path(config);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let lock_path = path.with_extension("jsonl.lock");
    reject_adoption_symlink(&lock_path)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .context("open adoption ledger lock")?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)
        .context("lock adoption ledger")?;
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))?;
    reject_adoption_symlink(&path)?;
    if fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= ADOPTION_MAX_BYTES) {
        let backup = path.with_extension("jsonl.1");
        reject_adoption_symlink(&backup)?;
        let _ = fs::remove_file(&backup);
        let _ = fs::rename(&path, &backup);
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("open adoption ledger {}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer(&mut file, entry)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

/// Latest adoption record for an exact cluster-qualified job, if any.
pub(crate) fn adoption_entry(config: &Config, cluster: &str, id: &str) -> Option<Value> {
    let path = adoption_path(config);
    let metadata = fs::metadata(&path).ok()?;
    if metadata.len() > ADOPTION_MAX_BYTES {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .rfind(|entry| {
            entry["cluster"].as_str() == Some(cluster) && entry["job_id"].as_str() == Some(id)
        })
}

pub(super) fn adoption_sha(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("batch_script_sha256 must be a 64-character hex digest");
    }
    Ok(())
}

#[cfg(test)]
#[path = "adoption/tests.rs"]
mod tests;
