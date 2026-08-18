use super::*;
use std::path::PathBuf;

#[test]
fn direct_aggregation_uses_complete_archive_caches_and_filters_canonically() {
    let directory = tempfile::tempdir().unwrap();
    let config = Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: String::new(),
        state_path: directory.path().join("state.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![crate::config::ClusterConfig {
            name: "local".into(),
            controller: None,
            transport: "local".into(),
            user: "offline".into(),
            ssh_host: String::new(),
            working_directory: directory.path().into(),
            accounting: true,
        }],
    };
    store_jobs(
        &cache_path(&config, &queue_cache_name("local")),
        &[Job {
            cluster: "local".into(),
            id: "2".into(),
            state: "RUNNING".into(),
            ..Job::default()
        }],
    );
    store_jobs(
        &cache_path(
            &config,
            &format!("archive-local-{}d", archive_horizon_days()),
        ),
        &[Job {
            cluster: "local".into(),
            id: "1".into(),
            state: "FAILED".into(),
            ..Job::default()
        }],
    );
    let (jobs, ledger, warnings) = all_jobs_direct(&config, "local", "all", true).unwrap();
    assert_eq!(
        jobs.iter().map(|job| job.id.as_str()).collect::<Vec<_>>(),
        ["2", "1"]
    );
    assert!(warnings.is_empty());
    assert!(ledger.baselined_clusters.contains(&"local".into()));
    assert_eq!(
        all_jobs_direct(&config, "local", "running", true)
            .unwrap()
            .0
            .len(),
        1
    );
    assert_eq!(
        all_jobs_direct(&config, "local", "failed", true)
            .unwrap()
            .0
            .len(),
        1
    );
}

#[test]
fn recent_end_parser_accepts_local_timestamps_without_an_explicit_offset() {
    let ended = OffsetDateTime::now_local()
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
        .replace_nanosecond(0)
        .unwrap();
    let ended = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        ended.year(),
        ended.month() as u8,
        ended.day(),
        ended.hour(),
        ended.minute(),
        ended.second()
    );
    assert!(recently_ended(
        &Job {
            ended,
            ..Job::default()
        },
        5
    ));
}

#[test]
fn terminal_lookup_rejects_bad_ids_and_unknown_clusters_before_process_spawn() {
    let config = Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: String::new(),
        state_path: PathBuf::from("/tmp/state.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: Vec::new(),
    };
    assert!(terminal_path(&config, "missing", "bad id").is_err());
    assert!(terminal_path(&config, "missing", "42").is_err());
    let job = Job {
        cluster: "missing".into(),
        id: "42".into(),
        state: "COMPLETED".into(),
        ..Job::default()
    };
    assert_eq!(final_details(&config, &job), job);
}
