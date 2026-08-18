use super::*;

use std::path::PathBuf;

#[test]
fn protocol_round_trips_query_and_reply() {
    let request = Request::Query {
        cluster: "both".into(),
        filter: "all".into(),
        archive: false,
        force: true,
    };
    let encoded = rmp_serde::to_vec(&request).unwrap();
    assert!(matches!(
        rmp_serde::from_slice(&encoded).unwrap(),
        Request::Query { force: true, .. }
    ));
    let reply = Reply {
        jobs: vec![Job {
            id: "42".into(),
            ..Job::default()
        }],
        ..empty_reply()
    };
    let decoded: Reply = rmp_serde::from_slice(&rmp_serde::to_vec(&reply).unwrap()).unwrap();
    assert_eq!(decoded.jobs[0].id, "42");
}

#[test]
fn control_requests_remain_compatible_with_pre_details_daemon() {
    #[derive(Serialize)]
    enum LegacyRequest {
        Query {
            cluster: String,
            filter: String,
            archive: bool,
            force: bool,
        },
        Ping,
        Stop,
    }
    for (legacy, expected_stop) in [(LegacyRequest::Ping, false), (LegacyRequest::Stop, true)] {
        let encoded = rmp_serde::to_vec(&legacy).unwrap();
        let decoded: Request = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(matches!(decoded, Request::Stop), expected_stop);
    }
    // Exercise the legacy Query variant as well so its field shape stays
    // pinned even though this test only needs the control requests.
    let query = LegacyRequest::Query {
        cluster: "both".into(),
        filter: "all".into(),
        archive: false,
        force: false,
    };
    assert!(matches!(
        rmp_serde::from_slice::<Request>(&rmp_serde::to_vec(&query).unwrap()).unwrap(),
        Request::Query { .. }
    ));
}

fn config(path: PathBuf) -> Config {
    Config {
        local_user: "alice".into(),
        remote_user: "alice".into(),
        ssh_host: "cluster".into(),
        state_path: path,
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![crate::config::ClusterConfig {
            name: "cispa".into(),
            controller: None,
            transport: "ssh".into(),
            user: "alice".into(),
            ssh_host: "cluster".into(),
            working_directory: PathBuf::from("/tmp"),
            accounting: false,
        }],
    }
}

#[test]
fn oversized_and_truncated_frames_are_rejected_without_allocation() {
    let (mut client, mut server) = UnixStream::pair().unwrap();
    client
        .write_all(&(64_u32 * 1024 * 1024 + 1).to_le_bytes())
        .unwrap();
    assert!(read_frame::<Request>(&mut server).is_err());

    let (mut client, mut server) = UnixStream::pair().unwrap();
    client.write_all(&10_u32.to_le_bytes()).unwrap();
    client.write_all(&[1, 2]).unwrap();
    drop(client);
    assert!(read_frame::<Request>(&mut server).is_err());
}

#[test]
fn cached_query_is_served_without_scheduler_access() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path().join("state.json"));
    let job = Job {
        cluster: "cispa".into(),
        id: "cached".into(),
        state: "COMPLETED".into(),
        ..Job::default()
    };
    let reply = Reply {
        jobs: vec![job.clone()],
        ..empty_reply()
    };
    let now = Instant::now();
    let cache = Arc::new(Mutex::new(HashMap::from([(
        cache_key("both", false),
        CachedReply {
            created: now - Duration::from_secs(30),
            last_access: now,
            refreshing: false,
            last_force: None,
            reply: Arc::new(reply),
        },
    )])));
    let (mut client, mut server) = UnixStream::pair().unwrap();
    write_frame(
        &mut client,
        &Request::Query {
            cluster: "both".into(),
            filter: "all".into(),
            archive: false,
            force: false,
        },
    )
    .unwrap();
    Ledger::dismiss(&config.state_path, &[job]).unwrap();
    let details = Arc::new(Mutex::new(HashMap::new()));
    assert!(!handle_stream(&config, &cache, &details, &mut server).unwrap());
    let received: Reply = read_frame(&mut client).unwrap();
    assert_eq!(received.jobs[0].id, "cached");
    assert!(received.ledger.dismissed.contains_key("cispa:cached"));
    assert!(
        crate::slurm::visible_jobs(
            received.jobs,
            &received.ledger,
            crate::slurm::HistoryMode::Live,
            false,
        )
        .is_empty()
    );
}

