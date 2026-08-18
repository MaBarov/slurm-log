use super::*;

#[test]
fn accounting_aggregates_multiple_steps_out_of_order() {
    let main = "100|train|COMPLETED|None|gpu|acct|normal|sub|start|end|00:10:00|600|01:00:00|1|8|8|8|4Gc|2G|1G|cpu=8,gres/gpu:a100=2|cpu=8,mem=32G,gres/gpu:a100=2|01:00:00|4800|0:0|node1||";
    let step0 = "100.0|task0|COMPLETED||||||||||||||||4G||||00:30:00|||node1|gres/gpuutil=40%|gres/gpumem=2G";
    let step_batch = "100.batch|batch|COMPLETED||||||||||||||||8G||||01:00:00|||node1|gres/gpuutil=95%|gres/gpumem=8G";
    let step_ext = "100.extern|extern|COMPLETED||||||||||||||||1G||||00:01:00|||node1|gres/gpuutil=10%|gres/gpumem=1G";

    let input = format!("{step_ext}\n{main}\n{step0}\n{step_batch}\n");
    let parsed = parse_accounting(&input, "cispa", "100")
        .expect("multi-step out of order accounting parsing");

    assert_eq!(parsed.id, "100");
    assert_eq!(parsed.name, "train");
    assert_eq!(parsed.max_rss_bytes, 8 * 1024 * 1024 * 1024);
    assert_eq!(parsed.gpu_utilization, Some(95.0));
    assert_eq!(parsed.gpu_memory_bytes, Some(8 * 1024 * 1024 * 1024));
    assert_eq!(parsed.total_cpu_seconds, 3600);
}

#[test]
fn accounting_handles_missing_tres_and_empty_fields_gracefully() {
    let line =
        "200|eval|FAILED|None|gpu||||||00:00:10|10|01:00:00|1|2|2|2||||||00:00:05|5|1:0|node2||";
    let parsed = parse_accounting(line, "sprint", "200").expect("sparse fields");
    assert_eq!(parsed.id, "200");
    assert_eq!(parsed.state, "FAILED");
    assert_eq!(parsed.exit_code, "1:0");
    assert_eq!(parsed.cpus, 2);
    assert_eq!(parsed.memory_bytes, 0);
    assert_eq!(parsed.max_rss_bytes, 0);
}
