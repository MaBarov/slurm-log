// Submit, cancel, and verified-cancel for the sbatch bank.
//
// This file is an include! stream into bank.rs.  Keep `//` comments only (no
// doc comments that would confuse `#![forbid(rustdoc::broken_intra_doc_links)]`
// downstream), no top-level `use` items (the parent supplies them), and no
// test module (tests live in other streams).

pub fn submit(config: &Config, script: &Script, cluster: &str) -> Result<Job> {
    let target = config.cluster(cluster)?;
    validate_script_controller(script, target)?;
    let mut args = vec!["--parsable"];
    if target.binds_controller() {
        args.extend(["--clusters", target.controller()]);
    }
    let output = if target.remote() {
        let remote = remote_scheduler_command("sbatch", &args, Some(&target.working_directory));
        ssh_with_input(&target.ssh_host, &remote, &script.bytes)?
    } else {
        text_with_input(
            "sbatch",
            &args,
            &script.bytes,
            Some(&target.working_directory),
        )?
    };
    let mut parts = output.trim().split(';');
    let id = parts.next().unwrap_or("");
    if !valid_job_id(id) {
        bail!("sbatch returned an invalid job ID: {:?}", output.trim());
    }
    let actual_controller = parts.next();
    if target.remote() && target.binds_controller() && actual_controller.is_none() {
        bail!(
            "remote sbatch did not return a controller identity for configured controller {:?}",
            target.controller()
        );
    }
    if target.binds_controller()
        && let Some(actual_controller) = actual_controller
        && actual_controller != target.controller()
    {
        bail!(
            "sbatch submitted to controller {actual_controller:?}, not configured controller {:?}",
            target.controller()
        );
    }
    if parts.next().is_some() {
        bail!("sbatch returned malformed parsable output: {:?}", output.trim());
    }
    crate::slurm::invalidate_caches(config);
    Ok(Job {
        cluster: cluster.into(),
        id: id.into(),
        state: "PENDING".into(),
        name: script.name.clone(),
        ..Job::default()
    })
}

pub fn cancel(config: &Config, jobs: &[Job]) -> Result<Vec<String>> {
    let mut checked = Vec::new();
    for job in jobs.iter().filter(|job| job.active()) {
        if !valid_job_id(&job.id) {
            bail!("invalid job ID {}", job.id);
        }
        let fresh = crate::slurm::fresh_cancellable_job(config, &job.cluster, &job.id)?;
        if !job.name.is_empty() && job.name != fresh.name {
            bail!(
                "job {}:{} changed name before cancellation",
                job.cluster,
                job.id
            );
        }
        checked.push(fresh);
    }
    cancel_verified(config, &checked)
}

// Dispatch only jobs that have just passed `fresh_cancellable_job`.  MCP
// cancellation invokes that check itself to compare the user-supplied name,
// while the CLI and picker enter through `cancel` above.
pub(crate) fn cancel_verified(config: &Config, jobs: &[Job]) -> Result<Vec<String>> {
    let mut grouped: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for job in jobs.iter().filter(|job| job.active()) {
        if !valid_job_id(&job.id) {
            bail!("invalid job ID {}", job.id);
        }
        grouped.entry(&job.cluster).or_default().push(&job.id);
    }
    let mut failures = Vec::new();
    for (cluster, ids) in grouped {
        let target = config.cluster(cluster)?;
        let mut args = Vec::new();
        if target.binds_controller() {
            args.extend(["--clusters", target.controller()]);
        }
        args.extend(ids);
        let result = if target.remote() {
            let command = remote_scheduler_command("scancel", &args, None);
            crate::command::ssh(&target.ssh_host, &command).map(|_| ())
        } else {
            text("scancel", &args).map(|_| ())
        };
        if let Err(error) = result {
            failures.push(format!("{cluster}: {error:#}"));
        }
    }
    crate::slurm::invalidate_caches(config);
    Ok(failures)
}
