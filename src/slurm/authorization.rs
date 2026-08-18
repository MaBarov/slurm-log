/// Typed lookup failure so MCP can report a machine-readable `error_type`
/// instead of a raw scheduler stderr cascade.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExactJobError {
    /// The scheduler RPC itself failed (unreachable host, broken SSH, etc.).
    Scheduler(String),
    /// The job is not an active job owned by the configured user and the
    /// target has no accounting to consult.
    NotFound,
    /// The accounting record does not exist or is not owned by the
    /// configured user.
    NotOwned,
    /// The requested identity is syntactically invalid.
    Invalid,
}

impl ExactJobError {
    pub fn kind(&self) -> &'static str {
        match self {
            ExactJobError::Scheduler(_) => "scheduler_unreachable",
            ExactJobError::NotFound => "job_not_found",
            ExactJobError::NotOwned => "job_not_owned",
            ExactJobError::Invalid => "invalid_request",
        }
    }
}

impl std::fmt::Display for ExactJobError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExactJobError::Scheduler(error) => write!(formatter, "{error}"),
            ExactJobError::NotFound => formatter.write_str(
                "job is not an active job owned by the configured user and accounting is disabled",
            ),
            ExactJobError::NotOwned => {
                formatter.write_str("job is not owned by the configured Slurm user")
            }
            ExactJobError::Invalid => formatter.write_str("invalid job ID"),
        }
    }
}

impl std::error::Error for ExactJobError {}

/// Fail closed unless the exact job belongs to the configured Slurm owner.
pub fn authorize_exact_job(config: &Config, cluster: &str, id: &str) -> Result<Job> {
    authorize_exact_job_typed(config, cluster, id).map_err(anyhow::Error::new)
}

/// Typed variant of `authorize_exact_job` for tools that classify failures.
pub fn authorize_exact_job_typed(
    config: &Config,
    cluster: &str,
    id: &str,
) -> std::result::Result<Job, ExactJobError> {
    if !valid_job_id(id) {
        return Err(ExactJobError::Invalid);
    }
    // Authorization is never satisfied from the shared queue cache.  This
    // dedicated query returns the scheduler's user column and checks it
    // explicitly, rather than merely trusting a `-u` filter to have been
    // applied by an arbitrary/lagging controller response.
    match query_exact_queued(config, cluster, id) {
        Ok(Some(job)) => return Ok(job),
        Ok(None) => {}
        Err(error) => return Err(ExactJobError::Scheduler(format!("{error:#}"))),
    }
    let target = config.cluster(cluster).map_err(|error| ExactJobError::Scheduler(format!("{error:#}")))?;
    if !target.accounting {
        return Err(ExactJobError::NotFound);
    }
    match query_exact_accounting(config, cluster, id) {
        Ok(Some(job)) => Ok(job),
        Ok(None) => Err(ExactJobError::NotOwned),
        Err(error) => Err(ExactJobError::Scheduler(format!("{error:#}"))),
    }
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
    let value = match scheduler_text(config, cluster, "squeue", &args) {
        Ok(value) => value,
        Err(error) => {
            let message = format!("{error:#}");
            if message.contains("Invalid job id") || message.contains("slurm_load_jobs error") {
                return Ok(None);
            }
            return Err(error);
        }
    };
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
