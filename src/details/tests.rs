use super::*;
#[path = "tests/accounting.rs"]
mod accounting;
#[path = "tests/render.rs"]
mod render;

#[test]
fn parses_units_durations_and_gpu_tres() {
    assert_eq!(parse_duration("1-02:03:04"), Some(93784));
    assert_eq!(parse_duration("03:04"), Some(184));
    assert_eq!(parse_bytes("1.5G"), Some(1_610_612_736));
    assert_eq!(
        parse_gpus("cpu=8,mem=32G,gres/gpu:a100=2"),
        (2, "a100".into())
    );
    assert_eq!(
        parse_gpus("cpu=8,gres/gpu=1,gres/gpu:a100=1"),
        (1, "a100".into()),
        "generic and typed GPU TRES describe the same device"
    );
    assert_eq!(allocation_memory("cpu=8", "cpu=8,mem=1M"), 0);
    assert_eq!(display_requested_memory("1Mc"), "");
    assert_eq!(
        allocation_memory("cpu=8", "cpu=8,mem=3905M"),
        3_905 * 1024 * 1024
    );
    assert_eq!(display_requested_memory("3905M"), "3905M");
    assert_eq!(parse_bytes(&format!("{}G", "9".repeat(400))), None);
    assert_eq!(parse_gpus("malformed,cpu=8"), (0, String::new()));
    assert!(refresh_phase("42") < 10_000);
}

#[test]
fn retryable_error_details_preserve_identity_and_explanation() {
    let details = error_details("cispa", "3209343_2", "accounting is delayed");
    assert_eq!(details.cluster, "cispa");
    assert_eq!(details.id, "3209343_2");
    assert_eq!(details.state, "UNAVAILABLE");
    assert_eq!(details.source, "retryable error");
    assert_eq!(details.stale_error, "accounting is delayed");
    assert!(!details.terminal);
}

#[test]
fn accounting_matches_array_task_job_ids_and_their_steps() {
    let main = "3209343_2|array-task|COMPLETED|None|gpu|acct|normal|sub|start|end|00:01:00|60|01:00:00|1|4|4|4|1G|512M|256M|cpu=4|cpu=4,mem=1G|00:02:00|240|0:0|node1||";
    let step = "3209343_2.batch|batch|COMPLETED|None|gpu|acct|normal|sub|start|end|00:01:00|60|01:00:00|1|4|4|4|1G|768M|256M|cpu=4|cpu=4,mem=1G|00:02:00|240|0:0|node1||";
    let parsed = parse_accounting(&format!("{main}\n{step}\n"), "cispa", "3209343_2")
        .expect("array task must match its JobID field");
    assert_eq!(parsed.id, "3209343_2");
    assert_eq!(parsed.max_rss_bytes, 768 * 1024 * 1024);
    assert!(parsed.terminal);
}

#[test]
fn accounting_derives_efficiency_and_step_peak() {
    let line = "42|train|COMPLETED|None|gpu|acct|normal|sub|start|end|00:10:00|600|01:00:00|1|8|8|8|4Gc|2G|1G|cpu=8,gres/gpu:a100=2|cpu=8,mem=32G,gres/gpu:a100=2|01:00:00|4800|0:0|node1||";
    let mut step = vec![""; 28];
    step[0] = "42.batch";
    step[1] = "batch";
    step[2] = "COMPLETED";
    step[18] = "8G";
    step[22] = "01:00:00";
    let parsed = parse_accounting(&format!("{line}\n{}\n", step.join("|")), "cispa", "42").unwrap();
    assert_eq!(parsed.gpus, 2);
    assert_eq!(parsed.max_rss_bytes, 8 * 1024 * 1024 * 1024);
    assert_eq!(parsed.cpu_efficiency, Some(75.0));
    assert_eq!(parsed.memory_efficiency, Some(25.0));
}

