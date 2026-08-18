//! Diagnostic and lifecycle tools: doctor, refresh, wait, pending
//! explanation, and scheduler probes.

use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::{
    McpServer,
    adoption::{AdoptionEntry, adoption_sha, append_adoption},
    audit,
    helpers::*,
};
use crate::{bank, config::Config, slurm};

const WAIT_MAX_SECONDS: u64 = 40;
const PARTITION_ROWS_LIMIT: usize = 2_000;

impl McpServer {
    /// End-to-end health: configured clusters are checked with real `squeue`,
    /// `sacct`, and `sinfo` calls; bank health is reported separately from
    /// scheduler health so a catalog failure can never masquerade as
    /// scheduler health and vice versa.
    pub(crate) fn doctor(&self) -> Result<Value> {
        let mut cluster_checks = Vec::new();
        let mut warnings = Vec::new();
        let mut scheduler_ok = true;
        for cluster in &self.config.clusters {
            let squeue = check_squeue(&self.config, cluster);
            let sacct = cluster
                .accounting
                .then(|| check_sacct(&self.config, cluster));
            let sinfo = check_sinfo(&self.config, cluster);
            let mut cluster_warnings = Vec::new();
            if squeue.is_err() {
                cluster_warnings.push("squeue".to_string());
            }
            if sinfo.is_err() {
                cluster_warnings.push("sinfo".to_string());
            }
            if sacct.as_ref().is_some_and(Result::is_err) {
                cluster_warnings.push("sacct".to_string());
            }
            if cluster.remote() && cluster.controller.is_none() {
                cluster_warnings.push(
                    "cluster alias doubles as the Slurm federation name (no explicit controller)"
                        .to_string(),
                );
            }
            if !cluster_warnings.is_empty() {
                scheduler_ok = false;
                for probe in &cluster_warnings {
                    warnings.push(format!("{}: {} probe failed", cluster.name, probe));
                }
            }
            cluster_checks.push(json!({
                "name": cluster.name,
                "transport": if cluster.remote() { "ssh" } else { "local" },
                "controller": cluster.controller(),
                "explicit_controller": cluster.controller,
                "accounting": cluster.accounting,
                "squeue": probe_status(&squeue),
                "sacct": sacct.as_ref().map(probe_status).unwrap_or_else(|| "disabled".into()),
                "sinfo": probe_status(&sinfo),
                "warnings": cluster_warnings
            }));
        }
        let daemon_ok = crate::daemon::ensure_running(&self.config).is_ok();
        if !daemon_ok {
            warnings
                .push("private daemon access failed; log resolution is unavailable".to_string());
        }
        let bank_health = match bank::catalog(&self.config) {
            Ok(catalog) => json!({
                "ok": catalog.catalog_ok,
                "banks": catalog.banks.iter().map(|bank| json!({
                    "name": bank.name,
                    "path": bank.path,
                    "scripts": bank.scripts,
                    "indexed_at": iso_timestamp(bank.indexed_at),
                    "repo_commit": bank.repo_commit,
                    "fingerprint": format!("{:016x}", bank.fingerprint),
                    "error": bank.error
                })).collect::<Vec<_>>(),
                "warnings": catalog.warnings
            }),
            Err(error) => {
                warnings.push(format!("sbatch bank catalog unavailable: {error:#}"));
                json!({
                    "ok": false,
                    "banks": [],
                    "warnings": [format!("catalog unavailable: {error:#}")]
                })
            }
        };
        Ok(json!({
            "ok": true,
            "scheduler_health": {
                "ok": scheduler_ok,
                "clusters": cluster_checks
            },
            "bank_health": bank_health,
            "daemon_health": {
                "ok": daemon_ok,
                "error": (!daemon_ok).then_some(json!("daemon did not answer"))
            },
            "warnings": warnings
        }))
    }

