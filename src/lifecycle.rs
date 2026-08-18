use std::{
    env, fs,
    fs::OpenOptions,
    io::{BufReader, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};

use crate::{
    config,
    config::Config,
    daemon,
    release_download::{DownloadedRelease, download_release},
};

const MAX_RELEASE_BYTES: u64 = 128 * 1024 * 1024;

pub fn update(config: &Config, explicit_binary: Option<&str>) -> Result<()> {
    let downloaded: Option<DownloadedRelease> = if explicit_binary.is_none() {
        Some(download_release()?)
    } else {
        None
    };
    let candidate = match (explicit_binary, downloaded.as_ref()) {
        (Some(path), _) => PathBuf::from(path),
        (None, Some(release)) => release.candidate_path(),
        (None, None) => unreachable!(),
    };
    validate_candidate(&candidate)?;

    let candidate_version = binary_version(&candidate)?;
    if let Some(release) = &downloaded
        && candidate_version != parse_version(&release.manifest.version)?
    {
        bail!("signed release manifest version does not match its binary");
    }

    let target = canonical_regular_file(&config.executable, "installed executable")?;
    let candidate = canonical_regular_file(&candidate, "release binary")?;
    let current_version = parse_version(env!("CARGO_PKG_VERSION"))?;
    if candidate_version < current_version {
        bail!(
            "refusing to replace slurm-log {} with older release {}",
            env!("CARGO_PKG_VERSION"),
            format_version(candidate_version)
        );
    }
    if same_contents(&candidate, &target)? {
        println!("slurm-log is already up to date: {}", target.display());
        return Ok(());
    }

    let was_running = daemon::stop_for_lifecycle(config)?;
    if let Err(error) = atomic_replace(&candidate, &target) {
        if was_running {
            let _ = daemon::start_for_lifecycle(config);
        }
        return Err(error);
    }
    if was_running {
        daemon::start_for_lifecycle(config).context("restart daemon after update")?;
    }

    println!("Updated: {}", target.display());
    println!("Configuration and job history were preserved.");
    Ok(())
}

pub fn uninstall(config: &Config, purge: bool) -> Result<()> {
    let target = canonical_regular_file(&config.executable, "installed executable")?;
    let was_running = daemon::stop_for_lifecycle(config)?;
    if let Err(error) = fs::remove_file(&target) {
        if was_running {
            let _ = daemon::start_for_lifecycle(config);
        }
        return Err(error).with_context(|| format!("remove {}", target.display()));
    }

    println!("Removed {}", target.display());
    if purge {
        purge_user_data(config)?;
        println!("Removed configuration and state.");
    } else {
        println!("Configuration and state were preserved. Use --purge to remove them.");
    }
    Ok(())
}

fn validate_candidate(path: &Path) -> Result<()> {
    let path = canonical_regular_file(path, "release binary")?;
    let metadata = fs::metadata(&path).context("read release binary metadata")?;
    if metadata.len() == 0 || metadata.len() > MAX_RELEASE_BYTES {
        bail!("release binary has an unsafe size");
    }
    let status = Command::new(&path)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("start release binary {}", path.display()))?;
    if !status.success() {
        bail!("release binary failed its startup check");
    }
    Ok(())
}

fn binary_version(path: &Path) -> Result<(u64, u64, u64)> {
    let output = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("read release version from {}", path.display()))?;
    if !output.status.success() || output.stdout.len() > 256 {
        bail!("release binary did not report a valid version");
    }
    let text = String::from_utf8(output.stdout).context("release version is not UTF-8")?;
    let version = text
        .strip_prefix("slurm-log ")
        .ok_or_else(|| anyhow::anyhow!("release binary reported an unexpected identity"))?;
    parse_version(version.trim())
}

fn parse_version(value: &str) -> Result<(u64, u64, u64)> {
    let mut parts = value.split('.');
    let parsed = (
        parts.next().unwrap_or_default().parse()?,
        parts.next().unwrap_or_default().parse()?,
        parts.next().unwrap_or_default().parse()?,
    );
    if parts.next().is_some() {
        bail!("invalid slurm-log release version");
    }
    Ok(parsed)
}

fn format_version(version: (u64, u64, u64)) -> String {
    format!("{}.{}.{}", version.0, version.1, version.2)
}

fn canonical_regular_file(path: &Path, label: &str) -> Result<PathBuf> {
    let original = fs::symlink_metadata(path).with_context(|| format!("inspect {label}"))?;
    if original.file_type().is_symlink() || !original.file_type().is_file() {
        bail!("{label} is not a regular file");
    }
    let canonical = fs::canonicalize(path).with_context(|| format!("locate {label}"))?;
    let metadata = fs::symlink_metadata(&canonical).with_context(|| format!("inspect {label}"))?;
    if !metadata.file_type().is_file() {
        bail!("{label} is not a regular file");
    }
    Ok(canonical)
}

fn same_contents(left: &Path, right: &Path) -> Result<bool> {
    if fs::metadata(left)?.len() != fs::metadata(right)?.len() {
        return Ok(false);
    }
    let mut left = BufReader::new(fs::File::open(left)?);
    let mut right = BufReader::new(fs::File::open(right)?);
    let mut left_buffer = [0_u8; 64 * 1024];
    let mut right_buffer = [0_u8; 64 * 1024];
    loop {
        let left_read = left.read(&mut left_buffer)?;
        let right_read = right.read(&mut right_buffer)?;
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

fn atomic_replace(candidate: &Path, target: &Path) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("installed executable has no parent directory"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(".slurm-log.update.{}.{nonce}", std::process::id()));
    atomic_replace_at(candidate, target, &temporary)
}

fn atomic_replace_at(candidate: &Path, target: &Path, temporary: &Path) -> Result<()> {
    let mut created = false;
    let result = (|| -> Result<()> {
        let input = fs::File::open(candidate).context("open release binary")?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .open(temporary)
            .context("create atomic update file")?;
        created = true;
        let copied = std::io::copy(&mut input.take(MAX_RELEASE_BYTES + 1), &mut output)?;
        if copied == 0 || copied > MAX_RELEASE_BYTES {
            bail!("release binary has an unsafe size");
        }
        output.flush()?;
        output.sync_all()?;
        fs::set_permissions(temporary, fs::Permissions::from_mode(0o755))?;
        fs::rename(temporary, target).context("atomically install release binary")?;
        Ok(())
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn purge_user_data(config: &Config) -> Result<()> {
    remove_application_path(&config::config_path(), false)?;
    let state_directory = config.state_path.parent().unwrap_or_else(|| Path::new("."));
    if state_directory
        .file_name()
        .is_some_and(|name| name == "slurm-log")
    {
        remove_application_path(state_directory, true)?;
    } else {
        for path in [
            config.state_path.clone(),
            config.state_path.with_extension("lock"),
            config.state_path.with_extension("archive-cache.json"),
            state_directory.join("daemon.sock"),
            state_directory.join("daemon.lock"),
            state_directory.join("mcp-audit.jsonl"),
            state_directory.join("mcp-audit.jsonl.1"),
            state_directory.join("mcp-audit.jsonl.lock"),
        ] {
            remove_application_path(&path, false)?;
        }
    }
    Ok(())
}

fn remove_application_path(path: &Path, recursive: bool) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if recursive && metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))
    } else if metadata.is_dir() {
        fs::remove_dir(path).with_context(|| format!("remove {}", path.display()))
    } else {
        fs::remove_file(path).with_context(|| format!("remove {}", path.display()))
    }
}

#[cfg(test)]
mod tests;
