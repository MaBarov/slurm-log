use std::{
    env, fs,
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::release_auth::{
    MAX_ARCHIVE_BYTES, MAX_MANIFEST_BYTES, MAX_SIGNATURE_BYTES, ReleaseManifest,
    compiled_public_key, verify_manifest,
};

const RELEASE_ROOT: &str = "https://github.com/MaBarov/slurm-log/releases";
const MAX_CHECKSUM_BYTES: u64 = 4096;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
const EXTRACTION_TIMEOUT: Duration = Duration::from_secs(30);

pub struct DownloadedRelease {
    temporary: PrivateTempDir,
    pub manifest: ReleaseManifest,
}

impl DownloadedRelease {
    pub fn candidate_path(&self) -> PathBuf {
        self.temporary.0.join("payload/slurm-log/bin/slurm-log")
    }
}

struct PrivateTempDir(PathBuf);

impl Drop for PrivateTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn download_release() -> Result<DownloadedRelease> {
    let (architecture, target) = match env::consts::ARCH {
        "x86_64" => ("x86_64", "x86_64-unknown-linux-musl"),
        other => bail!("no prebuilt slurm-log release is available for {other}"),
    };
    // Fetching is intentionally impossible until a trusted, compiled-in key is
    // available. A mirror cannot select its own verification key.
    let public_key = compiled_public_key()?;
    let root = env::var("SLURM_LOG_RELEASE_ROOT").unwrap_or_else(|_| RELEASE_ROOT.into());
    validate_release_root(&root)?;
    let asset = format!("slurm-log-linux-{architecture}.tar.gz");
    let base = format!("{}/latest/download", root.trim_end_matches('/'));
    let temporary = private_temp_dir()?;
    let archive = temporary.0.join(&asset);
    let checksum = temporary.0.join(format!("{asset}.sha256"));
    let manifest_path = temporary.0.join(format!("{asset}.manifest"));
    let signature_path = temporary.0.join(format!("{asset}.manifest.sig"));

    println!("Downloading the latest signed slurm-log release for Linux {architecture}...");
    download_file(
        &format!("{base}/{asset}.manifest"),
        &manifest_path,
        MAX_MANIFEST_BYTES as u64,
    )?;
    download_file(
        &format!("{base}/{asset}.manifest.sig"),
        &signature_path,
        MAX_SIGNATURE_BYTES as u64,
    )?;
    let manifest_bytes = read_limited(&manifest_path, MAX_MANIFEST_BYTES as u64)?;
    let signature = read_limited(&signature_path, MAX_SIGNATURE_BYTES as u64)?;
    let manifest = verify_manifest(&manifest_bytes, &signature, &public_key)?;
    if manifest.archive != asset || manifest.target != target {
        bail!("signed release manifest does not describe this platform asset");
    }

    download_file(&format!("{base}/{asset}"), &archive, manifest.size)?;
    download_file(
        &format!("{base}/{asset}.sha256"),
        &checksum,
        MAX_CHECKSUM_BYTES,
    )?;
    verify_archive(&archive, &checksum, &manifest)?;

    let payload = temporary.0.join("payload");
    fs::create_dir(&payload).context("create release extraction directory")?;
    extract_candidate(&archive, &payload)?;
    Ok(DownloadedRelease {
        temporary,
        manifest,
    })
}

pub(crate) fn validate_release_root(root: &str) -> Result<()> {
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

fn download_file(url: &str, destination: &Path, maximum: u64) -> Result<()> {
    if maximum == 0 || maximum > MAX_ARCHIVE_BYTES {
        bail!("invalid release download limit");
    }
    if let Some(path) = url.strip_prefix("file://") {
        return copy_file_limited(Path::new(path), destination, maximum);
    }
    let timeout = DOWNLOAD_TIMEOUT.as_secs().to_string();
    let maximum_text = maximum.to_string();
    let mut child = Command::new("curl")
        .args([
            "-fsSL",
            "--retry",
            "2",
            "--connect-timeout",
            "10",
            "--max-time",
            &timeout,
            "--max-filesize",
            &maximum_text,
            "--proto",
            "=https",
            "--output",
            "-",
        ])
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("curl with bounded release-download controls is required")?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("curl release downloader did not expose stdout"))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)
        .with_context(|| format!("create {}", destination.display()))?;
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = stdout
            .read(&mut buffer)
            .context("read bounded curl output")?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > maximum {
            let _ = child.kill();
            let _ = child.wait();
            bail!("release download exceeds its safety limit");
        }
        output.write_all(&buffer[..read])?;
    }
    output.flush()?;
    let status = child.wait().context("wait for release downloader")?;
    if !status.success() {
        bail!("failed to download {url}");
    }
    if total == 0 {
        bail!("release download was empty");
    }
    read_limited(destination, maximum).map(|_| ())
}

