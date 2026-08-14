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

#[test]
fn tool_pagination_emits_a_cursor_only_when_a_next_page_exists() {
    let template = base_tool("tool", "dummy", object(BTreeMap::new(), &[]));
    let tools: Vec<Tool> = std::iter::repeat_n(template, 51).collect();
    let first = paginate_tools(&tools, None).unwrap();
    assert_eq!(first.tools.len(), 50);
    assert_eq!(first.next_cursor.as_deref(), Some("t:50"));
    let cursor_request =
        |cursor: &str| serde_json::from_value(json!({ "cursor": cursor })).unwrap();
    let second = paginate_tools(&tools, Some(cursor_request("t:50"))).unwrap();
    assert_eq!(second.tools.len(), 1);
    assert!(second.next_cursor.is_none());
    assert!(paginate_tools(&tools, Some(cursor_request("bad"))).is_err());
}
