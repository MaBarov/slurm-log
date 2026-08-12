use anyhow::{Result, bail};
use fs2::FileExt;
use std::{
    collections::HashSet,
    fs,
    fs::OpenOptions,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime},
};
use time::{
    Duration as TimeDuration, OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339,
};

use crate::{
    command::{shell_quote, ssh, text},
    config::Config,
    model::{Job, valid_job_id},
    state::Ledger,
};

// These are scheduler RPC budgets, not UI frame rates. Per-user daemons cannot
// coalesce across Unix users, so conservative TTLs keep aggregate cluster load
// bounded when the tool is widely installed.
const QUEUE_CACHE_TTL: Duration = Duration::from_secs(15);
const RECENT_CACHE_TTL: Duration = Duration::from_secs(60);
const ARCHIVE_CACHE_TTL: Duration = Duration::from_secs(60);
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_ARCHIVE_DAYS: i64 = 365;

pub fn validate_query(cluster: &str, filter: &str) -> Result<()> {
    if cluster != "all"
        && cluster != "both"
        && (cluster.is_empty()
            || !cluster
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)))
    {
        bail!("invalid cluster selector {cluster}");
    }
    if !["all", "running", "failed", "blocked"].contains(&filter) {
        bail!("invalid job filter {filter}");
    }
    Ok(())
}

fn cache_path(config: &Config, name: &str) -> PathBuf {
    config
        .state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{name}-cache.json"))
}

fn queue_cache_name(cluster: &str) -> String {
    // The queue payload gained the interactive marker in v2. A versioned
    // namespace prevents long-lived panes from an older binary from
    // continually overwriting the new daemon's cache during rolling updates.
    format!("queue-v2-{cluster}")
}

fn cached_jobs(path: &Path, ttl: Duration) -> Option<Vec<Job>> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.len() > MAX_CACHE_BYTES {
        return None;
    }
    let modified = metadata.modified().ok()?;
    if SystemTime::now().duration_since(modified).ok()? > ttl {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn query_lock(path: &Path) -> Result<fs::File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("query.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    file.lock_exclusive()?;
    Ok(file)
}

fn store_jobs(path: &Path, jobs: &[Job]) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    if OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .and_then(|file| serde_json::to_writer(file, jobs).map_err(std::io::Error::other))
        .is_ok()
    {
        let _ = fs::rename(temporary, path);
    } else {
        let _ = fs::remove_file(temporary);
    }
}

pub fn invalidate_caches(config: &Config) {
    // Remove pre-configurable-cluster cache names during rolling upgrades.
    for legacy in ["queue-sprint", "queue-cispa", "recent", "archive"] {
        let _ = fs::remove_file(cache_path(config, legacy));
    }
    for cluster in &config.clusters {
        for prefix in ["queue", "queue-v2", "recent", "archive"] {
            let _ = fs::remove_file(cache_path(config, &format!("{prefix}-{}", cluster.name)));
        }
        let _ = fs::remove_file(cache_path(
            config,
            &format!("archive-{}-{}d", cluster.name, archive_horizon_days()),
        ));
    }
}

pub(crate) fn scheduler_text(
    config: &Config,
    cluster: &str,
    program: &str,
    args: &[&str],
) -> Result<String> {
    let target = config.cluster(cluster)?;
    if target.remote() {
        let command = std::iter::once(shell_quote(program))
            .chain(args.iter().map(|argument| shell_quote(argument)))
            .collect::<Vec<_>>()
            .join(" ");
        ssh(&target.ssh_host, &command)
    } else {
        text(program, args)
    }
}

pub fn parse_queue(input: &str, cluster: &str) -> Vec<Job> {
    input
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(9, '|').map(str::trim);
            let id = fields.next()?;
            let state = fields.next()?;
            let name = fields.next()?;
            let elapsed = fields.next()?;
            if !valid_job_id(id) {
                return None;
            }
            let reason = fields.next().unwrap_or("");
            let partition = fields.next().unwrap_or("");
            let start_time = fields.next().unwrap_or("");
            let priority = fields.next().unwrap_or("");
            let command = fields.next().unwrap_or("");
            Some(Job {
                cluster: cluster.into(),
                id: id.into(),
                state: state.into(),
                name: name.into(),
                elapsed: elapsed.into(),
                reason: reason.into(),
                ended: String::new(),
                partition: partition.into(),
                start_time: start_time.into(),
                priority: priority.into(),
                interactive: interactive_command(command),
                ..Job::default()
            })
        })
        .collect()
}

