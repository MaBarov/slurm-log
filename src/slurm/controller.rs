pub(crate) fn scheduler_text(
    config: &Config,
    cluster: &str,
    program: &str,
    args: &[&str],
) -> Result<String> {
    let target = config.cluster(cluster)?;
    let bound = controller_bound_args(target, program, args);
    let bound_refs: Vec<_> = bound.iter().map(String::as_str).collect();
    if target.remote() {
        let command = remote_scheduler_command(program, &bound_refs, None);
        ssh(&target.ssh_host, &command)
    } else {
        text(program, &bound_refs)
    }
}

fn controller_bound_args(
    target: &crate::config::ClusterConfig,
    program: &str,
    args: &[&str],
) -> Vec<String> {
    let mut bound = args
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    if !target.binds_controller() {
        return bound;
    }
    match program {
        // scontrol treats this as a global option; keep it before the
        // subcommand so the controller choice cannot be parsed as a job-show
        // argument on older Slurm releases.
        "scontrol" => {
            let mut with_controller = vec!["--cluster".into(), target.controller().into()];
            with_controller.append(&mut bound);
            with_controller
        }
        "squeue" | "sacct" | "sbatch" | "scancel" => {
            bound.extend(["--clusters".into(), target.controller().into()]);
            bound
        }
        _ => bound,
    }
}

/// Shell-based scheduler queries use this explicit option rather than an
/// inherited SLURM_CLUSTERS environment variable. The string is shell-quoted
/// because it is inserted only into internally constructed command text.
pub(crate) fn controller_option(config: &Config, cluster: &str) -> Result<String> {
    let target = config.cluster(cluster)?;
    Ok(if target.binds_controller() {
        format!("--clusters {}", shell_quote(target.controller()))
    } else {
        String::new()
    })
}
