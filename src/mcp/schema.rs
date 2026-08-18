use std::{collections::BTreeMap, sync::Arc};

use rmcp::model::{JsonObject, ListToolsResult, PaginatedRequestParams, Tool, ToolAnnotations};
use serde_json::{Value, json};

use crate::config::Config;

pub fn tools(config: &Config) -> Vec<Tool> {
    let clusters: Vec<_> = config
        .clusters
        .iter()
        .map(|cluster| Value::String(cluster.name.clone()))
        .collect();
    let all_clusters = std::iter::once(Value::String("all".into()))
        .chain(clusters.iter().cloned())
        .collect::<Vec<_>>();
    let exact = cluster_property(&clusters);
    let listing = cluster_property(&all_clusters);
    vec![
        read_tool(
            "slurm_list_clusters",
            "List configured clusters without SSH hosts or credentials.",
            object(BTreeMap::new(), &[]),
        ),
        read_tool(
            "slurm_list_jobs",
            "List owner-scoped jobs with bounded history, filters, and cursor pagination.",
            object(
                BTreeMap::from([
                    ("cluster", listing.clone()),
                    (
                        "history",
                        enum_property(&["live", "2h", "12h", "1d", "1w", "all"], Some("live")),
                    ),
                    (
                        "states",
                        json!({"type":"array","maxItems":32,"items":{"type":"string","maxLength":64}}),
                    ),
                    ("include_blocked", json!({"type":"boolean","default":false})),
                    ("search", json!({"type":"string","maxLength":256})),
                    ("cursor", json!({"type":"string","maxLength":128})),
                    ("limit", bounded_integer(1, 200, 50)),
                ]),
                &[],
            ),
        ),
        read_tool(
            "slurm_inspect_job",
            "Inspect one exact cluster-qualified job, including scheduler, usage, placement, exit, and log metadata.",
            job_input(exact.clone()),
        ),
        read_tool(
            "slurm_workspace_context",
            "Return unambiguous open slurm-log tmux workspaces and focused jobs.",
            object(BTreeMap::new(), &[]),
        ),
        read_tool(
            "slurm_read_log",
            "Read a bounded tail or generation-aware incremental segment of an exact job log.",
            object(
                BTreeMap::from([
                    ("cluster", exact.clone()),
                    ("job_id", job_id()),
                    ("cursor", json!({"type":"string","maxLength":256})),
                    ("lines", bounded_integer(1, 2000, 200)),
                    (
                        "filter",
                        enum_property(
                            &["hide_warnings", "all", "warnings", "exceptions"],
                            Some("hide_warnings"),
                        ),
                    ),
                ]),
                &["cluster", "job_id"],
            ),
        ),
        read_tool(
            "slurm_search_log",
            "Search only the newest 4 MiB using a literal string or Rust linear-time regex.",
            object(
                BTreeMap::from([
                    ("cluster", exact.clone()),
                    ("job_id", job_id()),
                    (
                        "pattern",
                        json!({"type":"string","minLength":1,"maxLength":1024}),
                    ),
                    ("regex", json!({"type":"boolean","default":false})),
                    ("max_matches", bounded_integer(1, 500, 100)),
                    ("context_lines", bounded_integer(0, 20, 2)),
                ]),
                &["cluster", "job_id", "pattern"],
            ),
        ),
        read_tool(
            "slurm_diagnose_job",
            "Produce deterministic findings and checks from state, details, and a bounded log window.",
            job_input(exact.clone()),
        ),
        read_tool(
            "slurm_list_scripts",
            "List configured-bank sbatch scripts, directives, and eligible clusters.",
            object(
                BTreeMap::from([
                    ("cluster", listing),
                    ("search", json!({"type":"string","maxLength":256})),
                    ("cursor", json!({"type":"string","maxLength":128})),
                    ("limit", bounded_integer(1, 200, 50)),
                ]),
                &[],
            ),
        ),
        read_tool(
            "slurm_doctor",
            "End-to-end health: real squeue, sacct, and sinfo probes per cluster plus separate bank-catalog and daemon health with warnings.",
            object(BTreeMap::new(), &[]),
        ),
        mutation_tool(
            "slurm_refresh_bank",
            "Force a fresh scan of every configured sbatch bank and report catalog generation and per-bank provenance.",
            object(BTreeMap::new(), &[]),
            false,
        ),
        read_tool(
            "slurm_wait_job",
            "Bounded server-side wait for a state change, completion, or log change of one exact owned job.",
            object(
                BTreeMap::from([
                    ("cluster", exact.clone()),
                    ("job_id", job_id()),
                    (
                        "until",
                        json!({
                            "type":"array",
                            "maxItems":3,
                            "items":{"type":"string","enum":["state_change","completion","log_change"]},
                            "default":["state_change"]
                        }),
                    ),
                    ("timeout_seconds", bounded_integer(1, 40, 30)),
                    ("poll_interval", bounded_integer(1, 10, 3)),
                ]),
                &["cluster", "job_id"],
            ),
        ),
        read_tool(
            "slurm_explain_pending",
            "Explain why one exact owned pending job is pending, including reservation conflicts and partition availability; never auto-switches partitions.",
            job_input(exact.clone()),
        ),
        mutation_tool(
            "slurm_adopt_job",
            "Adopt a job that was submitted outside MCP into the provenance ledger as externally submitted, recording the observed batch-script hash.",
            object(
                BTreeMap::from([
                    ("cluster", exact.clone()),
                    ("job_id", job_id()),
                    (
                        "expected_job_name",
                        json!({"type":"string","minLength":1,"maxLength":256}),
                    ),
                    (
                        "batch_script_sha256",
                        json!({"type":"string","pattern":"^[0-9a-fA-F]{64}$"}),
                    ),
                ]),
                &["cluster", "job_id", "expected_job_name"],
            ),
            false,
        ),
        mutation_tool(
            "slurm_preview_submission",
            "Preview an exact configured-bank script for an explicitly selected cluster and mint a five-minute one-use token.",
            object(
                BTreeMap::from([
                    ("cluster", exact.clone()),
                    (
                        "script",
                        json!({"type":"string","minLength":1,"maxLength":4096}),
                    ),
                ]),
                &["cluster", "script"],
            ),
            false,
        ),
        mutation_tool(
            "slurm_submit_job",
            "Consume one valid preview token and submit the unchanged exact bank script.",
            object(
                BTreeMap::from([(
                    "preview_token",
                    json!({"type":"string","minLength":32,"maxLength":256}),
                )]),
                &["preview_token"],
            ),
            true,
        ),
        mutation_tool(
            "slurm_cancel_job",
            "Cancel exactly one active cluster-qualified ordinary job or one controller-proven array task after expected-name revalidation; array masters and ranges are rejected.",
            object(
                BTreeMap::from([
                    ("cluster", exact),
                    ("job_id", job_id()),
                    (
                        "expected_job_name",
                        json!({"type":"string","minLength":1,"maxLength":256}),
                    ),
                ]),
                &["cluster", "job_id", "expected_job_name"],
            ),
            true,
        ),
    ]
}

