fn draw(
    details: &JobDetails,
    compact: bool,
    paused: bool,
    activity: &str,
    cpu: &VecDeque<f64>,
    memory: &VecDeque<f64>,
    gpu: &VecDeque<f64>,
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
    let state_text = if details.state.starts_with("PENDING") {
        let explanation = crate::model::pending_explanation(&details.reason);
        if explanation.is_empty() || explanation == "pending" {
            format!("{}  {}", details.state, clean(&details.reason))
        } else {
            format!("{}  {} ({})", details.state, explanation, clean(&details.reason))
        }
    } else {
        format!("{}  {}", details.state, clean(&details.reason))
    };
    line(&mut out, "State", &state_text)?;
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
        if details.gpus > 0 && details.gpu_utilization.is_some() {
            line(&mut out, "GPU trend", &spark(gpu))?;
        }
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
        format!("{} peak", bytes(details.max_rss_bytes))
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
    spark_padded(values, 30)
}


fn spark_padded(values: &VecDeque<f64>, width: usize) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if width == 0 {
        return String::new();
    }
    let count = values.len().min(width);
    let padding = width.saturating_sub(count);
    let mut result = String::with_capacity(width * 4);
    for _ in 0..padding {
        result.push('·');
    }
    for &value in values.iter().skip(values.len() - count) {
        let index = ((value.clamp(0.0, 100.0) * 0.07).round() as usize).min(7);
        result.push(BARS[index]);
    }
    result
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
