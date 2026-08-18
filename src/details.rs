use std::{
    collections::VecDeque,
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use serde::{Deserialize, Serialize};

use crate::{
    command::shell_quote,
    config::Config,
    model::{Job, valid_job_id},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct JobDetails {
    pub cluster: String,
    pub id: String,
    pub name: String,
    pub state: String,
    pub reason: String,
    pub partition: String,
    pub account: String,
    pub qos: String,
    pub submit: String,
    pub start: String,
    pub end: String,
    pub elapsed: String,
    pub elapsed_seconds: u64,
    pub time_limit: String,
    pub nodes: u64,
    pub cpus: u64,
    pub requested_cpus: u64,
    pub memory_bytes: u64,
    pub requested_memory: String,
    pub max_rss_bytes: u64,
    pub gpus: u64,
    pub gpu_types: String,
    pub gpu_utilization: Option<f64>,
    pub gpu_memory_bytes: Option<u64>,
    pub total_cpu_seconds: u64,
    pub cpu_efficiency: Option<f64>,
    pub memory_efficiency: Option<f64>,
    pub alloc_tres: String,
    pub req_tres: String,
    pub node_list: String,
    pub exit_code: String,
    pub source: String,
    pub sampled_at: String,
    pub terminal: bool,
    pub stale_error: String,
}

pub fn validate_cluster(config: &Config, cluster: &str) -> Result<()> {
    config.cluster(cluster).map(|_| ())
}

pub fn fetch(
    config: &Config,
    cluster: &str,
    id: &str,
    previous: Option<&JobDetails>,
) -> Result<JobDetails> {
    validate_cluster(config, cluster)?;
    if !valid_job_id(id) {
        bail!("invalid job ID {id}");
    }
    // Details are an MCP-visible read path.  Do not borrow the 15-second
    // rendering cache here: a freshly returned owner-scoped job is the
    // authorization object bound to the subsequent scontrol/sstat request.
    let job = crate::slurm::authorize_exact_job(config, cluster, id)?;
    if job.pending() {
        return Ok(from_pending(job));
    }
    if job.running() {
        if let Some(previous) = previous {
            return sample_running(config, cluster, id, job, previous);
        }
        // Active jobs must not depend on sacct: accounting commonly lags
        // behind squeue, especially for array tasks. Build the first frame
        // from live scheduler data and enrich it with sstat when available.
        let base =
            live_details(config, job.clone()).unwrap_or_else(|_| from_live_queue(job.clone()));
        return sample_running(config, cluster, id, job, &base).or(Ok(base));
    }
    if !config.cluster(cluster)?.accounting {
        bail!("accounting is unavailable on {cluster}, and job {id} is no longer active");
    }
    // JobIDRaw drops the array-task suffix (for example 3209343_2 becomes
    // 3209343), so it cannot identify the selected task. A wide JobID field
    // preserves both array suffixes and step suffixes without truncation.
    let fields = "JobID%100,JobName,State,Reason,Partition,Account,QOS,Submit,Start,End,Elapsed,ElapsedRaw,Timelimit,NNodes,NCPUS,AllocCPUS,ReqCPUS,ReqMem,MaxRSS,AveRSS,AllocTRES,ReqTRES,TotalCPU,CPUTimeRAW,ExitCode,NodeList,TRESUsageInAve,TRESUsageInMax,User,Cluster";
    let target = config.cluster(cluster)?;
    let cluster_option = crate::slurm::accounting_cluster_option(config, cluster)?;
    let command = format!(
        "sacct{cluster_option} -j {} -u {} -n -P --format={} 2>/dev/null",
        shell_quote(id),
        shell_quote(&target.user),
        shell_quote(fields)
    );
    let output = crate::slurm::scheduler_text(config, cluster, "sh", &["-c", &command])?;
    let output = owned_accounting_rows(&output, target, id)?;
    parse_accounting(&output, cluster, id)
        .ok_or_else(|| anyhow::anyhow!("no accounting details found for {cluster}:{id}"))
}

/// Return only rows which remain bound to the exact requested job and owner.
/// The User and Cluster fields are deliberately appended to the existing
/// parser schema so numeric accounting columns keep their historical indexes.
fn owned_accounting_rows(
    output: &str,
    target: &crate::config::ClusterConfig,
    wanted: &str,
) -> Result<String> {
    let mut rows = Vec::new();
    for line in output.lines() {
        let fields: Vec<_> = line.split('|').map(str::trim).collect();
        if fields.len() < 30 {
            continue;
        }
        let base = fields[0].split('.').next().unwrap_or(fields[0]);
        if base != wanted || fields[fields.len() - 2] != target.user {
            continue;
        }
        if target.binds_controller() && fields[fields.len() - 1] != target.controller() {
            continue;
        }
        rows.push(line);
    }
    if rows.is_empty() {
        bail!("accounting metadata does not match the configured job owner");
    }
    Ok(rows.join("\n"))
}

include!("details/live.rs");
include!("details/parse.rs");
include!("details/control.rs");
include!("details/render.rs");

#[cfg(test)]
mod tests;
