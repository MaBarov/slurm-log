use std::time::Instant;

use anyhow::{Context, Result, bail};
use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::{McpServer, Preview, audit, helpers::*};
use crate::bank;

impl McpServer {
    pub(crate) fn preview_submission(&self, args: &JsonObject, client: &str) -> Result<Value> {
        let cluster = required_string(args, "cluster")?;
        self.config.cluster(cluster)?;
        let wanted = required_string(args, "script")?;
        let result = (|| {
            let config = self.current_bank_config()?;
            let snapshot = bank::catalog_fresh(&config)?;
            let script = exact_script(&snapshot.scripts, wanted, cluster)?;
            bank::validate_script_controller(script, config.cluster(cluster)?)?;
            let digest = sha256(&script.bytes);
            let catalog_generation = format!("{:016x}", script.bank_fingerprint);
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
                catalog_generation: catalog_generation.clone(),
                repo_commit: script.repo_commit.clone(),
            };
            let token = preview_token()?;
            let mut previews = self
                .previews
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            previews.retain(|_, value| value.created.elapsed() < super::service::PREVIEW_TTL);
            if previews.len() >= 256 {
                bail!("too many active submission previews");
            }
            previews.insert(token.clone(), preview.clone());
            Ok(json!({
                "ok":true,"preview_token":token,"expires_in_seconds":super::service::PREVIEW_TTL.as_secs(),
                "cluster":cluster,"script":preview.script,"script_sha256":digest,
                "directives":preview.directives,"working_directory":preview.working_directory,
                "job_name":preview.job_name,
                "catalog_generation":catalog_generation,
                "repo_commit":preview.repo_commit,
                "catalog_indexed_at":iso_timestamp(script.indexed_at),
                "resources":script_resources(&script.directives),
                "warnings":snapshot.warnings
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

    pub(crate) fn submit_job(&self, args: &JsonObject, client: &str) -> Result<Value> {
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
        let audit_id = audit::audit_id()?;
        let result = (|| {
            if preview.created.elapsed() >= super::service::PREVIEW_TTL {
                bail!("preview token expired");
            }
            let config = self.current_bank_config()?;
            let snapshot = bank::catalog_fresh(&config)?;
            let script = exact_script(&snapshot.scripts, &preview.script, &preview.cluster)?;
            bank::validate_script_controller(script, config.cluster(&preview.cluster)?)?;
            let digest = sha256(&script.bytes);
            let working_directory = self
                .config
                .cluster(&preview.cluster)?
                .working_directory
                .display()
                .to_string();
            let catalog_generation = format!("{:016x}", script.bank_fingerprint);
            if digest != preview.digest
                || script.directives != preview.directives
                || script.name != preview.job_name
                || working_directory != preview.working_directory
                || catalog_generation != preview.catalog_generation
                || script.repo_commit != preview.repo_commit
            {
                bail!("preview is stale because the script or target configuration changed");
            }
            let job = bank::submit(&config, script, &preview.cluster)?;
            Ok(json!({
                "ok":true,
                "cluster":job.cluster,
                "job_id":job.id,
                "job_name":job.name,
                "script_sha256":digest,
                "initial_state":"PENDING",
                "submitted_at":now_iso(),
                "log_uri":format!("slurm-log://jobs/{}/{}/log", job.cluster, job.id),
                "audit_id":audit_id,
                "provenance":{
                    "source":"mcp_preview",
                    "script":preview.script,
                    "catalog_generation":catalog_generation,
                    "repo_commit":script.repo_commit
                }
            }))
        })();
        let status = result.as_ref().map(|_| "submitted").unwrap_or("rejected");
        let _ = audit::record_with_id(
            &self.config,
            client,
            "slurm_submit_job",
            &preview.cluster,
            &preview.script,
            Some(&preview.digest),
            status,
            &audit_id,
        );
        result
    }
}
