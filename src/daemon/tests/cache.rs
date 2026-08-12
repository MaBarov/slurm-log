use super::*;

fn cached(created: Instant, last_access: Instant, refreshing: bool) -> CachedReply {
    CachedReply {
        created,
        last_access,
        refreshing,
        last_force: None,
        reply: Arc::new(empty_reply()),
    }
}

#[test]
fn due_refresh_selection_respects_ttl_activity_and_in_flight_state() {
    let now = Instant::now();
    let mut entries = HashMap::from([
        (
            cache_key("cispa", false),
            cached(
                now - MEMORY_TTL - Duration::from_millis(1),
                now - Duration::from_secs(1),
                false,
            ),
        ),
        (
            cache_key("sprint", true),
            cached(
                now - Duration::from_secs(61),
                now - Duration::from_secs(2),
                false,
            ),
        ),
        (cache_key("fresh", false), cached(now, now, false)),
        (
            cache_key("idle", false),
            cached(
                now - Duration::from_secs(30),
                now - Duration::from_secs(21),
                false,
            ),
        ),
        (
            cache_key("busy", false),
            cached(now - Duration::from_secs(30), now, true),
        ),
    ]);
    let mut due = mark_due_refreshes(&mut entries);
    due.sort();
    assert_eq!(
        due,
        vec![
            (cache_key("cispa", false), "cispa".into(), false),
            (cache_key("sprint", true), "sprint".into(), true),
        ]
    );
    assert!(entries[&cache_key("cispa", false)].refreshing);
    assert!(!entries[&cache_key("fresh", false)].refreshing);
}

#[test]
fn refresh_result_updates_entry_and_invalidates_older_combined_cache() {
    let old = Instant::now() - Duration::from_secs(20);
    let mut entries = HashMap::from([
        (cache_key("cispa", false), cached(old, old, true)),
        (cache_key("all", false), cached(old, old, false)),
        (cache_key("both", false), cached(old, old, false)),
    ]);
    let jobs = vec![Job {
        cluster: "cispa".into(),
        id: "42".into(),
        ..Job::default()
    }];
    apply_refresh_result(
        &mut entries,
        &cache_key("cispa", false),
        "cispa",
        false,
        Ok((jobs, Ledger::default(), vec!["notice".into()])),
    );
    let entry = &entries[&cache_key("cispa", false)];
    assert!(!entry.refreshing);
    assert_eq!(entry.reply.jobs[0].id, "42");
    assert_eq!(entry.reply.warnings, ["notice"]);
    assert!(!entries.contains_key(&cache_key("all", false)));
    assert!(!entries.contains_key(&cache_key("both", false)));
}

#[test]
fn failed_or_obsolete_refresh_is_non_destructive() {
    let now = Instant::now();
    let key = cache_key("cispa", false);
    let mut entries = HashMap::from([(key.clone(), cached(now, now, true))]);
    apply_refresh_result(
        &mut entries,
        &key,
        "cispa",
        false,
        Err(anyhow::anyhow!("scheduler unavailable")),
    );
    assert!(!entries[&key].refreshing);
    assert!(entries[&key].reply.jobs.is_empty());
    apply_refresh_result(
        &mut entries,
        "removed\0false",
        "removed",
        false,
        Ok((Vec::new(), Ledger::default(), Vec::new())),
    );
    assert_eq!(entries.len(), 1);
}

#[test]
fn combined_refresh_never_invalidates_itself() {
    let now = Instant::now();
    let mut entries = HashMap::from([(cache_key("all", false), cached(now, now, false))]);
    invalidate_older_combined(&mut entries, "all", false, now);
    invalidate_older_combined(&mut entries, "both", false, now);
    assert_eq!(entries.len(), 1);
}

fn detail(cluster: &str, id: &str, terminal: bool) -> JobDetails {
    JobDetails {
        cluster: cluster.into(),
        id: id.into(),
        terminal,
        state: if terminal { "COMPLETED" } else { "RUNNING" }.into(),
        ..JobDetails::default()
    }
}

fn detail_entry(created: Instant, accessed: Instant, details: JobDetails) -> DetailEntry {
    DetailEntry {
        created,
        last_access: accessed,
        failures: 0,
        details,
    }
}

