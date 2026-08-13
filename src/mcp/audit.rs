use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::Serialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::config::Config;

const MAX_AUDIT_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Serialize)]
struct AuditEntry {
    timestamp: String,
    client: String,
    tool: String,
    cluster: String,
    identifier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<String>,
    result: String,
}

pub fn record(
    config: &Config,
    client: &str,
    tool: &str,
    cluster: &str,
    identifier: &str,
    digest: Option<&str>,
    result: &str,
) -> Result<()> {
    let path = audit_path(config);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let lock_path = path.with_extension("jsonl.lock");
    reject_symlink(&lock_path)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .context("open MCP audit lock")?;
    lock.lock_exclusive().context("lock MCP audit log")?;
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))?;
    reject_symlink(&path)?;
    if fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= MAX_AUDIT_BYTES) {
        rotate(&path)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("open MCP audit log {}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let entry = AuditEntry {
        timestamp: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "unknown".into()),
        client: clean(client, 200),
        tool: clean(tool, 100),
        cluster: clean(cluster, 100),
        identifier: clean(identifier, 500),
        digest: digest.map(|value| clean(value, 128)),
        result: clean(result, 500),
    };
    serde_json::to_writer(&mut file, &entry)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

fn audit_path(config: &Config) -> PathBuf {
    config
        .state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("mcp-audit.jsonl")
}

fn rotate(path: &Path) -> Result<()> {
    let backup = path.with_extension("jsonl.1");
    reject_symlink(&backup)?;
    if backup.exists() {
        fs::remove_file(&backup).context("remove old MCP audit backup")?;
    }
    fs::rename(path, &backup).context("rotate MCP audit log")?;
    fs::set_permissions(backup, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn reject_symlink(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!("refusing symlinked MCP audit path {}", path.display());
    }
    Ok(())
}

fn clean(value: &str, limit: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || *character == '\t')
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ClusterConfig, Config};

    fn config(path: PathBuf) -> Config {
        Config {
            local_user: "offline".into(),
            remote_user: "offline".into(),
            ssh_host: String::new(),
            state_path: path,
            executable: PathBuf::from("/bin/false"),
            sbatch_banks: Vec::new(),
            clusters: vec![ClusterConfig {
                name: "alpha".into(),
                controller: None,
                transport: "local".into(),
                user: "offline".into(),
                ssh_host: String::new(),
                working_directory: PathBuf::from("/tmp"),
                accounting: false,
            }],
        }
    }

    #[test]
    fn audit_result_removes_newlines_and_is_bounded() {
        let value = clean(&format!("secret\n{}", "x".repeat(600)), 20);
        assert!(!value.contains('\n'));
        assert_eq!(value.chars().count(), 20);
    }

    #[test]
    fn audit_rotates_once_and_remains_private() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path().join("state.json"));
        let path = audit_path(&config);
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.set_len(MAX_AUDIT_BYTES).unwrap();
        record(
            &config,
            "client\nname",
            "slurm_submit_job",
            "alpha",
            "Bank/train.sbatch",
            Some(&"a".repeat(64)),
            "submitted",
        )
        .unwrap();
        assert!(path.with_extension("jsonl.1").exists());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let text = fs::read_to_string(path).unwrap();
        assert!(!text.contains("client\nname"));
    }

    #[test]
    fn audit_rejects_symlink_targets() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path().join("state.json"));
        let target = directory.path().join("unrelated");
        fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, audit_path(&config)).unwrap();
        assert!(record(&config, "c", "t", "alpha", "1", None, "x").is_err());
        assert_eq!(fs::read(target).unwrap(), b"keep");
    }
}
