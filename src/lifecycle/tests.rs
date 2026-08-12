use super::*;

#[test]
fn release_roots_and_checksums_are_strict() {
    assert!(validate_release_root("https://example.invalid/releases").is_ok());
    assert!(validate_release_root("file:///tmp/releases").is_ok());
    assert!(validate_release_root("http://example.invalid").is_err());
    assert!(validate_release_root("https://example.invalid\nforged").is_err());

    let digest = "0123456789abcdef".repeat(4);
    assert_eq!(
        parse_checksum(&format!("{digest}  release.tar.gz\n")).unwrap(),
        digest
    );
    assert!(parse_checksum("abcd").is_err());
    assert!(parse_checksum(&"g".repeat(64)).is_err());
    assert_eq!(parse_version("1.24.3").unwrap(), (1, 24, 3));
    assert!(parse_version("1.2").is_err());
    assert!(parse_version("1.2.3.4").is_err());
    assert_eq!(format_version((2, 0, 1)), "2.0.1");
}

#[test]
fn content_comparison_is_streamed_and_exact() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    fs::write(&first, b"same bytes").unwrap();
    fs::write(&second, b"same bytes").unwrap();
    assert!(same_contents(&first, &second).unwrap());
    fs::write(&second, b"same byteZ").unwrap();
    assert!(!same_contents(&first, &second).unwrap());
    fs::write(&second, b"short").unwrap();
    assert!(!same_contents(&first, &second).unwrap());
}

#[test]
fn atomic_replacement_rejects_directories_and_preserves_mode() {
    let directory = tempfile::tempdir().unwrap();
    let candidate = directory.path().join("candidate");
    let target = directory.path().join("slurm-log");
    fs::write(&candidate, b"new executable").unwrap();
    fs::write(&target, b"old executable").unwrap();
    atomic_replace(&candidate, &target).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"new executable");
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(canonical_regular_file(directory.path(), "fixture").is_err());
}

#[test]
fn custom_state_purge_removes_only_known_files() {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("custom-state.json");
    let unrelated = directory.path().join("keep-me");
    fs::write(&state, b"{}").unwrap();
    fs::write(state.with_extension("lock"), b"").unwrap();
    fs::write(&unrelated, b"private data").unwrap();
    let config_path = directory.path().join("missing-config");

    remove_application_path(&config_path, false).unwrap();
    remove_application_path(&state, false).unwrap();
    remove_application_path(&state.with_extension("lock"), false).unwrap();
    assert!(!state.exists());
    assert!(unrelated.exists());
}
