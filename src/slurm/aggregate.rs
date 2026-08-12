pub fn all_jobs(
    config: &Config,
    cluster: &str,
    filter: &str,
    archive: bool,
) -> Result<(Vec<Job>, Ledger, Vec<String>)> {
    crate::daemon::query(config, cluster, filter, archive, false)
        .or_else(|_| all_jobs_direct(config, cluster, filter, archive))
}
pub fn all_jobs_fresh(
    config: &Config,
    cluster: &str,
    filter: &str,
    archive: bool,
) -> Result<(Vec<Job>, Ledger, Vec<String>)> {
    crate::daemon::query(config, cluster, filter, archive, true).or_else(|_| {
        invalidate_caches(config);
        all_jobs_direct(config, cluster, filter, archive)
    })
}

pub fn all_jobs_direct(
    config: &Config,
    cluster: &str,
    filter: &str,
    archive: bool,
) -> Result<(Vec<Job>, Ledger, Vec<String>)> {
    validate_query(cluster, filter)?;
    let clusters: Vec<_> = config
        .selected_clusters(cluster)?
        .into_iter()
        .map(|item| item.name.as_str())
        .collect();
    let mut jobs = Vec::new();
    let mut seen = HashSet::new();
    let mut warnings = accounting_warnings(config, &clusters, archive);
    let mut complete = HashSet::new();
    let cached_queues: Option<Vec<Vec<Job>>> = clusters
        .iter()
        .map(|item| {
            cached_jobs(
                &cache_path(config, &queue_cache_name(item)),
                QUEUE_CACHE_TTL,
            )
        })
        .collect();
    let cached_accounting: Option<Vec<Vec<Job>>> = clusters
        .iter()
        .map(|item| {
            if config
                .cluster(item)
                .is_ok_and(|cluster| !cluster.accounting)
            {
                Some(Vec::new())
            } else {
                cached_jobs(
                    &cache_path(
                        config,
                        &if archive {
                            format!("archive-{item}-{}d", archive_horizon_days())
                        } else {
                            format!("recent-{item}")
                        },
                    ),
                    if archive {
                        ARCHIVE_CACHE_TTL
                    } else {
                        RECENT_CACHE_TTL
                    },
                )
            }
        })
        .collect();
    if let (Some(queues), Some(accounting)) = (cached_queues, cached_accounting) {
        extend_unique(&mut jobs, &mut seen, queues.into_iter().flatten());
        extend_unique(&mut jobs, &mut seen, accounting.into_iter().flatten());
        if archive {
            complete.extend(
                clusters
                    .iter()
                    .filter(|cluster| config.cluster(cluster).is_ok_and(|item| item.accounting))
                    .map(|cluster| (*cluster).to_string()),
            );
        }
    } else {
        // On a cache miss, queue and accounting queries are independent and
        // run concurrently to hide local/remote scheduler latency.
        thread::scope(|scope| {
            let queue_queries: Vec<_> = clusters
                .iter()
                .map(|&item| (item, scope.spawn(move || queued(config, item))))
                .collect();
            let recent_queries: Vec<_> = clusters
                .iter()
                .map(|&item| (item, scope.spawn(move || recent(config, item, archive))))
                .collect();

            for (item, query) in queue_queries {
                match query
                    .join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("lookup worker panicked")))
                {
                    Ok(found) => extend_unique(&mut jobs, &mut seen, found),
                    Err(error) => warnings.push(format!("{item}: {error:#}")),
                }
            }
            for (item, query) in recent_queries {
                match query
                    .join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("lookup worker panicked")))
                {
                    Ok(found) => {
                        extend_unique(&mut jobs, &mut seen, found);
                        if archive && config.cluster(item).is_ok_and(|cluster| cluster.accounting) {
                            complete.insert(item.to_string());
                        }
                    }
                    Err(error) => warnings.push(format!("{item} accounting: {error:#}")),
                }
            }
        });
    }
    let ledger = Ledger::sync(&config.state_path, &jobs, &complete)?;
    restore_interactive_classification(&mut jobs, &ledger);
    jobs.retain(|job| match filter {
        "running" => job.running(),
        "failed" => job.failed(),
        "blocked" => job.blocked_category(),
        _ => true,
    });
    jobs.sort_by_cached_key(|job| {
        std::cmp::Reverse(
            job.id
                .split('_')
                .next()
                .unwrap_or("0")
                .parse::<u64>()
                .unwrap_or(0),
        )
    });
    Ok((jobs, ledger, warnings))
}

fn accounting_warnings(config: &Config, clusters: &[&str], archive: bool) -> Vec<String> {
    if !archive {
        return Vec::new();
    }
    clusters
        .iter()
        .filter_map(|name| {
            config
                .cluster(name)
                .ok()
                .filter(|cluster| !cluster.accounting)
                .map(|_| {
                    format!(
                        "{name}: completed jobs unavailable because sacct/accounting is disabled; only active squeue jobs can be listed"
                    )
                })
        })
        .collect()
}

fn extend_unique(
    jobs: &mut Vec<Job>,
    seen: &mut HashSet<String>,
    found: impl IntoIterator<Item = Job>,
) {
    jobs.extend(found.into_iter().filter(|job| seen.insert(job.key())));
}

fn restore_interactive_classification(jobs: &mut [Job], ledger: &Ledger) {
    let mut key = String::new();
    for job in jobs {
        job.write_key(&mut key);
        if ledger.interactive_jobs.contains_key(&key) {
            job.interactive = true;
        }
    }
}

#[cfg(test)]
pub fn recently_ended(job: &Job, seconds: i64) -> bool {
    recently_ended_at(
        job,
        seconds,
        OffsetDateTime::now_utc(),
        UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC),
    )
}

fn recently_ended_at(
    job: &Job,
    seconds: i64,
    now: OffsetDateTime,
    local_offset: UtcOffset,
) -> bool {
    if job.ended.is_empty() || job.ended == "Unknown" || job.ended == "None" {
        return false;
    }
    let parsed = OffsetDateTime::parse(&job.ended, &Rfc3339).or_else(|_| {
        let seconds = local_offset.whole_seconds();
        let sign = if seconds < 0 { '-' } else { '+' };
        let minutes = seconds.unsigned_abs() / 60;
        OffsetDateTime::parse(
            &format!(
                "{}{sign}{:02}:{:02}",
                job.ended,
                minutes / 60,
                minutes % 60
            ),
            &Rfc3339,
        )
    });
    parsed.is_ok_and(|ended| {
        let age = (now - ended).whole_seconds();
        (0..=seconds).contains(&age)
    })
}

pub fn visible_jobs(
    jobs: Vec<Job>,
    ledger: &Ledger,
    history_mode: u8,
    show_blocked: bool,
) -> Vec<Job> {
    let mut key = String::new();
    let now = OffsetDateTime::now_utc();
    let local_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    jobs.into_iter()
        .filter(|job| {
            if job.blocked_category() && !show_blocked {
                return false;
            }
            if history_mode == 2 {
                return true;
            }
            job.write_key(&mut key);
            if ledger.dismissed.contains_key(&key) {
                return false;
            }
            if job.active() || !ledger.opened.contains_key(&key) {
                return true;
            }
            let horizon = if history_mode == 1 { 20 * 60 } else { 2 * 60 };
            recently_ended_at(job, horizon, now, local_offset)
        })
        .collect()
}
