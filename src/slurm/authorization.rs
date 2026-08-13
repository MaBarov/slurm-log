/// Fail closed unless the exact job belongs to the configured Slurm owner.
pub fn authorize_exact_job(config: &Config, cluster: &str, id: &str) -> Result<Job> {
    if !valid_job_id(id) {
        bail!("invalid job ID {id}");
    }
    // Authorization is never satisfied from the shared queue cache.  This
    // dedicated query returns the scheduler's user column and checks it
    // explicitly, rather than merely trusting a `-u` filter to have been
    // applied by an arbitrary/lagging controller response.
    if let Some(job) = query_exact_queued(config, cluster, id)? {
        return Ok(job);
    }
    let target = config.cluster(cluster)?;
    if !target.accounting {
        bail!("job {cluster}:{id} is not an active job owned by the configured user");
    }
    query_exact_accounting(config, cluster, id)?.context(format!(
        "job {cluster}:{id} is not owned by the configured Slurm user"
    ))
}

/// Query a job's controller record on the same explicitly bound controller
/// used for exact authorization.
pub(crate) fn control_job_text(config: &Config, cluster: &str, id: &str) -> Result<String> {
    scheduler_text(config, cluster, "scontrol", &["show", "job", "-o", id])
}

/// Safe command fragment for commands nested inside `sh -c`. Those commands
/// bypass `scheduler_text`'s argv injection and need an explicit controller.
pub(crate) fn accounting_cluster_option(config: &Config, cluster: &str) -> Result<String> {
    let option = controller_option(config, cluster)?;
    Ok(if option.is_empty() {
        String::new()
    } else {
        format!(" {option}")
    })
}

/// Fresh, exact squeue authorization with a returned owner field.  `squeue`
/// normally omits the user name from the lightweight list query, so this must
/// not share the cached rendering format.
fn query_exact_queued(config: &Config, cluster: &str, id: &str) -> Result<Option<Job>> {
    let target = config.cluster(cluster)?;
    let mut args = vec!["-h", "-u", target.user.as_str(), "-j", id];
    // Keep the user immediately after JobId so this parser cannot silently
    // mistake a changed squeue output layout for an ownership grant.
    args.extend(["-o", "%i|%u|%T|%j|%M|%R|%P|%S|%Q|%o"]);
    let value = scheduler_text(config, cluster, "squeue", &args)?;
    Ok(parse_exact_queued_response(&value, cluster, id, &target.user))
}

fn parse_exact_queued_response(
    value: &str,
    cluster: &str,
    id: &str,
    owner: &str,
) -> Option<Job> {
    for line in value.lines() {
        let fields: Vec<_> = line.split('|').map(str::trim).collect();
        if fields.len() != 10 || fields[0] != id || fields[1] != owner {
            continue;
        }
        // Feed the existing bounded renderer parser its canonical nine-field
        // shape after ownership has been validated from the raw response.
        let canonical = std::iter::once(fields[0])
            .chain(fields[2..].iter().copied())
            .collect::<Vec<_>>()
            .join("|");
        if let Some(job) = parse_queue(&canonical, cluster).into_iter().next()
            && job.id == id
        {
            return Some(job);
        }
    }
    None
}

/// Fresh, exact sacct authorization for terminal jobs.  The user and cluster
/// columns are part of the returned record and must both agree before an
/// accounting row is allowed to authorize a later log/details read.
fn query_exact_accounting(config: &Config, cluster: &str, id: &str) -> Result<Option<Job>> {
    let target = config.cluster(cluster)?;
    let cluster_option = accounting_cluster_option(config, cluster)?;
    let command = format!(
        "sacct -X{cluster_option} -j {} -u {} -n -P --format=JobID,User,State,JobName,Elapsed,End,ExitCode,MaxRSS,AllocTRES,Partition,Cluster 2>/dev/null",
        shell_quote(id),
        shell_quote(&target.user)
    );
    let value = scheduler_text(config, cluster, "sh", &["-c", &command])?;
    Ok(parse_exact_accounting_response(
        &value,
        cluster,
        id,
        &target.user,
        target.binds_controller().then(|| target.controller()),
    ))
}

fn parse_exact_accounting_response(
    value: &str,
    cluster: &str,
    id: &str,
    owner: &str,
    expected_cluster: Option<&str>,
) -> Option<Job> {
    for line in value.lines() {
        let fields: Vec<_> = line.split('|').map(str::trim).collect();
        if fields.len() != 11
            || fields[0] != id
            || fields[1] != owner
            || expected_cluster.is_some_and(|name| fields[10] != name)
        {
            continue;
        }
        let canonical = [
            fields[0], fields[2], fields[3], fields[4], fields[5], fields[6], fields[7],
            fields[8], fields[9],
        ]
        .join("|");
        if let Some(job) = parse_recent(&canonical, cluster).into_iter().next()
            && job.id == id
        {
            return Some(job);
        }
    }
    None
}
