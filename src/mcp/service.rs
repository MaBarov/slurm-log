use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, JsonObject};
use serde_json::{Value, json};

use super::{McpServer, audit, helpers::*};
use crate::{bank, slurm};

const PREVIEW_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub struct Preview {
    pub created: Instant,
    pub cluster: String,
    pub script: String,
    pub digest: String,
    pub directives: Vec<String>,
    pub working_directory: String,
    pub job_name: String,
}

impl McpServer {
    pub fn dispatch_tool(&self, request: CallToolRequestParams, client: &str) -> CallToolResult {
        let name = request.name.as_ref();
        let args = request.arguments.unwrap_or_default();
        let result = super::validation::tool_arguments(name, &args).and_then(|()| match name {
            "slurm_list_clusters" => self.list_clusters(),
            "slurm_list_jobs" => self.list_jobs(&args),
            "slurm_inspect_job" => self.inspect_job(&args),
            "slurm_workspace_context" => self.workspace_context(),
            "slurm_read_log" => self.read_log(&args),
            "slurm_search_log" => self.search_log(&args),
            "slurm_diagnose_job" => self.diagnose_job(&args),
            "slurm_list_scripts" => self.list_scripts(&args),
            "slurm_preview_submission" => self.preview_submission(&args, client),
            "slurm_submit_job" => self.submit_job(&args, client),
            "slurm_cancel_job" => self.cancel_job(&args, client),
            _ => Err(anyhow::anyhow!("unknown tool {name}")),
        });
        match result {
            Ok(value) => {
                let mut result = CallToolResult::structured(value);
                result.content = vec![ContentBlock::text(format!(
                    "{name} completed; structured result attached."
                ))];
                result
            }
            Err(error) => {
                let message = bounded_error(&format!("{error:#}"));
                let mut result = CallToolResult::structured_error(json!({
                    "ok": false,
                    "error": message
                }));
                result.content = vec![ContentBlock::text(format!("{name}: {message}"))];
                result
            }
        }
    }

    fn list_clusters(&self) -> Result<Value> {
        let clusters = self
            .config
            .clusters
            .iter()
            .map(|cluster| {
                let connectivity = slurm::all_jobs(&self.config, &cluster.name, "all", false)
                    .map(|(_, _, warnings)| {
                        if warnings.is_empty() {
                            "reachable"
                        } else {
                            "degraded"
                        }
                    })
                    .unwrap_or("unreachable");
                json!({
                    "name": cluster.name,
                    "transport": if cluster.remote() { "ssh" } else { "local" },
                    "accounting": cluster.accounting,
                    "connectivity": connectivity
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"ok":true,"clusters":clusters}))
    }

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
        let archive = self.config.cluster(cluster)?.accounting;
        let (jobs, _, warnings) = slurm::all_jobs(&self.config, cluster, "all", archive)?;
        let job = jobs.into_iter().find(|job| job.id == id);
        let details = crate::daemon::job_details(&self.config, cluster, id, false).ok();
        if job.is_none() && details.is_none() {
            bail!("job {cluster}:{id} was not found");
        }
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

    fn workspace_context(&self) -> Result<Value> {
        let output = crate::command::output(
            "tmux",
            &[
                "list-panes",
                "-a",
                "-F",
                "#S|#{pane_active}|#{@slurm_log_cluster}|#{@slurm_log_job_id}",
            ],
        );
        let Ok(output) = output else {
            return Ok(json!({"ok":true,"workspaces":[],"focused_jobs":[]}));
        };
        if !output.status.success() {
            return Ok(json!({"ok":true,"workspaces":[],"focused_jobs":[]}));
        }
        let mut workspaces = HashSet::new();
        let mut focused = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let fields: Vec<_> = line.splitn(4, '|').collect();
            if fields.len() != 4 || !fields[0].starts_with("slurm-logs-") {
                continue;
            }
            workspaces.insert(fields[0].to_string());
            if fields[1] == "1" && !fields[2].is_empty() && !fields[3].is_empty() {
                focused.push(json!({"workspace":fields[0],"cluster":fields[2],"job_id":fields[3]}));
            }
        }
        let mut workspaces = workspaces.into_iter().collect::<Vec<_>>();
        workspaces.sort();
        Ok(json!({"ok":true,"workspaces":workspaces,"focused_jobs":focused}))
    }

