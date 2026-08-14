use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::{McpServer, audit, helpers::*, present::probe_nonce};
use crate::{bank, slurm};

impl McpServer {
    pub(crate) fn wait_job(&self, args: &JsonObject) -> Result<Value> {
        let (cluster, id) = exact_job(&self.config, args)?;
        let until = optional_string(args, "until").unwrap_or("completion");
        if !["state_change", "completion", "log_change"].contains(&until) {
            bail!("invalid wait condition {until}");
        }
        let timeout_seconds = optional_usize(args, "timeout_seconds", 30)?.clamp(1, 30);
        let interval_seconds = optional_usize(args, "interval_seconds", 2)?.clamp(1, 10);
        let config = self.config.as_ref();
        let initial = slurm::authorize_exact_job(config, cluster, id)?;
        let initial_state = initial.state.clone();
        let initial_log = crate::daemon::log_metadata(config, cluster, id).ok();
        let mut current_state = initial_state.clone();
        let mut current_log = initial_log.clone();
        let mut changed = false;
        let mut completed = !initial.active();
        let mut polls = 0_usize;
        let started = Instant::now();
        let deadline = Duration::from_secs(timeout_seconds as u64);
        while !changed && !completed && started.elapsed() < deadline {
            std::thread::sleep(Duration::from_secs(interval_seconds as u64));
            polls += 1;
            match slurm::authorize_exact_job(config, cluster, id) {
                Ok(job) => {
                    current_state = job.state.clone();
                    completed = !job.active();
                    changed = match until {
                        "state_change" => current_state != initial_state,
                        "completion" => completed,
                        _ => false,
                    };
                }
                Err(_) => {
                    // Leaving the active queue without an accounting record is
                    // itself a state change toward completion.
                    completed = true;
                    changed = true;
                }
            }
            if until == "log_change" {
                current_log = crate::daemon::log_metadata(config, cluster, id).ok();
                changed = current_log.as_ref().map(|log| &log.generation)
                    != initial_log.as_ref().map(|log| &log.generation)
                    || current_log.as_ref().map(|log| log.size)
                        != initial_log.as_ref().map(|log| log.size);
            }
        }
        Ok(json!({
            "ok": true,
            "cluster": cluster,
            "job_id": id,
            "until": until,
            "initial_state": initial_state,
            "final_state": current_state,
            "changed": changed,
            "completed": completed,
            "timed_out": !changed && !completed,
            "polls": polls,
            "elapsed_seconds": started.elapsed().as_secs(),
            "log": current_log.as_ref().map(|log| json!({
                "status": log.status,
                "generation": log.generation,
                "size": log.size,
                "terminal": log.terminal,
            })),
        }))
    }

    pub(crate) fn explain_pending(&self, args: &JsonObject) -> Result<Value> {
        let (cluster, id) = exact_job(&self.config, args)?;
        let job = slurm::authorize_exact_job(&self.config, cluster, id)?;
        if !job.pending() {
            bail!("job {cluster}:{id} is not pending (state {})", job.state);
        }
        let partitions = self.partition_availability(cluster).unwrap_or_default();
        Ok(json!({
            "ok": true,
            "cluster": cluster,
            "job_id": id,
            "state": job.state,
            "reason": job.reason,
            "explanation": crate::model::pending_explanation(&job.reason),
            "requested_partition": job.partition,
            "priority": job.priority,
            "start_time": job.start_time,
            "partitions": partitions,
        }))
    }

    pub(crate) fn partition_availability(&self, cluster: &str) -> Result<Value> {
        let args = ["-h", "-o", "%P|%a|%D|%t"];
        let value = slurm::scheduler_text(&self.config, cluster, "sinfo", &args)?;
        let mut partitions = Vec::new();
        for line in value.lines() {
            let fields: Vec<_> = line.split('|').map(str::trim).collect();
            if fields.len() != 4 || fields[0].is_empty() || fields[0].ends_with('*') {
                continue;
            }
            partitions.push(json!({
                "partition": fields[0],
                "availability": fields[1],
                "nodes": fields[2],
                "state": fields[3],
            }));
        }
        Ok(Value::Array(partitions))
    }

