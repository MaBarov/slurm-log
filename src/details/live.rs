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
