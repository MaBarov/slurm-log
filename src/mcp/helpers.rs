use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use rmcp::model::JsonObject;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::service::Preview;
use crate::{bank, config::Config, slurm};

static TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(super) fn exact_job<'a>(config: &Config, args: &'a JsonObject) -> Result<(&'a str, &'a str)> {
    let cluster = required_string(args, "cluster")?;
    config.cluster(cluster)?;
    let id = required_string(args, "job_id")?;
    if !crate::model::valid_job_id(id) {
        bail!("invalid job ID {id}");
    }
    Ok((cluster, id))
}

pub(super) fn required_string<'a>(args: &'a JsonObject, name: &str) -> Result<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{name} is required"))
}

pub(super) fn optional_string<'a>(args: &'a JsonObject, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str)
}

pub(super) fn optional_bool(args: &JsonObject, name: &str) -> Option<bool> {
    args.get(name).and_then(Value::as_bool)
}

pub(super) fn optional_usize(args: &JsonObject, name: &str, default: usize) -> Result<usize> {
    match args.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .with_context(|| format!("{name} must be a non-negative integer")),
    }
}

pub(super) fn optional_strings(args: &JsonObject, name: &str) -> Result<Vec<String>> {
    let Some(values) = args.get(name) else {
        return Ok(Vec::new());
    };
    values
        .as_array()
        .context("states must be an array")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_uppercase)
                .context("state filters must be strings")
        })
        .collect()
}

pub(super) fn page(
    args: &JsonObject,
    prefix: &str,
    default: usize,
    max: usize,
) -> Result<(usize, usize)> {
    let limit = optional_usize(args, "limit", default)?.clamp(1, max);
    let start = match optional_string(args, "cursor") {
        None => 0,
        Some(cursor) => cursor
            .strip_prefix(&format!("{prefix}:"))
            .and_then(|value| value.parse::<usize>().ok())
            .context("invalid pagination cursor")?,
    };
    Ok((start, limit))
}

pub(super) fn history_mode(value: &str) -> Result<slurm::HistoryMode> {
    Ok(match value {
        "live" => slurm::HistoryMode::Live,
        "2h" => slurm::HistoryMode::Hours2,
        "12h" => slurm::HistoryMode::Hours12,
        "1d" => slurm::HistoryMode::Day1,
        "1w" => slurm::HistoryMode::Week1,
        "all" => slurm::HistoryMode::All,
        _ => bail!("invalid history window {value}"),
    })
}

pub(super) fn dependencies(config: &Config, cluster: &str, id: &str) -> Result<Vec<String>> {
    let value = slurm::scheduler_text(config, cluster, "scontrol", &["show", "job", "-o", id])?;
    let dependency = value
        .split_whitespace()
        .find_map(|field| field.strip_prefix("Dependency="))
        .unwrap_or_default();
    if dependency.is_empty() || matches!(dependency, "(null)" | "None") {
        return Ok(Vec::new());
    }
    Ok(dependency.split(',').map(str::to_string).collect())
}

pub(super) fn exact_script<'a>(
    scripts: &'a [bank::Script],
    wanted: &str,
    cluster: &str,
) -> Result<&'a bank::Script> {
    let mut matches = scripts
        .iter()
        .filter(|script| bank::supports_cluster(script, cluster) && script_id(script) == wanted);
    let first = matches
        .next()
        .context("script is not in an eligible configured bank")?;
    if matches.next().is_some() {
        bail!("script identity is ambiguous");
    }
    Ok(first)
}

