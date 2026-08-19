use anyhow::{Result, bail};
use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::OsStr,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    config::Config,
    model::{Job, Pane},
};

fn tmux<I, S>(args: I) -> Result<std::process::Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Ok(Command::new("tmux").args(args).output()?)
}

fn watcher(config: &Config, job: &Job, lines: usize, show_log_warnings: bool) -> Vec<String> {
    let mut values = vec![config.executable.display().to_string()];
    values.extend(config.child_args());
    values.extend([
        "--pane-follow".into(),
        "--lines".into(),
        lines.to_string(),
        "--initial-state".into(),
        job.state.clone(),
    ]);
    if !job.reason.is_empty() {
        values.extend(["--reason".into(), job.reason.clone()]);
    }
    if show_log_warnings {
        values.push("--show-log-warnings".into());
    }
    values.extend([job.cluster.clone(), job.id.clone()]);
    values
}

fn detail_watcher(config: &Config, cluster: &str, job_id: &str) -> Vec<String> {
    let mut values = vec![
        "env".into(),
        "SLURM_LOG_DETAILS_COMPACT=1".into(),
        "SLURM_LOG_DETAILS_PANE=1".into(),
        config.executable.display().to_string(),
    ];
    values.extend(config.child_args());
    values.extend([
        "details".into(),
        job_id.into(),
        "--cluster".into(),
        cluster.into(),
    ]);
    values
}

pub fn panes(session: &str) -> Result<Vec<Pane>> {
    let out = tmux([
        "list-panes",
        "-t",
        session,
        "-F",
        "#{pane_id}|#{@slurm_log_cluster}|#{@slurm_log_job_id}",
    ])?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let values: Vec<_> = line.splitn(3, '|').collect();
            (values.len() == 3 && !values[1].is_empty() && !values[2].is_empty()).then(|| Pane {
                id: values[0].into(),
                cluster: values[1].into(),
                job_id: values[2].into(),
            })
        })
        .collect())
}

fn label(pane: &str, job: &Job) -> Result<()> {
    tmux(label_args(pane, job))?;
    Ok(())
}

fn label_args(pane: &str, job: &Job) -> Vec<String> {
    let mut args = vec![
        "set-option".into(),
        "-p".into(),
        "-t".into(),
        pane.into(),
        "@slurm_log_cluster".into(),
        job.cluster.clone(),
        ";".into(),
        "set-option".into(),
        "-p".into(),
        "-t".into(),
        pane.into(),
        "@slurm_log_job_id".into(),
        job.id.clone(),
        ";".into(),
    ];
    // A direct CLUSTER JOB_ID open initially has no name. Its follower resolves
    // the name using the scontrol lookup it already needs. Do not race with and
    // overwrite that better value using a fallback from the parent process.
    if !job.name.trim().is_empty() {
        args.extend([
            "set-option".into(),
            "-p".into(),
            "-t".into(),
            pane.into(),
            "@slurm_log_job_name".into(),
            pane_job_name(&job.name),
            ";".into(),
        ]);
    }
    args.extend([
        "select-pane".into(),
        "-t".into(),
        pane.into(),
        "-T".into(),
        format!("{}:{}", job.cluster, job.id),
    ]);
    args
}

fn pane_job_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .filter(|character| !character.is_control())
        .take(100)
        .collect();
    let safe = safe.trim();
    if safe.is_empty() {
        "Slurm job".into()
    } else {
        safe.into()
    }
}

pub fn set_pane_job_name(pane: &str, name: &str) {
    let _ = tmux([
        "set-option",
        "-p",
        "-t",
        pane,
        "@slurm_log_job_name",
        &pane_job_name(name),
    ]);
}

fn persistent_job_status_format() -> &'static str {
    "#{?@slurm_log_job_id,#{?@slurm_log_job_name,#{@slurm_log_job_name},Slurm job} · job #{@slurm_log_job_id},slurm-log}"
}

include!("tmux/workspace.rs");
include!("tmux/detail_pane.rs");
include!("tmux/monitor.rs");
include!("tmux/reconcile.rs");

#[cfg(test)]
mod tests;
