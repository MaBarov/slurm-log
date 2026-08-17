use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use regex::Regex;
use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::{McpServer, helpers::*};
use crate::log_service::{LogData, MAX_LOG_PAYLOAD, MAX_LOG_WINDOW};

impl McpServer {
    pub(crate) fn read_log(&self, args: &JsonObject) -> Result<Value> {
        let (cluster, id) = exact_job(&self.config, args)?;
        crate::slurm::authorize_exact_job(&self.config, cluster, id)?;
        let lines = optional_usize(args, "lines", 200)?.clamp(1, 2000);
        let filter = optional_string(args, "filter").unwrap_or("hide_warnings");
        if !["hide_warnings", "all", "warnings", "exceptions"].contains(&filter) {
            bail!("invalid log filter {filter}");
        }
        let mut cursor_reset = false;
        let data = if let Some(cursor) = optional_string(args, "cursor") {
            let (generation, offset) = parse_cursor(cursor)?;
            let metadata = crate::daemon::log_metadata(&self.config, cluster, id)?;
            if metadata.status != "available"
                || metadata.generation != generation
                || offset > metadata.size
            {
                cursor_reset = true;
                crate::daemon::log_window(&self.config, cluster, id, MAX_LOG_WINDOW)?
            } else {
                crate::daemon::log_range(&self.config, cluster, id, offset, MAX_LOG_PAYLOAD)?
            }
        } else {
            crate::daemon::log_window(&self.config, cluster, id, MAX_LOG_WINDOW)?
        };
        let incremental = args.contains_key("cursor") && !cursor_reset;
        let selected = if incremental {
            data.bytes.clone()
        } else {
            tail_lines(&data.bytes, lines)
        };
        let clean = sanitize(&selected);
        let filtered = filter_text(&clean, filter);
        let (text, text_truncated) = bounded_text(&filtered, MAX_LOG_PAYLOAD);
        let consumed = if incremental {
            data.offset.saturating_add(data.bytes.len() as u64)
        } else {
            data.size
        };
        let more_available = consumed < data.size;
        let next = (data.status == "available").then(|| make_cursor(&data.generation, consumed));
        Ok(json!({
            "ok":true,"cluster":cluster,"job_id":id,"status":data.status,
            "job_name":data.job_name,"state":data.state,"terminal":data.terminal,
            "log_text":text,"untrusted_data":true,"cursor_reset":cursor_reset,
            "next_cursor":next,"file_size":data.size,
            "truncated":text_truncated || more_available,"more_available":more_available
        }))
    }

    pub(crate) fn search_log(&self, args: &JsonObject) -> Result<Value> {
        let (cluster, id) = exact_job(&self.config, args)?;
        crate::slurm::authorize_exact_job(&self.config, cluster, id)?;
        let pattern = required_string(args, "pattern")?;
        let regex = optional_bool(args, "regex").unwrap_or(false);
        let maximum = optional_usize(args, "max_matches", 100)?.clamp(1, 500);
        let context = optional_usize(args, "context_lines", 2)?.min(20);
        let data = crate::daemon::log_window(&self.config, cluster, id, MAX_LOG_WINDOW)?;
        let text = sanitize(&data.bytes);
        let lines: Vec<_> = text.lines().collect();
        let expression = regex
            .then(|| Regex::new(pattern))
            .transpose()
            .context("invalid regex")?;
        let mut matching = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let found = expression
                .as_ref()
                .map_or_else(|| line.contains(pattern), |value| value.is_match(line));
            if found {
                matching.push(index);
                if matching.len() == maximum {
                    break;
                }
            }
        }
        let mut selected = BTreeSet::new();
        for index in &matching {
            for item in index.saturating_sub(context)
                ..=(*index + context).min(lines.len().saturating_sub(1))
            {
                selected.insert(item);
            }
        }
        let mut matches = Vec::new();
        let mut output_bytes = 0_usize;
        let mut output_limited = false;
        for index in selected {
            let line = bounded_line(lines[index], 2000);
            let cost = line.len().saturating_add(64);
            if output_bytes.saturating_add(cost) > MAX_LOG_PAYLOAD {
                output_limited = true;
                break;
            }
            output_bytes += cost;
            matches.push(json!({
                "window_line":index + 1,
                "text":line,
                "matched":matching.contains(&index)
            }));
        }
        Ok(json!({
            "ok":true,"cluster":cluster,"job_id":id,"status":data.status,
            "regex":regex,"match_count":matching.len(),"matches":matches,
            "scan_bytes":data.bytes.len(),"scan_limit_bytes":MAX_LOG_WINDOW,
            "untrusted_data":true,
            "limited":matching.len() == maximum || output_limited,
            "output_limit_bytes":MAX_LOG_PAYLOAD
        }))
    }

    pub(crate) fn diagnose_job(&self, args: &JsonObject) -> Result<Value> {
        let (cluster, id) = exact_job(&self.config, args)?;
        let authorized = crate::slurm::authorize_exact_job(&self.config, cluster, id)?;
        let archive = self.config.cluster(cluster)?.accounting;
        let (_, _, warnings) = crate::slurm::all_jobs(&self.config, cluster, "all", archive)?;
        let job = authorized;
        let details = crate::daemon::job_details(&self.config, cluster, id, false).ok();
        let log = crate::daemon::log_window(&self.config, cluster, id, MAX_LOG_WINDOW)?;
        let text = sanitize(&log.bytes);
        let findings = findings(Some(&job), details.as_ref(), &log, &text);
        Ok(json!({
            "ok":true,"cluster":cluster,"job_id":id,"job":job,"details":details,
            "log_status":log.status,"findings":findings,"scheduler_warnings":warnings,
            "diagnosis_scope":{"log_window_bytes":log.bytes.len(),"cross_run_comparison":false}
        }))
    }
}

