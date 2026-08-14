use anyhow::{Context, Result};
use serde_json::Value;

use super::helpers::script_id;
use crate::bank;

pub(super) fn bounded_error(value: &str) -> String {
    value.chars().take(2000).collect()
}

pub(super) fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

pub(super) fn probe_nonce() -> Result<String> {
    let mut bytes = [0_u8; 6];
    getrandom::fill(&mut bytes).context("obtain preflight probe nonce")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Human-readable fallback that no longer hides the structured result. It
/// reports ok, counts, warnings, the next cursor, and a few leading matches so
/// clients that ignore `structuredContent` still see the actual outcome.
pub(super) fn fallback_text(name: &str, value: &Value) -> String {
    let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(true);
    if !ok {
        let error = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("operation failed");
        return format!("{name}: {error}");
    }
    let mut parts = Vec::new();
    if let Some(total) = value.get("total").and_then(Value::as_u64) {
        parts.push(format!("{total} result(s)"));
        if name == "slurm_list_scripts" && total == 0 {
            let catalog = value.get("catalog");
            let indexed = catalog
                .and_then(|value| value.get("indexed_at"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let available = catalog
                .and_then(|value| value.get("available"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            parts.push(if available {
                format!(
                    "catalog healthy; indexed at {indexed}; local/uncommitted files are not indexed — stage or refresh required"
                )
            } else {
                "catalog unavailable".to_string()
            });
        }
    }
    if let Some(count) = value.get("match_count").and_then(Value::as_u64) {
        parts.push(format!("{count} match(es)"));
    }
    if let Some(warnings) = value.get("warnings").and_then(Value::as_array)
        && !warnings.is_empty()
    {
        parts.push(format!("{} warning(s)", warnings.len()));
    }
    if let Some(cursor) = value.get("next_cursor").and_then(Value::as_str) {
        parts.push(format!("next cursor {cursor}"));
    }
    if let Some(samples) = first_matches(name, value) {
        parts.push(samples);
    }
    if parts.is_empty() {
        format!("{name}: ok")
    } else {
        format!("{name}: {}", parts.join("; "))
    }
}

fn first_matches(name: &str, value: &Value) -> Option<String> {
    let (list, field, label) = match name {
        "slurm_list_jobs" => ("jobs", Some("name"), "job"),
        "slurm_list_scripts" => ("scripts", Some("job_name"), "script"),
        "slurm_list_clusters" => ("clusters", Some("name"), "cluster"),
        "slurm_search_log" => ("matches", Some("text"), "match"),
        "slurm_diagnose_job" => ("findings", Some("classification"), "finding"),
        _ => return None,
    };
    let items = value.get(list)?.as_array()?;
    if items.is_empty() {
        return None;
    }
    let names = items
        .iter()
        .take(5)
        .map(|item| match field {
            Some(field) => item
                .get(field)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            None => item.as_str().unwrap_or_default().to_string(),
        })
        .map(|name| {
            name.lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(60)
                .collect::<String>()
        })
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    if names.is_empty() {
        return None;
    }
    let suffix = if items.len() > 5 { ", …" } else { "" };
    Some(format!("{label}: {}{suffix}", names.join(", ")))
}

pub(super) fn match_field(script: &bank::Script, needle: &str) -> Option<&'static str> {
    if script_id(script).to_lowercase().contains(needle) {
        return Some("script_id");
    }
    if script.name.to_lowercase().contains(needle) {
        return Some("job_name");
    }
    if script
        .directives
        .iter()
        .any(|directive| directive.to_lowercase().contains(needle))
    {
        return Some("directives");
    }
    None
}

pub(super) fn script_stale(script: &bank::Script, bank: Option<&&bank::BankMeta>) -> bool {
    let Some(bank) = bank else { return false };
    let Some(indexed_at) = bank.indexed_at.as_deref() else {
        return false;
    };
    let Ok(indexed) =
        time::OffsetDateTime::parse(indexed_at, &time::format_description::well_known::Rfc3339)
    else {
        return false;
    };
    let indexed: std::time::SystemTime = indexed.into();
    let path = bank.path.join(&script.relative);
    let Some(modified) = std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
    else {
        return false;
    };
    modified > indexed
}
