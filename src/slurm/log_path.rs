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
