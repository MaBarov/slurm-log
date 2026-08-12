fn cache_key(cluster: &str, archive: bool) -> String {
    format!("{cluster}\0{archive}")
}

fn invalidate_older_combined(
    entries: &mut HashMap<String, CachedReply>,
    cluster: &str,
    archive: bool,
    cluster_created: Instant,
) {
    if matches!(cluster, "all" | "both") {
        return;
    }
    for combined in ["all", "both"] {
        let key = cache_key(combined, archive);
        if entries
            .get(&key)
            .is_some_and(|entry| entry.created <= cluster_created)
        {
            entries.remove(&key);
        }
    }
}

fn filtered_reply(reply: &Reply, cluster: &str, filter: &str) -> Reply {
    Reply {
        jobs: reply
            .jobs
            .iter()
            .filter(|job| {
                (matches!(cluster, "all" | "both") || job.cluster == cluster)
                    && match filter {
                        "running" => job.running(),
                        "failed" => job.failed(),
                        "blocked" => job.blocked_category(),
                        _ => true,
                    }
            })
            .cloned()
            .collect(),
        ledger: reply.ledger.clone(),
        warnings: reply.warnings.clone(),
        error: reply.error.clone(),
        details: reply.details.clone(),
    }
}

fn start_refresh_loop(config: Config, cache: SharedCache) {
    thread::spawn(move || {
        loop {
            thread::sleep(MEMORY_TTL);
            let due: Vec<(String, String, bool)> = {
                let mut entries = cache.lock().unwrap_or_else(|error| error.into_inner());
                entries
                    .iter_mut()
                    .filter_map(|(key, entry)| {
                        let archive = key.ends_with("\0true");
                        let ttl = if archive {
                            Duration::from_secs(60)
                        } else {
                            MEMORY_TTL
                        };
                        let active_window = if archive {
                            Duration::from_secs(70)
                        } else {
                            Duration::from_secs(20)
                        };
                        if !entry.refreshing
                            && entry.created.elapsed() >= ttl
                            && entry.last_access.elapsed() < active_window
                        {
                            entry.refreshing = true;
                            let cluster = key.split('\0').next().unwrap_or("both").to_string();
                            Some((key.clone(), cluster, archive))
                        } else {
                            None
                        }
                    })
                    .collect()
            };
            for (key, cluster, archive) in due {
                let config = config.clone();
                let cache = Arc::clone(&cache);
                thread::spawn(move || refresh_cached(config, cache, key, cluster, archive));
            }
        }
    });
}

fn refresh_cached(config: Config, cache: SharedCache, key: String, cluster: String, archive: bool) {
    let result = crate::slurm::all_jobs_direct(&config, &cluster, "all", archive);
    let mut entries = cache.lock().unwrap_or_else(|error| error.into_inner());
    let Some(entry) = entries.get_mut(&key) else {
        return;
    };
    entry.refreshing = false;
    if let Ok((jobs, ledger, warnings)) = result {
        let refreshed_at = Instant::now();
        entry.reply = Arc::new(Reply {
            jobs,
            ledger,
            warnings,
            error: None,
            details: None,
        });
        entry.created = refreshed_at;
        invalidate_older_combined(&mut entries, &cluster, archive, refreshed_at);
    }
}

fn encode_reply(reply: &Reply) -> Result<Vec<u8>> {
    encode_frame(reply)
}

fn write_reply(stream: &mut UnixStream, reply: &Reply) -> Result<()> {
    stream.write_all(&encode_reply(reply)?)?;
    Ok(())
}

fn encode_frame(value: &impl Serialize) -> Result<Vec<u8>> {
    let payload = rmp_serde::to_vec(value)?;
    let length = u32::try_from(payload.len()).context("daemon message too large")?;
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn write_frame(stream: &mut UnixStream, value: &impl Serialize) -> Result<()> {
    stream.write_all(&encode_frame(value)?)?;
    Ok(())
}

fn read_frame<T: for<'de> Deserialize<'de>>(stream: &mut UnixStream) -> Result<T> {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header)?;
    let length = u32::from_le_bytes(header) as usize;
    if length > 64 * 1024 * 1024 {
        bail!("daemon message exceeds 64 MiB limit");
    }
    let mut payload = vec![0_u8; length];
    stream.read_exact(&mut payload)?;
    Ok(rmp_serde::from_slice(&payload)?)
}

fn empty_reply() -> Reply {
    Reply {
        jobs: Vec::new(),
        ledger: Ledger::default(),
        warnings: Vec::new(),
        error: None,
        details: None,
    }
}