fn findings(
    job: Option<&crate::model::Job>,
    details: Option<&crate::details::JobDetails>,
    log: &LogData,
    text: &str,
) -> Vec<Value> {
    let mut found = Vec::new();
    let state = job
        .map(|value| value.state.as_str())
        .or_else(|| details.map(|value| value.state.as_str()))
        .unwrap_or("");
    let reason = job
        .map(|value| value.reason.as_str())
        .or_else(|| details.map(|value| value.reason.as_str()))
        .unwrap_or("");
    let exit_code = job
        .map(|value| value.exit_code.as_str())
        .or_else(|| details.map(|value| value.exit_code.as_str()))
        .unwrap_or("");
    if state.starts_with("PENDING") {
        push_finding(
            &mut found,
            "pending_cause",
            "high",
            &[reason],
            "Check the Slurm pending reason, requested resources, priority, reservation, and QOS.",
        );
    }
    if reason.contains("DependencyNeverSatisfied")
        || reason.contains("Dependency") && reason.contains("failed")
    {
        push_finding(
            &mut found,
            "dependency_failure",
            "high",
            &[reason],
            "Inspect the named prerequisite job and correct or remove the failed dependency.",
        );
    }
    match log.status.as_str() {
        "pending_log" => push_finding(
            &mut found,
            "pending_log",
            "high",
            &[],
            "The scheduler has not exposed a readable stdout file yet; check again after allocation.",
        ),
        "no_stdout" => push_finding(
            &mut found,
            "no_stdout",
            "high",
            &[],
            "The job has no usable Slurm StdOut path; inspect the sbatch output directive.",
        ),
        "accounting_unavailable" | "not_found" => push_finding(
            &mut found,
            "log_unavailable",
            "medium",
            &[],
            "Confirm the exact cluster and job ID and whether accounting retains the completed job.",
        ),
        "available" if log.bytes.is_empty() => push_finding(
            &mut found,
            "no_recent_output",
            "medium",
            &[],
            "No recent output is visible; check process activity and application flush behavior without assuming buffering.",
        ),
        _ => {}
    }
    if signal_exit_code(exit_code) {
        push_finding(
            &mut found,
            "signal",
            "high",
            &[exit_code],
            "Compare the terminating signal with scheduler, node, and memory events.",
        );
    }
    let categories = [
        (
            "python_traceback",
            &["Traceback (most recent call last):"][..],
            "Inspect the final exception and the first project frame.",
        ),
        (
            "rust_panic",
            &["panicked at", "thread '"][..],
            "Enable or inspect RUST_BACKTRACE and the first application frame.",
        ),
        (
            "cuda_out_of_memory",
            &["CUDA out of memory", "CUDNN_STATUS_ALLOC_FAILED"][..],
            "Reduce peak GPU memory or allocation size and inspect per-rank memory.",
        ),
        (
            "nccl_error",
            &["NCCL error", "ncclSystemError", "ncclUnhandledCudaError"][..],
            "Inspect all ranks, network fabric, and the first failed collective.",
        ),
        (
            "assertion_failure",
            &["AssertionError", "assertion failed"][..],
            "Inspect the asserted invariant and the values immediately before failure.",
        ),
        (
            "nan_or_inf",
            &["NaN", " nan", "Inf", "infinite loss"][..],
            "Check the first non-finite tensor or metric and its upstream inputs.",
        ),
        (
            "signal",
            &[
                "Killed",
                "Segmentation fault",
                "signal 9",
                "SIGTERM",
                "SIGKILL",
            ][..],
            "Compare exit code and scheduler state with node and memory events.",
        ),
    ];
    for (class, needles, check) in categories {
        let evidence = evidence_lines(text, needles, 3);
        if !evidence.is_empty() {
            push_finding(&mut found, class, "high", &evidence, check);
        }
    }
    if state.starts_with("OUT_OF_MEMORY") || text.contains("oom-kill") {
        push_finding(
            &mut found,
            "slurm_out_of_memory",
            "high",
            &[state],
            "Compare MaxRSS with allocated memory and inspect cgroup OOM events.",
        );
    }
    if state.starts_with("TIMEOUT") {
        push_finding(
            &mut found,
            "slurm_timeout",
            "high",
            &[state],
            "Compare elapsed time with the requested time limit and checkpoint cadence.",
        );
    }
    if state.starts_with("NODE_FAIL") || text.contains("node failure") {
        push_finding(
            &mut found,
            "node_failure",
            "high",
            &[state],
            "Inspect Slurm node reason and retry on a healthy allocation.",
        );
    }
    if state.starts_with("CANCELLED") {
        push_finding(
            &mut found,
            "job_cancelled",
            "high",
            &[state],
            "Confirm whether the user, the scheduler, or a pending-time policy cancelled the job before resubmitting.",
        );
    }
    let environment_evidence = evidence_lines(
        text,
        &[
            "No module named",
            "ModuleNotFoundError",
            "command not found",
        ],
        3,
    );
    if !environment_evidence.is_empty() {
        push_finding(
            &mut found,
            "environment_setup",
            "medium",
            &environment_evidence,
            "Check that the required module, venv, or working directory was available inside the allocation.",
        );
    }
    found
}

