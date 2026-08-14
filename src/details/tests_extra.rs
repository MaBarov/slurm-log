use super::*;

#[test]
fn accounting_rows_are_filtered_to_the_exact_owner_and_controller() {
    let target = crate::config::ClusterConfig {
        name: "cispa".into(),
        controller: Some("real-cispa".into()),
        transport: "ssh".into(),
        user: "alice".into(),
        ssh_host: "cispa".into(),
        working_directory: std::path::PathBuf::from("/tmp"),
        accounting: true,
    };
    let mut row = vec!["x"; 30];
    row[0] = "42";
    row[28] = "alice";
    row[29] = "real-cispa";
    assert!(owned_accounting_rows(&row.join("|"), &target, "42").is_ok());

    let mut short = vec!["x"; 29];
    short[0] = "42";
    assert!(owned_accounting_rows(&short.join("|"), &target, "42").is_err());

    let mut other_base = row.clone();
    other_base[0] = "99";
    assert!(owned_accounting_rows(&other_base.join("|"), &target, "42").is_err());

    let mut other_owner = row.clone();
    other_owner[28] = "bob";
    assert!(owned_accounting_rows(&other_owner.join("|"), &target, "42").is_err());

    let mut other_controller = row.clone();
    other_controller[29] = "other";
    assert!(owned_accounting_rows(&other_controller.join("|"), &target, "42").is_err());
}

#[test]
fn fetch_rejects_an_invalid_job_id_before_any_scheduler_lookup() {
    let config = Config {
        local_user: "alice".into(),
        remote_user: "alice".into(),
        ssh_host: String::new(),
        state_path: std::path::PathBuf::from("/tmp/state.json"),
        executable: std::path::PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![crate::config::ClusterConfig {
            name: "cispa".into(),
            controller: None,
            transport: "local".into(),
            user: "alice".into(),
            ssh_host: String::new(),
            working_directory: std::path::PathBuf::from("/tmp"),
            accounting: false,
        }],
    };
    assert!(fetch(&config, "cispa", "not valid", None).is_err());
}
