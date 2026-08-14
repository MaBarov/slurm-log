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

#[cfg(test)]
mod bundle_tests {
    use super::*;

    #[test]
    fn manifest_paths_reject_escapes_controls_and_empties() {
        for valid in ["a.txt", "data/train.jsonl", "runs/42/out.txt", "./a.txt"] {
            assert!(validate_manifest_path(valid).is_ok(), "rejected {valid:?}");
        }
        for hostile in [
            "",
            "..",
            "../x",
            "/etc/passwd",
            "a/../../b",
            "a\nb",
            "a\tb",
            "a\0b",
            &"x".repeat(1025),
        ] {
            assert!(validate_manifest_path(hostile).is_err(), "accepted {hostile:?}");
        }
    }

    #[test]
    fn suspicious_paths_cover_known_secret_locations() {
        for hostile in [
            ".env",
            "config/.env",
            ".ssh/id_rsa",
            "certs/server.pem",
            "creds/credentials",
            "my_secret.txt",
            "secrets/keys/api.key",
            "aws/credentials.yml",
        ] {
            assert!(suspicious_path(hostile), "missed {hostile:?}");
        }
        for benign in [
            "data/train.jsonl",
            "src/model.py",
            "README.md",
            "outputs/checkpoint.pt",
            "id_rsa.pub",
        ] {
            assert!(!suspicious_path(benign), "flagged {benign:?}");
        }
    }

    #[test]
    fn secret_markers_catch_private_key_material() {
        assert!(contains_secret_markers(
            b"-----BEGIN RSA PRIVATE KEY-----\nMII..."
        ));
        assert!(contains_secret_markers(
            b"-----BEGIN OPENSSH PRIVATE KEY-----"
        ));
        assert!(!contains_secret_markers(b"pub: ssh-ed25519 AAA..."));
        assert!(!contains_secret_markers(b"{\"seed\": 42}"));
    }

    #[test]
    fn bundle_is_deterministic_content_addressed_and_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir_all(root.join("data")).unwrap();
        fs::write(root.join("data/b.json"), b"{\"x\":1}").unwrap();
        fs::write(root.join("a.txt"), b"hello\n").unwrap();

        let manifest = vec!["data/b.json".into(), "a.txt".into()];
        let first = build_bundle(&root, &manifest).unwrap();
        let second = build_bundle(&root, &manifest).unwrap();
        assert_eq!(first.bytes, second.bytes, "bundle must be deterministic");
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(first.sha256.len(), 64);
        assert_eq!(first.entries.len(), 2);
        assert_eq!(first.entries[0].0, "a.txt");

