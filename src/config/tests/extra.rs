use super::*;

fn valid() -> Config {
    Config {
        local_user: "alice".into(),
        remote_user: "remote".into(),
        ssh_host: String::new(),
        state_path: PathBuf::from("/tmp/state.json"),
        executable: PathBuf::from("slurm-log"),
        sbatch_banks: Vec::new(),
        clusters: vec![ClusterConfig {
            name: "local".into(),
            transport: "local".into(),
            user: "alice".into(),
            ssh_host: String::new(),
            working_directory: PathBuf::from("/tmp"),
            accounting: false,
        }],
    }
}

#[test]
fn every_configuration_boundary_is_rejected() {
    let mut cases = Vec::new();
    let mut value = valid();
    value.local_user.clear();
    cases.push(value);
    let mut value = valid();
    value.state_path = PathBuf::new();
    cases.push(value);
    let mut value = valid();
    value.clusters.clear();
    cases.push(value);
    let mut value = valid();
    value.sbatch_banks = (0..65)
        .map(|id| SbatchBankConfig {
            path: PathBuf::from(format!("/{id}")),
            name: None,
        })
        .collect();
    cases.push(value);
    let mut value = valid();
    value.sbatch_banks.push(SbatchBankConfig {
        path: PathBuf::new(),
        name: None,
    });
    cases.push(value);
    let long_bank = "x".repeat(81);
    for name in ["", " ", long_bank.as_str(), "bad\nname"] {
        let mut value = valid();
        value.sbatch_banks.push(SbatchBankConfig {
            path: PathBuf::from("/bank"),
            name: Some(name.into()),
        });
        cases.push(value);
    }
    let long_cluster = "x".repeat(49);
    for name in ["all", "both", long_cluster.as_str()] {
        let mut value = valid();
        value.clusters[0].name = name.into();
        cases.push(value);
    }
    let mut value = valid();
    value.clusters[0].user.clear();
    cases.push(value);
    let mut value = valid();
    value.clusters[0].working_directory = PathBuf::new();
    cases.push(value);
    for host in ["", "-host", "bad\nhost"] {
        let mut value = valid();
        value.clusters[0].transport = "ssh".into();
        value.clusters[0].ssh_host = host.into();
        cases.push(value);
    }
    for value in cases {
        assert!(value.validate().is_err(), "accepted {value:?}");
    }
}

#[test]
fn lookup_selection_remote_and_accounting_defaults_are_covered() {
    let value = valid();
    assert_eq!(value.cluster("local").unwrap().name, "local");
    assert!(value.cluster("missing").is_err());
    assert_eq!(value.selected_clusters("all").unwrap().len(), 1);
    assert_eq!(value.selected_clusters("both").unwrap().len(), 1);
    assert_eq!(value.selected_clusters("local").unwrap().len(), 1);
    assert!(value.selected_clusters("missing").is_err());
    assert!(!value.clusters[0].remote());

    let parsed: ClusterConfig = serde_json::from_str(
        r#"{"name":"remote","transport":"ssh","user":"u","sshHost":"h","workingDirectory":"/tmp"}"#,
    )
    .unwrap();
    assert!(parsed.remote());
    assert!(parsed.accounting);
}

#[test]
fn hardening_a_missing_file_is_a_noop() {
    let directory = tempfile::tempdir().unwrap();
    harden_existing(&directory.path().join("missing")).unwrap();
}
