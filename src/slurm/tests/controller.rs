use super::*;

#[test]
fn controller_arguments_never_injected_unless_controller_is_explicit() {
    let config = Config {
        local_user: "alice".into(),
        remote_user: "alice".into(),
        ssh_host: String::new(),
        state_path: PathBuf::from("/tmp/state.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![
            crate::config::ClusterConfig {
                name: "local".into(),
                controller: None,
                transport: "local".into(),
                user: "alice".into(),
                ssh_host: String::new(),
                working_directory: PathBuf::from("/tmp"),
                accounting: false,
            },
            crate::config::ClusterConfig {
                name: "remote_cluster".into(),
                controller: None,
                transport: "ssh".into(),
                user: "alice".into(),
                ssh_host: "remote_host".into(),
                working_directory: PathBuf::from("/home/alice"),
                accounting: true,
            },
            crate::config::ClusterConfig {
                name: "federated".into(),
                controller: Some("fed_controller".into()),
                transport: "ssh".into(),
                user: "bob".into(),
                ssh_host: "fed-host".into(),
                working_directory: PathBuf::from("/home/bob"),
                accounting: true,
            },
        ],
    };

    let local = &config.clusters[0];
    assert_eq!(
        controller_bound_args(local, "squeue", &["-u", "alice"]),
        vec!["-u", "alice"]
    );
    assert_eq!(
        controller_bound_args(local, "scontrol", &["show", "job", "42"]),
        vec!["show", "job", "42"]
    );
    assert_eq!(controller_option(&config, "local").unwrap(), "");

    let remote = &config.clusters[1];
    assert_eq!(
        controller_bound_args(remote, "squeue", &["-u", "alice"]),
        vec!["-u", "alice"]
    );
    assert_eq!(
        controller_bound_args(remote, "scontrol", &["show", "job", "42"]),
        vec!["show", "job", "42"]
    );
    assert_eq!(controller_option(&config, "remote_cluster").unwrap(), "");
    let bound = &config.clusters[2];
    assert_eq!(
        controller_bound_args(bound, "squeue", &["-u", "bob"]),
        vec!["-u", "bob", "--clusters", "fed_controller"]
    );
    assert_eq!(
        controller_bound_args(bound, "scontrol", &["show", "job", "42"]),
        vec!["--cluster", "fed_controller", "show", "job", "42"]
    );
    assert_eq!(
        controller_option(&config, "federated").unwrap(),
        "--clusters fed_controller"
    );
}