#[test]
fn detail_cache_handles_terminal_active_forced_and_expired_entries() {
    let now = Instant::now();
    let mut entries = HashMap::from([
        (
            "cispa\0terminal".into(),
            detail_entry(
                now - Duration::from_secs(120),
                now,
                detail("cispa", "terminal", true),
            ),
        ),
        (
            "cispa\0active".into(),
            detail_entry(now, now, detail("cispa", "active", false)),
        ),
        (
            "cispa\0expired".into(),
            detail_entry(
                now,
                now - Duration::from_secs(61),
                detail("cispa", "expired", false),
            ),
        ),
    ]);
    let (terminal, previous) = cached_detail(&mut entries, "cispa\0terminal", false);
    assert!(terminal.unwrap().terminal);
    assert_eq!(previous.unwrap().id, "terminal");
    let (active, _) = cached_detail(&mut entries, "cispa\0active", false);
    assert_eq!(active.unwrap().state, "RUNNING");
    let (forced, _) = cached_detail(&mut entries, "cispa\0active", true);
    assert!(forced.is_some());
    assert!(!entries.contains_key("cispa\0expired"));

    entries.get_mut("cispa\0active").unwrap().created = now - Duration::from_secs(31);
    let (stale, previous) = cached_detail(&mut entries, "cispa\0active", false);
    assert!(stale.is_none());
    assert_eq!(previous.unwrap().id, "active");
}

#[test]
fn detail_cache_backoff_is_bounded_and_force_uses_short_minimum() {
    let now = Instant::now();
    let mut entry = detail_entry(
        now - Duration::from_secs(20),
        now,
        detail("cispa", "42", false),
    );
    entry.failures = u32::MAX;
    let mut entries = HashMap::from([(concat!("cispa\0", "42").into(), entry)]);
    assert!(
        cached_detail(&mut entries, concat!("cispa\0", "42"), false)
            .0
            .is_some()
    );
    assert!(
        cached_detail(&mut entries, concat!("cispa\0", "42"), true)
            .0
            .is_none()
    );
}

#[test]
fn storing_details_evicts_oldest_only_when_capacity_needs_space() {
    let now = Instant::now();
    let mut entries = HashMap::new();
    for id in 0..64 {
        entries.insert(
            format!("cispa\0{id}"),
            detail_entry(
                now - Duration::from_secs(id + 1),
                now - Duration::from_secs(id + 1),
                detail("cispa", &id.to_string(), false),
            ),
        );
    }
    let reply = store_detail(
        &mut entries,
        "cispa\0new".into(),
        detail("cispa", "new", false),
    );
    assert_eq!(reply.details.unwrap().id, "new");
    assert_eq!(entries.len(), 64);
    assert!(!entries.contains_key(concat!("cispa\0", "63")));

    let replacement = store_detail(
        &mut entries,
        "cispa\0new".into(),
        detail("cispa", "new", true),
    );
    assert!(replacement.details.unwrap().terminal);
    assert_eq!(entries.len(), 64);
}

#[test]
fn failed_detail_returns_stale_snapshot_or_bounded_error() {
    let now = Instant::now();
    let key = concat!("cispa\0", "42");
    let mut entries = HashMap::from([(
        key.into(),
        detail_entry(now, now, detail("cispa", "42", false)),
    )]);
    let reply = failed_detail(&mut entries, key, &anyhow::anyhow!("temporary failure"));
    assert!(
        reply
            .details
            .unwrap()
            .stale_error
            .contains("temporary failure")
    );
    assert_eq!(entries[key].failures, 1);
    let reply = failed_detail(&mut entries, "missing", &anyhow::anyhow!("gone"));
    assert_eq!(reply.error.as_deref(), Some("gone"));
}

fn local_config(path: PathBuf) -> Config {
    Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: String::new(),
        state_path: path,
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![crate::config::ClusterConfig {
            name: "local".into(),
            transport: "local".into(),
            user: "offline".into(),
            ssh_host: String::new(),
            working_directory: PathBuf::from("/tmp"),
            accounting: false,
        }],
    }
}

fn handle(request: Request) -> Result<(bool, Reply)> {
    let directory = tempfile::tempdir()?;
    let config = local_config(directory.path().join("state.json"));
    let cache = Arc::new(Mutex::new(HashMap::new()));
    let details = Arc::new(Mutex::new(HashMap::new()));
    let (mut client, mut server) = UnixStream::pair()?;
    write_frame(&mut client, &request)?;
    let stop = handle_stream(&config, &cache, &details, &mut server)?;
    Ok((stop, read_frame(&mut client)?))
}