#[test]
fn repeated_forced_query_is_rate_limited_without_scheduler_access() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path().join("state.json"));
    let now = Instant::now();
    let cache = Arc::new(Mutex::new(HashMap::from([(
        cache_key("both", false),
        CachedReply {
            created: now,
            last_access: now,
            refreshing: false,
            last_force: Some(now),
            reply: Arc::new(Reply {
                jobs: vec![Job {
                    id: "rate-limited".into(),
                    ..Job::default()
                }],
                ..empty_reply()
            }),
        },
    )])));
    let details = Arc::new(Mutex::new(HashMap::new()));
    let (mut client, mut server) = UnixStream::pair().unwrap();
    write_frame(
        &mut client,
        &Request::Query {
            cluster: "both".into(),
            filter: "all".into(),
            archive: false,
            force: true,
        },
    )
    .unwrap();
    assert!(!handle_stream(&config, &cache, &details, &mut server).unwrap());
    let reply: Reply = read_frame(&mut client).unwrap();
    assert_eq!(reply.jobs[0].id, "rate-limited");
}

#[test]
fn canonical_snapshot_derives_filters_without_scheduler_access() {
    let reply = Reply {
        jobs: vec![
            Job {
                cluster: "sprint".into(),
                id: "1".into(),
                state: "RUNNING".into(),
                ..Job::default()
            },
            Job {
                cluster: "cispa".into(),
                id: "2".into(),
                state: "FAILED".into(),
                ..Job::default()
            },
        ],
        ..empty_reply()
    };
    assert_eq!(filtered_reply(&reply, "sprint", "running").jobs.len(), 1);
    assert_eq!(filtered_reply(&reply, "cispa", "failed").jobs[0].id, "2");
    assert!(filtered_reply(&reply, "sprint", "failed").jobs.is_empty());
    assert_eq!(filtered_reply(&reply, "all", "all").jobs.len(), 2);
    assert_eq!(filtered_reply(&reply, "both", "all").jobs.len(), 2);
}

#[test]
#[ignore = "release-mode performance budget"]
fn sparse_filter_does_not_clone_an_entire_large_snapshot() {
    let reply = Reply {
        jobs: (0..100_000)
            .map(|id| Job {
                cluster: if id % 2 == 0 { "sprint" } else { "cispa" }.into(),
                id: id.to_string(),
                state: if id % 100 == 0 {
                    "RUNNING".into()
                } else {
                    "COMPLETED".into()
                },
                name: "large-archive-entry".into(),
                ..Job::default()
            })
            .collect(),
        ..empty_reply()
    };
    let started = Instant::now();
    let filtered = filtered_reply(&reply, "sprint", "running");
    let optimized = started.elapsed();
    assert_eq!(filtered.jobs.len(), 1_000);
    assert!(optimized < Duration::from_millis(if cfg!(coverage) { 500 } else { 100 }));

    // Retain the former clone-everything approach as an offline benchmark
    // oracle so a future refactor cannot silently lose the sparse-filter
    // algorithmic advantage.
    let baseline_started = Instant::now();
    let mut baseline = reply.clone();
    baseline
        .jobs
        .retain(|job| job.cluster == "sprint" && job.running());
    let baseline_elapsed = baseline_started.elapsed();
    assert_eq!(baseline.jobs.len(), filtered.jobs.len());
    assert!(optimized < baseline_elapsed);
    eprintln!("sparse archive filter: optimized={optimized:?}, clone-all={baseline_elapsed:?}");
}

#[test]
fn newer_cluster_snapshot_invalidates_only_older_combined_snapshots() {
    let now = Instant::now();
    let cached = |created| CachedReply {
        created,
        last_access: now,
        refreshing: false,
        last_force: None,
        reply: Arc::new(empty_reply()),
    };
    let mut entries = HashMap::from([
        (
            cache_key("all", false),
            cached(now - Duration::from_secs(2)),
        ),
        (
            cache_key("both", false),
            cached(now - Duration::from_secs(1)),
        ),
        (cache_key("all", true), cached(now)),
    ]);

    invalidate_older_combined(&mut entries, "cispa", false, now);
    assert!(!entries.contains_key(&cache_key("all", false)));
    assert!(!entries.contains_key(&cache_key("both", false)));
    assert!(entries.contains_key(&cache_key("all", true)));

    entries.insert(cache_key("all", false), cached(now));
    invalidate_older_combined(&mut entries, "sprint", false, now - Duration::from_secs(1));
    assert!(entries.contains_key(&cache_key("all", false)));
}

