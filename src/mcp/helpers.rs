use anyhow::{Context, Result, bail};
use rmcp::model::JsonObject;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{bank, config::Config, slurm};

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
    // Dependencies are returned by a second controller RPC, so repeat the
    // exact-owner decision immediately before it rather than inheriting an
    // inspect/list cache membership decision.
    slurm::authorize_exact_job(config, cluster, id)?;
    let value = slurm::control_job_text(config, cluster, id)?;
    slurm::validate_control_identity(config, cluster, id, &value)?;
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

/// The result-file globs a job's batch script declared via `#SLURM_LOG-RESULT:`
/// markers, resolved through recorded provenance. The producer hash recorded
/// at MCP submission or adoption is the primary binding; the job name is a
/// secondary match. When neither resolves to an eligible configured script,
/// the job has no declared results and reading fails closed.
pub(super) fn declared_results_for_job(
    config: &Config,
    cluster: &str,
    job: &crate::model::Job,
) -> Result<Vec<String>> {
    let (scripts, _) = bank::configured_scripts_fresh(config)?;
    let target = config.cluster(cluster)?;
    let eligible = |script: &bank::Script| {
        bank::supports_cluster(script, cluster)
            && bank::validate_script_controller(script, target).is_ok()
    };
    if let Some(hash) = crate::state::Ledger::producer_hash(&config.state_path, cluster, &job.id) {
        let mut by_hash = scripts
            .iter()
            .filter(|script| eligible(script) && sha256(&script.bytes) == hash);
        let unique = by_hash.next().filter(|_| by_hash.next().is_none());
        if let Some(script) = unique {
            return Ok(script.declared_results.clone());
        }
    }
    let mut by_name = scripts
        .iter()
        .filter(|script| eligible(script) && script.name == job.name);
    let first = by_name
        .next()
        .context("no configured script records this job, so it has no declared results")?;
    if by_name.next().is_some() {
        bail!(
            "multiple configured scripts share the job name {name:?}",
            name = job.name
        );
    }
    Ok(first.declared_results.clone())
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn preview_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).context("obtain secure preview-token randomness")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
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
    use std::path::PathBuf;

    use rmcp::model::JsonObject;
    use serde_json::{Value, json};

    use super::*;
    use crate::config::{ClusterConfig, Config, SbatchBankConfig};

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
                controller: None,
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
            declared_results: Vec::new(),
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

        let first = preview_token().unwrap();
        assert_eq!(first.len(), 64);
        assert_ne!(first, preview_token().unwrap());
    }

    #[test]
    fn declared_results_resolve_via_hash_then_fall_back_to_name() {
        let directory = tempfile::tempdir().unwrap();
        let bank = directory.path().join("bank");
        std::fs::create_dir(&bank).unwrap();
        let bytes = b"#!/bin/sh\n#SBATCH --job-name=train\n#SLURM_LOG-RESULT:model.pth\n";
        std::fs::write(bank.join("train.sbatch"), bytes).unwrap();
        let mut config = config();
        config.state_path = directory.path().join("state.json");
        config.sbatch_banks = vec![SbatchBankConfig {
            path: bank,
            name: Some("Bank".into()),
        }];

        let digest = sha256(bytes);
        crate::state::Ledger::mark_submitted(&config.state_path, "alpha", "42", &digest).unwrap();
        let matched = crate::model::Job {
            cluster: "alpha".into(),
            id: "42".into(),
            name: "train".into(),
            ..crate::model::Job::default()
        };
        assert_eq!(
            declared_results_for_job(&config, "alpha", &matched).unwrap(),
            ["model.pth"]
        );

        crate::state::Ledger::mark_submitted(&config.state_path, "alpha", "43", &"0".repeat(64))
            .unwrap();
        let by_name = crate::model::Job {
            cluster: "alpha".into(),
            id: "43".into(),
            name: "train".into(),
            ..crate::model::Job::default()
        };
        assert_eq!(
            declared_results_for_job(&config, "alpha", &by_name).unwrap(),
            ["model.pth"]
        );
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
