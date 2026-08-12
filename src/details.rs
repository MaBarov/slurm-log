use std::{
    collections::VecDeque,
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use serde::{Deserialize, Serialize};

use crate::{
    command::shell_quote,
    config::Config,
    model::{Job, valid_job_id},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct JobDetails {
    pub cluster: String,
    pub id: String,
    pub name: String,
    pub state: String,
    pub reason: String,
    pub partition: String,
    pub account: String,
    pub qos: String,
    pub submit: String,
    pub start: String,
    pub end: String,
    pub elapsed: String,
    pub elapsed_seconds: u64,
    pub time_limit: String,
    pub nodes: u64,
    pub cpus: u64,
    pub requested_cpus: u64,
    pub memory_bytes: u64,
    pub requested_memory: String,
    pub max_rss_bytes: u64,
    pub gpus: u64,
    pub gpu_types: String,
    pub gpu_utilization: Option<f64>,
    pub gpu_memory_bytes: Option<u64>,
    pub total_cpu_seconds: u64,
    pub cpu_efficiency: Option<f64>,
    pub memory_efficiency: Option<f64>,
    pub alloc_tres: String,
    pub req_tres: String,
    pub node_list: String,
    pub exit_code: String,
    pub source: String,
    pub sampled_at: String,
    pub terminal: bool,
    pub stale_error: String,
}

pub fn validate_cluster(config: &Config, cluster: &str) -> Result<()> {
    config.cluster(cluster).map(|_| ())
}

pub fn fetch(
    config: &Config,
    cluster: &str,
    id: &str,
    previous: Option<&JobDetails>,
) -> Result<JobDetails> {
    validate_cluster(config, cluster)?;
    if !valid_job_id(id) {
        bail!("invalid job ID {id}");
    }
    if let Ok(queue) = crate::slurm::queued(config, cluster)
        && let Some(job) = queue.into_iter().find(|job| job.id == id)
    {
        if job.pending() {
            return Ok(from_pending(job));
        }
        if job.running() {
            if let Some(previous) = previous {
                return sample_running(config, cluster, id, job, previous);
            }
            // Active jobs must not depend on sacct: accounting commonly lags
            // behind squeue, especially for array tasks. Build the first frame
            // from live scheduler data and enrich it with sstat when available.
            let base =
                live_details(config, job.clone()).unwrap_or_else(|_| from_live_queue(job.clone()));
            return sample_running(config, cluster, id, job, &base).or(Ok(base));
        }
    }
    if !config.cluster(cluster)?.accounting {
        bail!("accounting is unavailable on {cluster}, and job {id} is no longer active");
    }
    // JobIDRaw drops the array-task suffix (for example 3209343_2 becomes
    // 3209343), so it cannot identify the selected task. A wide JobID field
    // preserves both array suffixes and step suffixes without truncation.
    let fields = "JobID%100,JobName,State,Reason,Partition,Account,QOS,Submit,Start,End,Elapsed,ElapsedRaw,Timelimit,NNodes,NCPUS,AllocCPUS,ReqCPUS,ReqMem,MaxRSS,AveRSS,AllocTRES,ReqTRES,TotalCPU,CPUTimeRAW,ExitCode,NodeList,TRESUsageInAve,TRESUsageInMax";
    let command = format!(
        "sacct -j {} -n -P --format={} 2>/dev/null",
        shell_quote(id),
        shell_quote(fields)
    );
    let output = crate::slurm::scheduler_text(config, cluster, "sh", &["-c", &command])?;
    parse_accounting(&output, cluster, id)
        .ok_or_else(|| anyhow::anyhow!("no accounting details found for {cluster}:{id}"))
}

fn sample_running(
    config: &Config,
    cluster: &str,
    id: &str,
    job: Job,
    previous: &JobDetails,
) -> Result<JobDetails> {
    let fields = "JobID,NTasks,AllocTRES,AveCPU,MaxRSS,AveRSS,TRESUsageInAve,TRESUsageInMax";
    let command = format!(
        "sstat -a -j {} -n -P --format={} 2>/dev/null",
        shell_quote(id),
        shell_quote(fields)
    );
    let output = crate::slurm::scheduler_text(config, cluster, "sh", &["-c", &command])?;
    Ok(merge_running(previous.clone(), job, &output))
}

fn merge_running(mut details: JobDetails, job: Job, output: &str) -> JobDetails {
    details.state = job.state;
    details.reason = job.reason;
    details.partition = job.partition;
    details.start = job.start_time;
    details.elapsed = job.elapsed;
    details.elapsed_seconds = parse_duration(&details.elapsed).unwrap_or(details.elapsed_seconds);
    let mut total_cpu: Option<u64> = None;
    let mut max_rss = 0_u64;
    let mut gpu_utilization: Option<f64> = None;
    let mut gpu_memory: Option<u64> = None;
    for fields in output
        .lines()
        .map(|line| line.split('|').collect::<Vec<_>>())
    {
        if fields.len() < 8 || fields[0].split('.').next().unwrap_or(fields[0]) != details.id {
            continue;
        }
        let tasks = number(fields[1]).unwrap_or(1);
        if let Some(cpu) = parse_duration(fields[3]).and_then(|value| value.checked_mul(tasks)) {
            total_cpu = Some(total_cpu.unwrap_or(0).saturating_add(cpu));
        }
        max_rss = max_rss.max(parse_bytes(fields[4]).unwrap_or(0));
        for value in [fields[6], fields[7]] {
            if let Some(util) = tres_value(value, "gres/gpuutil")
                .and_then(|value| value.trim_end_matches('%').parse::<f64>().ok())
                .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
            {
                gpu_utilization = Some(gpu_utilization.map_or(util, |old| old.max(util)));
            }
            if let Some(memory) = tres_value(value, "gres/gpumem").and_then(parse_bytes) {
                gpu_memory = Some(gpu_memory.map_or(memory, |old| old.max(memory)));
            }
        }
    }
    details.max_rss_bytes = max_rss.max(details.max_rss_bytes);
    if let Some(total_cpu) = total_cpu {
        let efficiency = cpu_efficiency(total_cpu, details.elapsed_seconds, details.cpus);
        if efficiency.is_some() {
            details.total_cpu_seconds = total_cpu;
            details.cpu_efficiency = efficiency;
        }
    }
    details.memory_efficiency = (details.memory_bytes > 0 && details.max_rss_bytes > 0)
        .then_some(details.max_rss_bytes as f64 * 100.0 / details.memory_bytes as f64);
    details.gpu_utilization = gpu_utilization.or(details.gpu_utilization);
    details.gpu_memory_bytes = gpu_memory.or(details.gpu_memory_bytes);
    details.source = "sstat".into();
    details.sampled_at = timestamp();
    details.stale_error.clear();
    details
}

fn from_pending(job: Job) -> JobDetails {
    JobDetails {
        cluster: job.cluster,
        id: job.id,
        name: job.name,
        state: job.state,
        reason: job.reason,
        partition: job.partition,
        start: job.start_time,
        alloc_tres: job.alloc_tres,
        sampled_at: timestamp(),
        source: "squeue".into(),
        ..JobDetails::default()
    }
}

fn from_live_queue(job: Job) -> JobDetails {
    JobDetails {
        cluster: job.cluster,
        id: job.id,
        name: job.name,
        state: job.state,
        reason: job.reason,
        partition: job.partition,
        start: job.start_time,
        elapsed: job.elapsed.clone(),
        elapsed_seconds: parse_duration(&job.elapsed).unwrap_or(0),
        sampled_at: timestamp(),
        source: "squeue".into(),
        ..JobDetails::default()
    }
}

fn live_details(config: &Config, job: Job) -> Result<JobDetails> {
    let output = crate::slurm::scheduler_text(
        config,
        &job.cluster,
        "scontrol",
        &["show", "job", "-o", &job.id],
    )?;
    parse_live_control(&output, job).ok_or_else(|| anyhow::anyhow!("invalid scontrol response"))
}

fn parse_live_control(input: &str, job: Job) -> Option<JobDetails> {
    let value = |name: &str| {
        input.split_whitespace().find_map(|field| {
            field
                .strip_prefix(name)
                .and_then(|value| value.strip_prefix('='))
        })
    };
    let alloc_tres = value("AllocTRES").unwrap_or("");
    let req_tres = value("ReqTRES").unwrap_or("");
    let nodes = number(value("NumNodes").unwrap_or("")).unwrap_or(0);
    let cpus = number(value("NumCPUs").unwrap_or(""))
        .or_else(|| tres_number(alloc_tres, "cpu"))
        .unwrap_or(0);
    let requested_cpus = tres_number(req_tres, "cpu").unwrap_or(cpus);
    let memory_bytes = allocation_memory(alloc_tres, req_tres);
    let (mut gpus, mut gpu_types) = parse_gpus(alloc_tres);
    if gpus == 0 {
        (gpus, gpu_types) = parse_gpus(req_tres);
    }
    let elapsed = value("RunTime").unwrap_or(&job.elapsed).to_string();
    Some(JobDetails {
        cluster: job.cluster,
        id: job.id,
        name: value("JobName").unwrap_or(&job.name).into(),
        state: value("JobState").unwrap_or(&job.state).into(),
        reason: value("Reason").unwrap_or(&job.reason).into(),
        partition: value("Partition").unwrap_or(&job.partition).into(),
        account: value("Account").unwrap_or("").into(),
        qos: value("QOS").unwrap_or("").into(),
        submit: value("SubmitTime").unwrap_or("").into(),
        start: value("StartTime").unwrap_or(&job.start_time).into(),
        end: value("EndTime").unwrap_or("").into(),
        elapsed_seconds: parse_duration(&elapsed).unwrap_or(0),
        elapsed,
        time_limit: value("TimeLimit").unwrap_or("").into(),
        nodes,
        cpus,
        requested_cpus,
        memory_bytes,
        requested_memory: display_requested_memory(tres_value(req_tres, "mem").unwrap_or("")),
        gpus,
        gpu_types,
        alloc_tres: alloc_tres.into(),
        req_tres: req_tres.into(),
        node_list: value("NodeList").unwrap_or("").into(),
        exit_code: value("ExitCode").unwrap_or("").into(),
        source: "scontrol".into(),
        sampled_at: timestamp(),
        terminal: false,
        ..JobDetails::default()
    })
}

fn parse_accounting(input: &str, cluster: &str, wanted: &str) -> Option<JobDetails> {
    let rows: Vec<Vec<&str>> = input
        .lines()
        .map(|line| line.split('|').collect())
        .filter(|fields: &Vec<_>| fields.len() >= 26)
        .collect();
    let main = rows.iter().find(|fields| {
        fields[0].split('.').next().unwrap_or(fields[0]) == wanted && !fields[0].contains('.')
    })?;
    let max_rss = rows
        .iter()
        .filter(|row| row[0].split('.').next().unwrap_or(row[0]) == wanted)
        .filter_map(|row| parse_bytes(row.get(18).copied().unwrap_or("")))
        .max()
        .unwrap_or(0);
    let total_cpu = rows
        .iter()
        .filter(|row| row[0].split('.').next().unwrap_or(row[0]) == wanted)
        .filter_map(|row| parse_duration(row.get(22).copied().unwrap_or("")))
        .max()
        .unwrap_or(0);
    let alloc_tres = main.get(20).copied().unwrap_or("");
    let req_tres = main.get(21).copied().unwrap_or("");
    let cpus = number(main.get(15).copied().unwrap_or(""))
        .or_else(|| tres_number(alloc_tres, "cpu"))
        .unwrap_or(0);
    let requested_cpus = number(main.get(16).copied().unwrap_or(""))
        .or_else(|| tres_number(req_tres, "cpu"))
        .unwrap_or(0);
    let elapsed_seconds = number(main.get(11).copied().unwrap_or(""))
        .or_else(|| parse_duration(main.get(10).copied().unwrap_or("")))
        .unwrap_or(0);
    let memory_bytes = allocation_memory(alloc_tres, req_tres);
    let (mut gpus, mut gpu_types) = parse_gpus(alloc_tres);
    if gpus == 0 {
        (gpus, gpu_types) = parse_gpus(req_tres);
    }
    let gpu_utilization = rows
        .iter()
        .filter_map(|row| row.get(26))
        .filter_map(|value| tres_value(value, "gres/gpuutil"))
        .filter_map(|value| value.trim_end_matches('%').parse::<f64>().ok())
        .reduce(f64::max);
    let gpu_memory_bytes = rows
        .iter()
        .filter_map(|row| row.get(27))
        .filter_map(|value| tres_value(value, "gres/gpumem"))
        .filter_map(parse_bytes)
        .max();
    let cpu_efficiency = (total_cpu > 0 || !state_running(main.get(2).copied().unwrap_or("")))
        .then(|| cpu_efficiency(total_cpu, elapsed_seconds, cpus))
        .flatten();
    let memory_efficiency =
        (memory_bytes > 0 && max_rss > 0).then_some(max_rss as f64 * 100.0 / memory_bytes as f64);
    let state = main.get(2).copied().unwrap_or("").to_string();
    Some(JobDetails {
        cluster: cluster.into(),
        id: wanted.into(),
        name: main.get(1).copied().unwrap_or("").into(),
        state: state.clone(),
        reason: main.get(3).copied().unwrap_or("").into(),
        partition: main.get(4).copied().unwrap_or("").into(),
        account: main.get(5).copied().unwrap_or("").into(),
        qos: main.get(6).copied().unwrap_or("").into(),
        submit: main.get(7).copied().unwrap_or("").into(),
        start: main.get(8).copied().unwrap_or("").into(),
        end: main.get(9).copied().unwrap_or("").into(),
        elapsed: main.get(10).copied().unwrap_or("").into(),
        elapsed_seconds,
        time_limit: main.get(12).copied().unwrap_or("").into(),
        nodes: number(main.get(13).copied().unwrap_or("")).unwrap_or(0),
        cpus,
        requested_cpus,
        memory_bytes,
        requested_memory: display_requested_memory(main.get(17).copied().unwrap_or("")),
        max_rss_bytes: max_rss,
        gpus,
        gpu_types,
        gpu_utilization,
        gpu_memory_bytes,
        total_cpu_seconds: total_cpu,
        cpu_efficiency,
        memory_efficiency,
        alloc_tres: alloc_tres.into(),
        req_tres: req_tres.into(),
        node_list: main.get(25).copied().unwrap_or("").into(),
        exit_code: main.get(24).copied().unwrap_or("").into(),
        source: "sacct".into(),
        sampled_at: timestamp(),
        terminal: !state.starts_with("RUNNING") && !state.starts_with("PENDING"),
        stale_error: String::new(),
    })
}

fn state_running(state: &str) -> bool {
    state.starts_with("RUNNING")
}

fn cpu_efficiency(total_cpu: u64, elapsed: u64, cpus: u64) -> Option<f64> {
    let capacity = elapsed.checked_mul(cpus)?;
    if capacity == 0 {
        return None;
    }
    let value = total_cpu as f64 * 100.0 / capacity as f64;
    (value.is_finite() && (0.0..=1_000.0).contains(&value)).then_some(value)
}

fn number(value: &str) -> Option<u64> {
    value.parse().ok()
}

fn parse_duration(value: &str) -> Option<u64> {
    // Slurm uses very large integer sentinels for unavailable time values.
    // Reject them here instead of turning them into quadrillion-percent CPU.
    const MAX_DURATION_SECONDS: u64 = 100 * 366 * 24 * 60 * 60;
    if value.is_empty() || value == "Unknown" {
        return None;
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        return value
            .parse()
            .ok()
            .filter(|seconds| *seconds <= MAX_DURATION_SECONDS);
    }
    let (days, clock) = value.split_once('-').unwrap_or(("0", value));
    let mut parts = clock.split(':');
    let first = parts.next()?.parse::<u64>().ok()?;
    let second = parts.next()?.parse::<u64>().ok()?;
    let third = parts.next().map(str::parse::<u64>).transpose().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let seconds = match third {
        Some(seconds) => first
            .checked_mul(3600)?
            .checked_add(second.checked_mul(60)?)?
            .checked_add(seconds)?,
        None => first.checked_mul(60)?.checked_add(second)?,
    };
    days.parse::<u64>()
        .ok()?
        .checked_mul(86400)?
        .checked_add(seconds)
        .filter(|seconds| *seconds <= MAX_DURATION_SECONDS)
}

fn parse_bytes(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() || value == "Unknown" || value == "N/A" {
        return None;
    }
    let split = value.find(|c: char| !c.is_ascii_digit() && c != '.')?;
    let amount: f64 = value[..split].parse().ok()?;
    if !amount.is_finite() || amount < 0.0 {
        return None;
    }
    let unit = value[split..]
        .trim_end_matches(['c', 'n'])
        .to_ascii_uppercase();
    let multiplier = match unit.as_str() {
        "K" | "KB" | "KIB" => 1024_f64,
        "M" | "MB" | "MIB" => 1024_f64.powi(2),
        "G" | "GB" | "GIB" => 1024_f64.powi(3),
        "T" | "TB" | "TIB" => 1024_f64.powi(4),
        "B" => 1.0,
        _ => return None,
    };
    Some((amount * multiplier) as u64)
}

fn tres_value<'a>(tres: &'a str, key: &str) -> Option<&'a str> {
    tres.split(',').find_map(|item| {
        let (name, value) = item.rsplit_once('=')?;
        let suffix_matches = name
            .strip_suffix(key)
            .is_some_and(|prefix| prefix.ends_with('/'));
        (name == key || suffix_matches).then_some(value)
    })
}