    pub(crate) fn find_artifact(&self, args: &JsonObject) -> Result<Value> {
        let (cluster, id) = exact_job(&self.config, args)?;
        let job = crate::slurm::authorize_exact_job(&self.config, cluster, id)?;
        let target = self.config.cluster(cluster)?;
        if target.remote() {
            bail!("artifact search requires a local cluster working directory");
        }
        let pattern = required_string(args, "pattern")?;
        let declared = super::helpers::declared_results_for_job(&self.config, cluster, &job)?;
        if !declared.iter().any(|candidate| candidate == pattern) {
            bail!(
                "pattern {pattern:?} is not declared by the batch script of job {cluster}:{id}; declared results: {declared:?}"
            );
        }
        let search_root = optional_string(args, "search_root").unwrap_or(".");
        let subdir = super::artifact::validate_search_root(search_root)?;
        let content_max = optional_usize(args, "max_bytes", super::artifact::MAX_ARTIFACT_CONTENT)?
            .clamp(1, super::artifact::MAX_ARTIFACT_CONTENT);
        let result =
            super::artifact::search(&target.working_directory, &subdir, pattern, content_max)?;
        Ok(json!({
            "ok": true,
            "cluster": cluster,
            "job_id": id,
            "root": target.working_directory.display().to_string(),
            "search_root": search_root,
            "pattern": pattern,
            "declared_results": declared,
            "matches": result.matches,
            "total": result.matches.len(),
            "scanned_entries": result.scanned,
            "truncated": result.truncated,
            "max_content_bytes": content_max,
        }))
    }

    pub(crate) fn read_declared_result(&self, args: &JsonObject) -> Result<Value> {
        let (cluster, id) = exact_job(&self.config, args)?;
        let job = crate::slurm::authorize_exact_job(&self.config, cluster, id)?;
        let target = self.config.cluster(cluster)?;
        if target.remote() {
            bail!("declared-result reading requires a local cluster working directory");
        }
        let declared = super::helpers::declared_results_for_job(&self.config, cluster, &job)?;
        if declared.is_empty() {
            bail!(
                "job {cluster}:{id} declares no result files; add #SLURM_LOG-RESULT markers to its batch script"
            );
        }
        let wanted = optional_string(args, "result");
        let patterns: Vec<&str> = match wanted {
            Some(value) => vec![
                declared
                    .iter()
                    .find(|pattern| pattern.as_str() == value)
                    .with_context(|| {
                        format!("{value:?} is not a declared result of job {cluster}:{id}")
                    })?
                    .as_str(),
            ],
            None => declared.iter().map(String::as_str).collect(),
        };
        let search_root = optional_string(args, "search_root").unwrap_or(".");
        let subdir = super::artifact::validate_search_root(search_root)?;
        let content_max = optional_usize(args, "max_bytes", super::artifact::MAX_ARTIFACT_CONTENT)?
            .clamp(1, super::artifact::MAX_ARTIFACT_CONTENT);
        let mut matches = Vec::new();
        let mut scanned = 0_usize;
        let mut truncated = false;
        for pattern in patterns {
            let result =
                super::artifact::search(&target.working_directory, &subdir, pattern, content_max)?;
            scanned = scanned.saturating_add(result.scanned);
            truncated |= result.truncated;
            matches.extend(result.matches);
            if truncated {
                break;
            }
        }
        Ok(json!({
            "ok": true,
            "cluster": cluster,
            "job_id": id,
            "job_name": job.name,
            "declared_results": declared,
            "requested": wanted,
            "root": target.working_directory.display().to_string(),
            "search_root": search_root,
            "matches": matches,
            "total": matches.len(),
            "scanned_entries": scanned,
            "truncated": truncated,
            "max_content_bytes": content_max,
        }))
    }

