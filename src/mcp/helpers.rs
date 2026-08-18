use anyhow::{Context, Result, bail};
use rmcp::model::JsonObject;
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

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

pub(super) fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// RFC3339 rendering of an epoch timestamp; empty for zero values.
pub(super) fn iso_timestamp(epoch: i64) -> String {
    OffsetDateTime::from_unix_timestamp(epoch)
        .map(|value| value.format(&Rfc3339).unwrap_or_else(|_| "unknown".into()))
        .unwrap_or_default()
}

pub(super) fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
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

/// Extract resource-shaped sbatch directives without executing anything.
pub(super) fn script_resources(directives: &[String]) -> Value {
    let mut resources = serde_json::Map::new();
    for directive in directives {
        for key in [
            "--gres",
            "--gpus",
            "--mem",
            "--cpus-per-task",
            "--ntasks",
            "--nodes",
            "--time",
            "--partition",
            "--qos",
            "--account",
        ] {
            if let Some(value) = directive_value(directive, key) {
                resources.insert(
                    key.trim_start_matches('-').into(),
                    Value::String(value.into()),
                );
                break;
            }
        }
    }
    Value::Object(resources)
}

pub(super) fn directive_value<'a>(directive: &'a str, key: &str) -> Option<&'a str> {
    let rest = directive.strip_prefix(key)?;
    if !rest.is_empty() && !rest.starts_with(['=', ' ']) {
        return None;
    }
    Some(rest.trim_start_matches(['=', ' ']).trim())
}

pub(super) fn sanitize(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len());
    let mut i = 0;
    let len = bytes.len();

    while i < len {
        let start = i;
        // Fast-path: scan printable ASCII and normal whitespace in 8-byte chunks
        while i + 8 <= len {
            let chunk: [u8; 8] = [
                bytes[i],
                bytes[i + 1],
                bytes[i + 2],
                bytes[i + 3],
                bytes[i + 4],
                bytes[i + 5],
                bytes[i + 6],
                bytes[i + 7],
            ];
            if chunk
                .iter()
                .all(|&b| (0x20..=0x7E).contains(&b) || b == b'\n' || b == b'\t')
            {
                i += 8;
            } else {
                break;
            }
        }
        while i < len {
            let b = bytes[i];
            if (0x20..=0x7E).contains(&b) || b == b'\n' || b == b'\t' {
                i += 1;
            } else {
                break;
            }
        }
        if i > start
            && let Ok(valid) = std::str::from_utf8(&bytes[start..i])
        {
            output.push_str(valid);
        }
        if i >= len {
            break;
        }

        let b = bytes[i];
        if b == 0x1b {
            i += 1;
            if i < len && bytes[i] == b'[' {
                i += 1;
                while i < len {
                    let c = bytes[i];
                    i += 1;
                    if (0x40..=0x7E).contains(&c) {
                        break;
                    }
                }
            } else if i < len && bytes[i] == b']' {
                i += 1;
                while i < len {
                    let c = bytes[i];
                    i += 1;
                    if c == 0x07 || (c == 0x1b && i < len && bytes[i] == b'\\') {
                        if c == 0x1b {
                            i += 1;
                        }
                        break;
                    }
                }
            }
            continue;
        }
        if b < 0x20 {
            // Skip unsupported ASCII control characters
            i += 1;
            continue;
        }

        // Multi-byte UTF-8 sequences
        let remaining = &bytes[i..];
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                output.push_str(valid);
                break;
            }
            Err(err) => {
                let valid_len = err.valid_up_to();
                if valid_len > 0 {
                    if let Ok(valid) = std::str::from_utf8(&remaining[..valid_len]) {
                        output.push_str(valid);
                    }
                    i += valid_len;
                }
                if let Some(err_len) = err.error_len() {
                    output.push('\u{FFFD}');
                    i += err_len;
                } else {
                    output.push('\u{FFFD}');
                    break;
                }
            }
        }
    }
    output
}

#[cfg(test)]
#[path = "helpers/tests.rs"]
mod tests;
