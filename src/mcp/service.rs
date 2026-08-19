use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, JsonObject};
use serde_json::{Value, json};

use super::{McpServer, audit, helpers::*};
use crate::{bank, config::Config, slurm};

pub(super) const PREVIEW_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub struct Preview {
    pub created: Instant,
    pub cluster: String,
    pub script: String,
    pub digest: String,
    pub directives: Vec<String>,
    pub working_directory: String,
    pub job_name: String,
    pub catalog_generation: String,
    pub repo_commit: Option<String>,
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
            "slurm_doctor" => self.doctor(),
            "slurm_refresh_bank" => self.refresh_bank(),
            "slurm_wait_job" => self.wait_job(&args),
            "slurm_explain_pending" => self.explain_pending(&args),
            "slurm_adopt_job" => self.adopt_job(&args, client),
            _ => Err(anyhow::anyhow!("unknown tool {name}")),
        });
        match result {
            Ok(value) => {
                let mut result = CallToolResult::structured(value.clone());
                result.content = vec![ContentBlock::text(super::fallback::fallback_text(
                    name, &value,
                ))];
                result
            }
            Err(error) => {
                let message = bounded_error(&format!("{error:#}"));
                let mut error_value = json!({"ok": false, "error": message});
                if let Some(kind) = error
                    .downcast_ref::<crate::slurm::ExactJobError>()
                    .map(crate::slurm::ExactJobError::kind)
                {
                    error_value["error_type"] = Value::String(kind.into());
                }
                let mut result = CallToolResult::structured_error(error_value);
                result.content = vec![ContentBlock::text(format!("{name}: failed: {message}"))];
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
        let authorized = slurm::authorize_exact_job(&self.config, cluster, id)?;
        let archive = self.config.cluster(cluster)?.accounting;
        // A listing failure must not cascade raw scheduler stderr into an
        // otherwise exact inspect response.
        let warnings = slurm::all_jobs(&self.config, cluster, "all", archive)
            .map(|(_, _, warnings)| warnings)
            .unwrap_or_else(|error| vec![format!("listing unavailable: {error:#}")]);
        // Rendering caches may lag.  The exact fresh authorization object is
        // the only job metadata allowed to accompany protected follow-up
        // reads in an inspect response.
        let job = authorized;
        let details = crate::daemon::job_details(&self.config, cluster, id, false).ok();
        let log = crate::daemon::log_metadata(&self.config, cluster, id)?;
        let dependencies = dependencies(&self.config, cluster, id).unwrap_or_default();
        let mut response = json!({
            "ok": true,
            "cluster": cluster,
            "job_id": id,
            "job": job,
            "details": details,
            "dependencies": dependencies,
            "log": log,
            "warnings": warnings
        });
        if let Some(adoption) = super::adoption::adoption_entry(&self.config, cluster, id) {
            response["submission_provenance"] = json!({
                "externally_submitted": true,
                "adopted": adoption,
                "note": "this job was submitted outside MCP; the MCP preview chain never authorized it"
            });
        }
        Ok(response)
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
        let config = self.current_bank_config()?;
        let snapshot = bank::catalog(&config)?;
        if !snapshot.catalog_ok {
            let failures = snapshot
                .banks
                .iter()
                .filter_map(|bank| bank.error.as_deref())
                .map(str::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            bail!("sbatch bank catalog unavailable: {failures}");
        }
        let mut scripts = snapshot.scripts;
        scripts.retain(|script| cluster == "all" || bank::supports_cluster(script, cluster));
        let needle = optional_string(args, "search").map(str::to_lowercase);
        let mut matched_field = vec![None; scripts.len()];
        if let Some(needle) = &needle {
            for (index, script) in scripts.iter().enumerate() {
                let identity = script_id(script).to_lowercase();
                let name = script.name.to_lowercase();
                if identity.contains(needle) {
                    matched_field[index] = Some("script_id");
                } else if name.contains(needle) {
                    matched_field[index] = Some("job_name");
                }
            }
            let mut kept = Vec::new();
            let mut fields = Vec::new();
            for (script, field) in scripts.into_iter().zip(matched_field) {
                if field.is_some() {
                    kept.push(script);
                    fields.push(field);
                }
            }
            scripts = kept;
            matched_field = fields;
        }
        let (start, limit) = page(args, "s", 50, 200)?;
        let total = scripts.len();
        let end = start.saturating_add(limit).min(total);
        let items = scripts
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .zip(matched_field.get(start..end).unwrap_or_default())
            .map(|(script, field)| {
                let eligible = config
                    .clusters
                    .iter()
                    .filter(|cluster| bank::supports_cluster(script, &cluster.name))
                    .map(|cluster| cluster.name.clone())
                    .collect::<Vec<_>>();
                let mut item = json!({
                    "script": script_id(script),
                    "job_name": script.name,
                    "directives": script.directives,
                    "eligible_clusters": eligible,
                    "bank": script.bank,
                    "script_sha256": sha256(&script.bytes),
                    "repo_commit": script.repo_commit,
                    "indexed_at": iso_timestamp(script.indexed_at),
                });
                if let Some(field) = field {
                    item["matched_field"] = Value::String((*field).into());
                }
                item
            })
            .collect::<Vec<_>>();
        let catalog_indexed = snapshot
            .banks
            .iter()
            .map(|bank| bank.indexed_at)
            .max()
            .unwrap_or_default();
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
        let generation = sha256(&generation_bytes);
        let explanation = (needle.is_some() && total == 0).then(|| {
            format!(
                "0 matches; catalog healthy; indexed at {}; local/uncommitted files not indexed; refresh or stage required",
                iso_timestamp(catalog_indexed)
            )
        });
        Ok(json!({
            "ok":true,"scripts":items,"warnings":snapshot.warnings,"total":total,
            "next_cursor":(end < total).then(|| format!("s:{end}")),
            "catalog":{
                "ok":true,
                "indexed_at":iso_timestamp(catalog_indexed),
                "generation":generation,
                "banks":snapshot.banks.iter().map(|bank| json!({
                    "name":bank.name,"path":bank.path,"scripts":bank.scripts,
                    "indexed_at":iso_timestamp(bank.indexed_at),
                    "repo_commit":bank.repo_commit,
                    "fingerprint":format!("{:016x}",bank.fingerprint),
                    "error":bank.error,
                })).collect::<Vec<_>>()
            },
            "explanation":explanation
        }))
    }

    pub(crate) fn current_bank_config(&self) -> Result<Config> {
        let current = Config::load_for_setup().context("reload sbatch bank configuration")?;
        let mut config = self.config.as_ref().clone();
        // Keep the server's own banks when the reload finds none, so a server
        // built with an explicit bank list stays hermetic on hosts without a
        // slurm-log configuration.
        if !current.sbatch_banks.is_empty() {
            config.sbatch_banks = current.sbatch_banks;
        }
        config.validate()?;
        Ok(config)
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
            let job = slurm::fresh_cancellable_job(&self.config, cluster, id)?;
            if job.name != expected {
                bail!(
                    "job name changed: expected {expected:?}, found {:?}",
                    job.name
                );
            }
            let failures = bank::cancel_verified(&self.config, std::slice::from_ref(&job))?;
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
