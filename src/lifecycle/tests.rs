use super::*;

#[test]
fn versions_are_strict() {
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
    let linked = directory.path().join("linked");
    std::os::unix::fs::symlink(&target, &linked).unwrap();
    assert!(canonical_regular_file(&linked, "fixture").is_err());
}

#[test]
fn checksum_candidate_and_atomic_failure_paths_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let empty = directory.path().join("empty");
    fs::write(&empty, b"").unwrap();
    fs::set_permissions(&empty, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(validate_candidate(&empty).is_err());

    let target = directory.path().join("target");
    let temporary = directory.path().join("temporary");
    fs::write(&target, b"old").unwrap();
    assert!(atomic_replace_at(&empty, &target, &temporary).is_err());
    assert!(!temporary.exists());
    assert_eq!(fs::read(&target).unwrap(), b"old");
}

#[test]
fn version_and_directory_removal_failures_are_explicit() {
    let directory = tempfile::tempdir().unwrap();
    let candidate = directory.path().join("candidate");
    fs::write(
        &candidate,
        b"#!/bin/sh\ncase \"$1\" in --help) exit 0;; --version) exit 9;; esac\n",
    )
    .unwrap();
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(binary_version(&candidate).is_err());

    let empty_directory = directory.path().join("empty-directory");
    fs::create_dir(&empty_directory).unwrap();
    remove_application_path(&empty_directory, false).unwrap();
    assert!(!empty_directory.exists());
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
