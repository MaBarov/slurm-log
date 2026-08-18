pub fn terminal_path(config: &Config, cluster: &str, id: &str) -> Result<(Option<String>, String)> {
    let authorized = authorize_exact_job(config, cluster, id)?;
    terminal_path_authorized(config, cluster, id, &authorized)
}

/// Resolve terminal metadata immediately after a fresh owner authorization.
/// Callers that already performed that fresh check use this to avoid a second
/// scheduler round trip while retaining validation of every returned field.
pub(crate) fn terminal_path_authorized(
    config: &Config,
    cluster: &str,
    id: &str,
    authorized: &Job,
) -> Result<(Option<String>, String)> {
    if !valid_job_id(id) || authorized.cluster != cluster || authorized.id != id {
        bail!("invalid or mismatched authorized job {cluster}:{id}");
    }
    let metadata = if let Ok(value) = control_job_text(config, cluster, id) {
        active_terminal_metadata(config, cluster, id, &value)?
    } else {
        let target = config.cluster(cluster)?;
        let cluster_option = accounting_cluster_option(config, cluster)?;
        if !target.accounting {
            bail!("job {cluster}:{id} is no longer active and accounting is unavailable");
        }
        let command = format!(
            "sacct -X{cluster_option} -j {} -u {} --format=JobIDRaw,JobID,User,JobName,StdOut,Cluster -n -P 2>/dev/null | awk 'NF {{print; exit}}'",
            shell_quote(id),
            shell_quote(&target.user),
        );
        let value = scheduler_text(config, cluster, "sh", &["-c", &command])?;
        accounting_terminal_metadata(config, cluster, id, &value)?
    };
    let (path, name) = resolve_terminal_metadata(metadata);
    Ok((path, terminal_text(&name)))
}

type TerminalMetadata = (String, String, String, String);

fn active_terminal_metadata(
    config: &Config,
    cluster: &str,
    id: &str,
    value: &str,
) -> Result<TerminalMetadata> {
    validate_control_identity(config, cluster, id, value)?;
    let name = token(value, "JobName=").unwrap_or("job").to_string();
    let path = usable_stdout(token(value, "StdOut="))
        .unwrap_or_default()
        .to_string();
    Ok((id.to_string(), id.to_string(), name, path))
}

fn accounting_terminal_metadata(
    config: &Config,
    cluster: &str,
    id: &str,
    value: &str,
) -> Result<TerminalMetadata> {
    let fields: Vec<_> = value.trim().splitn(6, '|').collect();
    if fields.len() != 6 {
        bail!("no stdout metadata for {cluster} job {id}");
    }
    let target = config.cluster(cluster)?;
    if !job_id_matches(fields[1], id)
        || fields[2].trim() != target.user
        || (target.binds_controller() && fields[5].trim() != target.controller())
    {
        bail!("accounting metadata identity does not match {cluster}:{id}");
    }
    Ok((
        fields[0].into(),
        fields[1].into(),
        fields[3].into(),
        fields[4].into(),
    ))
}

/// Verify that `scontrol` did not return metadata for a different user or
/// job after a fresh authorization decision.  A configured cluster selects
/// the controller; when scontrol includes a cluster token it must agree too.
pub(crate) fn validate_control_identity(
    config: &Config,
    cluster: &str,
    id: &str,
    value: &str,
) -> Result<()> {
    let target = config.cluster(cluster)?;
    let returned_id = token(value, "JobId=")
        .or_else(|| token(value, "JobID="))
        .context("scontrol response omitted JobId")?;
    if !control_job_id_matches(value, returned_id, id) {
        bail!("scontrol response job ID does not match {cluster}:{id}");
    }
    let owner = token(value, "UserId=")
        .or_else(|| token(value, "UserID="))
        .map(|value| value.split('(').next().unwrap_or(value))
        .context("scontrol response omitted UserId")?;
    if owner != target.user {
        bail!("scontrol response owner does not match configured user");
    }
    if target.binds_controller()
        && let Some(returned_cluster) = token(value, "ClusterName=").or_else(|| token(value, "Cluster="))
        && returned_cluster != target.controller()
    {
        bail!("scontrol response cluster does not match configured cluster");
    }
    Ok(())
}

fn control_job_id_matches(value: &str, returned_id: &str, wanted: &str) -> bool {
    if job_id_matches(returned_id, wanted) {
        return true;
    }
    if let Some((wanted_master, wanted_task)) = wanted.split_once('_') {
        let master_matches = token(value, "ArrayJobId=")
            .map(|array_master| array_master == wanted_master)
            .unwrap_or_else(|| job_id_matches(returned_id, wanted_master));
        if master_matches
            && let Some(array_task) = token(value, "ArrayTaskId=")
            && array_task == wanted_task
        {
            return true;
        }
    }
    false
}

pub(crate) fn job_id_matches(returned: &str, wanted: &str) -> bool {
    returned.split('.').next().unwrap_or(returned) == wanted
}

fn resolve_terminal_metadata(metadata: TerminalMetadata) -> (Option<String>, String) {
    let (raw, logical, name, template) = metadata;
    let logical = logical.split('.').next().unwrap_or(&logical);
    let (master, task) = logical.split_once('_').unwrap_or((logical, "4294967294"));
    if usable_stdout(Some(&template)).is_none() {
        return (None, name);
    }
    (
        Some(expand_path(
            &template,
            &name,
            raw.split('.').next().unwrap_or(&raw),
            master,
            task,
        )),
        name,
    )
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
        && let Ok(controller) = controller_option(config, &job.cluster)
    {
        let command = format!(
            "sacct {} -X -j {} -n -P --format=JobID,State,JobName,Elapsed,End,ExitCode,MaxRSS,AllocTRES,Partition 2>/dev/null | awk 'NF {{print; exit}}'",
            controller,
            shell_quote(&job.id)
        );
        if let Ok(value) = scheduler_text(config, &job.cluster, "sh", &["-c", &command])
            && let Some(found) = parse_recent(&value, &job.cluster).into_iter().next()
        {
            return found;
        }
    }
    let mut details = job.clone();
    if let Ok(value) = control_job_text(config, &job.cluster, &job.id)
        && validate_control_identity(config, &job.cluster, &job.id, &value).is_ok()
    {
        apply_control_details(&mut details, &value);
    }
    details
}

fn apply_control_details(details: &mut Job, value: &str) {
    details.state = token(value, "JobState=").unwrap_or(&details.state).into();
    details.exit_code = token(value, "ExitCode=").unwrap_or("").into();
    details.partition = token(value, "Partition=").unwrap_or("").into();
    details.reason = token(value, "Reason=").unwrap_or(&details.reason).into();
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
