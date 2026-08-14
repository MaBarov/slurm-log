use anyhow::Result;
use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::{McpServer, helpers::*};
use crate::slurm;

impl McpServer {
    pub(crate) fn list_jobs(&self, args: &JsonObject) -> Result<Value> {
        let cluster = optional_string(args, "cluster").unwrap_or("all");
        self.config.selected_clusters(cluster)?;
        let history = optional_string(args, "history").unwrap_or("live");
        let mode = history_mode(history)?;
        let (jobs, ledger, warnings) =
            slurm::all_jobs(&self.config, cluster, "all", mode.scheduler_archive())?;
        let include_blocked = optional_bool(args, "include_blocked").unwrap_or(false);
        let mut jobs = slurm::visible_jobs(jobs, &ledger, mode, include_blocked);
        let states = optional_strings(args, "states")?;
        if !states.is_empty() {
            jobs.retain(|job| states.iter().any(|state| job.state.starts_with(state)));
        }
        if let Some(search) = optional_string(args, "search") {
            let needle = search.to_lowercase();
            jobs.retain(|job| {
                [
                    job.id.as_str(),
                    job.name.as_str(),
                    job.state.as_str(),
                    job.reason.as_str(),
                ]
                .iter()
                .any(|value| value.to_lowercase().contains(&needle))
            });
        }
        let (start, limit) = page(args, "j", 50, 200)?;
        let total = jobs.len();
        let end = start.saturating_add(limit).min(total);
        let page = jobs.get(start..end).unwrap_or_default();
        Ok(json!({
            "ok": true,
            "cluster": cluster,
            "history": history,
            "jobs": page,
            "warnings": warnings,
            "next_cursor": (end < total).then(|| format!("j:{end}")),
            "total": total
        }))
    }

    pub(crate) fn inspect_job(&self, args: &JsonObject) -> Result<Value> {
        let (cluster, id) = exact_job(&self.config, args)?;
        let authorized = slurm::authorize_exact_job(&self.config, cluster, id)?;
        let archive = self.config.cluster(cluster)?.accounting;
        let (_, _, warnings) = slurm::all_jobs(&self.config, cluster, "all", archive)?;
        // Rendering caches may lag.  The exact fresh authorization object is
        // the only job metadata allowed to accompany protected follow-up
        // reads in an inspect response.
        let job = authorized;
        let details = crate::daemon::job_details(&self.config, cluster, id, false).ok();
        let log = crate::daemon::log_metadata(&self.config, cluster, id)?;
        let dependencies = dependencies(&self.config, cluster, id).unwrap_or_default();
        Ok(json!({
            "ok": true,
            "cluster": cluster,
            "job_id": id,
            "job": job,
            "details": details,
            "dependencies": dependencies,
            "log": log,
            "warnings": warnings
        }))
    }
}
