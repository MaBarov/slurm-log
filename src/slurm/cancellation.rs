/// Re-read one active job and prove that a cancellation addresses either an
/// ordinary job or exactly one array task. A bare array-master ID is never a
/// safe substitute for an array task: `scancel <master>` has array-wide
/// semantics on Slurm, so it is rejected even when it is otherwise queued and
/// owned by the configured user.
pub fn fresh_cancellable_job(config: &Config, cluster: &str, id: &str) -> Result<Job> {
    if !valid_job_id(id) {
        bail!("invalid job ID {id}");
    }
    let queued = query_queued(config, cluster, Some(id))?
        .into_iter()
        .find(|job| job.id == id)
        .context("job is not active")?;
    if !queued.active() {
        bail!("job is no longer active");
    }
    let target = config.cluster(cluster)?;
    let metadata = CancelMetadata::from_scontrol(&scheduler_text(
        config,
        cluster,
        "scontrol",
        &["show", "job", "-o", id],
    )?)?;
    if metadata.id != id {
        bail!(
            "fresh scheduler metadata identified job {:?}, not requested job {id:?}",
            metadata.id
        );
    }
    if metadata.owner != target.user {
        bail!(
            "fresh scheduler metadata says job {id} belongs to {:?}, not configured user {:?}",
            metadata.owner,
            target.user
        );
    }
    let structured = Job {
        cluster: cluster.into(),
        id: metadata.id.clone(),
        state: terminal_text(&metadata.state),
        name: terminal_text(&metadata.name),
        ..Job::default()
    };
    if !structured.active() {
        bail!("job is no longer active");
    }
    if queued.name != structured.name || queued.state != structured.state {
        bail!("fresh queue and controller metadata disagree about job name or state");
    }
    metadata.prove_exact_cancel_scope(id)?;
    Ok(structured)
}

#[derive(Debug)]
struct CancelMetadata {
    id: String,
    owner: String,
    name: String,
    state: String,
    array_job_id: Option<String>,
    array_task_id: Option<String>,
}

impl CancelMetadata {
    fn from_scontrol(value: &str) -> Result<Self> {
        let id = scontrol_value(value, "JobId=")
            .context("fresh controller metadata has no JobId")?
            .to_string();
        let owner_field =
            scontrol_value(value, "UserId=").context("fresh controller metadata has no UserId")?;
        let owner = owner_field
            .split_once('(')
            .map_or(owner_field, |(name, _)| name)
            .to_string();
        let name = scontrol_value(value, "JobName=")
            .context("fresh controller metadata has no JobName")?
            .to_string();
        let state = scontrol_value(value, "JobState=")
            .context("fresh controller metadata has no JobState")?
            .to_string();
        let array_job_id = scontrol_value(value, "ArrayJobId=")
            .filter(|value| !matches!(*value, "" | "0" | "N/A" | "(null)"))
            .map(str::to_string);
        let array_task_id = scontrol_value(value, "ArrayTaskId=")
            .filter(|value| !matches!(*value, "" | "N/A" | "(null)"))
            .map(str::to_string);
        Ok(Self {
            id,
            owner,
            name,
            state,
            array_job_id,
            array_task_id,
        })
    }

    fn prove_exact_cancel_scope(&self, requested: &str) -> Result<()> {
        match (requested.split_once('_'), self.array_job_id.as_deref()) {
            (None, None) => Ok(()),
            (None, Some(master)) => bail!(
                "job {requested} is array master {master}; cancel one exact array task such as {master}_N"
            ),
            (Some(_), None) => bail!(
                "job {requested} is not proven to be an exact array task by fresh controller metadata"
            ),
            (Some((master, task)), Some(array_master)) => {
                let exact_task = self
                    .array_task_id
                    .as_deref()
                    .filter(|value| value.bytes().all(|byte| byte.is_ascii_digit()));
                if master != array_master || exact_task != Some(task) || self.id != requested {
                    bail!(
                        "job {requested} is not proven to be one exact array task by fresh controller metadata"
                    );
                }
                Ok(())
            }
        }
    }
}

fn scontrol_value<'a>(value: &'a str, key: &str) -> Option<&'a str> {
    value
        .split_whitespace()
        .find_map(|field| field.strip_prefix(key))
}
