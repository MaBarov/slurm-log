use anyhow::{Context, Result, bail};
use rmcp::model::JsonObject;
use serde_json::Value;

pub fn tool_arguments(name: &str, args: &JsonObject) -> Result<()> {
    match name {
        "slurm_list_clusters" | "slurm_workspace_context" => no_arguments(args),
        "slurm_doctor" | "slurm_refresh_bank" => no_arguments(args),
        "slurm_wait_job" => {
            job(args, &["until", "timeout_seconds", "poll_interval"])?;
            string_array(args, "until", 3, 24)?;
            integer(args, "timeout_seconds", 1, 40)?;
            integer(args, "poll_interval", 1, 10)
        }
        "slurm_explain_pending" => job(args, &[]),
        "slurm_adopt_job" => {
            job(args, &["expected_job_name", "batch_script_sha256"])?;
            required_string(args, "expected_job_name", 256)?;
            strings(args, &[("batch_script_sha256", 64)])
        }
        "slurm_list_jobs" => {
            keys(
                args,
                &[
                    "cluster",
                    "history",
                    "states",
                    "include_blocked",
                    "search",
                    "cursor",
                    "limit",
                ],
            )?;
            strings(
                args,
                &[
                    ("cluster", 48),
                    ("history", 8),
                    ("search", 256),
                    ("cursor", 128),
                ],
            )?;
            boolean(args, "include_blocked")?;
            integer(args, "limit", 1, 200)?;
            string_array(args, "states", 32, 64)
        }
        "slurm_inspect_job" | "slurm_diagnose_job" => job(args, &[]),
        "slurm_read_log" => {
            job(args, &["cursor", "lines", "filter"])?;
            strings(args, &[("cursor", 256), ("filter", 32)])?;
            integer(args, "lines", 1, 2000)
        }
        "slurm_search_log" => {
            job(args, &["pattern", "regex", "max_matches", "context_lines"])?;
            required_string(args, "pattern", 1024)?;
            boolean(args, "regex")?;
            integer(args, "max_matches", 1, 500)?;
            integer(args, "context_lines", 0, 20)
        }
        "slurm_list_scripts" => {
            keys(args, &["cluster", "search", "cursor", "limit"])?;
            strings(args, &[("cluster", 48), ("search", 256), ("cursor", 128)])?;
            integer(args, "limit", 1, 200)
        }
        "slurm_preview_submission" => {
            keys(args, &["cluster", "script"])?;
            required_string(args, "cluster", 48)?;
            required_string(args, "script", 4096)
        }
        "slurm_submit_job" => {
            keys(args, &["preview_token"])?;
            required_string(args, "preview_token", 256)
        }
        "slurm_cancel_job" => {
            job(args, &["expected_job_name"])?;
            required_string(args, "expected_job_name", 256)
        }
        _ => bail!("unknown tool {name}"),
    }
}

fn job(args: &JsonObject, extra: &[&str]) -> Result<()> {
    let mut allowed = vec!["cluster", "job_id"];
    allowed.extend_from_slice(extra);
    keys(args, &allowed)?;
    required_string(args, "cluster", 48)?;
    required_string(args, "job_id", 128)
}

fn no_arguments(args: &JsonObject) -> Result<()> {
    if args.is_empty() {
        Ok(())
    } else {
        bail!("this tool accepts no arguments")
    }
}

fn keys(args: &JsonObject, allowed: &[&str]) -> Result<()> {
    if let Some(key) = args.keys().find(|key| !allowed.contains(&key.as_str())) {
        bail!("unknown argument {key}");
    }
    Ok(())
}

fn strings(args: &JsonObject, fields: &[(&str, usize)]) -> Result<()> {
    for (name, maximum) in fields {
        if let Some(value) = args.get(*name) {
            let value = value
                .as_str()
                .with_context(|| format!("{name} must be a string"))?;
            if value.len() > *maximum {
                bail!("{name} exceeds {maximum} bytes");
            }
        }
    }
    Ok(())
}

fn required_string(args: &JsonObject, name: &str, maximum: usize) -> Result<()> {
    strings(args, &[(name, maximum)])?;
    if args
        .get(name)
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        bail!("{name} is required");
    }
    Ok(())
}

fn boolean(args: &JsonObject, name: &str) -> Result<()> {
    if args.get(name).is_some_and(|value| !value.is_boolean()) {
        bail!("{name} must be a boolean");
    }
    Ok(())
}

fn integer(args: &JsonObject, name: &str, minimum: u64, maximum: u64) -> Result<()> {
    if let Some(value) = args.get(name) {
        let value = value
            .as_u64()
            .with_context(|| format!("{name} must be a non-negative integer"))?;
        if !(minimum..=maximum).contains(&value) {
            bail!("{name} must be between {minimum} and {maximum}");
        }
    }
    Ok(())
}

