const MAX_LOG_JOBS: usize = 64;
const MAX_LOG_CACHE_BYTES: usize = 64 * 1024 * 1024;
const ACTIVE_LOG_TTL: Duration = Duration::from_secs(5);
const TERMINAL_LOG_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
enum LogRead {
    Metadata,
    Window(usize),
    Range(u64, usize),
}

fn resolve_log_request(
    config: &Config,
    cache: &LogCache,
    cluster: &str,
    id: &str,
    request: LogRead,
) -> Result<LogData> {
    config.cluster(cluster)?;
    if !crate::model::valid_job_id(id) {
        bail!("invalid job ID {id}");
    }
    let key = format!("{cluster}\0{id}");
    if let Some(data) = cached_log(cache, &key, &request) {
        return Ok(data);
    }
    let data = match request {
        LogRead::Metadata => crate::log_service::metadata(config, cluster, id)?,
        LogRead::Window(maximum) => {
            crate::log_service::recent_window(config, cluster, id, maximum)?
        }
        LogRead::Range(start, maximum) => {
            let mut data = crate::log_service::range(config, cluster, id, start, maximum)?;
            apply_cached_generation(cache, &key, &mut data);
            return Ok(data);
        }
    };
    let data = store_log(cache, key, data);
    Ok(if matches!(request, LogRead::Metadata) {
        data.metadata_only()
    } else {
        data
    })
}

fn cached_log(cache: &LogCache, key: &str, request: &LogRead) -> Option<LogData> {
    let mut entries = cache.lock().unwrap_or_else(|error| error.into_inner());
    let entry = entries.get_mut(key)?;
    let ttl = if entry.data.terminal {
        TERMINAL_LOG_TTL
    } else {
        ACTIVE_LOG_TTL
    };
    if entry.created.elapsed() >= ttl {
        return None;
    }
    entry.last_access = Instant::now();
    match request {
        LogRead::Metadata => Some(entry.data.clone().metadata_only()),
        LogRead::Window(maximum)
            if entry.data.status != "available"
                || (entry.data.offset.saturating_add(entry.data.bytes.len() as u64)
                    == entry.data.size
                    && entry.data.bytes.len()
                        >= (*maximum).min(entry.data.size as usize)) =>
        {
            let mut data = entry.data.clone();
            if data.bytes.len() > *maximum {
                let remove = data.bytes.len() - *maximum;
                data.bytes.drain(..remove);
                data.offset = data.offset.saturating_add(remove as u64);
            }
            Some(data)
        }
        LogRead::Range(start, maximum) => {
            let end = start.saturating_add(*maximum as u64).min(entry.data.size);
            let cached_end = entry.data.offset.saturating_add(entry.data.bytes.len() as u64);
            if *start < entry.data.offset || end > cached_end {
                return None;
            }
            let first = (*start - entry.data.offset) as usize;
            let last = (end - entry.data.offset) as usize;
            let mut data = entry.data.clone();
            data.bytes = data.bytes[first..last].to_vec();
            data.offset = *start;
            Some(data)
        }
        _ => None,
    }
}

fn store_log(cache: &LogCache, key: String, mut data: LogData) -> LogData {
    let mut entries = cache.lock().unwrap_or_else(|error| error.into_inner());
    let base_generation = data.generation.clone();
    let generation_epoch = entries.get(&key).map_or(0, |previous| {
        if previous.base_generation == base_generation {
            previous.generation_epoch.saturating_add(u64::from(
                data.status == "available" && data.size < previous.data.size,
            ))
        } else {
            0
        }
    });
    data.generation = effective_generation(&base_generation, generation_epoch);
    let now = Instant::now();
    entries.insert(
        key,
        LogEntry {
            created: now,
            last_access: now,
            base_generation,
            generation_epoch,
            data: data.clone(),
        },
    );
    loop {
        let bytes: usize = entries.values().map(|entry| entry.data.bytes.len()).sum();
        if entries.len() <= MAX_LOG_JOBS && bytes <= MAX_LOG_CACHE_BYTES {
            break;
        }
        let Some(oldest) = entries
            .iter()
            .min_by_key(|(_, entry)| entry.last_access)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        entries.remove(&oldest);
    }
    data
}

fn apply_cached_generation(cache: &LogCache, key: &str, data: &mut LogData) {
    let entries = cache.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(entry) = entries.get(key)
        && entry.base_generation == data.generation
    {
        data.generation.clone_from(&entry.data.generation);
    }
}

fn effective_generation(base: &str, epoch: u64) -> String {
    if epoch == 0 || base.is_empty() {
        return base.into();
    }
    use sha2::Digest as _;
    let mut digest = sha2::Sha256::new();
    digest.update(b"slurm-log-truncation-v1\0");
    digest.update(base.as_bytes());
    digest.update(epoch.to_le_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod log_cache_tests {
    use super::*;

    #[test]
    fn cached_ranges_are_generation_preserving_and_bounded() {
        let cache: LogCache = Arc::new(Mutex::new(HashMap::new()));
        let _ = store_log(
            &cache,
            "one\u{0}123".into(),
            LogData {
                status: "available".into(),
                generation: "generation".into(),
                size: 6,
                offset: 0,
                bytes: b"abcdef".to_vec(),
                ..LogData::default()
            },
        );
        let value = cached_log(&cache, "one\u{0}123", &LogRead::Range(2, 3)).unwrap();
        assert_eq!(value.bytes, b"cde");
        assert_eq!(value.offset, 2);
        assert_eq!(value.generation, "generation");
    }

    #[test]
    fn observed_truncation_advances_generation_until_rotation() {
        let cache: LogCache = Arc::new(Mutex::new(HashMap::new()));
        let base = "a".repeat(64);
        let first = store_log(
            &cache,
            "alpha\u{0}123".into(),
            LogData {
                status: "available".into(),
                generation: base.clone(),
                size: 10,
                ..LogData::default()
            },
        );
        let truncated = store_log(
            &cache,
            "alpha\u{0}123".into(),
            LogData {
                status: "available".into(),
                generation: base.clone(),
                size: 2,
                ..LogData::default()
            },
        );
        let appended = store_log(
            &cache,
            "alpha\u{0}123".into(),
            LogData {
                status: "available".into(),
                generation: base,
                size: 4,
                ..LogData::default()
            },
        );
        assert_ne!(first.generation, truncated.generation);
        assert_eq!(truncated.generation, appended.generation);
        assert_eq!(truncated.generation.len(), 64);
    }

    #[test]
    fn job_count_limit_evicts_the_least_recent_entry() {
        let cache: LogCache = Arc::new(Mutex::new(HashMap::new()));
        for index in 0..=MAX_LOG_JOBS {
            let _ = store_log(
                &cache,
                format!("alpha\u{0}{index}"),
                LogData {
                    status: "available".into(),
                    generation: format!("{index:064x}"),
                    size: 1,
                    bytes: vec![b'x'],
                    ..LogData::default()
                },
            );
        }
        let entries = cache.lock().unwrap();
        assert_eq!(entries.len(), MAX_LOG_JOBS);
        assert!(!entries.contains_key("alpha\u{0}0"));
    }
}
