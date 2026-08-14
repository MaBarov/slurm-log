use super::*;
use serde_json::json;

fn object(value: Value) -> JsonObject {
    value.as_object().unwrap().clone()
}

#[test]
fn hostile_unknown_and_wrong_typed_arguments_are_rejected() {
    assert!(
        tool_arguments(
            "slurm_read_log",
            &object(json!({
                "cluster":"alpha","job_id":"1","path":"/etc/passwd"
            }))
        )
        .is_err()
    );
    assert!(tool_arguments("slurm_list_jobs", &object(json!({"limit":0}))).is_err());
    assert!(tool_arguments("slurm_list_jobs", &object(json!({"cluster":7}))).is_err());
    assert!(tool_arguments("missing", &JsonObject::new()).is_err());
}

#[test]
fn every_tool_shape_and_bound_is_validated() {
    for name in ["slurm_list_clusters", "slurm_workspace_context"] {
        assert!(tool_arguments(name, &JsonObject::new()).is_ok());
        assert!(tool_arguments(name, &object(json!({"extra":true}))).is_err());
    }
    assert!(
        tool_arguments(
            "slurm_list_jobs",
            &object(json!({
                "cluster":"alpha", "history":"1d", "states":["RUNNING"],
                "include_blocked":false, "search":"x", "cursor":"jobs:1", "limit":200
            }))
        )
        .is_ok()
    );
    assert!(
        tool_arguments(
            "slurm_inspect_job",
            &object(json!({"cluster":"a","job_id":"1"}))
        )
        .is_ok()
    );
    assert!(
        tool_arguments(
            "slurm_diagnose_job",
            &object(json!({"cluster":"a","job_id":"1"}))
        )
        .is_ok()
    );
    assert!(
        tool_arguments(
            "slurm_read_log",
            &object(json!({
                "cluster":"a","job_id":"1","cursor":"v1:x","lines":2000,"filter":"all"
            }))
        )
        .is_ok()
    );
    assert!(
        tool_arguments(
            "slurm_search_log",
            &object(json!({
                "cluster":"a","job_id":"1","pattern":"x","regex":true,
                "max_matches":500,"context_lines":0
            }))
        )
        .is_ok()
    );
    assert!(tool_arguments("slurm_list_scripts", &object(json!({"limit":1}))).is_ok());
    assert!(tool_arguments("slurm_doctor", &JsonObject::new()).is_ok());
    assert!(tool_arguments("slurm_doctor", &object(json!({"force":true}))).is_err());
    assert!(tool_arguments("slurm_refresh_bank", &JsonObject::new()).is_ok());
    assert!(tool_arguments("slurm_refresh_bank", &object(json!({"x":1}))).is_err());
    assert!(
        tool_arguments(
            "slurm_wait_job",
            &object(json!({
                "cluster":"a","job_id":"1","until":"state_change",
                "timeout_seconds":30,"interval_seconds":10
            }))
        )
        .is_ok()
    );
    assert!(
        tool_arguments(
            "slurm_wait_job",
            &object(json!({"cluster":"a","job_id":"1","until":"now","timeout_seconds":31}))
        )
        .is_err()
    );
    assert!(
        tool_arguments(
            "slurm_explain_pending",
            &object(json!({"cluster":"a","job_id":"1"}))
        )
        .is_ok()
    );
    assert!(
        tool_arguments(
            "slurm_find_artifact",
            &object(json!({
                "cluster":"a","job_id":"1","pattern":"*.json",
                "search_root":"runs","max_bytes":262144
            }))
        )
        .is_ok()
    );
    assert!(
        tool_arguments(
            "slurm_find_artifact",
            &object(json!({"cluster":"a","job_id":"1"}))
        )
        .is_err()
    );
    assert!(
        tool_arguments(
            "slurm_find_artifact",
            &object(json!({"cluster":"a","job_id":"1","pattern":"x","max_bytes":0}))
        )
        .is_err()
    );
    assert!(
        tool_arguments(
            "slurm_adopt_job",
            &object(json!({
                "cluster":"a","job_id":"1",
                "batch_script_sha256":"a".repeat(64)
            }))
        )
        .is_ok()
    );
    assert!(
        tool_arguments(
            "slurm_adopt_job",
            &object(json!({"cluster":"a","job_id":"1","batch_script_sha256":"xyz"}))
        )
        .is_err()
    );
    assert!(
        tool_arguments(
            "slurm_stage_bundle",
            &object(json!({
                "entries":["a.txt","data/train.jsonl"],
                "bank":"repo","destination":"local"
            }))
        )
        .is_ok()
    );
    assert!(tool_arguments("slurm_stage_bundle", &object(json!({"entries":[]}))).is_err());
    assert!(
        tool_arguments(
            "slurm_stage_bundle",
            &object(json!({"entries":["../etc/passwd"]}))
        )
        .is_err()
    );
    assert!(
        tool_arguments(
            "slurm_stage_bundle",
            &object(json!({"entries":["a.txt"],"destination":"cloud"}))
        )
        .is_err()
    );
    assert!(
        tool_arguments(
            "slurm_stage_bundle",
            &object(json!({"entries":["a.txt"],"version":"v2"}))
        )
        .is_err()
    );
    assert!(
        tool_arguments(
            "slurm_stage_bundle",
            &object(json!({"entries":vec!["x"; 513]}))
        )
        .is_err()
    );
    assert!(
        tool_arguments(
            "slurm_preview_submission",
            &object(json!({
                "cluster":"a","script":"Bank/x.sbatch"
            }))
        )
        .is_ok()
    );
    assert!(
        tool_arguments(
            "slurm_preflight_job",
            &object(json!({
                "cluster":"a","script":"Bank/x.sbatch","wait_seconds":60
            }))
        )
        .is_ok()
    );
    assert!(
        tool_arguments(
            "slurm_preflight_job",
            &object(json!({"cluster":"a","script":"Bank/x.sbatch","wait_seconds":61}))
        )
        .is_err()
    );
    assert!(
        tool_arguments(
            "slurm_preview_resubmit",
            &object(json!({
                "cluster":"a","job_id":"1","script":"Bank/x.sbatch",
                "schedule_overrides":{"partition":"gpu","time":"01:00:00"}
            }))
        )
        .is_ok()
    );
    assert!(
        tool_arguments(
            "slurm_preview_resubmit",
            &object(json!({
                "cluster":"a","job_id":"1","script":"Bank/x.sbatch",
                "schedule_overrides":{"job-name":"sneaky"}
            }))
        )
        .is_err()
    );
    assert!(
        tool_arguments(
            "slurm_preview_resubmit",
            &object(json!({
                "cluster":"a","job_id":"1","script":"Bank/x.sbatch",
                "schedule_overrides":{"partition":""}
            }))
        )
        .is_err()
    );
    assert!(tool_arguments("slurm_submit_job", &object(json!({"preview_token":"x"}))).is_ok());
    assert!(
        tool_arguments(
            "slurm_cancel_job",
            &object(json!({
                "cluster":"a","job_id":"1","expected_job_name":"train"
            }))
        )
        .is_ok()
    );

    assert!(
        tool_arguments(
            "slurm_search_log",
            &object(json!({
                "cluster":"a","job_id":"1","pattern":"x","regex":"yes"
            }))
        )
        .is_err()
    );
    assert!(tool_arguments("slurm_list_jobs", &object(json!({"states":"RUNNING"}))).is_err());
    assert!(tool_arguments("slurm_list_jobs", &object(json!({"states":[7]}))).is_err());
    assert!(tool_arguments("slurm_list_jobs", &object(json!({"states":vec!["x"; 33]}))).is_err());
    assert!(tool_arguments("slurm_submit_job", &object(json!({"preview_token":""}))).is_err());
    assert!(
        tool_arguments(
            "slurm_search_log",
            &object(json!({
                "cluster":"a","job_id":"1","pattern":"x","context_lines":21
            }))
        )
        .is_err()
    );
}
