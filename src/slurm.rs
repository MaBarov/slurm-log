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
                        &format!("{}-{item}", if archive { "archive" } else { "recent" }),
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
    for job in jobs {
        if ledger.interactive_jobs.contains_key(&job.key()) {
            job.interactive = true;
        }
    }
}

pub fn recently_ended(job: &Job, seconds: i64) -> bool {
    if job.ended.is_empty() || job.ended == "Unknown" || job.ended == "None" {
        return false;
    }
    let parsed = OffsetDateTime::parse(&job.ended, &Rfc3339).or_else(|_| {
        let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
        OffsetDateTime::parse(&format!("{}{}", job.ended, offset), &Rfc3339)
    });
    parsed.is_ok_and(|ended| {
        let age = (OffsetDateTime::now_utc() - ended).whole_seconds();
        (0..=seconds).contains(&age)
    })
}

pub fn visible_jobs(
    jobs: Vec<Job>,
    ledger: &Ledger,
    history_mode: u8,
    show_blocked: bool,
) -> Vec<Job> {
    jobs.into_iter()
        .filter(|job| {
            if job.blocked_category() && !show_blocked {
                return false;
            }
            let key = job.key();
            if ledger.dismissed.contains_key(&key) && history_mode != 2 {
                return false;
            }
            let history = history_mode == 2
                || history_mode == 1 && recently_ended(job, 20 * 60)
                || history_mode == 0 && recently_ended(job, 2 * 60);
            job.active() || !ledger.opened.contains_key(&key) || history
        })
        .collect()
}

pub fn terminal_path(config: &Config, cluster: &str, id: &str) -> Result<(Option<String>, String)> {
    if !valid_job_id(id) {
        bail!("invalid job ID {id}");
    }
    let (raw, logical, name, template) = if let Ok(value) =
        scheduler_text(config, cluster, "scontrol", &["show", "job", id])
    {
        let name = token(&value, "JobName=").unwrap_or("job").to_string();
        let path = usable_stdout(token(&value, "StdOut="))
            .unwrap_or_default()
            .to_string();
        (id.to_string(), id.to_string(), name, path)
    } else {
        if !config.cluster(cluster)?.accounting {
            bail!("job {cluster}:{id} is no longer active and accounting is unavailable");
        }
        let command = format!(
            "sacct -X -j {} --format=JobIDRaw,JobID,JobName,StdOut -n -P 2>/dev/null | awk 'NF {{print; exit}}'",
            shell_quote(id)
        );
        let value = scheduler_text(config, cluster, "sh", &["-c", &command])?;
        let fields: Vec<_> = value.trim().splitn(4, '|').collect();
        if fields.len() != 4 {
            bail!("no stdout for {cluster} job {id}");
        }
        (
            fields[0].into(),
            fields[1].into(),
            fields[2].into(),
            fields[3].into(),
        )
    };
    let logical = logical.split('.').next().unwrap_or(&logical);
    let (master, task) = logical.split_once('_').unwrap_or((logical, "4294967294"));
    if usable_stdout(Some(&template)).is_none() {
        return Ok((None, name));
    }
    Ok((
        Some(expand_path(
            &template,
            &name,
            raw.split('.').next().unwrap_or(&raw),
            master,
            task,
        )),
        name,
    ))
}

fn usable_stdout(value: Option<&str>) -> Option<&str> {
    let value = value?.trim();
    if value.is_empty()
        || value.eq_ignore_ascii_case("none")
        || value == "(null)"
        || value == "/dev/null"
    {
        None
    } else {
        Some(value)
    }
}