    fn list_scripts(&self, args: &JsonObject) -> Result<Value> {
        let cluster = optional_string(args, "cluster").unwrap_or("all");
        self.config.selected_clusters(cluster)?;
        let (mut scripts, warnings) = bank::configured_scripts(&self.config)?;
        scripts.retain(|script| cluster == "all" || bank::supports_cluster(script, cluster));
        if let Some(search) = optional_string(args, "search") {
            let needle = search.to_lowercase();
            scripts.retain(|script| script_id(script).to_lowercase().contains(&needle));
        }
        let (start, limit) = page(args, "s", 50, 200)?;
        let total = scripts.len();
        let end = start.saturating_add(limit).min(total);
        let items = scripts
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .map(|script| {
                let eligible = self
                    .config
                    .clusters
                    .iter()
                    .filter(|cluster| bank::supports_cluster(script, &cluster.name))
                    .map(|cluster| cluster.name.clone())
                    .collect::<Vec<_>>();
                json!({
                    "script": script_id(script),
                    "job_name": script.name,
                    "directives": script.directives,
                    "eligible_clusters": eligible
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "ok":true,"scripts":items,"warnings":warnings,"total":total,
            "next_cursor":(end < total).then(|| format!("s:{end}"))
        }))
    }

    fn preview_submission(&self, args: &JsonObject, client: &str) -> Result<Value> {
        let cluster = required_string(args, "cluster")?;
        self.config.cluster(cluster)?;
        let wanted = required_string(args, "script")?;
        let result = (|| {
            let (scripts, warnings) = bank::configured_scripts_fresh(&self.config)?;
            let script = exact_script(&scripts, wanted, cluster)?;
            let digest = sha256(&script.bytes);
            let preview = Preview {
                created: Instant::now(),
                cluster: cluster.into(),
                script: script_id(script),
                digest: digest.clone(),
                directives: script.directives.clone(),
                working_directory: self
                    .config
                    .cluster(cluster)?
                    .working_directory
                    .display()
                    .to_string(),
                job_name: script.name.clone(),
            };
            let token = preview_token(&preview);
            let mut previews = self
                .previews
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            previews.retain(|_, value| value.created.elapsed() < PREVIEW_TTL);
            if previews.len() >= 256 {
                bail!("too many active submission previews");
            }
            previews.insert(token.clone(), preview.clone());
            Ok(json!({
                "ok":true,"preview_token":token,"expires_in_seconds":PREVIEW_TTL.as_secs(),
                "cluster":cluster,"script":preview.script,"script_sha256":digest,
                "directives":preview.directives,"working_directory":preview.working_directory,
                "job_name":preview.job_name,"warnings":warnings
            }))
        })();
        let status = result.as_ref().map(|_| "previewed").unwrap_or("rejected");
        let digest = result
            .as_ref()
            .ok()
            .and_then(|value| value["script_sha256"].as_str());
        audit::record(
            &self.config,
            client,
            "slurm_preview_submission",
            cluster,
            wanted,
            digest,
            status,
        )?;
        result
    }

    fn submit_job(&self, args: &JsonObject, client: &str) -> Result<Value> {
        let token = required_string(args, "preview_token")?;
        let preview = self
            .previews
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(token)
            .context("preview token is invalid or already consumed")?;
        audit::record(
            &self.config,
            client,
            "slurm_submit_job",
            &preview.cluster,
            &preview.script,
            Some(&preview.digest),
            "attempted",
        )?;
        let result = (|| {
            if preview.created.elapsed() >= PREVIEW_TTL {
                bail!("preview token expired");
            }
            let (scripts, _) = bank::configured_scripts_fresh(&self.config)?;
            let script = exact_script(&scripts, &preview.script, &preview.cluster)?;
            let digest = sha256(&script.bytes);
            let working_directory = self
                .config
                .cluster(&preview.cluster)?
                .working_directory
                .display()
                .to_string();
            if digest != preview.digest
                || script.directives != preview.directives
                || script.name != preview.job_name
                || working_directory != preview.working_directory
            {
                bail!("preview is stale because the script or target configuration changed");
            }
            let job = bank::submit(&self.config, script, &preview.cluster)?;
            Ok(
                json!({"ok":true,"cluster":job.cluster,"job_id":job.id,"job_name":job.name,"script_sha256":digest}),
            )
        })();
        let status = result.as_ref().map(|_| "submitted").unwrap_or("rejected");
        let _ = audit::record(
            &self.config,
            client,
            "slurm_submit_job",
            &preview.cluster,
            &preview.script,
            Some(&preview.digest),
            status,
        );
        result
    }

    fn cancel_job(&self, args: &JsonObject, client: &str) -> Result<Value> {
        let (cluster, id) = exact_job(&self.config, args)?;
        let expected = required_string(args, "expected_job_name")?;
        audit::record(
            &self.config,
            client,
            "slurm_cancel_job",
            cluster,
            id,
            None,
            "attempted",
        )?;
        let result = (|| {
            let job = slurm::queued(&self.config, cluster)?
                .into_iter()
                .find(|job| job.id == id)
                .context("job is not active")?;
            if !job.active() {
                bail!("job is no longer active");
            }
            if job.name != expected {
                bail!(
                    "job name changed: expected {expected:?}, found {:?}",
                    job.name
                );
            }
            let failures = bank::cancel(&self.config, std::slice::from_ref(&job))?;
            if !failures.is_empty() {
                bail!("{}", failures.join("; "));
            }
            Ok(
                json!({"ok":true,"cluster":cluster,"job_id":id,"job_name":job.name,"cancelled":true}),
            )
        })();
        let status = result.as_ref().map(|_| "cancelled").unwrap_or("rejected");
        let _ = audit::record(
            &self.config,
            client,
            "slurm_cancel_job",
            cluster,
            id,
            None,
            status,
        );
        result
    }
}

fn bounded_error(value: &str) -> String {
    value.chars().take(2000).collect()
}

#[cfg(test)]
#[path = "service/tests.rs"]
mod tests;
