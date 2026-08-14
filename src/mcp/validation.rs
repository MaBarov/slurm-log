use anyhow::{Context, Result, bail};
use rmcp::model::JsonObject;
use serde_json::Value;

pub fn tool_arguments(name: &str, args: &JsonObject) -> Result<()> {
    match name {
        "slurm_list_clusters" | "slurm_workspace_context" => no_arguments(args),
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
        "slurm_doctor" | "slurm_refresh_bank" => no_arguments(args),
        "slurm_wait_job" => {
            job(args, &["until", "timeout_seconds", "interval_seconds"])?;
            strings(args, &[("until", 16)])?;
            integer(args, "timeout_seconds", 1, 30)?;
            integer(args, "interval_seconds", 1, 10)
        }
        "slurm_explain_pending" => job(args, &[]),
        "slurm_find_artifact" => {
            job(args, &["pattern", "search_root", "max_bytes"])?;
            required_string(args, "pattern", 256)?;
            strings(args, &[("search_root", 512)])?;
            integer(args, "max_bytes", 1, 262144)
        }
        "slurm_read_declared_result" => {
            job(args, &["result", "search_root", "max_bytes"])?;
            strings(args, &[("result", 128), ("search_root", 512)])?;
            integer(args, "max_bytes", 1, 262144)
        }
        "slurm_stage_bundle" => {
            keys(args, &["bank", "entries", "destination", "version"])?;
            strings(args, &[("bank", 48), ("destination", 16), ("version", 8)])?;
            if let Some(destination) = args.get("destination").and_then(Value::as_str)
                && !matches!(destination, "local" | "remote")
            {
                bail!("destination must be local or remote");
            }
            if let Some(version) = args.get("version").and_then(Value::as_str)
                && version != "v1"
            {
                bail!("unsupported bundle version {version}");
            }
            let entries = args
                .get("entries")
                .and_then(Value::as_array)
                .with_context(|| "entries is required")?;
            if entries.is_empty() || entries.len() > 512 {
                bail!("entries must contain 1..512 paths");
            }
            for entry in entries {
                let entry = entry
                    .as_str()
                    .with_context(|| "each bundle entry must be a string")?;
                if entry.is_empty() || entry.len() > 1024 {
                    bail!("each bundle entry must be 1..1024 bytes");
                }
                crate::bank::validate_manifest_path(entry)?;
            }
            Ok(())
        }
        "slurm_adopt_job" => {
            job(args, &["batch_script_sha256"])?;
            strings(args, &[("batch_script_sha256", 64)])?;
            if let Some(hash) = args.get("batch_script_sha256").and_then(Value::as_str)
                && (hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
            {
                bail!("batch_script_sha256 must be a 64-character lowercase hex digest");
            }
            Ok(())
        }
        "slurm_preflight_job" => {
            keys(args, &["cluster", "script", "wait_seconds"])?;
            required_string(args, "cluster", 48)?;
            required_string(args, "script", 4096)?;
            integer(args, "wait_seconds", 1, 60)
        }
        "slurm_preview_resubmit" => {
            keys(args, &["cluster", "job_id", "script", "schedule_overrides"])?;
            required_string(args, "cluster", 48)?;
            required_string(args, "job_id", 128)?;
            required_string(args, "script", 4096)?;
            if let Some(overrides) = args.get("schedule_overrides") {
                let object = overrides
                    .as_object()
                    .with_context(|| "schedule_overrides must be an object")?;
                if object.len() > 12 {
                    bail!("schedule_overrides exceeds 12 keys");
                }
                for (key, value) in object {
                    if !crate::bank::SCHEDULE_OVERRIDE_KEYS.contains(&key.as_str()) {
                        bail!("unknown schedule override {key}");
                    }
                    let value = value
                        .as_str()
                        .with_context(|| format!("schedule override {key} must be a string"))?;
                    let maximum = if key == "gres" || key == "dependency" {
                        256
                    } else {
                        128
                    };
                    if value.is_empty() || value.len() > maximum {
                        bail!("schedule override {key} must be 1..{maximum} bytes");
                    }
                }
            }
            Ok(())
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
#[path = "validation/tests.rs"]
mod tests;
