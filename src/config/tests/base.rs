use super::*;
use std::os::unix::fs::PermissionsExt;

fn config() -> Config {
    Config {
        local_user: "alice".into(),
        remote_user: "alice.cluster".into(),
        ssh_host: "cluster-alias".into(),
        state_path: PathBuf::from("/tmp/slurm-log-test-state.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![ClusterConfig {
            name: "sprint".into(),
            controller: None,
            transport: "local".into(),
            user: "alice".into(),
            ssh_host: String::new(),
            working_directory: PathBuf::from("/tmp"),
            accounting: false,
        }],
    }
}

#[test]
fn child_args_preserve_uniform_cli_overrides_without_flattening_hosts() {
    let mut value = config();
    value.local_user = "override-local".into();
    value.clusters[0].user = "override-local".into();
    assert!(
        value
            .child_args()
            .windows(2)
            .any(|pair| pair == ["--local-user", "override-local"])
    );

    value.remote_user = "override-remote".into();
    value.ssh_host = "override.invalid".into();
    for name in ["one", "two"] {
        value.clusters.push(ClusterConfig {
            name: name.into(),
            controller: None,
            transport: "ssh".into(),
            user: "override-remote".into(),
            ssh_host: format!("{name}.invalid"),
            working_directory: PathBuf::from("/tmp"),
            accounting: true,
        });
    }
    let args = value.child_args();
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--remote-user", "override-remote"])
    );
    assert!(!args.iter().any(|argument| argument == "--ssh-host"));
    for cluster in value.clusters.iter_mut().filter(|cluster| cluster.remote()) {
        cluster.ssh_host = "override.invalid".into();
    }
    assert!(
        value
            .child_args()
            .windows(2)
            .any(|pair| pair == ["--ssh-host", "override.invalid"])
    );
}

#[test]
fn private_state_directory_is_created_secured_and_never_follows_a_link() {
    let temporary = tempfile::tempdir().unwrap();
    let state = temporary.path().join("private/state.json");
    secure_state_directory(&state).unwrap();
    let directory = state.parent().unwrap();
    assert_eq!(
        fs::metadata(directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let target = temporary.path().join("target");
    fs::create_dir(&target).unwrap();
    let linked = temporary.path().join("linked/state.json");
    std::os::unix::fs::symlink(&target, linked.parent().unwrap()).unwrap();
    assert!(secure_state_directory(&linked).is_err());
}

#[test]
fn safe_configuration_is_accepted() {
    assert!(config().validate().is_ok());
    let mut local_only = config();
    local_only.ssh_host.clear();
    assert!(local_only.validate().is_ok());
}

#[test]
fn fresh_configuration_has_one_neutral_local_cluster() {
    let clusters = default_clusters("alice");
    assert_eq!(
        (
            clusters.len(),
            clusters[0].name.as_str(),
            clusters[0].transport.as_str(),
            clusters[0].user.as_str(),
            clusters[0].accounting
        ),
        (1, "local", "local", "alice", false)
    );
}

#[test]
fn only_explicit_local_controllers_are_bound_to_scheduler_commands() {
    let local = config();
    assert_eq!(local.clusters[0].name, "sprint");
    assert!(!local.clusters[0].binds_controller());

    let mut explicit = local.clusters[0].clone();
    explicit.controller = Some("federated-sprint".into());
    assert!(explicit.binds_controller());

    let remote = ClusterConfig {
        name: "cispa".into(),
        controller: None,
        transport: "ssh".into(),
        user: "alice".into(),
        ssh_host: "cispa".into(),
        working_directory: PathBuf::from("/tmp"),
        accounting: true,
    };
    assert!(remote.binds_controller());
    assert_eq!(remote.controller(), "cispa");
}

#[test]
fn accepts_new_and_legacy_bank_shapes() {
    let modern: FileConfig =
        serde_json::from_str(r#"{"sbatchBanks":[{"path":"/a","name":"A"},{"path":"/b"}]}"#)
            .unwrap();
    assert_eq!(modern.sbatch_banks.unwrap().len(), 2);
    let legacy: FileConfig = serde_json::from_str(r#"{"sbatchBank":"/old"}"#).unwrap();
    assert_eq!(legacy.sbatch_bank.unwrap(), PathBuf::from("/old"));
}

#[test]
fn ssh_option_injection_and_control_characters_are_rejected() {
    let mut value = config();
    value.ssh_host = "-oProxyCommand=evil".into();
    assert!(value.validate().is_err());
    value = config();
    value.remote_user = "alice\nother".into();
    assert!(value.validate().is_err());
}

#[test]
fn legacy_world_readable_metadata_is_migrated_to_private() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    fs::write(&path, b"{}").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
    harden_existing(&path).unwrap();
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn configurable_clusters_are_unique_and_safely_named() {
    let mut value = config();
    value.clusters.push(value.clusters[0].clone());
    assert!(value.validate().is_err());
    value = config();
    value.clusters[0].name = "../../host".into();
    assert!(value.validate().is_err());
    value = config();
    value.clusters[0].transport = "command".into();
    assert!(value.validate().is_err());
}