fn read_tool(name: &'static str, description: &'static str, input: JsonObject) -> Tool {
    base_tool(name, description, input).with_annotations(
        ToolAnnotations::with_title(name.replace('_', " "))
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
    )
}

fn mutation_tool(
    name: &'static str,
    description: &'static str,
    input: JsonObject,
    destructive: bool,
) -> Tool {
    base_tool(name, description, input).with_annotations(
        ToolAnnotations::with_title(name.replace('_', " "))
            .read_only(false)
            .destructive(destructive)
            .idempotent(false)
            .open_world(false),
    )
}

fn base_tool(name: &'static str, description: &'static str, input: JsonObject) -> Tool {
    Tool::new(name, description, input).with_raw_output_schema(Arc::new(output_schema()))
}

fn output_schema() -> JsonObject {
    value_object(json!({
        "type": "object",
        "properties": {"ok": {"type": "boolean"}},
        "required": ["ok"],
        "additionalProperties": true
    }))
}

fn job_input(cluster: Value) -> JsonObject {
    object(
        BTreeMap::from([("cluster", cluster), ("job_id", job_id())]),
        &["cluster", "job_id"],
    )
}

fn job_id() -> Value {
    json!({"type":"string","pattern":"^[0-9]+(?:_[0-9]+)?$","maxLength":128})
}

fn cluster_property(values: &[Value]) -> Value {
    json!({"type":"string","enum":values})
}

fn enum_property(values: &[&str], default: Option<&str>) -> Value {
    let mut value = json!({"type":"string","enum":values});
    if let Some(default) = default {
        value["default"] = Value::String(default.into());
    }
    value
}

fn bounded_integer(minimum: u64, maximum: u64, default: u64) -> Value {
    json!({"type":"integer","minimum":minimum,"maximum":maximum,"default":default})
}

fn object(properties: BTreeMap<&str, Value>, required: &[&str]) -> JsonObject {
    value_object(json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    }))
}

fn value_object(value: Value) -> JsonObject {
    value.as_object().cloned().expect("schema object")
}

pub fn paginate_tools(
    tools: &[Tool],
    request: Option<PaginatedRequestParams>,
) -> Result<ListToolsResult, String> {
    let start = match request.and_then(|value| value.cursor) {
        None => 0,
        Some(cursor) => cursor
            .strip_prefix("t:")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value <= tools.len())
            .ok_or_else(|| "invalid tool pagination cursor".to_string())?,
    };
    let end = (start + 50).min(tools.len());
    let mut result = ListToolsResult::with_all_items(tools[start..end].to_vec());
    if end < tools.len() {
        result.next_cursor = Some(format!("t:{end}"));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn schemas_are_object_rooted_and_mutations_are_not_read_only() {
        let config = Config {
            local_user: "alice".into(),
            remote_user: "alice".into(),
            ssh_host: String::new(),
            state_path: PathBuf::from("/tmp/state"),
            executable: PathBuf::from("/bin/slurm-log"),
            sbatch_banks: Vec::new(),
            clusters: vec![crate::config::ClusterConfig {
                name: "alpha".into(),
                controller: None,
                transport: "local".into(),
                user: "alice".into(),
                ssh_host: String::new(),
                working_directory: PathBuf::from("/tmp"),
                accounting: false,
            }],
        };
        let tools = tools(&config);
        assert_eq!(tools.len(), 16);
        for tool in &tools {
            assert_eq!(
                tool.input_schema.get("type").and_then(Value::as_str),
                Some("object")
            );
            assert_eq!(
                tool.output_schema
                    .as_ref()
                    .unwrap()
                    .get("type")
                    .and_then(Value::as_str),
                Some("object")
            );
        }
        let submit = tools
            .iter()
            .find(|tool| tool.name == "slurm_submit_job")
            .unwrap();
        assert_eq!(
            submit.annotations.as_ref().unwrap().read_only_hint,
            Some(false)
        );
        assert_eq!(
            submit.annotations.as_ref().unwrap().destructive_hint,
            Some(true)
        );
    }
}
