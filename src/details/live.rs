fn sample_running(
    config: &Config,
    cluster: &str,
    id: &str,
    _authorized: Job,
    previous: &JobDetails,
) -> Result<JobDetails> {
    // Sstat has no returned owner field. Repeat the exact owner query
    // immediately before this supplemental metadata access and bind the
    // rendered state to that fresh object.
    let job = crate::slurm::authorize_exact_job(config, cluster, id)?;
    let fields = "JobID,NTasks,AveCPU,MaxRSS,AveRSS,TRESUsageInAve,TRESUsageInMax";
    let cluster_option = crate::slurm::accounting_cluster_option(config, cluster)?;
    let command = format!(
        "sstat{cluster_option} -a -j {} -n -P --format={} 2>/dev/null",
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
        if fields.len() < 7 || fields[0].split('.').next().unwrap_or(fields[0]) != details.id {
            continue;
        }
        let tasks = number(fields[1]).unwrap_or(1);
        if let Some(cpu) = parse_duration(fields[2]).and_then(|value| value.checked_mul(tasks)) {
            total_cpu = Some(total_cpu.unwrap_or(0).saturating_add(cpu));
        }
        max_rss = max_rss.max(parse_bytes(fields[3]).unwrap_or(0));
        for value in [fields[5], fields[6]] {
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
    let output = crate::slurm::control_job_text(config, &job.cluster, &job.id)?;
    // This raw controller response is security-sensitive: the fresh squeue
    // authorization above cannot justify parsing fields from a different job
    // if an ID is reused or a controller response is mismatched.
    crate::slurm::validate_control_identity(config, &job.cluster, &job.id, &output)?;
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
    if gpus == 0 {
        for fallback in [
            value("TresPerNode"),
            value("TresPerJob"),
            value("TresPerTask"),
            value("AllocGres"),
            value("Gres"),
            value("ReqGres"),
        ]
        .into_iter()
        .flatten()
        {
            let (count, types) = parse_gpus(fallback);
            if count > 0 {
                gpus = count;
                gpu_types = types;
                break;
            }
        }
    }
    let elapsed = value("RunTime").unwrap_or(&job.elapsed).to_string();
    Some(JobDetails {
        cluster: job.cluster,
        id: job.id,
        name: crate::model::terminal_text(value("JobName").unwrap_or(&job.name)),
        state: crate::model::terminal_text(value("JobState").unwrap_or(&job.state)),
        reason: crate::model::terminal_text(value("Reason").unwrap_or(&job.reason)),
        partition: crate::model::terminal_text(value("Partition").unwrap_or(&job.partition)),
        account: crate::model::terminal_text(value("Account").unwrap_or("")),
        qos: crate::model::terminal_text(value("QOS").unwrap_or("")),
        submit: crate::model::terminal_text(value("SubmitTime").unwrap_or("")),
        start: crate::model::terminal_text(value("StartTime").unwrap_or(&job.start_time)),
        end: crate::model::terminal_text(value("EndTime").unwrap_or("")),
        elapsed_seconds: parse_duration(&elapsed).unwrap_or(0),
        elapsed: crate::model::terminal_text(&elapsed),
        time_limit: crate::model::terminal_text(value("TimeLimit").unwrap_or("")),
        nodes,
        cpus,
        requested_cpus,
        memory_bytes,
        requested_memory: crate::model::terminal_text(&display_requested_memory(tres_value(req_tres, "mem").unwrap_or(""))),
        gpus,
        gpu_types: crate::model::terminal_text(&gpu_types),
        alloc_tres: crate::model::terminal_text(alloc_tres),
        req_tres: crate::model::terminal_text(req_tres),
        node_list: crate::model::terminal_text(value("NodeList").unwrap_or("")),
        exit_code: crate::model::terminal_text(value("ExitCode").unwrap_or("")),
        source: "scontrol".into(),
        sampled_at: timestamp(),
        terminal: false,
        ..JobDetails::default()
    })
}
