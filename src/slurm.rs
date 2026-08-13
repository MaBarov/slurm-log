use anyhow::{Context, Result, bail};
use fs2::FileExt;
use std::{
    collections::HashSet,
    fs,
    fs::OpenOptions,
    io::{BufWriter, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime},
};
use time::{
    Duration as TimeDuration, OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339,
};

use crate::{
    command::{remote_scheduler_command, shell_quote, ssh, text},
    config::Config,
    model::{Job, terminal_text, valid_job_id},
    state::Ledger,
};

// These are scheduler RPC budgets, not UI frame rates. Per-user daemons cannot
// coalesce across Unix users, so conservative TTLs keep aggregate cluster load
// bounded when the tool is widely installed.
const QUEUE_CACHE_TTL: Duration = Duration::from_secs(15);
const RECENT_CACHE_TTL: Duration = Duration::from_secs(60);
const ARCHIVE_CACHE_TTL: Duration = Duration::from_secs(60);
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CACHE_JOBS: usize = 1_000_000;
const MAX_INITIAL_JOBS: usize = 100_000;
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
        .join(format!("{name}-cache.msgpack"))
}

fn legacy_cache_path(config: &Config, name: &str) -> PathBuf {
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
    format!("queue-v3-{cluster}")
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
    let bytes = fs::read(path).ok()?;
    (msgpack_sequence_len(&bytes)? <= MAX_CACHE_JOBS).then(|| {
        rmp_serde::from_slice::<BoundedJobs>(&bytes)
            .ok()
            .map(|jobs| jobs.0)
    })?
}

struct BoundedJobs(Vec<Job>);

impl<'de> serde::Deserialize<'de> for BoundedJobs {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct JobsVisitor;
        impl<'de> serde::de::Visitor<'de> for JobsVisitor {
            type Value = BoundedJobs;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a bounded job sequence")
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let initial = sequence.size_hint().unwrap_or(0).min(MAX_INITIAL_JOBS);
                let mut jobs = Vec::with_capacity(initial);
                while let Some(job) = sequence.next_element()? {
                    if jobs.len() == MAX_CACHE_JOBS {
                        return Err(serde::de::Error::custom("job cache exceeds item limit"));
                    }
                    jobs.push(job);
                }
                Ok(BoundedJobs(jobs))
            }
        }
        deserializer.deserialize_seq(JobsVisitor)
    }
}

fn msgpack_sequence_len(bytes: &[u8]) -> Option<usize> {
    match *bytes.first()? {
        marker @ 0x90..=0x9f => Some(usize::from(marker & 0x0f)),
        0xdc => Some(u16::from_be_bytes(bytes.get(1..3)?.try_into().ok()?) as usize),
        0xdd => Some(u32::from_be_bytes(bytes.get(1..5)?.try_into().ok()?) as usize),
        _ => None,
    }
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
        .and_then(|file| {
            let mut writer = BufWriter::with_capacity(256 * 1024, file);
            rmp_serde::encode::write(&mut writer, jobs).map_err(std::io::Error::other)?;
            writer.flush()
        })
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
        let _ = fs::remove_file(legacy_cache_path(config, legacy));
    }
    for cluster in &config.clusters {
        for prefix in [
            "queue",
            "queue-v2",
            "queue-v3",
            "recent",
            "recent-v2",
            "archive",
            "archive-v2",
        ] {
            let name = format!("{prefix}-{}", cluster.name);
            let _ = fs::remove_file(cache_path(config, &name));
            let _ = fs::remove_file(legacy_cache_path(config, &name));
        }
        let name = format!("archive-{}-{}d", cluster.name, archive_horizon_days());
        let _ = fs::remove_file(cache_path(config, &name));
        let _ = fs::remove_file(legacy_cache_path(config, &name));
    }
}

include!("slurm/listing_auth.rs");

include!("slurm/controller.rs");

