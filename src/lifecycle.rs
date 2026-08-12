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

use crate::{config, config::Config, daemon};

const MAX_RELEASE_BYTES: u64 = 128 * 1024 * 1024;
const RELEASE_ROOT: &str = "https://github.com/MaBarov/slurm-log/releases";

struct PrivateTempDir(PathBuf);

impl Drop for PrivateTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn update(config: &Config, explicit_binary: Option<&str>) -> Result<()> {
    let downloaded = if explicit_binary.is_none() {
        Some(download_release()?)
    } else {
        None
    };
    let candidate = match (explicit_binary, downloaded.as_ref()) {
        (Some(path), _) => PathBuf::from(path),
        (None, Some(directory)) => directory.0.join("payload/slurm-log/bin/slurm-log"),
        (None, None) => unreachable!(),
    };
    validate_candidate(&candidate)?;

    let target = canonical_regular_file(&config.executable, "installed executable")?;
    let candidate = canonical_regular_file(&candidate, "release binary")?;
    let current_version = parse_version(env!("CARGO_PKG_VERSION"))?;
    let candidate_version = binary_version(&candidate)?;
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

fn download_release() -> Result<PrivateTempDir> {
    let architecture = match env::consts::ARCH {
        "x86_64" => "x86_64",
        other => bail!("no prebuilt slurm-log release is available for {other}"),
    };
    let root = env::var("SLURM_LOG_RELEASE_ROOT").unwrap_or_else(|_| RELEASE_ROOT.into());
    validate_release_root(&root)?;
    let asset = format!("slurm-log-linux-{architecture}.tar.gz");
    let base = format!("{}/latest/download", root.trim_end_matches('/'));
    let temporary = private_temp_dir()?;
    let archive = temporary.0.join(&asset);
    let checksum = temporary.0.join(format!("{asset}.sha256"));

    println!("Downloading the latest slurm-log release for Linux {architecture}...");
    download_file(&format!("{base}/{asset}"), &archive)?;
    download_file(&format!("{base}/{asset}.sha256"), &checksum)?;
    verify_checksum(&archive, &checksum)?;

    let payload = temporary.0.join("payload");
    fs::create_dir(&payload).context("create release extraction directory")?;
    let status = Command::new("tar")
        .args(["-xzf"])
        .arg(&archive)
        .args(["-C"])
        .arg(&payload)
        .arg("slurm-log/bin/slurm-log")
        .stdin(Stdio::null())
        .status()
        .context("run tar to extract release")?;
    if !status.success() {
        bail!("release archive does not contain the expected binary");
    }
    validate_candidate(&payload.join("slurm-log/bin/slurm-log"))?;
    Ok(temporary)
}

fn validate_release_root(root: &str) -> Result<()> {
    if root.len() > 2048
        || root.chars().any(char::is_control)
        || !(root.starts_with("https://") || root.starts_with("file://"))
    {
        bail!("unsafe release URL");
    }
    Ok(())
}

fn private_temp_dir() -> Result<PrivateTempDir> {
    let base = env::temp_dir();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..8 {
        let path = base.join(format!(
            "slurm-log-update-{}-{nonce}-{attempt}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
                return Ok(PrivateTempDir(path));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("create private update directory"),
        }
    }
    bail!("could not allocate a private update directory")
}

fn download_file(url: &str, destination: &Path) -> Result<()> {
    let curl = Command::new("curl")
        .args(["-fsSL", "--retry", "2", "--connect-timeout", "10", "-o"])
        .arg(destination)
        .arg(url)
        .stdin(Stdio::null())
        .status();
    match curl {
        Ok(status) if status.success() => return Ok(()),
        Ok(_) => bail!("failed to download {url}"),
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(error).context("run curl");
        }
        Err(_) => {}
    }

    let status = Command::new("wget")
        .args(["-q", "--timeout=10", "-O"])
        .arg(destination)
        .arg(url)
        .stdin(Stdio::null())
        .status()
        .context("install curl or wget to download a release")?;
    if !status.success() {
        bail!("failed to download {url}");
    }
    Ok(())
}

fn verify_checksum(archive: &Path, checksum_file: &Path) -> Result<()> {
    let metadata = fs::metadata(checksum_file).context("read release checksum metadata")?;
    if metadata.len() > 4096 {
        bail!("release checksum file is unexpectedly large");
    }
    let text = fs::read_to_string(checksum_file).context("read release checksum")?;
    let expected = parse_checksum(&text)?;
    let output = Command::new("sha256sum")
        .arg(archive)
        .stdin(Stdio::null())
        .output()
        .context("sha256sum is required to verify updates")?;
    if !output.status.success() || output.stdout.len() > 4096 {
        bail!("could not calculate the release checksum");
    }
    let actual_text = String::from_utf8(output.stdout).context("invalid sha256sum output")?;
    let actual = parse_checksum(&actual_text)?;
    if expected != actual {
        bail!("release checksum verification failed; nothing was installed");
    }
    Ok(())
}

fn parse_checksum(text: &str) -> Result<String> {
    let value = text
        .split_ascii_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid release checksum");
    }
    Ok(value)
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
    let temporary = parent.join(format!(".slurm-log.update.{}", std::process::id()));
    let result = (|| -> Result<()> {
        let input = fs::File::open(candidate).context("open release binary")?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .open(&temporary)
            .context("create atomic update file")?;
        let copied = std::io::copy(&mut input.take(MAX_RELEASE_BYTES + 1), &mut output)?;
        if copied == 0 || copied > MAX_RELEASE_BYTES {
            bail!("release binary has an unsafe size");
        }
        output.flush()?;
        output.sync_all()?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))?;
        fs::rename(&temporary, target).context("atomically install release binary")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
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