#[test]
fn running_sstat_sample_updates_usage_without_losing_allocation() {
    let previous = JobDetails {
        cluster: "cispa".into(),
        id: "42".into(),
        cpus: 8,
        memory_bytes: 32 * 1024 * 1024 * 1024,
        ..JobDetails::default()
    };
    let job = Job {
        cluster: "cispa".into(),
        id: "42".into(),
        state: "RUNNING".into(),
        elapsed: "00:10:00".into(),
        ..Job::default()
    };
    let sample = "42.batch|8|00:07:30|8G|4G|gres/gpuutil=50|gres/gpumem=4G";
    let details = merge_running(previous, job, sample);
    assert_eq!(details.source, "sstat");
    assert_eq!(details.cpu_efficiency, Some(75.0));
    assert_eq!(details.memory_efficiency, Some(25.0));
    assert_eq!(details.gpu_utilization, Some(50.0));
}
#[test]
fn running_sstat_merges_live_process_samples_correctly() {
    let previous = JobDetails {
        id: "463740".into(),
        cpus: 8,
        memory_bytes: 48 * 1024 * 1024 * 1024,
        ..JobDetails::default()
    };
    let job = Job {
        id: "463740".into(),
        state: "RUNNING".into(),
        elapsed: "00:03:00".into(),
        ..Job::default()
    };
    let sstat_output = "463740.batch|1|00:01:30|2G|1G||gres/gpumem=1G\n\
                        463740.0|1|00:01:00|4G|2G|gres/gpuutil=95|gres/gpumem=3G";
    let details = merge_running(previous, job, sstat_output);
    assert_eq!(details.total_cpu_seconds, 150);
    assert_eq!(details.cpu_efficiency, Some(10.416666666666666));
    assert_eq!(details.max_rss_bytes, 4 * 1024 * 1024 * 1024);
    assert_eq!(details.gpu_utilization, Some(95.0));
    assert_eq!(details.gpu_memory_bytes, Some(3 * 1024 * 1024 * 1024));
    assert_eq!(details.source, "sstat");
}

#[test]
fn early_running_metrics_are_described_without_false_zero_or_gpu_warning() {
    let details = JobDetails {
        state: "RUNNING".into(),
        gpus: 0,
        ..JobDetails::default()
    };
    assert_eq!(cpu_percent(&details), "collecting…");
    assert_eq!(gpu_usage(&details), "none allocated");
}

#[test]
fn memory_peak_remains_useful_when_no_allocation_limit_exists() {
    let details = JobDetails {
        max_rss_bytes: 11 * 1024 * 1024 * 1024,
        ..JobDetails::default()
    };
    assert_eq!(memory_usage(&details), "11.0 GiB peak");
}

#[test]
fn accounting_falls_back_to_requested_memory_without_inventing_gpus() {
    let line = "3206866|cpu-only|COMPLETED|None|debug|acct|normal|sub|start|end|00:01:11|71|02:00:00|1|16|16|16|3905M|626744K||billing=16,cpu=16,node=1|billing=16,cpu=16,mem=3905M,node=1|00:11:51|1136|0:0|node-a100||";
    let parsed = parse_accounting(line, "cispa", "3206866").unwrap();
    assert_eq!(parsed.memory_bytes, 3_905 * 1024 * 1024);
    assert_eq!(parsed.gpus, 0, "node names must not imply GPU allocation");
    assert_eq!(parsed.gpu_types, "");
    assert!(parsed.terminal);
}

#[test]
fn live_scontrol_supplies_details_without_accounting() {
    let job = Job {
        cluster: "sprint".into(),
        id: "42".into(),
        name: "queued-name".into(),
        state: "RUNNING".into(),
        elapsed: "0:30".into(),
        ..Job::default()
    };
    let parsed = parse_live_control(
            "JobId=42 JobName=train JobState=RUNNING Reason=None RunTime=00:01:30 TimeLimit=02:00:00 NumNodes=1 NumCPUs=16 Partition=gpu NodeList=node-1 Account=lab QOS=normal SubmitTime=2026-08-11T12:00:00 StartTime=2026-08-11T12:01:00 EndTime=2026-08-11T14:01:00 ReqTRES=cpu=16,mem=64G,gres/gpu=2 AllocTRES=cpu=16,mem=64G,gres/gpu=2 ExitCode=0:0",
            job,
        )
        .unwrap();
    assert_eq!(parsed.name, "train");
    assert_eq!(parsed.cpus, 16);
    assert_eq!(parsed.gpus, 2);
    assert_eq!(parsed.memory_bytes, 64 * 1024 * 1024 * 1024);
    assert_eq!(parsed.elapsed_seconds, 90);
    assert_eq!(parsed.source, "scontrol");
}