#[test]
fn terminal_detail_snapshot_is_reused_by_the_cache_resolver() {
    let now = Instant::now();
    let details = Arc::new(Mutex::new(HashMap::from([(
        format!("cispa\0{}", "42"),
        DetailEntry {
            created: now - Duration::from_secs(30),
            last_access: now,
            failures: 0,
            details: JobDetails {
                cluster: "cispa".into(),
                id: "42".into(),
                state: "COMPLETED".into(),
                terminal: true,
                ..JobDetails::default()
            },
        },
    )])));
    let reply = resolve_detail_reply(&details, "cispa\0".to_string() + "42", false, |_| {
        panic!("terminal cache entry unexpectedly refreshed")
    });
    assert_eq!(reply.details.unwrap().state, "COMPLETED");
}

#[test]
fn socket_and_lock_are_scoped_to_the_private_state_directory() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path().join("nested/state.json"));
    let (socket, lock) = paths(&config);
    assert_eq!(socket, directory.path().join("nested/daemon.sock"));
    assert_eq!(lock, directory.path().join("nested/daemon.lock"));
}

#[test]
#[ignore = "release-mode performance budget"]
fn binary_protocol_handles_large_archive_within_budget() {
    let reply = Reply {
        jobs: (0..10_000)
            .map(|id| Job {
                cluster: "cispa".into(),
                id: id.to_string(),
                state: "COMPLETED".into(),
                name: "long-training-name".into(),
                elapsed: "01:23:45".into(),
                ended: "2026-08-11T12:00:00+02:00".into(),
                ..Job::default()
            })
            .collect(),
        ..empty_reply()
    };
    let started = Instant::now();
    let encoded = encode_reply(&reply).unwrap();
    let payload: Reply = rmp_serde::from_slice(&encoded[4..]).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(payload.jobs.len(), 10_000);
    assert!(elapsed < Duration::from_millis(if cfg!(coverage) { 500 } else { 100 }));
    eprintln!("binary round trip 10k jobs: {elapsed:?}");
}

#[test]
#[ignore = "release-mode performance budget"]
fn borrowed_daemon_reply_avoids_cloning_large_ledger() {
    let jobs: Vec<_> = (0..20_000)
        .map(|id| Job {
            cluster: if id % 2 == 0 { "sprint" } else { "cispa" }.into(),
            id: id.to_string(),
            state: if id % 20 == 0 {
                "RUNNING".into()
            } else {
                "COMPLETED".into()
            },
            name: "archive-job".into(),
            ..Job::default()
        })
        .collect();
    let reply = Reply {
        ledger: Ledger {
            known: jobs
                .iter()
                .map(|job| (job.key(), "2026-08-12T00:00:00Z".into()))
                .collect(),
            ..Ledger::default()
        },
        jobs,
        ..empty_reply()
    };
    let started = Instant::now();
    let borrowed = encode_filtered_reply(&reply, "sprint", "running").unwrap();
    let borrowed_elapsed = started.elapsed();
    let decoded: Reply = rmp_serde::from_slice(&borrowed[4..]).unwrap();
    assert_eq!(decoded.jobs.len(), 1_000);
    assert_eq!(decoded.ledger.known.len(), 20_000);

    let baseline_started = Instant::now();
    let owned = filtered_reply(&reply, "sprint", "running");
    let baseline = encode_reply(&owned).unwrap();
    let baseline_elapsed = baseline_started.elapsed();
    assert_eq!(
        rmp_serde::from_slice::<Reply>(&baseline[4..])
            .unwrap()
            .jobs
            .len(),
        1_000
    );
    assert!(borrowed_elapsed < Duration::from_millis(if cfg!(coverage) { 500 } else { 150 }));
    assert!(borrowed_elapsed < baseline_elapsed);
    eprintln!(
        "daemon filtered reply: borrowed={borrowed_elapsed:?}, clone-first={baseline_elapsed:?}"
    );
}
