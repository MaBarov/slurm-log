use super::*;
use std::path::PathBuf;

#[test]
fn schemas_are_object_rooted_and_mutations_are_not_read_only() {
    let config = Config {
        local_user: "alice".into(),
        remote_user: "alice".into(),
        ssh_host: String::new(),
        state_path: PathBuf::from("/tmp/state"),
        executable: PathBuf::from("/bin/slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![crate::config::ClusterConfig {
            name: "alpha".into(),
            controller: None,
            transport: "local".into(),
            user: "alice".into(),
            ssh_host: String::new(),
            working_directory: PathBuf::from("/tmp"),
            accounting: false,
        }],
    };
    let tools = tools(&config);
    assert_eq!(tools.len(), 21);
    for tool in &tools {
        assert_eq!(
            tool.input_schema.get("type").and_then(Value::as_str),
            Some("object")
        );
        assert_eq!(
            tool.output_schema
                .as_ref()
                .unwrap()
                .get("type")
                .and_then(Value::as_str),
            Some("object")
        );
    }
    let submit = tools
        .iter()
        .find(|tool| tool.name == "slurm_submit_job")
        .unwrap();
    assert_eq!(
        submit.annotations.as_ref().unwrap().read_only_hint,
        Some(false)
    );
    assert_eq!(
        submit.annotations.as_ref().unwrap().destructive_hint,
        Some(true)
    );
}
