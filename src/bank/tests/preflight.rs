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

    let (partition, gres) = scheduling_request(&directives(&["-p gpu?", "--gpus=2"])).unwrap();
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
    assert!(scheduling_request(&directives(&[&format!("--gres={}", "x".repeat(300))])).is_err());
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
    assert!(
        parse_overrides(Some(&serde_json::json!({})))
            .unwrap()
            .is_none()
    );
    assert!(
        parse_overrides(Some(&serde_json::json!({"job-name":"x"}))).is_err(),
        "job name is not a scheduling override"
    );
    assert!(parse_overrides(Some(&serde_json::json!({"partition":"gpu; rm -rf /"}))).is_err());
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
fn dependency_overrides_enforce_slurm_grammar() {
    let accepted = |value: &str| {
        parse_overrides(Some(&serde_json::json!({"dependency": value})))
            .unwrap()
            .unwrap()
            .remove("dependency")
            .unwrap()
    };
    for value in [
        "afterok:123",
        "afterok:123:456",
        "afterany:12_3?",
        "afternotok:45+",
        "after:1,afterok:2",
        "singleton",
        "afterok:123_1",
    ] {
        assert_eq!(accepted(value), value, "rejected dependency {value:?}");
    }
    for hostile in [
        "",
        "afterok",
        "afterok:",
        "afterok:abc",
        "afterok:12_x",
        "beforeok:1",
        "singleton:3",
        "afterok:1 afterok:2",
        "afterok:1; rm -rf /",
        "afterok:1\nsecond",
        "afterok:123:",
    ] {
        assert!(
            parse_overrides(Some(&serde_json::json!({"dependency": hostile}))).is_err(),
            "accepted dependency {hostile:?}"
        );
    }
}

#[test]
fn dependency_override_rewrites_long_and_attached_spellings() {
    let overrides = BTreeMap::from([("dependency".into(), "afterok:123".into())]);

    let script =
        b"#!/bin/bash\n#SBATCH --dependency=afterok:999\n#SBATCH --partition=cpu\necho hi\n";
    let result = String::from_utf8(apply_schedule_overrides(script, &overrides)).unwrap();
    assert!(
        result.contains("#SBATCH --dependency=afterok:123\n"),
        "{result}"
    );
    assert!(!result.contains("afterok:999"), "{result}");
    assert!(result.contains("#SBATCH --partition=cpu\n"), "{result}");
    assert!(result.contains("echo hi\n"), "{result}");

    let attached =
        apply_schedule_overrides(b"#!/bin/sh\n#SBATCH -dafterok:999\nsleep 1\n", &overrides);
    let attached = String::from_utf8(attached).unwrap();
    assert!(!attached.contains("afterok:999"), "{attached}");
    assert!(
        attached.contains("#SBATCH --dependency=afterok:123\n"),
        "{attached}"
    );
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

#[test]
fn option_key_rejects_unknown_short_flags() {
    assert_eq!(option_key("-p"), Some("partition"));
    assert_eq!(option_key("-pgpu"), Some("partition"));
    assert_eq!(option_key("-q"), None);
    assert_eq!(option_key("-qfoo"), None);
    assert_eq!(option_key("--unknown"), None);
    assert_eq!(option_key("plain"), None);
}

#[test]
fn overrides_reject_more_keys_than_the_allowlist() {
    let mut object = serde_json::Map::new();
    for index in 0..=SCHEDULE_OVERRIDE_KEYS.len() {
        object.insert(format!("key{index}"), serde_json::json!("value"));
    }
    assert!(parse_overrides(Some(&serde_json::Value::Object(object))).is_err());
}

#[test]
fn scheduling_request_skips_empty_and_irrelevant_keys() {
    let (partition, gres) = scheduling_request(&directives(&[
        "--time=01:00:00",
        "--partition=",
        "--gres=gpu:2",
    ]))
    .unwrap();
    assert_eq!(partition, None);
    assert_eq!(gres.as_deref(), Some("gpu:2"));
}