        fs::write(root.join("data/b.json"), b"changed").unwrap();
        assert_ne!(
            build_bundle(&root, &manifest).unwrap().sha256,
            first.sha256,
            "content changes must change the bundle address"
        );

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("big.bin"), vec![0u8; 9 * 1024 * 1024]).unwrap();
        assert!(
            build_bundle(&root, &["big.bin".into()]).is_err(),
            "oversized entry must be refused"
        );

        assert!(
            build_bundle(&root, &["nested/../big.bin".into()]).is_err(),
            "escaping entry must be refused"
        );
    }

    fn bundle_config(banks: Vec<SbatchBankConfig>, state: PathBuf) -> Config {
        Config {
            local_user: "offline".into(),
            remote_user: "offline".into(),
            ssh_host: String::new(),
            state_path: state,
            executable: PathBuf::from("slurm-log"),
            sbatch_banks: banks,
            clusters: vec![ClusterConfig {
                name: "local".into(),
                controller: None,
                transport: "local".into(),
                user: "offline".into(),
                ssh_host: String::new(),
                working_directory: PathBuf::from("/tmp"),
                accounting: false,
            }],
        }
    }

    fn bank(path: &Path, name: Option<&str>) -> SbatchBankConfig {
        SbatchBankConfig {
            path: path.to_path_buf(),
            name: name.map(str::to_string),
        }
    }

    #[test]
    fn bundle_root_resolves_explicit_inferred_or_single_bank() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("project");
        let loose = directory.path().join("loose");
        fs::create_dir_all(repository.join(".git")).unwrap();
        fs::create_dir(&loose).unwrap();
        let distinct = bundle_config(
            vec![
                bank(&repository, Some("Project")),
                bank(&loose, Some("Loose")),
            ],
            directory.path().join("distinct.json"),
        );
        assert_eq!(
            bundle_root(&distinct, Some("Project")).unwrap(),
            repository,
            "explicit names must resolve"
        );
        assert_eq!(
            bundle_root(&distinct, Some("Loose")).unwrap(),
            loose,
            "inferred names must resolve against named banks"
        );
        assert!(bundle_root(&distinct, None).is_err(), "two banks are ambiguous");

        let duplicated = bundle_config(
            vec![
                bank(&repository, Some("Project")),
                bank(&loose, Some("Project")),
            ],
            directory.path().join("duplicated.json"),
        );
        assert!(
            bundle_root(&duplicated, Some("Project")).is_err(),
            "duplicated bank names must be rejected"
        );
        assert!(
            bundle_root(&duplicated, Some("absent")).is_err(),
            "unknown banks must be rejected"
        );
        assert!(
            bundle_root(&bundle_config(Vec::new(), directory.path().join("s.json")), None).is_err(),
            "no configured banks must be rejected"
        );

        let single = bundle_config(
            vec![bank(&loose, None)],
            directory.path().join("single.json"),
        );
        assert_eq!(bundle_root(&single, None).unwrap(), loose);
        let inferred = inferred_name(&single.sbatch_banks[0]).unwrap();
        assert_eq!(
            bundle_root(&single, Some(&inferred)).unwrap(),
            loose,
            "inferred names must resolve"
        );
        assert!(
            bundle_root(&single, Some("project")).is_err(),
            "names of other banks must not resolve"
        );
    }

    #[test]
    fn bundle_manifest_rejects_empty_oversized_and_component_only_paths() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("a.txt"), b"a").unwrap();
        assert!(build_bundle(&root, &[]).is_err());
        let oversized_manifest = (0..MAX_MANIFEST_ENTRIES + 1)
            .map(|index| format!("f-{index:04}.txt"))
            .collect::<Vec<_>>();
        assert!(
            build_bundle(&root, &oversized_manifest).is_err(),
            "manifests beyond the entry cap must be rejected before any file open"
        );
        assert!(validate_manifest_path(".").is_err());
        assert!(validate_manifest_path("./").is_err());
        assert!(validate_manifest_path("a/./b").is_ok());
    }

    #[test]
    #[ignore = "writes ~70 MiB to exercise the aggregate bundle budget"]
    fn bundle_rejects_entries_exceeding_the_aggregate_budget() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir(&root).unwrap();
        let manifest = (0..9)
            .map(|index| {
                let name = format!("part-{index}.bin");
                fs::write(root.join(&name), vec![0_u8; 8 * 1024 * 1024]).unwrap();
                name
            })
            .collect::<Vec<_>>();
        assert!(build_bundle(&root, &manifest).is_err());
    }

    #[test]
    fn bundle_destinations_are_state_relative_and_content_addressed() {
        let config = bundle_config(Vec::new(), PathBuf::from("/var/slurm-log/state.json"));
        assert_eq!(
            local_bundle_dir(&config),
            PathBuf::from("/var/slurm-log/bundles")
        );
        assert_eq!(
            remote_bundle_file("abc123"),
            "~/.cache/slurm-log/bundles/abc123.bundle"
        );
        let top = bundle_config(Vec::new(), PathBuf::from("state.json"));
        assert_eq!(local_bundle_dir(&top), PathBuf::from("bundles"));
    }

    #[test]
    fn bundle_refuses_secret_paths_and_key_content() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir_all(root.join(".ssh")).unwrap();
        fs::write(root.join(".ssh/id_rsa"), b"key").unwrap();
        assert!(build_bundle(&root, &[".ssh/id_rsa".into()]).is_err());

        fs::write(root.join("notes.txt"), b"-----BEGIN PGP PRIVATE KEY BLOCK-----\nx\n").unwrap();
        assert!(build_bundle(&root, &["notes.txt".into()]).is_err());
    }
}
