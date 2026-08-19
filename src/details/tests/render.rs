use super::*;

#[test]
fn renderers_cover_full_compact_terminal_stale_and_metric_states() {
    let mut details = JobDetails {
        cluster: "alpha".into(),
        id: "42".into(),
        name: "training".into(),
        state: "RUNNING".into(),
        reason: "None".into(),
        partition: "gpu".into(),
        account: "research".into(),
        qos: "normal".into(),
        submit: "submit".into(),
        start: "start".into(),
        end: "Unknown".into(),
        elapsed: "00:02:00".into(),
        elapsed_seconds: 120,
        time_limit: "01:00:00".into(),
        nodes: 1,
        cpus: 8,
        requested_cpus: 8,
        memory_bytes: 16 * 1024 * 1024 * 1024,
        requested_memory: "16G".into(),
        max_rss_bytes: 8 * 1024 * 1024 * 1024,
        gpus: 2,
        gpu_types: "a100".into(),
        total_cpu_seconds: 60,
        cpu_efficiency: Some(6.25),
        memory_efficiency: Some(50.0),
        alloc_tres: "cpu=8,mem=16G,gres/gpu=2".into(),
        req_tres: "cpu=8,mem=16G,gres/gpu=2".into(),
        node_list: "node-a".into(),
        exit_code: "0:0".into(),
        source: "sstat".into(),
        sampled_at: "now".into(),
        stale_error: "temporary lag".into(),
        ..JobDetails::default()
    };
    let cpu = VecDeque::from([0.0, 25.0, 50.0, 75.0, 100.0]);
    let memory = VecDeque::from([100.0, 50.0]);
    let gpu = VecDeque::from([0.0, 50.0, 100.0]);
    draw(
        &details,
        false,
        true,
        "paused manually",
        &cpu,
        &memory,
        &gpu,
    )
    .unwrap();
    draw(&details, true, false, "", &cpu, &memory, &gpu).unwrap();

    assert_eq!(clean(""), "—");
    assert_eq!(clean("Unknown"), "—");
    assert_eq!(clean("None"), "—");
    assert_eq!(clean("value"), "value");
    assert_eq!(percent(Some(12.345)), "12.3%");
    assert_eq!(percent(Some(f64::NAN)), "not available");
    assert_eq!(percent(Some(-1.0)), "not available");
    assert_eq!(percent(Some(1_001.0)), "not available");
    assert_eq!(bytes(0), "—");
    assert_eq!(bytes(1), "1.0 B");
    assert_eq!(bytes(1024), "1.0 KiB");
    assert_eq!(bytes(1024_u64.pow(4)), "1.0 TiB");
    assert_eq!(spark(&VecDeque::new()), "······························");
    assert_eq!(spark(&cpu).chars().count(), 30);
    assert_eq!(spark_padded(&VecDeque::new(), 10), "··········");
    assert_eq!(spark_padded(&cpu, 10).chars().count(), 10);
    assert_eq!(spark_padded(&cpu, 3).chars().count(), 3);
    assert_eq!(spark_padded(&gpu, 10).chars().count(), 10);
    assert!(hint(&details).unwrap().contains("GPU utilization"));

    details.gpu_utilization = Some(0.0);
    assert!(hint(&details).unwrap().contains("CPU utilization"));
    details.cpu_efficiency = Some(80.0);
    details.memory_efficiency = Some(90.0);
    assert!(hint(&details).unwrap().contains("Peak memory"));
    details.memory_efficiency = Some(20.0);
    assert_eq!(hint(&details), None);
    details.terminal = true;
    details.stale_error.clear();
    draw(&details, false, false, "", &cpu, &memory, &gpu).unwrap();
    draw(&details, true, false, "", &cpu, &memory, &gpu).unwrap();
    print_text(&details);
}

#[test]
fn metric_labels_cover_recorded_and_missing_gpu_and_memory() {
    let mut details = JobDetails {
        state: "COMPLETED".into(),
        gpus: 1,
        gpu_utilization: Some(98.25),
        memory_efficiency: Some(11.5),
        cpu_efficiency: None,
        ..JobDetails::default()
    };
    assert_eq!(cpu_percent(&details), "not available");
    assert_eq!(memory_usage(&details), "11.5%");
    assert_eq!(gpu_usage(&details), "98.2%");
    details.gpu_utilization = None;
    details.max_rss_bytes = 0;
    details.memory_efficiency = None;
    assert_eq!(memory_usage(&details), "not available");
    assert_eq!(gpu_usage(&details), "not recorded");
}

#[test]
fn inline_and_full_sparklines_render_across_widths_and_sparse_metrics() {
    let mut history = VecDeque::new();
    assert_eq!(spark_padded(&history, 8), "········");
    assert_eq!(spark(&history), "······························");

    push_sample(&mut history, 0.0);
    assert_eq!(spark_padded(&history, 8), "·······▁");
    assert_eq!(spark(&history), "·····························▁");

    push_sample(&mut history, 50.0);
    push_sample(&mut history, 100.0);
    assert_eq!(spark_padded(&history, 2), "▅█");
    assert_eq!(spark_padded(&history, 8), "·····▁▅█");
    assert_eq!(spark(&history), "···························▁▅█");

    push_sample(&mut history, -10.0);
    push_sample(&mut history, 150.0);
    assert_eq!(spark_padded(&history, 2), "▁█");
}

#[test]
#[ignore = "release-mode performance budget"]
fn parses_large_accounting_snapshot_within_budget() {
    let main = "42|train|COMPLETED|None|gpu|acct|normal|sub|start|end|00:10:00|600|01:00:00|1|8|8|8|32G|||cpu=8,mem=32G,gres/gpu:a100=2|cpu=8,mem=32G,gres/gpu:a100=2|01:00:00|4800|0:0|node1||";
    let mut input = String::with_capacity(1_000_000);
    input.push_str(main);
    input.push('\n');
    for step in 0..5_000 {
        input.push_str(&format!(
            "42.{step}|step|COMPLETED||||||||||||||||1G||||00:01:00|||||\n"
        ));
    }
    let started = Instant::now();
    let parsed = parse_accounting(&input, "cispa", "42").unwrap();
    let elapsed = started.elapsed();
    assert_eq!(parsed.max_rss_bytes, 1024 * 1024 * 1024);
    assert!(elapsed < Duration::from_millis(if cfg!(coverage) { 300 } else { 75 }));
    eprintln!("parse 5k detail rows: {elapsed:?}");
}
