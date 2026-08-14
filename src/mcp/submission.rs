use std::time::Instant;

use anyhow::{Context, Result, bail};
use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::{
    McpServer, audit,
    helpers::*,
    present::now_rfc3339,
    service::{PREVIEW_TTL, Preview},
};
use crate::{bank, config::Config, slurm};

impl McpServer {
    pub(crate) fn preview_resubmit(&self, args: &JsonObject, client: &str) -> Result<Value> {
        let cluster = required_string(args, "cluster")?;
        self.config.cluster(cluster)?;
        let id = required_string(args, "job_id")?;
        if !crate::model::valid_job_id(id) {
            bail!("invalid job ID {id}");
        }
        let wanted = required_string(args, "script")?;
        let overrides = bank::parse_overrides(args.get("schedule_overrides"))?;
        audit::record(
            &self.config,
            client,
            "slurm_preview_resubmit",
            cluster,
            id,
            None,
            "attempted",
        )?;
        let result = (|| {
            let config = self.current_bank_config()?;
            let job = slurm::authorize_exact_job(&config, cluster, id)?;
            if job.active() {
                bail!(
                    "job {cluster}:{id} is still active (state {}); wait for a terminal state before resubmitting",
                    job.state
                );
            }
            let (scripts, warnings, catalog) = bank::catalog(&config, true)?;
            let script = exact_script(&scripts, wanted, cluster)?;
            bank::validate_script_controller(script, config.cluster(cluster)?)?;
            if script.name != job.name {
                bail!(
                    "job name mismatch: recorded {:?}, bank script {:?}",
                    job.name,
                    script.name
                );
            }
            let digest = sha256(&script.bytes);
            let recorded = crate::state::Ledger::producer_hash(&config.state_path, cluster, id)
                .context("job has no recorded producer hash to resubmit")?;
            if recorded != digest {
                bail!(
                    "producer script changed since the job ran; the recorded hash no longer matches the bank script"
                );
            }
            let working_directory = self
                .config
                .cluster(cluster)?
                .working_directory
                .display()
                .to_string();
            let preview = Preview {
                created: Instant::now(),
                cluster: cluster.into(),
                script: script_id(script),
                digest: digest.clone(),
                directives: script.directives.clone(),
                working_directory,
                job_name: script.name.clone(),
                catalog_generation: catalog.generation.clone(),
                overrides: overrides.clone(),
            };
            let token = preview_token()?;
            let mut previews = self
                .previews
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            previews.retain(|_, value| value.created.elapsed() < PREVIEW_TTL);
            if previews.len() >= 256 {
                bail!("too many active submission previews");
            }
            previews.insert(token.clone(), preview);
            Ok(json!({
                "ok": true,
                "preview_token": token,
                "expires_in_seconds": PREVIEW_TTL.as_secs(),
                "cluster": cluster,
                "job_id": id,
                "resubmit": true,
                "script": script_id(script),
                "script_sha256": digest,
                "catalog_generation": catalog.generation,
                "job_name": script.name,
                "schedule_overrides": overrides,
                "warnings": warnings,
            }))
        })();
        let status = result.as_ref().map(|_| "previewed").unwrap_or("rejected");
        let _ = audit::record(
            &self.config,
            client,
            "slurm_preview_resubmit",
            cluster,
            id,
            None,
            status,
        );
        result
    }

    pub(crate) fn adopt_job(&self, args: &JsonObject, client: &str) -> Result<Value> {
        let (cluster, id) = exact_job(&self.config, args)?;
        let hash = optional_string(args, "batch_script_sha256").filter(|value| {
            value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
        let job = slurm::authorize_exact_job(&self.config, cluster, id)?;
        crate::state::Ledger::mark_adopted(&self.config.state_path, &job, hash)?;
        audit::record(
            &self.config,
            client,
            "slurm_adopt_job",
            cluster,
            id,
            hash,
            "adopted",
        )?;
        Ok(json!({
            "ok": true,
            "cluster": cluster,
            "job_id": id,
            "job_name": job.name,
            "state": job.state,
            "adopted": true,
            "externally_submitted": true,
            "batch_script_sha256": hash,
            "preview_authorized": false,
        }))
    }

    pub(crate) fn preview_submission(&self, args: &JsonObject, client: &str) -> Result<Value> {
        let cluster = required_string(args, "cluster")?;
        self.config.cluster(cluster)?;
        let wanted = required_string(args, "script")?;
        let result = (|| {
            let config = self.current_bank_config()?;
            let (scripts, warnings, catalog) = bank::catalog(&config, true)?;
            let script = exact_script(&scripts, wanted, cluster)?;
            bank::validate_script_controller(script, config.cluster(cluster)?)?;
            let digest = sha256(&script.bytes);
            let catalog_generation = catalog.generation.clone();
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
                overrides: None,
            };
            let token = preview_token()?;
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
                "catalog_generation":catalog_generation,
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
        let result = (|| {
            if preview.created.elapsed() >= PREVIEW_TTL {
                bail!("preview token expired");
            }
            let config = self.current_bank_config()?;
            let (scripts, _) = bank::configured_scripts_fresh(&config)?;
            let script = exact_script(&scripts, &preview.script, &preview.cluster)?;
            bank::validate_script_controller(script, config.cluster(&preview.cluster)?)?;
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
                || bank::catalog_generation(&config) != preview.catalog_generation
            {
                bail!("preview is stale because the script or target configuration changed");
            }
            let mut submission = script.clone();
            if let Some(overrides) = &preview.overrides {
                submission.bytes = bank::apply_schedule_overrides(&script.bytes, overrides);
            }
            let job = bank::submit(&config, &submission, &preview.cluster)?;
            crate::state::Ledger::mark_submitted(
                &config.state_path,
                &preview.cluster,
                &job.id,
                &digest,
            )?;
            let submission_id = preview_token()?;
            let qualified = format!("{}:{}", job.cluster, job.id);
            Ok(json!({
                "ok":true,
                "cluster":job.cluster,
                "job_id":job.id,
                "qualified_job_id":qualified,
                "job_name":job.name,
                "script_sha256":digest,
                "submission_id":submission_id,
                "submit_timestamp":now_rfc3339(),
                "initial_state":"PENDING",
                "log_uri":format!("slurm-log://jobs/{}/{}", job.cluster, job.id),
                "log_resource":format!("slurm-log://jobs/{}/{}/log", job.cluster, job.id)
            }))
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

    pub(crate) fn current_bank_config(&self) -> Result<Config> {
        let current = Config::load_for_setup().context("reload sbatch bank configuration")?;
        let mut config = self.config.as_ref().clone();
        config.sbatch_banks = current.sbatch_banks;
        config.validate()?;
        Ok(config)
    }

    pub(crate) fn cancel_job(&self, args: &JsonObject, client: &str) -> Result<Value> {
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