fn tres_number(tres: &str, key: &str) -> Option<u64> {
    tres_value(tres, key)?.parse().ok()
}

fn allocation_memory(allocated: &str, requested: &str) -> u64 {
    if let Some(bytes) = tres_value(allocated, "mem").and_then(parse_bytes) {
        return bytes;
    }
    // Some Slurm configurations insert ReqTRES=mem=1M when the job has no
    // memory limit at all (MinMemoryNode=0). Treating that sentinel as a real
    // allocation produces absurd percentages for otherwise healthy jobs.
    tres_value(requested, "mem")
        .and_then(parse_bytes)
        .filter(|bytes| *bytes > 1024 * 1024)
        .unwrap_or(0)
}

fn display_requested_memory(value: &str) -> String {
    parse_bytes(value)
        .filter(|bytes| *bytes > 1024 * 1024)
        .map(|_| value.to_string())
        .unwrap_or_default()
}

fn parse_gpus(tres: &str) -> (u64, String) {
    let mut generic_count = 0;
    let mut typed_count = 0;
    let mut kinds = Vec::new();
    for item in tres.split(',') {
        let Some((name, value)) = item.rsplit_once('=') else {
            continue;
        };
        if name == "gres/gpu" || name == "gpu" {
            generic_count += value.parse::<u64>().unwrap_or(0);
        } else if let Some(kind) = name.strip_prefix("gres/gpu:") {
            typed_count += value.parse::<u64>().unwrap_or(0);
            kinds.push(kind.to_string());
        }
    }
    kinds.sort();
    kinds.dedup();
    // Slurm commonly emits both gres/gpu=N and gres/gpu:TYPE=N for the same
    // devices. The typed fields refine the generic field; they are not extra
    // GPUs and must not be summed with it.
    (
        if typed_count > 0 {
            typed_count
        } else {
            generic_count
        },
        kinds.join(", "),
    )
}

