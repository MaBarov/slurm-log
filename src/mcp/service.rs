use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use serde_json::json;

use super::{
    McpServer,
    present::{bounded_error, fallback_text},
};

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
    pub overrides: Option<BTreeMap<String, String>>,
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
            "slurm_doctor" => self.doctor(),
            "slurm_refresh_bank" => self.refresh_bank(),
            "slurm_wait_job" => self.wait_job(&args),
            "slurm_explain_pending" => self.explain_pending(&args),
            "slurm_find_artifact" => self.find_artifact(&args),
            "slurm_read_declared_result" => self.read_declared_result(&args),
            "slurm_stage_bundle" => self.stage_bundle(&args, client),
            "slurm_adopt_job" => self.adopt_job(&args, client),
            "slurm_preflight_job" => self.preflight_job(&args, client),
            "slurm_preview_resubmit" => self.preview_resubmit(&args, client),
            "slurm_preview_submission" => self.preview_submission(&args, client),
            "slurm_submit_job" => self.submit_job(&args, client),
            "slurm_cancel_job" => self.cancel_job(&args, client),
            _ => Err(anyhow::anyhow!("unknown tool {name}")),
        });
        match result {
            Ok(value) => {
                let fallback = fallback_text(name, &value);
                let mut result = CallToolResult::structured(value);
                result.content = vec![ContentBlock::text(fallback)];
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
}

#[cfg(test)]
#[path = "service/tests.rs"]
mod tests;
