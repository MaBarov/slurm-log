use super::*;
use crate::config::ClusterConfig;

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
        assert!(
            validate_manifest_path(hostile).is_err(),
            "accepted {hostile:?}"
        );
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
    assert!(
        bundle_root(&distinct, None).is_err(),
        "two banks are ambiguous"
    );

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
        bundle_root(
            &bundle_config(Vec::new(), directory.path().join("s.json")),
            None
        )
        .is_err(),
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

    fs::write(
        root.join("notes.txt"),
        b"-----BEGIN PGP PRIVATE KEY BLOCK-----\nx\n",
    )
    .unwrap();
    assert!(build_bundle(&root, &["notes.txt".into()]).is_err());
}