    pub(crate) fn stage_bundle(&self, args: &JsonObject, client: &str) -> Result<Value> {
        let bank_name = optional_string(args, "bank");
        let destination = optional_string(args, "destination").unwrap_or("remote");
        let manifest: Vec<String> = args
            .get("entries")
            .and_then(Value::as_array)
            .context("entries must be an array")?
            .iter()
            .map(|entry| entry.as_str().unwrap_or_default().to_string())
            .collect();
        audit::record(
            &self.config,
            client,
            "slurm_stage_bundle",
            "",
            "",
            None,
            "attempted",
        )?;
        let result = (|| {
            let config = self.current_bank_config()?;
            let root = bank::bundle_root(&config, bank_name)?;
            let bundle = bank::build_bundle(&root, &manifest)?;
            let local_path = if destination == "local" {
                let directory = bank::local_bundle_dir(&config);
                std::fs::create_dir_all(&directory).with_context(|| {
                    format!("create bundle staging directory {}", directory.display())
                })?;
                let path = directory.join(format!("{}.bundle", bundle.sha256));
                {
                    use std::io::Write;
                    let opened = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .with_context(|| format!("stage bundle at {}", path.display()))?;
                    let mut writer = std::io::BufWriter::new(opened);
                    writer.write_all(&bundle.bytes)?;
                    writer.flush()?;
                }
                Some(path)
            } else {
                None
            };
            Ok(json!({
                "ok": true,
                "destination": destination,
                "bundle_sha256": bundle.sha256,
                "bytes": bundle.bytes.len(),
                "entry_count": bundle.entries.len(),
                "entries": bundle.entries.iter().map(|(path, len)| {
                    json!({"path": path, "bytes": len})
                }).collect::<Vec<_>>(),
                "local_path": local_path.map(|path| path.display().to_string()),
                "remote_path": bank::remote_bundle_file(&bundle.sha256),
                "execution_approved": false,
            }))
        })();
        let status = result.as_ref().map(|_| "staged").unwrap_or("rejected");
        let sha = result
            .as_ref()
            .ok()
            .and_then(|value| value["bundle_sha256"].as_str());
        let _ = audit::record(
            &self.config,
            client,
            "slurm_stage_bundle",
            "",
            "",
            sha,
            status,
        );
        result
    }

    pub(crate) fn preflight_job(&self, args: &JsonObject, client: &str) -> Result<Value> {
        let cluster = required_string(args, "cluster")?;
        self.config.cluster(cluster)?;
        let wanted = required_string(args, "script")?;
        let wait_seconds = optional_usize(args, "wait_seconds", 30)?.clamp(1, 60);
        audit::record(
            &self.config,
            client,
            "slurm_preflight_job",
            cluster,
            wanted,
            None,
            "attempted",
        )?;
        let result = (|| {
            let config = self.current_bank_config()?;
            let (scripts, _) = bank::configured_scripts_fresh(&config)?;
            let script = exact_script(&scripts, wanted, cluster)?;
            bank::validate_script_controller(script, config.cluster(cluster)?)?;
            let (partition, gres) = bank::scheduling_request(&script.directives)?;
            let probe_name = format!("SLURM_LOG_PREFLIGHT_{}", probe_nonce()?);
            let bytes = bank::probe_script(&probe_name, partition.as_deref(), gres.as_deref());
            let probe = bank::Script {
                bank: script.bank.clone(),
                relative: PathBuf::from("preflight.sbatch"),
                name: probe_name.clone(),
                directives: Vec::new(),
                origin: None,
                declared_results: Vec::new(),
                bytes,
            };
            let job = bank::submit(&config, &probe, cluster)?;
            let deadline = Duration::from_secs(wait_seconds as u64);
            let started = Instant::now();
            let mut current = job.clone();
            let mut poll_failed = false;
            while current.active() && started.elapsed() < deadline {
                std::thread::sleep(Duration::from_secs(2));
                match slurm::authorize_exact_job(&config, cluster, &job.id) {
                    Ok(fresh) => current = fresh,
                    Err(_) => {
                        poll_failed = true;
                        break;
                    }
                }
            }
            let cancelled = if current.active() && (poll_failed || started.elapsed() >= deadline) {
                bank::cancel(&config, &[current.clone()]).is_ok_and(|failures| failures.is_empty())
            } else {
                false
            };
            let log = crate::daemon::log_window(&config, cluster, &job.id, 4096)
                .ok()
                .map(|log| sanitize(&log.bytes))
                .unwrap_or_default();
            Ok(json!({
                "ok": true,
                "cluster": cluster,
                "reference_script": script_id(script),
                "probe_job_id": job.id,
                "probe_name": probe_name,
                "partition": partition,
                "gres": gres,
                "state": current.state,
                "completed": !current.active(),
                "cancelled": cancelled,
                "wait_seconds": wait_seconds,
                "elapsed_seconds": started.elapsed().as_secs(),
                "log": log,
                "untrusted_data": true,
            }))
        })();
        let status = result.as_ref().map(|_| "probed").unwrap_or("rejected");
        let _ = audit::record(
            &self.config,
            client,
            "slurm_preflight_job",
            cluster,
            wanted,
            None,
            status,
        );
        result
    }
}