fn interactive_command(command: &str) -> bool {
    matches!(
        command.rsplit('/').next().unwrap_or(command),
        "bash" | "sh" | "zsh" | "fish" | "csh" | "tcsh" | "nu"
    )
}

pub fn parse_recent(input: &str, cluster: &str) -> Vec<Job> {
    input
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(9, '|').map(str::trim);
            let id = fields.next()?;
            let state = fields.next()?;
            let name = fields.next()?;
            let elapsed = fields.next()?;
            let ended = fields.next()?;
            if !valid_job_id(id) {
                return None;
            }
            Some(Job {
                cluster: cluster.into(),
                id: id.into(),
                state: state.into(),
                name: name.into(),
                elapsed: elapsed.into(),
                reason: String::new(),
                ended: ended.into(),
                exit_code: fields.next().unwrap_or("").into(),
                max_rss: fields.next().unwrap_or("").into(),
                alloc_tres: fields.next().unwrap_or("").into(),
                partition: fields.next().unwrap_or("").into(),
                ..Job::default()
            })
        })
        .collect()
}

pub fn queued(config: &Config, cluster: &str) -> Result<Vec<Job>> {
    let cache = cache_path(config, &queue_cache_name(cluster));
    if let Some(jobs) = cached_jobs(&cache, QUEUE_CACHE_TTL) {
        return Ok(jobs);
    }
    // Followers, the monitor, picker, and daemon are separate processes. Lock
    // and recheck so only one of them performs the scheduler RPC at expiry.
    let _lock = query_lock(&cache)?;
    if let Some(jobs) = cached_jobs(&cache, QUEUE_CACHE_TTL) {
        return Ok(jobs);
    }
    let format = "%i|%T|%j|%M|%R|%P|%S|%Q|%o";
    let owner = &config.cluster(cluster)?.user;
    let value = scheduler_text(
        config,
        cluster,
        "squeue",
        &["-h", "-u", owner, "-o", format],
    )?;
    let jobs = parse_queue(&value, cluster);
    store_jobs(&cache, &jobs);
    Ok(jobs)
}

pub fn recent(config: &Config, cluster: &str, archive: bool) -> Result<Vec<Job>> {
    if !config.cluster(cluster)?.accounting {
        return Ok(Vec::new());
    }
    let archive_days = archive.then(archive_horizon_days);
    let cache_name = archive_days.map_or_else(
        || format!("recent-{cluster}"),
        |days| format!("archive-{cluster}-{days}d"),
    );
    let cache = cache_path(config, &cache_name);
    let ttl = if archive {
        ARCHIVE_CACHE_TTL
    } else {
        RECENT_CACHE_TTL
    };
    if let Some(jobs) = cached_jobs(&cache, ttl) {
        return Ok(jobs);
    }
    let _lock = query_lock(&cache)?;
    if let Some(jobs) = cached_jobs(&cache, ttl) {
        return Ok(jobs);
    }
    let start = archive_days.map_or_else(|| "now-1hour".into(), archive_start);
    let command = format!(
        "sacct -X -S {start} -u {} -n -P --format=JobID,State,JobName,Elapsed,End,ExitCode,MaxRSS,AllocTRES,Partition 2>/dev/null",
        shell_quote(&config.cluster(cluster)?.user)
    );
    let jobs = parse_recent(
        &scheduler_text(config, cluster, "sh", &["-c", &command])?,
        cluster,
    );
    store_jobs(&cache, &jobs);
    Ok(jobs)
}

fn archive_horizon_days() -> i64 {
    validated_archive_days(std::env::var("SLURM_LOG_ARCHIVE_DAYS").ok().as_deref())
}

fn validated_archive_days(value: Option<&str>) -> i64 {
    value
        .and_then(|value| value.parse().ok())
        .filter(|days| (1..=3650).contains(days))
        .unwrap_or(DEFAULT_ARCHIVE_DAYS)
}

fn archive_start(days: i64) -> String {
    archive_start_at(OffsetDateTime::now_utc(), days)
}

fn archive_start_at(now: OffsetDateTime, days: i64) -> String {
    let date = (now - TimeDuration::days(days)).date();
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

include!("slurm/aggregate.rs");
include!("slurm/log_path.rs");

#[cfg(test)]
#[path = "slurm/tests/aggregate.rs"]
mod aggregate_tests;
#[cfg(test)]
mod tests;
