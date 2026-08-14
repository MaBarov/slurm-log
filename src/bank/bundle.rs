// Content-addressed bundle staging.
//
// A stage request packs repo-relative manifest entries into a deterministic,
// content-addressed bundle under a strict size budget, refuses prohibited
// paths and secret-like content, and writes the result to an immutable
// destination (a second stage of the same content fails rather than
// overwrites). Staging never executes anything: actually running a script
// that references the staged destination still requires the normal
// preview-then-submit flow, which is the separate execution approval.

pub const MAX_BUNDLE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ENTRY_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_MANIFEST_ENTRIES: usize = 512;

pub struct Bundle {
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub entries: Vec<(String, u64)>,
}

/// The source root for a staged bundle: the configured bank selected by name
/// (explicit or inferred) or the single configured bank. The manifest paths
/// are repo-relative to this root.
pub fn bundle_root(config: &Config, name: Option<&str>) -> Result<PathBuf> {
    if config.sbatch_banks.is_empty() {
        bail!("no sbatch banks are configured");
    }
    let bank = match name {
        Some(wanted) => {
            let matches: Vec<_> = config
                .sbatch_banks
                .iter()
                .filter(|bank| {
                    bank.name.as_deref() == Some(wanted)
                        || inferred_name(bank).ok().as_deref() == Some(wanted)
                })
                .collect();
            if matches.len() != 1 {
                bail!("bank {wanted:?} is ambiguous or unknown");
            }
            matches[0]
        }
        None => {
            if config.sbatch_banks.len() != 1 {
                bail!("select a bank explicitly with the bank argument");
            }
            &config.sbatch_banks[0]
        }
    };
    Ok(bank.path.clone())
}

/// Reject manifest paths that are not clean repo-relative file paths.
pub fn validate_manifest_path(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > 1024 {
        bail!("bundle entries must be at most 1024 bytes");
    }
    if path.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
        bail!("bundle entry {path:?} contains control characters");
    }
    let mut parts = 0_usize;
    for component in Path::new(path).components() {
        match component {
            std::path::Component::Normal(_) => parts += 1,
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {
                bail!("bundle entry {path:?} must be a repo-relative path")
            }
        }
    }
    if parts == 0 {
        bail!("bundle entry {path:?} must name a file");
    }
    Ok(())
}

/// Fail-closed path policy: refuse components that conventionally hold
/// credentials or secrets. Content scanning below catches actual key
/// material; this list refuses well-known secret paths outright.
pub fn suspicious_path(path: &str) -> bool {
    path.split('/').any(|component| {
        let lower = component.to_lowercase();
        matches!(
            lower.as_str(),
            ".git"
                | ".hg"
                | ".svn"
                | ".ssh"
                | ".gnupg"
                | ".aws"
                | ".env"
                | "id_rsa"
                | "id_dsa"
                | "id_ecdsa"
                | "id_ed25519"
                | "known_hosts"
                | "credentials"
                | "credentials.json"
                | "credentials.yml"
                | "credentials.yaml"
                | "password"
        ) || lower.contains("secret")
            || lower.contains("private_key")
            || lower.ends_with(".pem")
            || lower.ends_with(".key")
            || lower.ends_with(".ppk")
            || lower.ends_with(".p12")
            || lower.ends_with(".pfx")
    })
}

/// Refuse content that embeds private key material. Marker-based: any
/// private-key PEM header (RSA, EC, DSA, OpenSSH, PGP, generic encrypted)
/// fails the scan regardless of surrounding bytes.
pub fn contains_secret_markers(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    [
        "PRIVATE KEY-----",
        "BEGIN OPENSSH PRIVATE KEY",
        "BEGIN RSA PRIVATE KEY",
        "BEGIN EC PRIVATE KEY",
        "BEGIN DSA PRIVATE KEY",
        "BEGIN PGP PRIVATE KEY BLOCK",
        "BEGIN ENCRYPTED PRIVATE KEY",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

/// Build the deterministic content-addressed bundle. Entries are sorted,
/// deduplicated, read through the descriptor-confined root, and bounded both
/// per entry and in total. The returned SHA-256 covers the entire bundle.
pub fn build_bundle(root: &Path, manifest: &[String]) -> Result<Bundle> {
    let base = crate::secure_open::SecureDir::open_root(root)
        .context("securely open bundle source root")?;
    let mut sorted = manifest.to_vec();
    sorted.sort();
    sorted.dedup();
    if sorted.is_empty() || sorted.len() > MAX_MANIFEST_ENTRIES {
        bail!("manifest must contain 1..{MAX_MANIFEST_ENTRIES} entries");
    }
    let mut bundle = Vec::new();
    bundle.extend_from_slice(b"slurm-log-bundle-v1\n");
    bundle.extend_from_slice(format!("{}\n", sorted.len()).as_bytes());
    let mut entries = Vec::with_capacity(sorted.len());
    let mut total: u64 = 0;
    for path in &sorted {
        let relative = Path::new(path);
        let file = base
            .open_file(relative)
            .with_context(|| format!("open bundle entry {path}"))?;
        let metadata = file.metadata()?;
        if metadata.len() > MAX_ENTRY_BYTES {
            bail!("bundle entry {path} exceeds {} MiB", MAX_ENTRY_BYTES / 1024 / 1024);
        }
        total = total.saturating_add(metadata.len());
        if total > MAX_BUNDLE_BYTES {
            bail!("bundle exceeds {} MiB in total", MAX_BUNDLE_BYTES / 1024 / 1024);
        }
        if suspicious_path(path) {
            bail!("refusing bundle entry {path:?}: prohibited path component");
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_ENTRY_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_ENTRY_BYTES {
            bail!("bundle entry {path} exceeds {} MiB", MAX_ENTRY_BYTES / 1024 / 1024);
        }
        if contains_secret_markers(&bytes) {
            bail!("refusing bundle entry {path:?}: private key material detected");
        }
        bundle.extend_from_slice(path.as_bytes());
        bundle.push(b'\n');
        bundle.extend_from_slice(format!("{}\n", bytes.len()).as_bytes());
        bundle.extend_from_slice(&bytes);
        entries.push((path.clone(), bytes.len() as u64));
    }
    let sha256 = {
        use sha2::Digest as _;
        sha2::Sha256::digest(&bundle)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    };
    Ok(Bundle {
        bytes: bundle,
        sha256,
        entries,
    })
}

/// Local staging directory: `<state directory>/bundles`.
pub fn local_bundle_dir(config: &Config) -> PathBuf {
    config
        .state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("bundles")
}

/// Remote staging directory and bundle filename (content-addressed).
pub fn remote_bundle_file(sha256: &str) -> String {
    format!("~/.cache/slurm-log/bundles/{sha256}.bundle")
}
