// Preflight probes and scheduling-only resubmission overrides.
//
// A preflight probe is a tiny fixed sbatch script whose scheduling
// constraints (partition and GPU request) are copied verbatim from an exact
// configured-bank script. Resubmission overrides are validated against a
// strict scheduling-only allowlist and applied by rewriting `#SBATCH`
// scheduling lines; the producer body is never modified.

/// The only `#SBATCH` options a resubmission preview may override. Values
/// must consist of scheduling-safe characters and are emitted verbatim.
pub const SCHEDULE_OVERRIDE_KEYS: [&str; 12] = [
    "partition",
    "time",
    "mem",
    "cpus",
    "ntasks",
    "nodes",
    "gres",
    "qos",
    "account",
    "constraint",
    "exclude",
    "dependency",
];

/// Extract `(partition, gres)` scheduling constraints from sbatch directives.
/// Both long and short Slurm spellings are accepted; values are validated to
/// contain only scheduler-token characters before being reused in a probe.
pub fn scheduling_request(directives: &[String]) -> Result<(Option<String>, Option<String>)> {
    let mut partition = None;
    let mut gres = None;
    for directive in directives {
        let Some((key, value)) = directive_value(directive) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key {
            "partition" => {
                let value = validate_token(value, "partition")?;
                partition = Some(value.to_string());
            }
            "gres" => {
                let value = validate_token(value, "gres")?;
                gres = Some(value.to_string());
            }
            "gpus" => {
                let value = validate_token(value, "gpus")?;
                gres = Some(format!("gpu:{value}"));
            }
            _ => {}
        }
    }
    Ok((partition, gres))
}

/// Split a directive line into `(canonical scheduling key, value)`. The
/// argument must already have the `#SBATCH` prefix removed.
pub(crate) fn directive_value(directive: &str) -> Option<(&'static str, &str)> {
    let directive = directive.trim();
    let (option, value) = directive
        .split_once('=')
        .map_or((directive, ""), |(option, value)| (option.trim(), value));
    let mut tokens = option.split_whitespace();
    let token = tokens.next()?;
    let value = if value.is_empty() {
        tokens.next().unwrap_or("")
    } else {
        value
    };
    Some((option_key(token)?, value))
}

/// Canonical scheduling key for a `#SBATCH` option token, including attached
/// short-option spellings such as `-pgpu`.
fn option_key(token: &str) -> Option<&'static str> {
    let (name, attached) = token.split_once('=').unwrap_or((token, ""));
    let long = match name {
        "--partition" => "partition",
        "--time" => "time",
        "--mem" => "mem",
        "--cpus-per-task" => "cpus",
        "--ntasks" => "ntasks",
        "--nodes" => "nodes",
        "--gres" => "gres",
        "--gpus" => "gpus",
        "--qos" => "qos",
        "--account" => "account",
        "--constraint" => "constraint",
        "--exclude" => "exclude",
        "--dependency" => "dependency",
        _ => "",
    };
    if !long.is_empty() {
        return Some(long);
    }
    let _ = attached;
    if !name.starts_with('-') || name.starts_with("--") {
        return None;
    }
    let (short, tail) = name.split_at(2.min(name.len()));
    match short {
        "-p" => Some("partition"),
        "-t" => Some("time"),
        "-n" => Some("ntasks"),
        "-c" => Some("cpus"),
        "-N" => Some("nodes"),
        "-A" => Some("account"),
        "-C" => Some("constraint"),
        "-x" => Some("exclude"),
        "-d" => Some("dependency"),
        _ if tail.is_empty() => None,
        _ => None,
    }
}

fn validate_token<'a>(value: &'a str, field: &str) -> Result<&'a str> {
    let maximum = if field == "gres" { 256 } else { 128 };
    if value.is_empty()
        || value.len() > maximum
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_:,=-?*+./".contains(&byte))
    {
        bail!(
            "{field} value {value:?} must be at most {maximum} scheduler-token characters"
        );
    }
    Ok(value)
}

