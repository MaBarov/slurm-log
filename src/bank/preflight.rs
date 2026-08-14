// Preflight probes and scheduling-only resubmission overrides.
//
// A preflight probe is a tiny fixed sbatch script whose scheduling
// constraints (partition and GPU request) are copied verbatim from an exact
// configured-bank script. Resubmission overrides are validated against a
// strict scheduling-only allowlist and applied by rewriting `#SBATCH`
// scheduling lines; the producer body is never modified.

/// The only `#SBATCH` options a resubmission preview may override. Values
/// must consist of scheduling-safe characters and are emitted verbatim.
pub const SCHEDULE_OVERRIDE_KEYS: [&str; 11] = [
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
        overrides.insert(
            key.clone(),
            validate_token(value, key).map(str::to_string)?,
        );
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

#[cfg(test)]
mod preflight_tests {
    use super::*;

    fn directives(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn scheduling_request_accepts_long_and_short_spellings() {
        let (partition, gres) = scheduling_request(&directives(&[
            "--partition=gpu",
            "--gres=gpu:a100:2",
            "--job-name=train",
        ]))
        .unwrap();
        assert_eq!(partition.as_deref(), Some("gpu"));
        assert_eq!(gres.as_deref(), Some("gpu:a100:2"));

        let (partition, gres) = scheduling_request(&directives(&[
            "-p gpu?",
            "--gpus=2",
        ]))
        .unwrap();
        assert_eq!(partition.as_deref(), Some("gpu?"));
        assert_eq!(gres.as_deref(), Some("gpu:2"));
    }

    #[test]
    fn scheduling_request_rejects_hostile_tokens() {
        for hostile in [
            "gpu; rm -rf /",
            "gpu$(id)",
            "gpu`id`",
            "a b",
            "gpu\nsecond",
            "gpu\"x",
        ] {
            assert!(
                scheduling_request(&directives(&[&format!("--partition={hostile}")])).is_err(),
                "accepted partition {hostile:?}"
            );
            assert!(
                scheduling_request(&directives(&[&format!("--gres={hostile}")])).is_err(),
                "accepted gres {hostile:?}"
            );
        }
        assert!(
            scheduling_request(&directives(&[&format!("--gres={}", "x".repeat(300))])).is_err()
        );
    }

    #[test]
    fn probe_script_only_interpolates_validated_tokens() {
        let script = probe_script("SLURM_LOG_PREFLIGHT_ABC", Some("gpu"), Some("gpu:2"));
        let text = String::from_utf8(script).unwrap();
        assert!(text.starts_with("#!/bin/sh\n"));
        assert!(text.contains("#SBATCH --job-name=SLURM_LOG_PREFLIGHT_ABC\n"));
        assert!(text.contains("#SBATCH --partition=gpu\n"));
        assert!(text.contains("#SBATCH --gres=gpu:2\n"));
        assert!(text.contains("nvidia-smi -L 2>/dev/null || true"));
    }

    #[test]
    fn overrides_are_allowlist_only_and_validated() {
        let value = serde_json::json!({"partition":"gpu","time":"01:00:00"});
        let overrides = parse_overrides(Some(&value)).unwrap().unwrap();
        assert_eq!(overrides["partition"], "gpu");

        assert!(parse_overrides(None).unwrap().is_none());
        assert!(parse_overrides(Some(&serde_json::json!({}))).unwrap().is_none());
        assert!(
            parse_overrides(Some(&serde_json::json!({"job-name":"x"}))).is_err(),
            "job name is not a scheduling override"
        );
        assert!(
            parse_overrides(Some(&serde_json::json!({"partition":"gpu; rm -rf /"}))).is_err()
        );
        assert!(parse_overrides(Some(&serde_json::json!({"partition":7}))).is_err());
        assert!(parse_overrides(Some(&serde_json::json!("partition"))).is_err());
    }

    #[test]
    fn applying_overrides_replaces_only_scheduling_lines() {
        let script = b"#!/bin/bash\n#SBATCH --partition=cpu\n#SBATCH --time=04:00:00\n#SBATCH --job-name=train\necho hello\n";
        let overrides = BTreeMap::from([
            ("partition".into(), "gpu".into()),
            ("time".into(), "01:00:00".into()),
        ]);
        let result = String::from_utf8(apply_schedule_overrides(script, &overrides)).unwrap();
        assert!(result.contains("#SBATCH --partition=gpu\n"), "{result}");
        assert!(result.contains("#SBATCH --time=01:00:00\n"), "{result}");
        assert!(result.contains("#SBATCH --job-name=train\n"), "{result}");
        assert!(result.contains("echo hello\n"), "{result}");
        assert!(!result.contains("--partition=cpu"), "{result}");
        assert!(!result.contains("--time=04:00:00"), "{result}");

        let attached = apply_schedule_overrides(
            b"#!/bin/sh\n#SBATCH -pgpu\n#SBATCH --mem=2G\nsleep 1\n",
            &BTreeMap::from([("partition".into(), "cpu".into())]),
        );
        let attached = String::from_utf8(attached).unwrap();
        assert!(!attached.contains("-pgpu"), "{attached}");
        assert!(attached.contains("#SBATCH --partition=cpu"), "{attached}");
        assert!(attached.contains("#SBATCH --mem=2G"), "{attached}");
        assert!(attached.contains("sleep 1"), "{attached}");
    }

    #[test]
    fn body_without_directives_stays_untouched() {
        let script = b"#!/bin/sh\necho unchanged\n";
        let overrides = BTreeMap::from([("time".into(), "00:30:00".into())]);
        let result = String::from_utf8(apply_schedule_overrides(script, &overrides)).unwrap();
        assert!(result.contains("echo unchanged\n"), "{result}");
        assert!(result.contains("#SBATCH --time=00:30:00\n"), "{result}");
    }
}