#[test]
fn live_control_deduplicates_typed_gpus_and_ignores_default_memory_sentinel() {
    let job = Job {
        cluster: "cispa".into(),
        id: "3210715".into(),
        state: "RUNNING".into(),
        ..Job::default()
    };
    let parsed = parse_live_control(
        "JobId=3210715 JobName=train JobState=RUNNING NumNodes=1 NumCPUs=8 \
             ReqTRES=cpu=8,mem=1M,gres/gpu=1,gres/gpu:a100=1 \
             AllocTRES=cpu=8,gres/gpu=1,gres/gpu:a100=1",
        job,
    )
    .unwrap();
    assert_eq!(parsed.gpus, 1);
    assert_eq!(parsed.gpu_types, "a100");
    assert_eq!(parsed.memory_bytes, 0);
    assert_eq!(parsed.requested_memory, "");
}
#[test]
fn live_control_falls_back_to_tres_per_node_and_gres_when_alloc_tres_omits_gpu() {
    let job = Job {
        cluster: "sprint".into(),
        id: "463735".into(),
        state: "RUNNING".into(),
        ..Job::default()
    };
    let output = "JobId=463735 JobName=e3_mance_ns_optuna \
         UserId=mansur(1069) GroupId=mansur(1069) \
         JobState=RUNNING Reason=None \
         RunTime=00:04:18 TimeLimit=01:00:00 \
         Partition=all NodeList=sprint2 NumNodes=1 NumCPUs=8 \
         ReqTRES=cpu=8,mem=48G,node=1,billing=8 \
         AllocTRES=cpu=8,mem=48G,node=1,billing=8 \
         TresPerNode=gres/gpu:1 \
         TresPerTask=cpu=8";
    let parsed = parse_live_control(output, job.clone()).unwrap();
    assert_eq!(parsed.gpus, 1);
    assert_eq!(parsed.gpu_types, "");

    let typed_output = "JobId=463735 JobName=e3_mance_ns_optuna \
         JobState=RUNNING \
         ReqTRES=cpu=8,mem=48G,node=1,billing=8 \
         AllocTRES=cpu=8,mem=48G,node=1,billing=8 \
         TresPerNode=gres/gpu:a40:1";
    let typed_parsed = parse_live_control(typed_output, job.clone()).unwrap();
    assert_eq!(typed_parsed.gpus, 1);
    assert_eq!(typed_parsed.gpu_types, "a40");

    let gres_output = "JobId=463735 JobName=e3_mance_ns_optuna \
         JobState=RUNNING \
         ReqTRES=cpu=8,mem=48G,node=1,billing=8 \
         AllocTRES=cpu=8,mem=48G,node=1,billing=8 \
         Gres=gpu:a100:2";
    let gres_parsed = parse_live_control(gres_output, job).unwrap();
    assert_eq!(gres_parsed.gpus, 2);
    assert_eq!(gres_parsed.gpu_types, "a100");
}

#[test]
fn queue_fallback_preserves_live_identity_and_parses_elapsed_time() {
    let details = from_live_queue(Job {
        cluster: "sprint".into(),
        id: "42".into(),
        name: "training".into(),
        state: "RUNNING".into(),
        reason: "None".into(),
        partition: "gpu".into(),
        start_time: "start".into(),
        elapsed: "01:02".into(),
        ..Job::default()
    });
    assert_eq!(details.cluster, "sprint");
    assert_eq!(details.name, "training");
    assert_eq!(details.elapsed_seconds, 62);
    assert_eq!(details.source, "squeue");
    assert!(!details.terminal);
}