// Validate a Slurm `--dependency` value: comma-separated `type:jobid[:jobid...]`
// terms using after/afterany/afternotok/afterok or bare singleton; job IDs are
// digits with an optional `_N` array suffix and an optional `?`/`+` modifier.
fn validate_dependency(value: &str) -> Result<&str> {
    if value.is_empty() || value.len() > 256 {
        bail!("dependency value {value:?} must be 1..256 dependency characters");
    }
    let is_number = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    for term in value.split(',') {
        let term = term.trim();
        let (kind, jobs) = term.split_once(':').unwrap_or((term, ""));
        match kind {
            "singleton" => {
                if !jobs.is_empty() {
                    bail!("dependency singleton takes no job id, got {value:?}");
                }
            }
            "after" | "afterany" | "afternotok" | "afterok" => {
                if jobs.is_empty() {
                    bail!("dependency {kind} requires at least one job id, got {value:?}");
                }
                for job in jobs.split(':') {
                    if job.is_empty() {
                        bail!("dependency has an empty job id in {value:?}");
                    }
                    let job = job.strip_suffix(['?', '+']).unwrap_or(job);
                    let mut parts = job.split('_');
                    let number = parts.next().unwrap_or("");
                    if !is_number(number) {
                        bail!("dependency job id {job:?} in {value:?} is not numeric");
                    }
                    match parts.next() {
                        None => {}
                        Some(array) if is_number(array) && parts.next().is_none() => {}
                        _ => bail!(
                            "dependency job id {job:?} in {value:?} has an invalid array suffix"
                        ),
                    }
                }
            }
            _ => bail!(
                "dependency term {term:?} must use after, afterany, afternotok, afterok, or singleton"
            ),
        }
    }
    Ok(value)
}

/// Build the fixed probe script. Only the validated partition and GPU request
/// are interpolated; the body never executes any bank-provided content.
pub fn probe_script(job_name: &str, partition: Option<&str>, gres: Option<&str>) -> Vec<u8> {
    let mut script = String::from("#!/bin/sh\n");
    script.push_str(&format!("#SBATCH --job-name={job_name}\n"));
    script.push_str("#SBATCH --time=00:05:00\n");
    script.push_str("#SBATCH --ntasks=1 --cpus-per-task=1 --mem=1G\n");
    if let Some(partition) = partition {
        script.push_str(&format!("#SBATCH --partition={partition}\n"));
    }
    if let Some(gres) = gres {
        script.push_str(&format!("#SBATCH --gres={gres}\n"));
    }
    script.push_str(
        "echo \"slurm-log preflight begin\"\n\
         hostname\n\
         nvidia-smi -L 2>/dev/null || true\n\
         echo \"slurm-log preflight end\"\n",
    );
    script.into_bytes()
}

/// Validate and normalize the optional `schedule_overrides` argument.
/// Returns `None` when absent or empty, otherwise a map with only allowlisted
/// scheduling keys whose values contain safe scheduler-token characters.
pub fn parse_overrides(value: Option<&serde_json::Value>) -> Result<Option<BTreeMap<String, String>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("schedule_overrides must be an object"))?;
    if object.is_empty() {
        return Ok(None);
    }
    if object.len() > SCHEDULE_OVERRIDE_KEYS.len() {
        bail!("schedule_overrides contains too many keys");
    }
    let mut overrides = BTreeMap::new();
    for (key, value) in object {
        if !SCHEDULE_OVERRIDE_KEYS.contains(&key.as_str()) {
            bail!("schedule override {key:?} is not an allowed scheduling option");
        }
        let value = value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("schedule override {key:?} must be a string"))?;
        let value = if key == "dependency" {
            validate_dependency(value).map(str::to_string)?
        } else {
            validate_token(value, key).map(str::to_string)?
        };
        overrides.insert(key.clone(), value);
    }
    Ok(Some(overrides))
}

/// Rewrite `#SBATCH` scheduling lines according to the validated overrides.
/// Lines for overridden options are dropped and one `#SBATCH --key=value` line
/// per override is inserted right after the header directives (or prepended
/// when the script has no directive header). The script body and every
/// non-scheduling directive remain untouched apart from line-ending
/// normalization, so the producer hash stays meaningful for the unchanged
/// body.
pub fn apply_schedule_overrides(bytes: &[u8], overrides: &BTreeMap<String, String>) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let mut output = String::new();
    let mut saw_directive = false;
    let mut inserted = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#SBATCH") {
            saw_directive = true;
            let directive = trimmed.trim_start_matches('#').trim_start_matches("SBATCH").trim();
            if directive_value(directive).is_some_and(|(key, _)| overrides.contains_key(key)) {
                continue;
            }
        } else if saw_directive && !inserted {
            for (key, value) in overrides {
                output.push_str(&format!("#SBATCH --{key}={value}\n"));
            }
            inserted = true;
        }
        output.push_str(line);
        output.push('\n');
    }
    if !inserted {
        let mut prefix = String::new();
        for (key, value) in overrides {
            prefix.push_str(&format!("#SBATCH --{key}={value}\n"));
        }
        output = prefix + &output;
    }
    output.into_bytes()
}