pub fn parse_queue(input: &str, cluster: &str) -> Vec<Job> {
    let mut jobs = Vec::with_capacity(line_count(input).min(MAX_INITIAL_JOBS));
    for line in input.lines() {
        if jobs.len() == MAX_CACHE_JOBS {
            break;
        }
        let fields: Vec<_> = line.split('|').map(str::trim).collect();
        if !(4..=9).contains(&fields.len()) {
            continue;
        }
        let mut fields = fields.into_iter();
        let Some((id, state, name, elapsed)) = fields
            .next()
            .zip(fields.next())
            .zip(fields.next())
            .zip(fields.next())
            .map(|(((id, state), name), elapsed)| (id, state, name, elapsed))
        else {
            continue;
        };
        if !valid_job_id(id) {
            continue;
        }
        let reason = fields.next().unwrap_or("");
        let partition = fields.next().unwrap_or("");
        let start_time = fields.next().unwrap_or("");
        let priority = fields.next().unwrap_or("");
        let command = fields.next().unwrap_or("");
        jobs.push(Job {
            cluster: cluster.into(),
            id: id.into(),
            state: terminal_text(state),
            name: terminal_text(name),
            elapsed: terminal_text(elapsed),
            reason: terminal_text(reason),
            ended: String::new(),
            partition: terminal_text(partition),
            start_time: terminal_text(start_time),
            priority: terminal_text(priority),
            interactive: interactive_command(command),
            ..Job::default()
        });
    }
    jobs
}

fn interactive_command(command: &str) -> bool {
    matches!(
        command.rsplit('/').next().unwrap_or(command),
        "bash" | "sh" | "zsh" | "fish" | "csh" | "tcsh" | "nu"
    )
}

pub fn parse_recent(input: &str, cluster: &str) -> Vec<Job> {
    let mut jobs = Vec::with_capacity(line_count(input).min(MAX_INITIAL_JOBS));
    for line in input.lines() {
        if jobs.len() == MAX_CACHE_JOBS {
            break;
        }
        let fields: Vec<_> = line.split('|').map(str::trim).collect();
        if !(5..=9).contains(&fields.len()) {
            continue;
        }
        let mut fields = fields.into_iter();
        let Some((id, state, name, elapsed, ended)) = fields
            .next()
            .zip(fields.next())
            .zip(fields.next())
            .zip(fields.next())
            .zip(fields.next())
            .map(|((((id, state), name), elapsed), ended)| (id, state, name, elapsed, ended))
        else {
            continue;
        };
        if !valid_job_id(id) {
            continue;
        }
        jobs.push(Job {
            cluster: cluster.into(),
            id: id.into(),
            state: terminal_text(state),
            name: terminal_text(name),
            elapsed: terminal_text(elapsed),
            reason: String::new(),
            ended: terminal_text(ended),
            exit_code: terminal_text(fields.next().unwrap_or("")),
            max_rss: terminal_text(fields.next().unwrap_or("")),
            alloc_tres: terminal_text(fields.next().unwrap_or("")),
            partition: terminal_text(fields.next().unwrap_or("")),
            ..Job::default()
        });
    }
    jobs
}

fn line_count(input: &str) -> usize {
    input
        .as_bytes()
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count()
        + usize::from(!input.is_empty() && !input.ends_with('\n'))
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
    let jobs = query_queued(config, cluster, None)?;
    store_jobs(&cache, &jobs);
    Ok(jobs)
}

include!("slurm/cancellation.rs");
fn query_queued(config: &Config, cluster: &str, id: Option<&str>) -> Result<Vec<Job>> {
    let format = "%i|%T|%j|%M|%R|%P|%S|%Q|%o|%u";
    let target = config.cluster(cluster)?;
    let owner = &target.user;
    let mut args = vec!["-h", "-u", owner.as_str()];
    if let Some(id) = id {
        args.extend(["-j", id]);
    }
    args.extend(["-o", format]);
    let value = scheduler_text(config, cluster, "squeue", &args)?;
    Ok(parse_owned_queue(&value, cluster, owner))
}

pub fn recent(config: &Config, cluster: &str, archive: bool) -> Result<Vec<Job>> {
    if !config.cluster(cluster)?.accounting {
        return Ok(Vec::new());
    }
    let archive_days = archive.then(archive_horizon_days);
    let cache_name = archive_days.map_or_else(
        || format!("recent-v2-{cluster}"),
        |days| format!("archive-v2-{cluster}-{days}d"),
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
    let target = config.cluster(cluster)?;
    let command = format!(
        "sacct {} -X -S {start} -u {} -n -P --format=JobID,State,JobName,Elapsed,End,ExitCode,MaxRSS,AllocTRES,Partition,User,Cluster 2>/dev/null",
        controller_option(config, cluster)?,
        shell_quote(&target.user)
    );
    let jobs = parse_owned_recent(
        &scheduler_text(config, cluster, "sh", &["-c", &command])?,
        cluster,
        &target.user,
        target.controller.as_deref(),
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
include!("slurm/authorization.rs");
include!("slurm/log_path.rs");

#[cfg(test)]
#[path = "slurm/tests/aggregate.rs"]
mod aggregate_tests;
#[cfg(test)]
mod tests;