#[test]
fn metric_parsers_cover_zero_overflow_units_suffixes_and_tres_aliases() {
    assert_eq!(cpu_efficiency(1, 0, 8), None);
    assert_eq!(cpu_efficiency(8, 1, 1), Some(800.0));
    assert_eq!(cpu_efficiency(20, 1, 1), None);
    assert_eq!(parse_duration("0"), Some(0));
    assert_eq!(parse_duration("100:00:00"), Some(360_000));
    assert_eq!(parse_duration("bad"), None);
    assert_eq!(parse_duration("1:bad"), None);
    assert_eq!(parse_bytes("1K"), Some(1024));
    assert_eq!(parse_bytes("2M"), Some(2 * 1024 * 1024));
    assert_eq!(parse_bytes("3T"), Some(3 * 1024_u64.pow(4)));
    assert_eq!(parse_bytes("5B"), Some(5));
    assert_eq!(parse_bytes("-1G"), None);
    assert_eq!(parse_bytes("1XB"), None);
    assert_eq!(
        tres_value("cpu=8,foo/gres/gpuutil=72", "gres/gpuutil"),
        Some("72")
    );
    assert_eq!(tres_number("cpu=8", "cpu"), Some(8));
    assert_eq!(parse_gpus("gpu=2"), (2, String::new()));
}

#[test]
fn running_accounting_lag_does_not_report_false_zero_cpu() {
    let line = "7|train|RUNNING|None|gpu|acct|normal|sub|start|Unknown|00:00:10|10|01:00:00|1|8|8|8|4G|||cpu=8,mem=4G|cpu=8,mem=4G|00:00:00|80|0:0|node1||";
    let parsed = parse_accounting(line, "cispa", "7").unwrap();
    assert_eq!(parsed.cpu_efficiency, None);
    assert_eq!(cpu_percent(&parsed), "collecting…");
    assert!(!parsed.terminal);
}

#[test]
fn running_sample_aggregates_steps_and_ignores_other_jobs() {
    let previous = JobDetails {
        id: "42".into(),
        cpus: 4,
        memory_bytes: 8 * 1024 * 1024 * 1024,
        ..JobDetails::default()
    };
    let job = Job {
        id: "42".into(),
        state: "RUNNING".into(),
        elapsed: "00:10:00".into(),
        ..Job::default()
    };
    let samples = "42.batch|2|00:02:00|1G||gres/gpuutil=20|gres/gpumem=2G\n\
                   42.0|2|00:03:00|3G||gres/gpuutil=80|gres/gpumem=4G\n\
                   99.batch|99|01:00:00|99G||gres/gpuutil=99|gres/gpumem=99G";
    let parsed = merge_running(previous, job, samples);
    assert_eq!(parsed.total_cpu_seconds, 600);
    assert_eq!(parsed.max_rss_bytes, 3 * 1024 * 1024 * 1024);
    assert_eq!(parsed.gpu_utilization, Some(80.0));
    assert_eq!(parsed.gpu_memory_bytes, Some(4 * 1024 * 1024 * 1024));
}

#[test]
fn malformed_metrics_are_rejected_and_history_stays_bounded() {
    for value in ["", "Unknown", "1:2:3:4", "-1", "1XB", "NaN G"] {
        assert!(parse_duration(value).is_none() || parse_bytes(value).is_none());
    }
    assert!(parse_accounting("garbage\n||||", "cispa", "42").is_none());
    let mut history = VecDeque::new();
    for sample in 0..100 {
        push_sample(&mut history, sample as f64);
    }
    assert_eq!(history.len(), 40);
    assert_eq!(history.front(), Some(&60.0));
    assert_eq!(history.back(), Some(&99.0));
}

#[test]
fn slurm_time_sentinels_and_impossible_cpu_samples_are_not_displayed() {
    assert_eq!(parse_duration("18446744073709551614"), None);
    assert_eq!(parse_duration("18446744073709551615:59:59"), None);
    let previous = JobDetails {
        id: "42".into(),
        state: "RUNNING".into(),
        cpus: 16,
        elapsed_seconds: 24,
        total_cpu_seconds: 120,
        cpu_efficiency: Some(31.25),
        ..JobDetails::default()
    };
    let job = Job {
        id: "42".into(),
        state: "RUNNING".into(),
        elapsed: "0:24".into(),
        ..Job::default()
    };
    let sentinel = "42.batch|16|18446744073709551614|1G|||";
    let parsed = merge_running(previous, job, sentinel);
    assert_eq!(parsed.total_cpu_seconds, 120);
    assert_eq!(parsed.cpu_efficiency, Some(31.25));

    let corrupt = JobDetails {
        state: "RUNNING".into(),
        cpu_efficiency: Some(4_803_839_602_528_559.0),
        ..JobDetails::default()
    };
    assert_eq!(cpu_percent(&corrupt), "collecting…");
}
