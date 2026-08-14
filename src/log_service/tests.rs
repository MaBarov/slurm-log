use super::*;
use crate::config::{ClusterConfig, Config};
use std::path::PathBuf;

fn config(root: &Path, remote: bool) -> Config {
    Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: String::new(),
        state_path: root.join("state.json"),
        executable: PathBuf::from("/bin/false"),
        sbatch_banks: Vec::new(),
        clusters: vec![ClusterConfig {
            name: "alpha".into(),
            controller: None,
            transport: if remote { "ssh" } else { "local" }.into(),
            user: "offline".into(),
            ssh_host: if remote { "fake" } else { "" }.into(),
            working_directory: root.to_path_buf(),
            accounting: false,
        }],
    }
}

#[test]
fn bounds_are_bounded_and_range_starts_are_clamped() {
    assert_eq!(read_bounds(100, &ReadMode::Metadata), (100, 0));
    assert_eq!(read_bounds(100, &ReadMode::Window(20)), (80, 20));
    assert_eq!(read_bounds(10, &ReadMode::Window(20)), (0, 20));
    assert_eq!(read_bounds(10, &ReadMode::Range(99, 4)), (10, 4));
}

#[test]
fn generation_is_stable_but_cluster_and_inode_scoped() {
    let value = generation("one", "123", "1:2");
    assert_eq!(value.len(), 64);
    assert_eq!(value, generation("one", "123", "1:2"));
    assert_ne!(value, generation("two", "123", "1:2"));
    assert_ne!(value, generation("one", "123", "1:3"));
}

#[test]
fn local_reads_cover_metadata_tail_ranges_and_missing_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("job.log");
    std::fs::write(&path, b"0123456789").unwrap();
    let file = File::open(&path).unwrap();
    let metadata = local_read(&file, &ReadMode::Metadata).unwrap();
    assert_eq!((metadata.1, metadata.3, metadata.4), (10, 10, Vec::new()));
    let window = local_read(&file, &ReadMode::Window(4)).unwrap();
    assert_eq!((window.3, window.4), (6, b"6789".to_vec()));
    let range = local_read(&file, &ReadMode::Range(2, 3)).unwrap();
    assert_eq!((range.3, range.4), (2, b"234".to_vec()));
    assert!(File::open(directory.path().join("missing")).is_err());
}

#[test]
fn unavailable_metadata_and_resolved_job_preserve_identity() {
    let job = Job {
        cluster: "alpha".into(),
        id: "42".into(),
        name: "training".into(),
        state: "RUNNING".into(),
        ..Job::default()
    };
    let unavailable = LogData::unavailable("alpha", "42", Some(&job), "pending_log");
    assert_eq!(unavailable.job_name, "training");
    assert!(!unavailable.terminal);
    assert!(LogData::unavailable("alpha", "42", None, "not_found").terminal);

    let resolved = ResolvedLog {
        cluster: "alpha".into(),
        id: "42".into(),
        name: "training".into(),
        state: "RUNNING".into(),
        terminal: false,
        source: LogSource::Remote("job.log".into()),
    };
    assert_eq!(resolved_job(&resolved), job);

    let metadata = LogData {
        size: 17,
        offset: 2,
        bytes: b"payload".to_vec(),
        ..LogData::default()
    }
    .metadata_only();
    assert_eq!(metadata.offset, 17);
    assert!(metadata.bytes.is_empty());
}

#[test]
fn confined_sources_reject_escape_and_preserve_remote_relative_paths() {
    let directory = tempfile::tempdir().unwrap();
    let local = config(directory.path(), false);
    assert!(confined_log_source(&local, "alpha", "../outside.log").is_err());
    assert!(confined_log_source(&local, "alpha", "/outside.log").is_err());

    let remote = config(directory.path(), true);
    match confined_log_source(&remote, "alpha", "nested/job.log").unwrap() {
        LogSource::Remote(path) => assert_eq!(path, "nested/job.log"),
        LogSource::Local(_) => panic!("expected remote source"),
    }
}

#[test]
fn local_reads_reject_multi_link_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("job.log");
    let alias = directory.path().join("alias.log");
    std::fs::write(&path, b"secret").unwrap();
    std::fs::hard_link(&path, alias).unwrap();
    let file = File::open(path).unwrap();
    assert!(local_read(&file, &ReadMode::Metadata).is_err());
}

#[test]
fn resolve_rejects_invalid_job_ids_before_authorization() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path(), false);
    let error = resolve(&config, "alpha", "not a valid id").unwrap_err();
    assert!(format!("{error:#}").contains("invalid job ID"));
}