pub(super) fn script_id(script: &bank::Script) -> String {
    format!("{}/{}", script.bank, script.relative.display())
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn preview_token(preview: &Preview) -> String {
    let nonce = TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut digest = Sha256::new();
    digest.update(b"slurm-log-preview-v1\0");
    digest.update(preview.cluster.as_bytes());
    digest.update(preview.script.as_bytes());
    digest.update(preview.digest.as_bytes());
    digest.update(std::process::id().to_le_bytes());
    digest.update(now.to_le_bytes());
    digest.update(nonce.to_le_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn bounded_text(value: &str, maximum: usize) -> (String, bool) {
    if value.len() <= maximum {
        return (value.into(), false);
    }
    let mut start = value.len() - maximum;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    (value[start..].into(), true)
}

pub(super) fn bounded_line(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.into();
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].into()
}

pub(super) fn signal_exit_code(value: &str) -> bool {
    value
        .split_once(':')
        .and_then(|(_, signal)| signal.parse::<u32>().ok())
        .is_some_and(|signal| signal > 0)
}

pub(super) fn sanitize(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for value in chars.by_ref() {
                    if ('@'..='~').contains(&value) {
                        break;
                    }
                }
            } else if chars.peek() == Some(&']') {
                chars.next();
                while let Some(value) = chars.next() {
                    if value == '\x07' || value == '\x1b' && chars.next() == Some('\\') {
                        break;
                    }
                }
            }
            continue;
        }
        if !character.is_control() || matches!(character, '\n' | '\t') {
            output.push(character);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Instant};

    use rmcp::model::JsonObject;
    use serde_json::{Value, json};

    use super::*;
    use crate::config::{ClusterConfig, Config};

    fn object(value: Value) -> JsonObject {
        value.as_object().unwrap().clone()
    }

    fn config() -> Config {
        Config {
            local_user: "offline".into(),
            remote_user: "offline".into(),
            ssh_host: String::new(),
            state_path: PathBuf::from("/tmp/state"),
            executable: PathBuf::from("/bin/false"),
            sbatch_banks: Vec::new(),
            clusters: vec![ClusterConfig {
                name: "alpha".into(),
                transport: "local".into(),
                user: "offline".into(),
                ssh_host: String::new(),
                working_directory: PathBuf::from("/tmp"),
                accounting: false,
            }],
        }
    }

    #[test]
    fn argument_helpers_cover_defaults_errors_and_cursors() {
        let args = object(json!({
            "cluster":"alpha", "job_id":"12_3", "flag":true,
            "number":7, "states":["running", "pending"], "cursor":"jobs:4"
        }));
        assert_eq!(exact_job(&config(), &args).unwrap(), ("alpha", "12_3"));
        assert_eq!(required_string(&args, "cluster").unwrap(), "alpha");
        assert_eq!(optional_string(&args, "missing"), None);
        assert_eq!(optional_bool(&args, "flag"), Some(true));
        assert_eq!(optional_usize(&args, "missing", 9).unwrap(), 9);
        assert_eq!(optional_usize(&args, "number", 9).unwrap(), 7);
        assert_eq!(
            optional_strings(&args, "states").unwrap(),
            ["RUNNING", "PENDING"]
        );
        assert_eq!(
            optional_strings(&args, "missing").unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(page(&args, "jobs", 20, 100).unwrap(), (4, 20));

        assert!(
            exact_job(
                &config(),
                &object(json!({"cluster":"alpha","job_id":"bad/id"}))
            )
            .is_err()
        );
        assert!(
            exact_job(
                &config(),
                &object(json!({"cluster":"missing","job_id":"1"}))
            )
            .is_err()
        );
        assert!(required_string(&JsonObject::new(), "cluster").is_err());
        assert!(optional_usize(&object(json!({"number":-1})), "number", 1).is_err());
        assert!(optional_strings(&object(json!({"states":7})), "states").is_err());
        assert!(optional_strings(&object(json!({"states":[7]})), "states").is_err());
        assert!(page(&object(json!({"cursor":"wrong"})), "jobs", 20, 100).is_err());
    }

    #[test]
    fn histories_scripts_hashes_and_tokens_are_exact() {
        for value in ["live", "2h", "12h", "1d", "1w", "all"] {
            history_mode(value).unwrap();
        }
        assert!(history_mode("forever").is_err());

        let script = bank::Script {
            bank: "Bank".into(),
            relative: PathBuf::from("train.sbatch"),
            name: "train".into(),
            directives: vec!["--job-name=train".into()],
            origin: Some("alpha".into()),
            bytes: b"#!/bin/sh\n".to_vec(),
        };
        assert_eq!(script_id(&script), "Bank/train.sbatch");
        assert_eq!(
            exact_script(std::slice::from_ref(&script), "Bank/train.sbatch", "alpha")
                .unwrap()
                .name,
            "train"
        );
        assert!(exact_script(std::slice::from_ref(&script), "Bank/train.sbatch", "beta").is_err());
        assert!(exact_script(&[script.clone(), script], "Bank/train.sbatch", "alpha").is_err());
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );

        let preview = Preview {
            created: Instant::now(),
            cluster: "alpha".into(),
            script: "Bank/train.sbatch".into(),
            digest: "a".repeat(64),
            directives: Vec::new(),
            working_directory: "/tmp".into(),
            job_name: "train".into(),
        };
        let first = preview_token(&preview);
        assert_eq!(first.len(), 64);
        assert_ne!(first, preview_token(&preview));
    }

    #[test]
    fn bounded_and_sanitized_text_handles_unicode_signals_and_osc() {
        assert_eq!(bounded_text("short", 8), ("short".into(), false));
        assert_eq!(bounded_text("aébc", 3), ("bc".into(), true));
        assert_eq!(bounded_line("short", 8), "short");
        assert_eq!(bounded_line("aébc", 2), "a");
        assert!(signal_exit_code("1:9"));
        assert!(!signal_exit_code("0:0"));
        assert!(!signal_exit_code("invalid"));
        assert_eq!(sanitize(b"a\x1b]title\x07b\x1bXc\x01\n\t"), "abXc\n\t");
        assert_eq!(sanitize(b"a\x1b]title\x1b\\b"), "ab");
    }
}