#[test]
fn stream_handler_supports_ping_stop_and_rejects_invalid_inputs() {
    assert!(!handle(Request::Ping).unwrap().0);
    assert!(handle(Request::Stop).unwrap().0);

    let directory = tempfile::tempdir().unwrap();
    let config = local_config(directory.path().join("state.json"));
    let cache = Arc::new(Mutex::new(HashMap::new()));
    let details = Arc::new(Mutex::new(HashMap::new()));
    for request in [
        Request::Details {
            cluster: "missing".into(),
            id: "42".into(),
            force: false,
        },
        Request::Details {
            cluster: "local".into(),
            id: "bad id".into(),
            force: false,
        },
        Request::Query {
            cluster: "local".into(),
            filter: "invalid".into(),
            archive: false,
            force: false,
        },
    ] {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        write_frame(&mut client, &request).unwrap();
        assert!(handle_stream(&config, &cache, &details, &mut server).is_err());
    }
}

#[test]
fn stream_handler_serves_cached_queries_and_details_without_scheduler_calls() {
    let directory = tempfile::tempdir().unwrap();
    let config = local_config(directory.path().join("state.json"));
    let now = Instant::now();
    let snapshot = Reply {
        jobs: vec![Job {
            cluster: "local".into(),
            id: "42".into(),
            state: "RUNNING".into(),
            ..Job::default()
        }],
        ..empty_reply()
    };
    let query_key = cache_key("local", false);
    let cache = Arc::new(Mutex::new(HashMap::from([(
        query_key,
        CachedReply {
            created: now,
            last_access: now,
            refreshing: false,
            last_force: Some(now),
            reply: Arc::new(snapshot),
        },
    )])));
    let detail_key = concat!("local\0", "42");
    let details = Arc::new(Mutex::new(HashMap::from([(
        detail_key.into(),
        detail_entry(now, now, detail("local", "42", true)),
    )])));

    for force in [false, true] {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        write_frame(
            &mut client,
            &Request::Query {
                cluster: "local".into(),
                filter: "all".into(),
                archive: false,
                force,
            },
        )
        .unwrap();
        assert!(!handle_stream(&config, &cache, &details, &mut server).unwrap());
        let reply: Reply = read_frame(&mut client).unwrap();
        assert_eq!(reply.jobs[0].id, "42");
    }

    let (mut client, mut server) = UnixStream::pair().unwrap();
    write_frame(
        &mut client,
        &Request::Details {
            cluster: "local".into(),
            id: "42".into(),
            force: true,
        },
    )
    .unwrap();
    assert!(!handle_stream(&config, &cache, &details, &mut server).unwrap());
    let reply: Reply = read_frame(&mut client).unwrap();
    assert!(reply.details.unwrap().terminal);

    let (mut client, mut server) = UnixStream::pair().unwrap();
    write_frame(
        &mut client,
        &Request::Query {
            cluster: "missing".into(),
            filter: "all".into(),
            archive: false,
            force: true,
        },
    )
    .unwrap();
    assert!(!handle_stream(&config, &cache, &details, &mut server).unwrap());
    let reply: Reply = read_frame(&mut client).unwrap();
    assert!(reply.error.unwrap().contains("unknown cluster"));
}

#[test]
fn server_survives_a_malformed_client_rejects_duplicate_daemons_and_stops_cleanly() {
    let directory = tempfile::tempdir().unwrap();
    let config = local_config(directory.path().join("state/state.json"));
    let server_config = config.clone();
    let server = thread::spawn(move || run(&server_config));
    let (socket, _) = paths(&config);
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(socket.exists());

    let mut malformed = UnixStream::connect(&socket).unwrap();
    malformed.write_all(&[1, 0, 0, 0, 0xc1]).unwrap();
    let reply: Reply = read_frame(&mut malformed).unwrap();
    assert!(reply.error.unwrap().contains("invalid daemon request"));

    // The lock makes a concurrent daemon invocation a successful no-op.
    run(&config).unwrap();
    let reply = exchange(&socket, &Request::Ping).unwrap();
    assert!(reply.error.is_none());
    exchange(&socket, &Request::Stop).unwrap();
    server.join().unwrap().unwrap();
    assert!(!socket.exists());
}

#[test]
fn refresh_wrapper_clears_in_flight_state_after_a_bounded_query_error() {
    let directory = tempfile::tempdir().unwrap();
    let config = local_config(directory.path().join("state.json"));
    let key = cache_key("missing", false);
    let now = Instant::now();
    let cache = Arc::new(Mutex::new(HashMap::from([(
        key.clone(),
        cached(now, now, true),
    )])));
    refresh_cached(
        config,
        Arc::clone(&cache),
        key.clone(),
        "missing".into(),
        false,
    );
    assert!(!cache.lock().unwrap()[&key].refreshing);
}