fn string_array(args: &JsonObject, name: &str, maximum: usize, string_max: usize) -> Result<()> {
    let Some(value) = args.get(name) else {
        return Ok(());
    };
    let values = value
        .as_array()
        .with_context(|| format!("{name} must be an array"))?;
    if values.len() > maximum {
        bail!("{name} exceeds {maximum} items");
    }
    if values
        .iter()
        .any(|value| value.as_str().is_none_or(|value| value.len() > string_max))
    {
        bail!("{name} must contain strings of at most {string_max} bytes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> JsonObject {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn hostile_unknown_and_wrong_typed_arguments_are_rejected() {
        assert!(
            tool_arguments(
                "slurm_read_log",
                &object(json!({
                    "cluster":"alpha","job_id":"1","path":"/etc/passwd"
                }))
            )
            .is_err()
        );
        assert!(tool_arguments("slurm_list_jobs", &object(json!({"limit":0}))).is_err());
        assert!(tool_arguments("slurm_list_jobs", &object(json!({"cluster":7}))).is_err());
        assert!(tool_arguments("missing", &JsonObject::new()).is_err());
    }

    #[test]
    fn every_tool_shape_and_bound_is_validated() {
        for name in ["slurm_list_clusters", "slurm_workspace_context"] {
            assert!(tool_arguments(name, &JsonObject::new()).is_ok());
            assert!(tool_arguments(name, &object(json!({"extra":true}))).is_err());
        }
        assert!(
            tool_arguments(
                "slurm_list_jobs",
                &object(json!({
                    "cluster":"alpha", "history":"1d", "states":["RUNNING"],
                    "include_blocked":false, "search":"x", "cursor":"jobs:1", "limit":200
                }))
            )
            .is_ok()
        );
        assert!(
            tool_arguments(
                "slurm_inspect_job",
                &object(json!({"cluster":"a","job_id":"1"}))
            )
            .is_ok()
        );
        assert!(
            tool_arguments(
                "slurm_diagnose_job",
                &object(json!({"cluster":"a","job_id":"1"}))
            )
            .is_ok()
        );
        assert!(
            tool_arguments(
                "slurm_read_log",
                &object(json!({
                    "cluster":"a","job_id":"1","cursor":"v1:x","lines":2000,"filter":"all"
                }))
            )
            .is_ok()
        );
        assert!(
            tool_arguments(
                "slurm_search_log",
                &object(json!({
                    "cluster":"a","job_id":"1","pattern":"x","regex":true,
                    "max_matches":500,"context_lines":0
                }))
            )
            .is_ok()
        );
        assert!(tool_arguments("slurm_list_scripts", &object(json!({"limit":1}))).is_ok());
        for name in ["slurm_doctor", "slurm_refresh_bank"] {
            assert!(tool_arguments(name, &JsonObject::new()).is_ok());
            assert!(tool_arguments(name, &object(json!({"extra":1}))).is_err());
        }
        assert!(
            tool_arguments(
                "slurm_wait_job",
                &object(json!({
                    "cluster":"a","job_id":"1",
                    "until":["state_change","completion"],"timeout_seconds":40,"poll_interval":1
                }))
            )
            .is_ok()
        );
        assert!(
            tool_arguments(
                "slurm_wait_job",
                &object(json!({"cluster":"a","job_id":"1","until":["fly"]}))
            )
            .is_ok()
        );
        assert!(
            tool_arguments(
                "slurm_explain_pending",
                &object(json!({"cluster":"a","job_id":"1"}))
            )
            .is_ok()
        );
        assert!(
            tool_arguments(
                "slurm_adopt_job",
                &object(json!({
                    "cluster":"a","job_id":"1","expected_job_name":"train",
                    "batch_script_sha256":"a".repeat(64)
                }))
            )
            .is_ok()
        );
        assert!(
            tool_arguments(
                "slurm_adopt_job",
                &object(json!({"cluster":"a","job_id":"1","expected_job_name":"train"}))
            )
            .is_ok()
        );
        assert!(
            tool_arguments(
                "slurm_preview_submission",
                &object(json!({
                    "cluster":"a","script":"Bank/x.sbatch"
                }))
            )
            .is_ok()
        );
        assert!(tool_arguments("slurm_submit_job", &object(json!({"preview_token":"x"}))).is_ok());
        assert!(
            tool_arguments(
                "slurm_cancel_job",
                &object(json!({
                    "cluster":"a","job_id":"1","expected_job_name":"train"
                }))
            )
            .is_ok()
        );

        assert!(
            tool_arguments(
                "slurm_search_log",
                &object(json!({
                    "cluster":"a","job_id":"1","pattern":"x","regex":"yes"
                }))
            )
            .is_err()
        );
        assert!(tool_arguments("slurm_list_jobs", &object(json!({"states":"RUNNING"}))).is_err());
        assert!(tool_arguments("slurm_list_jobs", &object(json!({"states":[7]}))).is_err());
        assert!(
            tool_arguments("slurm_list_jobs", &object(json!({"states":vec!["x"; 33]}))).is_err()
        );
        assert!(tool_arguments("slurm_submit_job", &object(json!({"preview_token":""}))).is_err());
        assert!(
            tool_arguments(
                "slurm_search_log",
                &object(json!({
                    "cluster":"a","job_id":"1","pattern":"x","context_lines":21
                }))
            )
            .is_err()
        );
    }
}