fn push_finding(
    found: &mut Vec<Value>,
    class: &str,
    confidence: &str,
    evidence: &[&str],
    check: &str,
) {
    let evidence = evidence
        .iter()
        .filter(|value| !value.trim().is_empty())
        .map(|value| bounded_line(value, 500))
        .collect::<Vec<_>>();
    if found.iter().any(|value| value["classification"] == class) {
        return;
    }
    found.push(json!({"classification":class,"confidence":confidence,"evidence":evidence,"practical_check":check}));
}

fn evidence_lines<'a>(text: &'a str, needles: &[&str], maximum: usize) -> Vec<&'a str> {
    text.lines()
        .filter(|line| needles.iter().any(|needle| line.contains(needle)))
        .take(maximum)
        .collect()
}

fn make_cursor(generation: &str, offset: u64) -> String {
    format!("v1:{generation}:{offset}")
}

fn parse_cursor(value: &str) -> Result<(&str, u64)> {
    let mut fields = value.split(':');
    let version = fields.next();
    let generation = fields.next().unwrap_or_default();
    let offset = fields.next().and_then(|value| value.parse().ok());
    if version != Some("v1")
        || generation.len() != 64
        || !generation.bytes().all(|byte| byte.is_ascii_hexdigit())
        || fields.next().is_some()
        || offset.is_none()
    {
        bail!("invalid log cursor");
    }
    Ok((generation, offset.unwrap_or_default()))
}

fn tail_lines(bytes: &[u8], maximum: usize) -> Vec<u8> {
    let mut remaining = maximum;
    let mut start = bytes.len();
    while start > 0 && remaining > 0 {
        start -= 1;
        if bytes[start] == b'\n' && start + 1 < bytes.len() {
            remaining -= 1;
        }
    }
    if start < bytes.len() && bytes[start] == b'\n' {
        start += 1;
    }
    bytes[start..].to_vec()
}

fn filter_text(text: &str, mode: &str) -> String {
    let lines: Vec<_> = text.lines().collect();
    match mode {
        "all" => text.into(),
        "warnings" => lines
            .iter()
            .filter(|line| warning_line(line))
            .copied()
            .collect::<Vec<_>>()
            .join("\n"),
        "exceptions" => exception_blocks(&lines),
        _ => lines
            .iter()
            .filter(|line| !warning_line(line))
            .copied()
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn warning_line(line: &str) -> bool {
    [
        "FutureWarning:",
        "UserWarning:",
        "DeprecationWarning:",
        "RuntimeWarning:",
        "PendingDeprecationWarning:",
        "ResourceWarning:",
        "Warning:",
        "warnings.warn(",
    ]
    .iter()
    .any(|marker| line.contains(marker))
}

fn exception_blocks(lines: &[&str]) -> String {
    let mut output = Vec::new();
    let mut active = 0_usize;
    for line in lines {
        let starts = line.contains("Traceback (most recent call last):")
            || line.contains("panicked at")
            || line.starts_with("thread '");
        if starts {
            active = 80;
        }
        if active > 0 {
            output.push(*line);
            active -= 1;
            if line.trim().is_empty() && output.len() > 1 {
                active = 0;
            }
        }
        if output.len() >= 2000 {
            break;
        }
    }
    output.join("\n")
}

#[cfg(test)]
#[path = "logs/tests.rs"]
mod tests;