    /// Force a fresh scan of every configured bank and report status and
    /// catalog generation.
    pub(crate) fn refresh_bank(&self) -> Result<Value> {
        let config = self.current_bank_config()?;
        let snapshot = bank::catalog_fresh(&config)?;
        if !snapshot.catalog_ok {
            bail!(
                "sbatch bank catalog unavailable: {}",
                snapshot
                    .banks
                    .iter()
                    .filter_map(|bank| bank.error.as_deref())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
        let generation_bytes = snapshot
            .banks
            .iter()
            .flat_map(|bank| {
                format!(
                    "{}:{:016x}:{}\\n",
                    bank.name, bank.fingerprint, bank.indexed_at
                )
                .into_bytes()
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "ok": true,
            "refreshed_at": now_iso(),
            "catalog_generation": sha256(&generation_bytes),
            "total": snapshot.scripts.len(),
            "banks": snapshot.banks.iter().map(|bank| json!({
                "name": bank.name,
                "path": bank.path,
                "scripts": bank.scripts,
                "indexed_at": iso_timestamp(bank.indexed_at),
                "repo_commit": bank.repo_commit,
                "fingerprint": format!("{:016x}", bank.fingerprint),
                "error": bank.error
            })).collect::<Vec<_>>(),
            "warnings": snapshot.warnings
        }))
    }

    /// Bounded server-side wait for a state or log change.  Replaces manual
    /// client-side squeue polling; every poll is a fresh, owner-verified
    /// scheduler query.
    pub(crate) fn wait_job(&self, args: &JsonObject) -> Result<Value> {
        let (cluster, id) = exact_job(&self.config, args)?;
        let until = wait_until(args)?;
        let timeout =
            optional_usize(args, "timeout_seconds", 30)?.clamp(1, WAIT_MAX_SECONDS as usize) as u64;
        let poll = optional_usize(args, "poll_interval", 3)?.clamp(1, 10) as u64;
        let initial = slurm::authorize_exact_job(&self.config, cluster, id)?;
        let mut last_generation = if until.log_change {
            crate::daemon::log_metadata(&self.config, cluster, id)
                .ok()
                .map(|log| log.generation)
        } else {
            None
        };
        let started = Instant::now();
        let mut transitions = vec![json!({"at": 0, "state": initial.state.clone()})];
        let mut current = initial.clone();
        let mut changed = false;
        let mut completed = !current.active();
        while !completed && started.elapsed() < Duration::from_secs(timeout) {
            thread::sleep(Duration::from_secs(poll));
            if let Ok(job) = slurm::authorize_exact_job(&self.config, cluster, id) {
                current = job;
            }
            if until.state_change {
                let last_state = transitions
                    .last()
                    .and_then(|value| value["state"].as_str())
                    .unwrap_or_default();
                if current.state != last_state {
                    changed = true;
                    transitions.push(json!({
                        "at": started.elapsed().as_secs(),
                        "state": current.state
                    }));
                }
            }
            if until.log_change
                && let Ok(log) = crate::daemon::log_metadata(&self.config, cluster, id)
                && Some(&log.generation) != last_generation.as_ref()
            {
                changed = true;
                last_generation = Some(log.generation);
            }
            if until.completion && !current.active() {
                completed = true;
            }
        }
        let timeout_hit = !completed && started.elapsed() >= Duration::from_secs(timeout);
        Ok(json!({
            "ok": true,
            "cluster": cluster,
            "job_id": id,
            "job_name": current.name,
            "initial_state": initial.state,
            "final_state": current.state,
            "changed": changed,
            "completed": completed,
            "timeout": timeout_hit,
            "elapsed_seconds": started.elapsed().as_secs(),
            "poll_interval": poll,
            "transitions": transitions,
            "warnings": Vec::<String>::new()
        }))
    }

    /// Explain why an owned job is pending: reason, reservation conflict,
    /// and compatible partition availability.  Never auto-switches
    /// partitions; the response only explains.
    pub(crate) fn explain_pending(&self, args: &JsonObject) -> Result<Value> {
        let (cluster, id) = exact_job(&self.config, args)?;
        let job = slurm::authorize_exact_job(&self.config, cluster, id)?;
        if !job.pending() {
            return Ok(json!({
                "ok": true,
                "cluster": cluster,
                "job_id": id,
                "pending": false,
                "state": job.state,
                "note": "job is not pending; nothing to explain"
            }));
        }
        let reason = job.reason.clone();
        let lowered = reason.to_lowercase();
        let reservation_conflict = lowered.contains("reservation")
            || lowered.contains("qos")
            || lowered.contains("priority");
        let partitions = partitions(&self.config, cluster).unwrap_or_default();
        let requested = job.partition.clone();
        let compatible = partitions
            .iter()
            .filter(|partition| {
                (!requested.is_empty()
                    && partition["partition"].as_str() == Some(requested.as_str()))
                    && partition["state"].as_str() == Some("up")
                    && partition["availability"]
                        .as_str()
                        .is_some_and(|value| value != "down" && value != "drain")
            })
            .cloned()
            .collect::<Vec<_>>();
        Ok(json!({
            "ok": true,
            "cluster": cluster,
            "job_id": id,
            "pending": true,
            "reason": reason,
            "reservation_conflict": reservation_conflict,
            "requested_partition": requested,
            "compatible_partitions": compatible,
            "all_partitions": partitions,
            "note": "MCP never auto-switches partitions; a scheduling-only resubmission preview would show the exact delta (partition and nothing else)"
        }))
    }

    /// Adopt a manually submitted job into the MCP provenance ledger.
    /// Records the client-observed batch-script hash and marks the job
    /// `externally_submitted`; never claims an MCP preview authorized it.
    pub(crate) fn adopt_job(&self, args: &JsonObject, client: &str) -> Result<Value> {
        let (cluster, id) = exact_job(&self.config, args)?;
        let expected = required_string(args, "expected_job_name")?;
        let sha = optional_string(args, "batch_script_sha256");
        if let Some(sha) = sha {
            adoption_sha(sha)?;
        }
        audit::record(
            &self.config,
            client,
            "slurm_adopt_job",
            cluster,
            id,
            None,
            "attempted",
        )?;
        let result = (|| {
            let job = slurm::authorize_exact_job(&self.config, cluster, id)?;
            if job.name != expected {
                bail!(
                    "job name changed: expected {expected:?}, found {:?}",
                    job.name
                );
            }
            let entry = AdoptionEntry {
                adopted_at: now_iso(),
                cluster: cluster.into(),
                job_id: id.into(),
                job_name: job.name.clone(),
                observed_state: job.state.clone(),
                batch_script_sha256: sha.map(str::to_string),
                externally_submitted: true,
                source: "manual submission outside MCP".into(),
            };
            append_adoption(&self.config, &entry)?;
            Ok(json!({
                "ok": true,
                "adopted": true,
                "cluster": cluster,
                "job_id": id,
                "job_name": job.name,
                "observed_state": job.state,
                "provenance": {
                    "externally_submitted": true,
                    "batch_script_sha256": entry.batch_script_sha256,
                    "adopted_at": entry.adopted_at,
                    "source": entry.source,
                    "note": "the MCP preview chain never authorized this job"
                }
            }))
        })();
        let status = result.as_ref().map(|_| "adopted").unwrap_or("rejected");
        let _ = audit::record(
            &self.config,
            client,
            "slurm_adopt_job",
            cluster,
            id,
            None,
            status,
        );
        result
    }
}

struct WaitFlags {
    state_change: bool,
    completion: bool,
    log_change: bool,
}

fn wait_until(args: &JsonObject) -> Result<WaitFlags> {
    let values = optional_strings(args, "until")?
        .into_iter()
        .map(|value| value.to_lowercase())
        .collect::<Vec<_>>();
    let mut flags = WaitFlags {
        state_change: false,
        completion: false,
        log_change: false,
    };
    if values.is_empty() {
        flags.state_change = true;
    }
    for value in values {
        match value.as_str() {
            "state_change" => flags.state_change = true,
            "completion" => flags.completion = true,
            "log_change" => flags.log_change = true,
            _ => bail!("invalid wait condition {value}"),
        }
    }
    Ok(flags)
}

fn probe_status(result: &Result<bool>) -> String {
    match result {
        Ok(true) => "ok".into(),
        Ok(false) => "error".into(),
        Err(error) => format!("error: {}", bounded_line(&format!("{error:#}"), 120)),
    }
}

fn check_squeue(config: &Config, cluster: &crate::config::ClusterConfig) -> Result<bool> {
    slurm::scheduler_text(
        config,
        &cluster.name,
        "squeue",
        &["-h", "-u", cluster.user.as_str(), "-o", "%i"],
    )
    .map(|_| true)
}

fn check_sacct(config: &Config, cluster: &crate::config::ClusterConfig) -> Result<bool> {
    let cluster_option = slurm::accounting_cluster_option(config, &cluster.name)?;
    let command = format!(
        "sacct -X{cluster_option} -S now-1hour -u {} -n -P --format=JobID 2>/dev/null",
        crate::command::shell_quote(&cluster.user)
    );
    slurm::scheduler_text(config, &cluster.name, "sh", &["-c", &command]).map(|_| true)
}

fn check_sinfo(config: &Config, cluster: &crate::config::ClusterConfig) -> Result<bool> {
    slurm::scheduler_text(
        config,
        &cluster.name,
        "sinfo",
        &["-h", "-o", "%P|%a|%T|%C|%G"],
    )
    .map(|_| true)
}

fn partitions(config: &Config, cluster: &str) -> Result<Vec<Value>> {
    let value = slurm::scheduler_text(config, cluster, "sinfo", &["-h", "-o", "%P|%a|%T|%C|%G"])?;
    let mut rows = Vec::new();
    for line in value.lines() {
        if rows.len() == PARTITION_ROWS_LIMIT {
            break;
        }
        let fields: Vec<_> = line.split('|').map(str::trim).collect();
        if fields.len() != 5 {
            continue;
        }
        rows.push(json!({
            "partition": fields[0],
            "availability": fields[1],
            "state": fields[2],
            "cpus": fields[3],
            "gres": fields[4]
        }));
    }
    Ok(rows)
}

#[cfg(test)]
#[path = "ops/tests.rs"]
mod tests;