fn timestamp() -> String {
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "now".into())
}

pub fn run(config: &Config, cluster: &str, id: &str, compact: bool) -> Result<()> {
    validate_cluster(config, cluster)?;
    if !valid_job_id(id) {
        bail!("invalid job ID {id}");
    }
    if !io::stdout().is_terminal() {
        let details = crate::daemon::job_details(config, cluster, id, true)?;
        print_text(&details);
        return Ok(());
    }
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = terminal::disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
        }
    }
    let _guard = Guard;
    let mut paused = false;
    let mut force = true;
    let mut next = Instant::now();
    let mut phase_pending = true;
    let mut manual_refresh = false;
    let mut activity = String::new();
    let mut current: Option<JobDetails> = None;
    let mut cpu = VecDeque::with_capacity(40);
    let mut memory = VecDeque::with_capacity(40);
    loop {
        if !paused && !current.as_ref().is_some_and(|item| item.terminal) && Instant::now() >= next
        {
            let previous_sample = current.as_ref().map(|details| details.sampled_at.clone());
            let requested_manually = manual_refresh;
            manual_refresh = false;
            match crate::daemon::job_details(config, cluster, id, force) {
                Ok(details) => {
                    if let Some(value) = details.cpu_efficiency {
                        push_sample(&mut cpu, value);
                    }
                    if let Some(value) = details.memory_efficiency {
                        push_sample(&mut memory, value);
                    }
                    activity = if requested_manually {
                        if previous_sample.as_deref() == Some(details.sampled_at.as_str())
                            && !details.terminal
                        {
                            // Forced samples are coalesced for ten seconds.
                            // Preserve an early request and retry it instead of
                            // making the key press look as if it was ignored.
                            manual_refresh = true;
                            "refresh queued (10s rate limit)".into()
                        } else {
                            "refreshed".into()
                        }
                    } else {
                        String::new()
                    };
                    current = Some(details);
                }
                Err(error) => {
                    if requested_manually {
                        activity = "refresh failed".into();
                    }
                    if let Some(details) = current.as_mut() {
                        details.stale_error = format!("{error:#}");
                    } else {
                        current = Some(error_details(cluster, id, &format!("{error:#}")));
                    }
                }
            }
            force = manual_refresh;
            let delay = if manual_refresh {
                crate::daemon::FORCED_DETAIL_MINIMUM + Duration::from_millis(250)
            } else if phase_pending {
                phase_pending = false;
                Duration::from_secs(10) + Duration::from_millis(refresh_phase(id))
            } else {
                crate::daemon::ACTIVE_DETAIL_TTL
            };
            next = Instant::now() + delay;
            if let Some(details) = &current {
                draw(details, compact, paused, &activity, &cpu, &memory)?;
            }
        }
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Resize(_, _) => {
                    if let Some(details) = &current {
                        draw(details, compact, paused, &activity, &cpu, &memory)?;
                    }
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => return Ok(()),
                    KeyCode::Char(' ') => {
                        paused = !paused;
                        activity.clear();
                        if !paused {
                            next = Instant::now();
                        }
                        if let Some(details) = &current {
                            draw(details, compact, paused, &activity, &cpu, &memory)?;
                        }
                    }
                    KeyCode::Char('r') => {
                        if current.as_ref().is_some_and(|details| details.terminal) {
                            activity = "final snapshot".into();
                        } else {
                            activity = "refreshing…".into();
                            manual_refresh = true;
                            force = true;
                            next = Instant::now();
                        }
                        if let Some(details) = &current {
                            draw(details, compact, paused, &activity, &cpu, &memory)?;
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

fn error_details(cluster: &str, id: &str, error: &str) -> JobDetails {
    JobDetails {
        cluster: cluster.into(),
        id: id.into(),
        state: "UNAVAILABLE".into(),
        source: "retryable error".into(),
        sampled_at: timestamp(),
        stale_error: error.into(),
        ..JobDetails::default()
    }
}

fn refresh_phase(id: &str) -> u64 {
    id.bytes().fold(0_u64, |hash, byte| {
        hash.wrapping_mul(16777619).wrapping_add(byte as u64)
    }) % 10_000
}

fn push_sample(samples: &mut VecDeque<f64>, value: f64) {
    if samples.len() == 40 {
        samples.pop_front();
    }
    samples.push_back(value);
}

fn draw(
    details: &JobDetails,
    compact: bool,
    paused: bool,
    activity: &str,
    cpu: &VecDeque<f64>,
    memory: &VecDeque<f64>,
) -> Result<()> {
    let mut out = Vec::new();
    execute!(out, cursor::MoveTo(0, 0), terminal::Clear(ClearType::All))?;
    if compact {
        execute!(
            out,
            SetForegroundColor(Color::AnsiValue(183)),
            SetAttribute(Attribute::Bold),
            Print(format!(
                "DETAILS  {}:{}  {}\r\n",
                details.cluster, details.id, details.name
            )),
            ResetColor,
            SetAttribute(Attribute::Reset),
            Print(format!(
                "{}  ·  elapsed {} / {}\r\n",
                details.state,
                clean(&details.elapsed),
                clean(&details.time_limit)
            )),
            Print(format!(
                "ALLOC  {} node · {} CPU · {} GPU · {} memory\r\n",
                details.nodes,
                details.cpus,
                details.gpus,
                bytes(details.memory_bytes)
            )),
            Print(format!(
                "USAGE  CPU {} · memory {} · GPU {}\r\n",
                cpu_percent(details),
                memory_usage(details),
                gpu_usage(details)
            )),
            Print(format!(
                "PLACE  {} · {}\r\n",
                clean(&details.partition),
                clean(&details.node_list)
            ))
        )?;
        if let Some(note) = hint(details) {
            execute!(
                out,
                SetForegroundColor(Color::AnsiValue(180)),
                Print(format!("NOTE   {note}\r\n")),
                ResetColor
            )?;
        }
        execute!(
            out,
            SetAttribute(Attribute::Dim),
            Print(format!(
                "{} · {} · {}{}\r\nCtrl-b i / q / Esc / Enter close · Space pause · r refresh",
                details.sampled_at,
                details.source,
                if !activity.is_empty() {
                    activity
                } else if details.terminal {
                    "final"
                } else if paused {
                    "paused"
                } else {
                    "live · auto 30s"
                },
                if details.stale_error.is_empty() {
                    String::new()
                } else {
                    format!(" · stale: {}", details.stale_error)
                }
            )),
            SetAttribute(Attribute::Reset)
        )?;
        io::stdout().write_all(&out)?;
        io::stdout().flush()?;
        return Ok(());
    }
    section(&mut out, Color::AnsiValue(179), "JOB")?;
    line(
        &mut out,
        "Job",
        &format!("{}:{}  {}", details.cluster, details.id, details.name),
    )?;
    line(
        &mut out,
        "State",
        &format!("{}  {}", details.state, clean(&details.reason)),
    )?;
    line(
        &mut out,
        "Time",
        &format!(
            "elapsed {}  limit {}",
            clean(&details.elapsed),
            clean(&details.time_limit)
        ),
    )?;
    section(&mut out, Color::AnsiValue(139), "ALLOCATION")?;
    line(
        &mut out,
        "Compute",
        &format!(
            "{} node(s)  {} CPU(s)  {} GPU(s) {}",
            details.nodes, details.cpus, details.gpus, details.gpu_types
        ),
    )?;
    line(
        &mut out,
        "Memory",
        &format!(
            "allocated {}  peak {}",
            bytes(details.memory_bytes),
            bytes(details.max_rss_bytes)
        ),
    )?;
    line(
        &mut out,
        "Placement",
        &format!(
            "{}  {}",
            clean(&details.partition),
            clean(&details.node_list)
        ),
    )?;
    section(&mut out, Color::AnsiValue(109), "UTILIZATION")?;
    line(&mut out, "CPU", &cpu_percent(details))?;
    line(&mut out, "Memory", &memory_usage(details))?;
    line(&mut out, "GPU", &gpu_usage(details))?;
    if !compact {
        line(&mut out, "CPU trend", &spark(cpu))?;
        line(&mut out, "Memory trend", &spark(memory))?;
        section(&mut out, Color::AnsiValue(179), "SCHEDULING & ACCOUNTING")?;
        line(&mut out, "Submit", &clean(&details.submit))?;
        line(
            &mut out,
            "Start / end",
            &format!("{}  /  {}", clean(&details.start), clean(&details.end)),
        )?;
        line(
            &mut out,
            "Account",
            &format!("{}  QOS {}", clean(&details.account), clean(&details.qos)),
        )?;
        line(
            &mut out,
            "Requested",
            &format!(
                "{} CPU(s), memory {}, TRES {}",
                details.requested_cpus,
                clean(&details.requested_memory),
                clean(&details.req_tres)
            ),
        )?;
        line(&mut out, "Exit", &clean(&details.exit_code))?;
    }
    if let Some(hint) = hint(details) {
        section(&mut out, Color::AnsiValue(180), "NOTE")?;
        execute!(out, Print(hint), Print("\r\n"))?;
    }
    execute!(
        out,
        SetAttribute(Attribute::Dim),
        Print(format!(
            "\r\n{} · source {} · {}{}\r\nq/Esc/Enter close · Space pause · r refresh",
            details.sampled_at,
            details.source,
            if !activity.is_empty() {
                activity
            } else if details.terminal {
                "final"
            } else if paused {
                "paused"
            } else {
                "live · auto 30s"
            },
            if details.stale_error.is_empty() {
                String::new()
            } else {
                format!(" · stale: {}", details.stale_error)
            }
        )),
        SetAttribute(Attribute::Reset)
    )?;
    io::stdout().write_all(&out)?;
    io::stdout().flush()?;
    Ok(())
}

fn section(out: &mut Vec<u8>, color: Color, name: &str) -> Result<()> {
    execute!(
        out,
        SetForegroundColor(color),
        SetAttribute(Attribute::Bold),
        Print(format!("{name}\r\n")),
        ResetColor,
        SetAttribute(Attribute::Reset)
    )?;
    Ok(())
}
fn line(out: &mut Vec<u8>, label: &str, value: &str) -> Result<()> {
    execute!(out, Print(format!("  {label:<14} {value}\r\n")))?;
    Ok(())
}
fn clean(value: &str) -> String {
    if value.is_empty() || value == "Unknown" || value == "None" {
        "—".into()
    } else {
        value.into()
    }
}
fn percent(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite() && *value >= 0.0 && *value <= 1_000.0)
        .map(|v| format!("{v:.1}%"))
        .unwrap_or_else(|| "not available".into())
}
fn cpu_percent(details: &JobDetails) -> String {
    let efficiency = details
        .cpu_efficiency
        .filter(|value| value.is_finite() && (0.0..=1_000.0).contains(value));
    if efficiency.is_none() && details.state.starts_with("RUNNING") {
        "collecting…".into()
    } else {
        percent(efficiency)
    }
}
fn memory_usage(details: &JobDetails) -> String {
    if details
        .memory_efficiency
        .is_some_and(|value| value.is_finite() && (0.0..=1_000.0).contains(&value))
    {
        percent(details.memory_efficiency)
    } else if details.max_rss_bytes > 0 {
        format!("{} peak (limit unknown)", bytes(details.max_rss_bytes))
    } else {
        "not available".into()
    }
}
fn gpu_usage(details: &JobDetails) -> String {
    if details.gpus == 0 {
        "none allocated".into()
    } else {
        details
            .gpu_utilization
            .map(|value| format!("{value:.1}%"))
            .unwrap_or_else(|| "not recorded".into())
    }
}
fn bytes(value: u64) -> String {
    if value == 0 {
        return "—".into();
    }
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut amount = value as f64;
    let mut unit = 0;
    while amount >= 1024.0 && unit < UNITS.len() - 1 {
        amount /= 1024.0;
        unit += 1;
    }
    format!("{amount:.1} {}", UNITS[unit])
}
fn spark(values: &VecDeque<f64>) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if values.is_empty() {
        return "collecting…".into();
    }
    values
        .iter()
        .map(|value| BARS[((value.clamp(0.0, 100.0) / 100.0 * 7.0).round() as usize).min(7)])
        .collect()
}
fn hint(details: &JobDetails) -> Option<String> {
    if details.gpus > 0 && details.gpu_utilization.is_none() {
        return Some("GPU utilization counters are not recorded by this cluster.".into());
    }
    if details.elapsed_seconds >= 120 && details.cpu_efficiency.is_some_and(|v| v < 20.0) {
        return Some("CPU utilization is low relative to the allocated CPUs.".into());
    }
    if details.memory_efficiency.is_some_and(|v| v >= 85.0) {
        return Some("Peak memory is close to the allocated memory.".into());
    }
    None
}

fn print_text(details: &JobDetails) {
    println!("Job: {}:{} {}", details.cluster, details.id, details.name);
    println!("State: {} {}", details.state, details.reason);
    println!(
        "Allocation: {} nodes, {} CPUs, {} GPUs, {} memory",
        details.nodes,
        details.cpus,
        details.gpus,
        bytes(details.memory_bytes)
    );
    println!(
        "Utilization: CPU {}, memory {}, GPU {}",
        cpu_percent(details),
        memory_usage(details),
        gpu_usage(details)
    );
    println!(
        "Elapsed: {} / limit {}",
        details.elapsed, details.time_limit
    );
    println!(
        "Partition: {}  Nodes: {}",
        details.partition, details.node_list
    );
    println!(
        "Exit: {}  Sample: {} ({})",
        details.exit_code, details.sampled_at, details.source
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units_durations_and_gpu_tres() {
        assert_eq!(parse_duration("1-02:03:04"), Some(93784));
        assert_eq!(parse_duration("03:04"), Some(184));
        assert_eq!(parse_bytes("1.5G"), Some(1_610_612_736));
        assert_eq!(
            parse_gpus("cpu=8,mem=32G,gres/gpu:a100=2"),
            (2, "a100".into())
        );
        assert_eq!(
            parse_gpus("cpu=8,gres/gpu=1,gres/gpu:a100=1"),
            (1, "a100".into()),
            "generic and typed GPU TRES describe the same device"
        );
        assert_eq!(allocation_memory("cpu=8", "cpu=8,mem=1M"), 0);
        assert_eq!(display_requested_memory("1Mc"), "");
        assert_eq!(
            allocation_memory("cpu=8", "cpu=8,mem=3905M"),
            3_905 * 1024 * 1024
        );
        assert_eq!(display_requested_memory("3905M"), "3905M");
        assert!(refresh_phase("42") < 10_000);
    }

    #[test]
    fn retryable_error_details_preserve_identity_and_explanation() {
        let details = error_details("cispa", "3209343_2", "accounting is delayed");
        assert_eq!(details.cluster, "cispa");
        assert_eq!(details.id, "3209343_2");
        assert_eq!(details.state, "UNAVAILABLE");
        assert_eq!(details.source, "retryable error");
        assert_eq!(details.stale_error, "accounting is delayed");
        assert!(!details.terminal);
    }

    #[test]
    fn accounting_matches_array_task_job_ids_and_their_steps() {
        let main = "3209343_2|array-task|COMPLETED|None|gpu|acct|normal|sub|start|end|00:01:00|60|01:00:00|1|4|4|4|1G|512M|256M|cpu=4|cpu=4,mem=1G|00:02:00|240|0:0|node1||";
        let step = "3209343_2.batch|batch|COMPLETED|None|gpu|acct|normal|sub|start|end|00:01:00|60|01:00:00|1|4|4|4|1G|768M|256M|cpu=4|cpu=4,mem=1G|00:02:00|240|0:0|node1||";
        let parsed = parse_accounting(&format!("{main}\n{step}\n"), "cispa", "3209343_2")
            .expect("array task must match its JobID field");
        assert_eq!(parsed.id, "3209343_2");
        assert_eq!(parsed.max_rss_bytes, 768 * 1024 * 1024);
        assert!(parsed.terminal);
    }

    #[test]
    fn accounting_derives_efficiency_and_step_peak() {
        let line = "42|train|COMPLETED|None|gpu|acct|normal|sub|start|end|00:10:00|600|01:00:00|1|8|8|8|4Gc|2G|1G|cpu=8,gres/gpu:a100=2|cpu=8,mem=32G,gres/gpu:a100=2|01:00:00|4800|0:0|node1||";
        let mut step = vec![""; 28];
        step[0] = "42.batch";
        step[1] = "batch";
        step[2] = "COMPLETED";
        step[18] = "8G";
        step[22] = "01:00:00";
        let parsed =
            parse_accounting(&format!("{line}\n{}\n", step.join("|")), "cispa", "42").unwrap();
        assert_eq!(parsed.gpus, 2);
        assert_eq!(parsed.max_rss_bytes, 8 * 1024 * 1024 * 1024);
        assert_eq!(parsed.cpu_efficiency, Some(75.0));
        assert_eq!(parsed.memory_efficiency, Some(25.0));
    }

    #[test]
    fn running_sstat_sample_updates_usage_without_losing_allocation() {
        let previous = JobDetails {
            cluster: "cispa".into(),
            id: "42".into(),
            cpus: 8,
            memory_bytes: 32 * 1024 * 1024 * 1024,
            ..JobDetails::default()
        };
        let job = Job {
            cluster: "cispa".into(),
            id: "42".into(),
            state: "RUNNING".into(),
            elapsed: "00:10:00".into(),
            ..Job::default()
        };
        let sample = "42.batch|8|cpu=8,mem=32G|00:07:30|8G|4G|gres/gpuutil=50|gres/gpumem=4G";
        let details = merge_running(previous, job, sample);
        assert_eq!(details.source, "sstat");
        assert_eq!(details.cpu_efficiency, Some(75.0));
        assert_eq!(details.memory_efficiency, Some(25.0));
        assert_eq!(details.gpu_utilization, Some(50.0));
    }

    #[test]
    fn early_running_metrics_are_described_without_false_zero_or_gpu_warning() {
        let details = JobDetails {
            state: "RUNNING".into(),
            gpus: 0,
            ..JobDetails::default()
        };
        assert_eq!(cpu_percent(&details), "collecting…");
        assert_eq!(gpu_usage(&details), "none allocated");
    }

    #[test]
    fn memory_peak_remains_useful_when_no_allocation_limit_exists() {
        let details = JobDetails {
            max_rss_bytes: 11 * 1024 * 1024 * 1024,
            ..JobDetails::default()
        };
        assert_eq!(memory_usage(&details), "11.0 GiB peak (limit unknown)");
    }

    #[test]
    fn accounting_falls_back_to_requested_memory_without_inventing_gpus() {
        let line = "3206866|cpu-only|COMPLETED|None|debug|acct|normal|sub|start|end|00:01:11|71|02:00:00|1|16|16|16|3905M|626744K||billing=16,cpu=16,node=1|billing=16,cpu=16,mem=3905M,node=1|00:11:51|1136|0:0|node-a100||";
        let parsed = parse_accounting(line, "cispa", "3206866").unwrap();
        assert_eq!(parsed.memory_bytes, 3_905 * 1024 * 1024);
        assert_eq!(parsed.gpus, 0, "node names must not imply GPU allocation");
        assert_eq!(parsed.gpu_types, "");
        assert!(parsed.terminal);
    }

    #[test]
    fn live_scontrol_supplies_details_without_accounting() {
        let job = Job {
            cluster: "sprint".into(),
            id: "42".into(),
            name: "queued-name".into(),
            state: "RUNNING".into(),
            elapsed: "0:30".into(),
            ..Job::default()
        };
        let parsed = parse_live_control(
            "JobId=42 JobName=train JobState=RUNNING Reason=None RunTime=00:01:30 TimeLimit=02:00:00 NumNodes=1 NumCPUs=16 Partition=gpu NodeList=node-1 Account=lab QOS=normal SubmitTime=2026-08-11T12:00:00 StartTime=2026-08-11T12:01:00 EndTime=2026-08-11T14:01:00 ReqTRES=cpu=16,mem=64G,gres/gpu=2 AllocTRES=cpu=16,mem=64G,gres/gpu=2 ExitCode=0:0",
            job,
        )
        .unwrap();
        assert_eq!(parsed.name, "train");
        assert_eq!(parsed.cpus, 16);
        assert_eq!(parsed.gpus, 2);
        assert_eq!(parsed.memory_bytes, 64 * 1024 * 1024 * 1024);
        assert_eq!(parsed.elapsed_seconds, 90);
        assert_eq!(parsed.source, "scontrol");
    }

    #[test]
    fn live_control_deduplicates_typed_gpus_and_ignores_default_memory_sentinel() {
        let job = Job {
            cluster: "cispa".into(),
            id: "3210715".into(),
            state: "RUNNING".into(),
            ..Job::default()
        };
        let parsed = parse_live_control(
            "JobId=3210715 JobName=train JobState=RUNNING NumNodes=1 NumCPUs=8 \
             ReqTRES=cpu=8,mem=1M,gres/gpu=1,gres/gpu:a100=1 \
             AllocTRES=cpu=8,gres/gpu=1,gres/gpu:a100=1",
            job,
        )
        .unwrap();
        assert_eq!(parsed.gpus, 1);
        assert_eq!(parsed.gpu_types, "a100");
        assert_eq!(parsed.memory_bytes, 0);
        assert_eq!(parsed.requested_memory, "");
    }

    #[test]
    fn running_accounting_lag_does_not_report_false_zero_cpu() {
        let line = "7|train|RUNNING|None|gpu|acct|normal|sub|start|Unknown|00:00:10|10|01:00:00|1|8|8|8|4G|||cpu=8,mem=4G|cpu=8,mem=4G|00:00:00|80|0:0|node1||";
        let parsed = parse_accounting(line, "cispa", "7").unwrap();
        assert_eq!(parsed.cpu_efficiency, None);
        assert_eq!(cpu_percent(&parsed), "collecting…");
        assert!(!parsed.terminal);
    }

    #[test]
    fn running_sample_aggregates_steps_and_ignores_other_jobs() {
        let previous = JobDetails {
            id: "42".into(),
            cpus: 4,
            memory_bytes: 8 * 1024 * 1024 * 1024,
            ..JobDetails::default()
        };
        let job = Job {
            id: "42".into(),
            state: "RUNNING".into(),
            elapsed: "00:10:00".into(),
            ..Job::default()
        };
        let samples = "42.batch|2|cpu=2|00:02:00|1G||gres/gpuutil=20|gres/gpumem=2G\n\
                       42.0|2|cpu=2|00:03:00|3G||gres/gpuutil=80|gres/gpumem=4G\n\
                       99.batch|99|cpu=99|01:00:00|99G||gres/gpuutil=99|gres/gpumem=99G";
        let parsed = merge_running(previous, job, samples);
        assert_eq!(parsed.total_cpu_seconds, 600);
        assert_eq!(parsed.max_rss_bytes, 3 * 1024 * 1024 * 1024);
        assert_eq!(parsed.gpu_utilization, Some(80.0));
        assert_eq!(parsed.gpu_memory_bytes, Some(4 * 1024 * 1024 * 1024));
    }

    #[test]
    fn malformed_metrics_are_rejected_and_history_stays_bounded() {
        for value in ["", "Unknown", "1:2:3:4", "-1", "1XB", "NaN G"] {
            assert!(parse_duration(value).is_none() || parse_bytes(value).is_none());
        }
        assert!(parse_accounting("garbage\n||||", "cispa", "42").is_none());
        let mut history = VecDeque::new();
        for sample in 0..100 {
            push_sample(&mut history, sample as f64);
        }
        assert_eq!(history.len(), 40);
        assert_eq!(history.front(), Some(&60.0));
        assert_eq!(history.back(), Some(&99.0));
    }

    #[test]
    fn slurm_time_sentinels_and_impossible_cpu_samples_are_not_displayed() {
        assert_eq!(parse_duration("18446744073709551614"), None);
        assert_eq!(parse_duration("18446744073709551615:59:59"), None);
        let previous = JobDetails {
            id: "42".into(),
            state: "RUNNING".into(),
            cpus: 16,
            elapsed_seconds: 24,
            total_cpu_seconds: 120,
            cpu_efficiency: Some(31.25),
            ..JobDetails::default()
        };
        let job = Job {
            id: "42".into(),
            state: "RUNNING".into(),
            elapsed: "0:24".into(),
            ..Job::default()
        };
        let sentinel = "42.batch|16|cpu=16|18446744073709551614|1G||||";
        let parsed = merge_running(previous, job, sentinel);
        assert_eq!(parsed.total_cpu_seconds, 120);
        assert_eq!(parsed.cpu_efficiency, Some(31.25));

        let corrupt = JobDetails {
            state: "RUNNING".into(),
            cpu_efficiency: Some(4_803_839_602_528_559.0),
            ..JobDetails::default()
        };
        assert_eq!(cpu_percent(&corrupt), "collecting…");
    }

    #[test]
    #[ignore = "release-mode performance budget"]
    fn parses_large_accounting_snapshot_within_budget() {
        let main = "42|train|COMPLETED|None|gpu|acct|normal|sub|start|end|00:10:00|600|01:00:00|1|8|8|8|32G|||cpu=8,mem=32G,gres/gpu:a100=2|cpu=8,mem=32G,gres/gpu:a100=2|01:00:00|4800|0:0|node1||";
        let mut input = String::with_capacity(1_000_000);
        input.push_str(main);
        input.push('\n');
        for step in 0..5_000 {
            input.push_str(&format!(
                "42.{step}|step|COMPLETED||||||||||||||||1G||||00:01:00|||||\n"
            ));
        }
        let started = Instant::now();
        let parsed = parse_accounting(&input, "cispa", "42").unwrap();
        assert_eq!(parsed.max_rss_bytes, 1024 * 1024 * 1024);
        assert!(started.elapsed() < Duration::from_millis(75));
    }
}