fn copy_file_limited(source: &Path, destination: &Path, maximum: u64) -> Result<()> {
    let (input, metadata) = open_regular_no_follow(source)?;
    if metadata.len() == 0 || metadata.len() > maximum {
        bail!("local release asset exceeds its safety limit");
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)
        .with_context(|| format!("create {}", destination.display()))?;
    let copied = std::io::copy(&mut input.take(maximum + 1), &mut output)?;
    output.flush()?;
    if copied == 0 || copied > maximum {
        bail!("local release asset exceeds its safety limit");
    }
    Ok(())
}

fn read_limited(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let (file, metadata) = open_regular_no_follow(path)?;
    if metadata.len() == 0 || metadata.len() > maximum {
        bail!("release asset exceeds its safety limit");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;
    if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > maximum {
        bail!("release asset changed while being read");
    }
    Ok(bytes)
}

fn verify_archive(archive: &Path, checksum_file: &Path, manifest: &ReleaseManifest) -> Result<()> {
    let checksum = read_limited(checksum_file, MAX_CHECKSUM_BYTES)?;
    let expected =
        parse_checksum(std::str::from_utf8(&checksum).context("invalid checksum UTF-8")?)?;
    if expected != manifest.sha256 {
        bail!("release checksum sidecar does not match the signed manifest");
    }
    let actual = sha256_file(archive, manifest.size)?;
    if actual != manifest.sha256 {
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

fn sha256_file(path: &Path, expected_size: u64) -> Result<String> {
    let (mut file, metadata) = open_regular_no_follow(path)?;
    if metadata.len() != expected_size {
        bail!("release archive size does not match the signed manifest");
    }
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > expected_size {
            bail!("release archive exceeds its safety limit");
        }
        digest.update(&buffer[..read]);
    }
    if total != expected_size {
        bail!("release archive changed while being read");
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn open_regular_no_follow(path: &Path) -> Result<(fs::File, fs::Metadata)> {
    // Linux is the supported release target. O_NOFOLLOW prevents a hostile
    // downloader or concurrent local process from swapping a checked path for
    // a symlink between metadata inspection and open.
    const O_NOFOLLOW: i32 = 0o400_000;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("open regular release asset {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("inspect {}", path.display()))?;
    if !metadata.file_type().is_file() {
        bail!("release asset is not a regular file");
    }
    Ok((file, metadata))
}

fn extract_candidate(archive: &Path, payload: &Path) -> Result<()> {
    let mut command = Command::new("tar");
    command
        .args(["-xzf"])
        .arg(archive)
        .args(["--no-same-owner", "--no-same-permissions", "-C"])
        .arg(payload)
        .arg("slurm-log/bin/slurm-log")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    bounded_status(&mut command, "extract release archive", EXTRACTION_TIMEOUT)
}

fn bounded_status(command: &mut Command, label: &str, timeout: Duration) -> Result<()> {
    let mut child = command.spawn().with_context(|| format!("start {label}"))?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            bail!("{label} failed");
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{label} exceeded its safety deadline");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(bytes: &[u8]) -> ReleaseManifest {
        let digest = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        ReleaseManifest {
            version: "0.2.3".into(),
            target: "x86_64-unknown-linux-musl".into(),
            archive: "slurm-log-linux-x86_64.tar.gz".into(),
            sha256: digest,
            size: bytes.len() as u64,
        }
    }

    #[test]
    fn roots_and_local_downloads_are_bounded() {
        assert!(validate_release_root("https://example.invalid/releases").is_ok());
        assert!(validate_release_root("file:///tmp/releases").is_ok());
        assert!(validate_release_root("http://example.invalid").is_err());
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::write(&source, b"release").unwrap();
        copy_file_limited(&source, &destination, 7).unwrap();
        assert_eq!(fs::read(destination).unwrap(), b"release");
        assert!(copy_file_limited(&source, &directory.path().join("too-small"), 6).is_err());
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert!(copy_file_limited(&link, &directory.path().join("from-link"), 7).is_err());
    }

    #[test]
    fn checksum_must_match_signed_manifest_and_archive() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("archive");
        let checksum = directory.path().join("archive.sha256");
        let bytes = b"release bytes";
        fs::write(&archive, bytes).unwrap();
        let manifest = manifest(bytes);
        fs::write(&checksum, format!("{}  archive\n", manifest.sha256)).unwrap();
        verify_archive(&archive, &checksum, &manifest).unwrap();
        fs::write(&checksum, "0".repeat(64)).unwrap();
        assert!(verify_archive(&archive, &checksum, &manifest).is_err());
    }
}