pub fn final_details(config: &Config, job: &Job) -> Job {
    if config
        .cluster(&job.cluster)
        .is_ok_and(|cluster| cluster.accounting)
    {
        let command = format!(
            "sacct -X -j {} -n -P --format=JobID,State,JobName,Elapsed,End,ExitCode,MaxRSS,AllocTRES,Partition 2>/dev/null | awk 'NF {{print; exit}}'",
            shell_quote(&job.id)
        );
        if let Ok(value) = scheduler_text(config, &job.cluster, "sh", &["-c", &command])
            && let Some(found) = parse_recent(&value, &job.cluster).into_iter().next()
        {
            return found;
        }
    }
    let mut details = job.clone();
    if let Ok(value) = scheduler_text(config, &job.cluster, "scontrol", &["show", "job", &job.id]) {
        details.state = token(&value, "JobState=").unwrap_or(&details.state).into();
        details.exit_code = token(&value, "ExitCode=").unwrap_or("").into();
        details.partition = token(&value, "Partition=").unwrap_or("").into();
        details.reason = token(&value, "Reason=").unwrap_or(&details.reason).into();
    }
    details
}

fn token<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.split_whitespace()
        .find_map(|part| part.strip_prefix(prefix))
}

fn expand_path(template: &str, name: &str, raw: &str, master: &str, task: &str) -> String {
    let mut out = String::new();
    let mut chars = template.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('x') => out.push_str(name),
            Some('j') => out.push_str(raw),
            Some('A') => out.push_str(master),
            Some('a') => out.push_str(task),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_allocations_have_no_usable_stdout() {
        for missing in [
            None,
            Some(""),
            Some("  "),
            Some("None"),
            Some("(null)"),
            Some("/dev/null"),
        ] {
            assert_eq!(usable_stdout(missing), None);
        }
        assert_eq!(
            usable_stdout(Some("/logs/slurm-42.out")),
            Some("/logs/slurm-42.out")
        );
    }
    use std::{os::unix::fs::PermissionsExt, path::PathBuf};
    #[test]
    fn arrays_expand_correctly() {
        assert_eq!(
            expand_path("/log/%x_%j_%A_%a.log", "train", "3202710", "3202690", "1"),
            "/log/train_3202710_3202690_1.log"
        );
    }
    #[test]
    fn queue_parser_rejects_steps() {
        let jobs = parse_queue(
            "1|RUNNING|ok|0:01|node\n1.batch|RUNNING|step|0:01|node\n",
            "cispa",
        );
        assert_eq!(jobs.len(), 1);
    }

    #[test]
    fn queue_parser_classifies_shell_allocations_as_interactive() {
        let jobs = parse_queue(
            "41|RUNNING|batch|0:01|node|gpu|now|1|/work/train.sbatch\n42|RUNNING|named-shell|0:02|node|gpu|now|2|bash\n",
            "cispa",
        );
        assert!(!jobs[0].interactive);
        assert!(jobs[1].interactive);
        assert!(jobs[1].blocked_category());
        for command in ["/bin/zsh", "fish", "tcsh", "nu"] {
            assert!(interactive_command(command));
        }
        assert!(!interactive_command("python"));
        assert_eq!(queue_cache_name("cispa"), "queue-v2-cispa");
        assert_ne!(queue_cache_name("cispa"), "queue-cispa");

        let mut accounting_row = vec![Job {
            cluster: "cispa".into(),
            id: "42".into(),
            state: "COMPLETED".into(),
            ..Job::default()
        }];
        let mut ledger = Ledger::default();
        ledger
            .interactive_jobs
            .insert("cispa:42".into(), "now".into());
        restore_interactive_classification(&mut accounting_row, &ledger);
        assert!(accounting_row[0].interactive);
        assert!(accounting_row[0].blocked_category());
    }

    #[test]
    fn deduplication_is_single_pass_and_cluster_scoped() {
        let mut jobs = Vec::new();
        let mut seen = HashSet::new();
        extend_unique(
            &mut jobs,
            &mut seen,
            [
                Job {
                    cluster: "sprint".into(),
                    id: "7".into(),
                    ..Job::default()
                },
                Job {
                    cluster: "sprint".into(),
                    id: "7".into(),
                    ..Job::default()
                },
                Job {
                    cluster: "cispa".into(),
                    id: "7".into(),
                    ..Job::default()
                },
            ],
        );
        assert_eq!(jobs.len(), 2);
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn archive_horizon_is_bounded_and_date_based() {
        assert_eq!(validated_archive_days(None), 365);
        assert_eq!(validated_archive_days(Some("30")), 30);
        assert_eq!(validated_archive_days(Some("0")), 365);
        assert_eq!(validated_archive_days(Some("999999")), 365);
        let epoch = OffsetDateTime::from_unix_timestamp(0).unwrap();
        assert_eq!(archive_start_at(epoch, 365), "1969-01-01");
    }

    #[test]
    #[ignore = "release-mode performance budget"]
    fn parses_one_hundred_thousand_accounting_rows_within_budget() {
        let mut input = String::with_capacity(14 * 1024 * 1024);
        for id in 1..=100_000 {
            use std::fmt::Write as _;
            writeln!(
                input,
                "{id}|COMPLETED|training|01:02:03|2026-08-11T17:00:00+02:00|0:0|4G|cpu=8,mem=16G|gpu"
            )
            .unwrap();
        }
        let started = std::time::Instant::now();
        let jobs = parse_recent(&input, "cispa");
        assert_eq!(jobs.len(), 100_000);
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn extended_scheduler_fields_are_preserved() {
        let queued = parse_queue(
            "7|PENDING|train|0:00|Resources|gpu|2026-08-11T18:00:00|991\n",
            "cispa",
        );
        assert_eq!(queued[0].partition, "gpu");
        assert_eq!(queued[0].priority, "991");
        assert!(queued[0].insight().contains("estimated start"));

        let recent = parse_recent(
            "8|OUT_OF_MEMORY|train|1:00|2026-08-11T17:00:00+02:00|0:9|63G|gres/gpu=4|gpu\n",
            "cispa",
        );
        assert_eq!(recent[0].exit_code, "0:9");
        assert_eq!(recent[0].alloc_tres, "gres/gpu=4");
        assert_eq!(recent[0].insight(), "exit 0:9 · peak memory 63G");
    }

    #[test]
    fn visibility_matches_live_archive_and_dismiss_rules() {
        let running = Job {
            cluster: "cispa".into(),
            id: "1".into(),
            state: "RUNNING".into(),
            ..Job::default()
        };
        let failed = Job {
            cluster: "cispa".into(),
            id: "2".into(),
            state: "FAILED".into(),
            ..Job::default()
        };
        let mut ledger = Ledger::default();
        ledger.opened.insert(failed.key(), "now".into());
        assert_eq!(
            visible_jobs(vec![running.clone(), failed.clone()], &ledger, 0, false),
            vec![running.clone()]
        );
        assert_eq!(
            visible_jobs(vec![failed.clone()], &ledger, 2, false),
            vec![failed.clone()]
        );
        ledger.dismissed.insert(failed.key(), "now".into());
        assert!(visible_jobs(vec![failed.clone()], &ledger, 0, false).is_empty());
        assert_eq!(
            visible_jobs(vec![failed.clone()], &ledger, 2, false),
            vec![failed]
        );
        ledger.dismissed.insert(running.key(), "now".into());
        assert!(visible_jobs(vec![running.clone()], &ledger, 0, false).is_empty());
        assert_eq!(
            visible_jobs(vec![running.clone()], &ledger, 2, false),
            vec![running]
        );
    }

    #[test]
    fn scheduler_query_lock_is_private_and_cross_process_exclusive() {
        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path().join("queue-cache.json");
        let first = query_lock(&cache).unwrap();
        let lock_path = cache.with_extension("query.lock");
        assert_eq!(
            fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        assert!(second.try_lock_exclusive().is_err());
        drop(first);
        assert!(second.try_lock_exclusive().is_ok());
    }

    #[test]
    fn blocked_jobs_are_hidden_until_requested() {
        let blocked = Job {
            cluster: "sprint".into(),
            id: "3".into(),
            state: "PENDING".into(),
            reason: "DependencyNeverSatisfied".into(),
            ..Job::default()
        };
        assert!(visible_jobs(vec![blocked.clone()], &Ledger::default(), 0, false).is_empty());
        assert_eq!(
            visible_jobs(vec![blocked.clone()], &Ledger::default(), 0, true),
            vec![blocked]
        );
        let interactive = Job {
            cluster: "cispa".into(),
            id: "4".into(),
            state: "RUNNING".into(),
            interactive: true,
            ..Job::default()
        };
        assert!(visible_jobs(vec![interactive.clone()], &Ledger::default(), 0, false).is_empty());
        assert_eq!(
            visible_jobs(vec![interactive.clone()], &Ledger::default(), 0, true),
            vec![interactive]
        );
    }

    #[test]
    fn shared_job_cache_round_trips_and_invalidates() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config {
            local_user: "local".into(),
            remote_user: "remote".into(),
            ssh_host: "host".into(),
            state_path: directory.path().join("state.json"),
            executable: PathBuf::from("slurm-log"),
            sbatch_banks: Vec::new(),
            clusters: vec![crate::config::ClusterConfig {
                name: "cispa".into(),
                transport: "ssh".into(),
                user: "remote".into(),
                ssh_host: "host".into(),
                working_directory: PathBuf::from("/tmp"),
                accounting: false,
            }],
        };
        let path = cache_path(&config, "recent");
        let jobs = vec![Job {
            cluster: "cispa".into(),
            id: "42".into(),
            ..Job::default()
        }];
        store_jobs(&path, &jobs);
        assert_eq!(cached_jobs(&path, Duration::from_secs(3)), Some(jobs));
        assert_eq!(
            recent(&config, "cispa", false).unwrap(),
            Vec::<Job>::new(),
            "accounting-disabled clusters must return before invoking SSH or sacct"
        );
        assert!(accounting_warnings(&config, &["cispa"], false).is_empty());
        let warnings = accounting_warnings(&config, &["cispa"], true);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("completed jobs unavailable"));
        assert!(warnings[0].contains("only active squeue jobs"));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        invalidate_caches(&config);
        assert!(!path.exists());
    }

    #[test]
    fn malformed_scheduler_output_is_ignored_without_panicking() {
        let input = "\n|||\nabc|RUNNING|name|1:00|node\n1.batch|RUNNING|step|1:00|node\n\
                     42|RUNNING|valid|00:01|node\n999999999999999999999999|FAILED|huge|x|\n";
        let jobs = parse_queue(input, "cispa");
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].id, "42");
    }

    #[test]
    fn parsers_survive_deterministic_hostile_corpus() {
        let mut seed = 0x9e37_79b9_u32;
        for length in [0, 1, 2, 7, 31, 255, 4096, 65_535] {
            let mut input = String::with_capacity(length);
            for _ in 0..length {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                input.push(char::from_u32(1 + seed % 0x7e).unwrap());
            }
            let queued = parse_queue(&input, "cispa");
            let recent = parse_recent(&input, "cispa");
            assert!(queued.iter().all(|job| valid_job_id(&job.id)));
            assert!(recent.iter().all(|job| valid_job_id(&job.id)));
        }
    }

    #[test]
    fn corrupt_or_stale_cache_is_a_miss() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.json");
        fs::write(&path, b"not-json").unwrap();
        assert!(cached_jobs(&path, Duration::from_secs(60)).is_none());
        assert!(cached_jobs(&path, Duration::ZERO).is_none());
    }

    #[test]
    fn query_dimensions_are_strictly_bounded() {
        assert!(validate_query("both", "all").is_ok());
        assert!(validate_query("../../tmp", "all").is_err());
        assert!(validate_query("cispa", "arbitrary").is_err());
    }

    #[test]
    fn oversized_cache_is_rejected_before_reading() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.json");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_CACHE_BYTES + 1).unwrap();
        assert!(cached_jobs(&path, Duration::from_secs(60)).is_none());
    }
}
