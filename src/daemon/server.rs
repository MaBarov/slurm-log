fn run(config: &Config) -> Result<()> {
    let (socket, lock_path) = paths(config);
    if let Some(parent) = socket.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(());
    }
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    // A blocking accept thread gives immediate wakeups without the old 10 ms
    // polling quantum. recv_timeout provides idle shutdown independently.
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            if sender.send(stream).is_err() {
                break;
            }
        }
    });
    let cache: SharedCache = Arc::new(Mutex::new(HashMap::new()));
    let detail_cache: DetailCache = Arc::new(Mutex::new(HashMap::new()));
    start_refresh_loop(config.clone(), Arc::clone(&cache));
    while let Ok(mut stream) = receiver.recv_timeout(IDLE_TIMEOUT) {
        let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
        let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));
        match handle_stream(config, &cache, &detail_cache, &mut stream) {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) => {
                // A malformed or disconnected client must not terminate the
                // daemon. Return a bounded error when possible, then continue.
                let _ = write_reply(
                    &mut stream,
                    &Reply {
                        error: Some(format!("invalid daemon request: {error:#}")),
                        ..empty_reply()
                    },
                );
            }
        }
    }
    let _ = fs::remove_file(socket);
    Ok(())
}
fn handle_stream(
    config: &Config,
    cache: &SharedCache,
    detail_cache: &DetailCache,
    stream: &mut UnixStream,
) -> Result<bool> {
    let request: Request = read_frame(stream)?;
    let reply = match request {
        Request::Ping => empty_reply(),
        Request::Stop => {
            write_reply(stream, &empty_reply())?;
            return Ok(true);
        }
        Request::Details { cluster, id, force } => {
            crate::details::validate_cluster(config, &cluster)?;
            if !crate::model::valid_job_id(&id) {
                bail!("invalid job ID {id}");
            }
            let key = format!("{cluster}\0{id}");
            let reply = resolve_detail_reply(detail_cache, key, force, |previous| {
                crate::details::fetch(config, &cluster, &id, previous)
            });
            write_reply(stream, &reply)?;
            return Ok(false);
        }
        Request::Query {
            cluster,
            filter,
            archive,
            force,
        } => {
            crate::slurm::validate_query(&cluster, &filter)?;
            let key = cache_key(&cluster, archive);
            if force {
                let throttled = {
                    let mut entries = cache.lock().unwrap_or_else(|error| error.into_inner());
                    entries.get_mut(&key).and_then(|entry| {
                        entry.last_access = Instant::now();
                        entry
                            .last_force
                            .is_some_and(|last| last.elapsed() < FORCED_DETAIL_MINIMUM)
                            .then(|| Arc::clone(&entry.reply))
                    })
                };
                if let Some(snapshot) = throttled {
                    write_filtered_reply(stream, &snapshot, &cluster, &filter)?;
                    return Ok(false);
                }
            }
            if !force {
                let cached = {
                    let mut entries = cache.lock().unwrap_or_else(|error| error.into_inner());
                    entries.get_mut(&key).map(|entry| {
                        entry.last_access = Instant::now();
                        Arc::clone(&entry.reply)
                    })
                };
                if let Some(snapshot) = cached {
                    // A stale snapshot is deliberately returned immediately.
                    // The refresh loop updates it in the background, so opening
                    // another picker never waits for SSH or scheduler RPCs.
                    write_filtered_reply(stream, &snapshot, &cluster, &filter)?;
                    return Ok(false);
                }
            }
            if force {
                crate::slurm::invalidate_caches(config);
                cache
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(&key);
            }
            let canonical = match crate::slurm::all_jobs_direct(config, &cluster, "all", archive) {
                Ok((jobs, ledger, warnings)) => Reply {
                    jobs,
                    ledger,
                    warnings,
                    error: None,
                    details: None,
                },
                Err(error) => Reply {
                    error: Some(format!("{error:#}")),
                    ..empty_reply()
                },
            };
            write_filtered_reply(stream, &canonical, &cluster, &filter)?;
            let now = Instant::now();
            let mut entries = cache.lock().unwrap_or_else(|error| error.into_inner());
            entries.insert(
                key,
                CachedReply {
                    created: now,
                    last_access: now,
                    refreshing: false,
                    last_force: force.then_some(now),
                    reply: Arc::new(canonical),
                },
            );
            invalidate_older_combined(&mut entries, &cluster, archive, now);
            return Ok(false);
        }
    };
    write_reply(stream, &reply)?;
    Ok(false)
}

fn resolve_detail_reply(
    detail_cache: &DetailCache,
    key: String,
    force: bool,
    fetch: impl FnOnce(Option<&JobDetails>) -> Result<JobDetails>,
) -> Reply {
    let (cached, previous) = {
        let mut entries = detail_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        cached_detail(&mut entries, &key, force)
    };
    let result = cached.map_or_else(|| fetch(previous.as_ref()), Ok);
    let mut entries = detail_cache
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    match result {
        Ok(details) => store_detail(&mut entries, key, details),
        Err(error) => failed_detail(&mut entries, &key, &error),
    }
}

fn cached_detail(
    entries: &mut HashMap<String, DetailEntry>,
    key: &str,
    force: bool,
) -> (Option<JobDetails>, Option<JobDetails>) {
    entries.retain(|_, entry| entry.last_access.elapsed() < Duration::from_secs(60));
    let mut previous = None;
    let cached = entries.get_mut(key).and_then(|entry| {
        entry.last_access = Instant::now();
        previous = Some(entry.details.clone());
        let base = if entry.details.terminal {
            DETAIL_TTL
        } else {
            ACTIVE_DETAIL_TTL
        };
        let normal = base.saturating_mul(1_u32 << entry.failures.min(2));
        let minimum = if force { FORCED_DETAIL_MINIMUM } else { normal };
        (entry.details.terminal || entry.created.elapsed() < minimum)
            .then(|| entry.details.clone())
    });
    (cached, previous)
}

fn store_detail(
    entries: &mut HashMap<String, DetailEntry>,
    key: String,
    details: JobDetails,
) -> Reply {
    if entries.len() >= 64
        && !entries.contains_key(&key)
        && let Some(oldest) = entries
            .iter()
            .min_by_key(|(_, value)| value.last_access)
            .map(|(key, _)| key.clone())
    {
        entries.remove(&oldest);
    }
    let now = Instant::now();
    entries.insert(
        key,
        DetailEntry {
            created: now,
            last_access: now,
            failures: 0,
            details: details.clone(),
        },
    );
    Reply {
        details: Some(details),
        ..empty_reply()
    }
}

fn failed_detail(
    entries: &mut HashMap<String, DetailEntry>,
    key: &str,
    error: &anyhow::Error,
) -> Reply {
    if let Some(entry) = entries.get_mut(key) {
        entry.failures = entry.failures.saturating_add(1);
        entry.created = Instant::now();
        let mut stale = entry.details.clone();
        stale.stale_error = format!("{error:#}");
        Reply {
            details: Some(stale),
            ..empty_reply()
        }
    } else {
        Reply {
            error: Some(format!("{error:#}")),
            ..empty_reply()
        }
    }
}
