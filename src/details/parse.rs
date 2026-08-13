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
        name: crate::model::terminal_text(main.get(1).copied().unwrap_or("")),
        state: crate::model::terminal_text(&state),
        reason: crate::model::terminal_text(main.get(3).copied().unwrap_or("")),
        partition: crate::model::terminal_text(main.get(4).copied().unwrap_or("")),
        account: crate::model::terminal_text(main.get(5).copied().unwrap_or("")),
        qos: crate::model::terminal_text(main.get(6).copied().unwrap_or("")),
        submit: crate::model::terminal_text(main.get(7).copied().unwrap_or("")),
        start: crate::model::terminal_text(main.get(8).copied().unwrap_or("")),
        end: crate::model::terminal_text(main.get(9).copied().unwrap_or("")),
        elapsed: crate::model::terminal_text(main.get(10).copied().unwrap_or("")),
        elapsed_seconds,
        time_limit: crate::model::terminal_text(main.get(12).copied().unwrap_or("")),
        nodes: number(main.get(13).copied().unwrap_or("")).unwrap_or(0),
        cpus,
        requested_cpus,
        memory_bytes,
        requested_memory: crate::model::terminal_text(&display_requested_memory(main.get(17).copied().unwrap_or(""))),
        max_rss_bytes: max_rss,
        gpus,
        gpu_types: crate::model::terminal_text(&gpu_types),
        gpu_utilization,
        gpu_memory_bytes,
        total_cpu_seconds: total_cpu,
        cpu_efficiency,
        memory_efficiency,
        alloc_tres: crate::model::terminal_text(alloc_tres),
        req_tres: crate::model::terminal_text(req_tres),
        node_list: crate::model::terminal_text(main.get(25).copied().unwrap_or("")),
        exit_code: crate::model::terminal_text(main.get(24).copied().unwrap_or("")),
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
