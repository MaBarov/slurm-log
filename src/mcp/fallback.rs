use serde_json::Value;

use super::helpers::bounded_line;

/// Plain-text fallback for clients that ignore `structuredContent`.  Must
/// carry counts, first matches, warnings, and cursor state so a text-only
/// client can still diagnose an empty catalog versus a broken one.
pub(super) fn fallback_text(name: &str, value: &Value) -> String {
    let ok = match value.get("ok").and_then(Value::as_bool) {
        Some(true) => "ok".to_string(),
        Some(false) => "ok=false".to_string(),
        None => "completed".to_string(),
    };
    let body = match name {
        "slurm_list_clusters" => {
            let clusters = value["clusters"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            format!(
                                "{}({})",
                                item["name"].as_str().unwrap_or("?"),
                                item["connectivity"].as_str().unwrap_or("?")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            format!(
                "{} cluster(s): {clusters}",
                value["clusters"].as_array().map_or(0, Vec::len)
            )
        }
        "slurm_list_jobs" => listing_summary(value, "jobs", |item| {
            format!(
                "{} {} {}",
                item["id"].as_str().unwrap_or("?"),
                item["name"].as_str().unwrap_or("?"),
                item["state"].as_str().unwrap_or("?")
            )
        }),
        "slurm_inspect_job" => format!(
            "job {} state {}, log status {}",
            value["job_id"].as_str().unwrap_or("?"),
            value["job"]["state"].as_str().unwrap_or("?"),
            value["log"]["status"].as_str().unwrap_or("?")
        ),
        "slurm_workspace_context" => format!(
            "{} workspace(s), {} focused job(s)",
            value["workspaces"].as_array().map_or(0, Vec::len),
            value["focused_jobs"].as_array().map_or(0, Vec::len)
        ),
        "slurm_read_log" => format!(
            "status {}, {} bytes visible of {} total; truncated {}",
            value["status"].as_str().unwrap_or("?"),
            value["log_text"].as_str().map_or(0, str::len),
            value["file_size"].as_u64().unwrap_or(0),
            value["truncated"].as_bool().unwrap_or(false)
        ),
        "slurm_search_log" => format!(
            "{} match(es) in newest {} scanned bytes; limited {}",
            value["match_count"].as_u64().unwrap_or(0),
            value["scan_bytes"].as_u64().unwrap_or(0),
            value["limited"].as_bool().unwrap_or(false)
        ),
        "slurm_diagnose_job" => {
            let findings = value["findings"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .take(5)
                        .map(|item| {
                            format!(
                                "{}({})",
                                item["classification"].as_str().unwrap_or("?"),
                                item["confidence"].as_str().unwrap_or("?")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            format!(
                "{} finding(s), first: [{}]",
                value["findings"].as_array().map_or(0, Vec::len),
                findings
            )
        }
        "slurm_list_scripts" => {
            let listing = listing_summary(value, "scripts", |item| {
                item["script"].as_str().unwrap_or("?").to_string()
            });
            let indexed = value["catalog"]["indexed_at"].as_str().unwrap_or("?");
            let generation = value["catalog"]["generation"].as_str().unwrap_or("?");
            format!("{listing}; catalog indexed_at {indexed} generation {generation}")
        }
        "slurm_preview_submission" => format!(
            "preview_token issued for {} on {}; expires_in_seconds {}; catalog generation {}",
            value["script"].as_str().unwrap_or("?"),
            value["cluster"].as_str().unwrap_or("?"),
            value["expires_in_seconds"].as_u64().unwrap_or(0),
            value["catalog_generation"].as_str().unwrap_or("?")
        ),
        "slurm_submit_job" => format!(
            "submitted job {} ({}) on {}; initial state {}",
            value["job_id"].as_str().unwrap_or("?"),
            value["job_name"].as_str().unwrap_or("?"),
            value["cluster"].as_str().unwrap_or("?"),
            value["initial_state"].as_str().unwrap_or("?")
        ),
        "slurm_cancel_job" => format!(
            "cancelled {} ({})",
            value["job_id"].as_str().unwrap_or("?"),
            value["job_name"].as_str().unwrap_or("?")
        ),
        "slurm_doctor" => format!(
            "scheduler_health {}, bank_health {}, daemon_health {}",
            value["scheduler_health"]["ok"].as_bool().unwrap_or(false),
            value["bank_health"]["ok"].as_bool().unwrap_or(false),
            value["daemon_health"]["ok"].as_bool().unwrap_or(false)
        ),
        "slurm_refresh_bank" => format!(
            "refreshed; {} scripts across {} bank(s)",
            value["total"].as_u64().unwrap_or(0),
            value["banks"].as_array().map_or(0, Vec::len)
        ),
        "slurm_wait_job" => format!(
            "state {} -> {}; changed {}, elapsed {}s",
            value["initial_state"].as_str().unwrap_or("?"),
            value["final_state"].as_str().unwrap_or("?"),
            value["changed"].as_bool().unwrap_or(false),
            value["elapsed_seconds"].as_u64().unwrap_or(0)
        ),
        "slurm_explain_pending" => format!(
            "pending {}, reason {}, requested partition {}",
            value["pending"].as_bool().unwrap_or(false),
            value["reason"].as_str().unwrap_or("?"),
            value["requested_partition"].as_str().unwrap_or("?")
        ),
        "slurm_adopt_job" => format!(
            "adopted {} ({}) as externally_submitted",
            value["job_id"].as_str().unwrap_or("?"),
            value["job_name"].as_str().unwrap_or("?")
        ),
        _ => "structured result attached".into(),
    };
    let mut text = format!("{name}: {ok}; {body}");
    let warnings = value["warnings"].as_array();
    if let Some(warnings) = warnings
        && !warnings.is_empty()
    {
        text.push_str(&format!("; {} warning(s)", warnings.len()));
        if let Some(first) = warnings.first().and_then(Value::as_str) {
            text.push_str(&format!(" (first: {})", bounded_line(first, 120)));
        }
    }
    if let Some(cursor) = value.get("next_cursor").and_then(Value::as_str) {
        text.push_str(&format!("; next_cursor {cursor}"));
    }
    text
}

fn listing_summary(value: &Value, array: &str, render: impl Fn(&Value) -> String) -> String {
    let total = value.get("total").and_then(Value::as_u64).unwrap_or(0);
    let shown = value[array].as_array().map_or(0, Vec::len);
    if total == 0 {
        return format!("0 {array}");
    }
    let first = value[array]
        .as_array()
        .map(|items| {
            items
                .iter()
                .take(5)
                .map(&render)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    format!("{total} {array} ({shown} shown), first: [{first}]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fallback_text_covers_all_tools_and_formatting() {
        let clusters = json!({
            "ok": true,
            "total": 1,
            "clusters": [{"name": "sprint", "transport": "local", "accounting": true}],
            "warnings": ["minor lag"],
            "next_cursor": "c1"
        });
        let text = fallback_text("slurm_list_clusters", &clusters);
        assert!(text.contains("slurm_list_clusters: ok"));
        assert!(text.contains("1 cluster(s)"));
        assert!(text.contains("minor lag"));
        assert!(text.contains("next_cursor c1"));

        let jobs = json!({
            "ok": true,
            "total": 2,
            "jobs": [
                {"cluster": "c", "job_id": "1", "job_name": "train", "state": "RUNNING"},
                {"cluster": "c", "job_id": "2", "job_name": "eval", "state": "PENDING"}
            ]
        });
        let text = fallback_text("slurm_list_jobs", &jobs);
        assert!(text.contains("2 jobs (2 shown)"));

        let empty_jobs = json!({"ok": true, "total": 0, "jobs": []});
        assert!(fallback_text("slurm_list_jobs", &empty_jobs).contains("0 jobs"));

        let inspect = json!({"ok": true, "cluster": "c", "job_id": "1", "job": {"state": "RUNNING"}, "log": {"status": "ok"}});
        assert!(
            fallback_text("slurm_inspect_job", &inspect)
                .contains("job 1 state RUNNING, log status ok")
        );

        let ws = json!({"ok": true, "workspaces": [{"name": "1"}, {"name": "2"}], "focused_jobs": ["c:1"]});
        assert!(
            fallback_text("slurm_workspace_context", &ws)
                .contains("2 workspace(s), 1 focused job(s)")
        );

        let log = json!({"ok": true, "cluster": "c", "job_id": "1", "status": "ok", "log_text": "hello", "file_size": 100, "truncated": false});
        assert!(
            fallback_text("slurm_read_log", &log)
                .contains("status ok, 5 bytes visible of 100 total")
        );
        let search = json!({"ok": true, "match_count": 1, "scan_bytes": 50, "limited": false});
        assert!(
            fallback_text("slurm_search_log", &search)
                .contains("1 match(es) in newest 50 scanned bytes")
        );

        let diag = json!({"ok": true, "findings": [{"classification": "out of memory", "confidence": "high"}]});
        assert!(
            fallback_text("slurm_diagnose_job", &diag)
                .contains("1 finding(s), first: [out of memory(high)]")
        );

        let diag_empty = json!({"ok": true, "findings": []});
        assert!(fallback_text("slurm_diagnose_job", &diag_empty).contains("0 finding(s)"));

        let scripts = json!({"ok": true, "total": 1, "scripts": [{"script": "train.sbatch"}], "indexed_at": "now", "catalog_generation": "gen"});
        assert!(fallback_text("slurm_list_scripts", &scripts).contains("1 scripts (1 shown)"));

        let preview = json!({"ok": true, "script": "train.sbatch", "cluster": "c", "expires_in_seconds": 300, "catalog_generation": "gen"});
        assert!(
            fallback_text("slurm_preview_submission", &preview)
                .contains("preview_token issued for train.sbatch on c")
        );

        let submit = json!({"ok": true, "cluster": "c", "job_id": "42", "job_name": "train", "initial_state": "PENDING"});
        assert!(
            fallback_text("slurm_submit_job", &submit).contains("submitted job 42 (train) on c")
        );

        let cancel = json!({"ok": true, "job_id": "42", "job_name": "train"});
        assert!(fallback_text("slurm_cancel_job", &cancel).contains("cancelled 42 (train)"));

        let doctor = json!({"ok": true, "scheduler_health": {"ok": true}, "bank_health": {"ok": true}, "daemon_health": {"ok": true}});
        assert!(
            fallback_text("slurm_doctor", &doctor)
                .contains("scheduler_health true, bank_health true, daemon_health true")
        );

        let refresh = json!({"ok": true, "total": 5, "banks": [{"name": "b1"}]});
        assert!(
            fallback_text("slurm_refresh_bank", &refresh).contains("5 scripts across 1 bank(s)")
        );

        let wait = json!({"ok": true, "initial_state": "PENDING", "final_state": "RUNNING", "changed": true, "elapsed_seconds": 10});
        assert!(
            fallback_text("slurm_wait_job", &wait)
                .contains("state PENDING -> RUNNING; changed true, elapsed 10s")
        );

        let pending = json!({"ok": true, "pending": true, "reason": "Resources", "requested_partition": "gpu"});
        assert!(
            fallback_text("slurm_explain_pending", &pending)
                .contains("pending true, reason Resources, requested partition gpu")
        );

        let adopt =
            json!({"ok": true, "job_id": "42", "job_name": "train", "observed_state": "RUNNING"});
        assert!(fallback_text("slurm_adopt_job", &adopt).contains("adopted 42 (train)"));

        let unknown = json!({"ok": false});
        assert!(
            fallback_text("unknown_tool", &unknown)
                .contains("ok=false; structured result attached")
        );
    }
}
