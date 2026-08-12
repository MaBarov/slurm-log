use std::{env, fs, os::unix::fs::PermissionsExt, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, serde::Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClusterConfig {
    pub name: String,
    pub transport: String,
    pub user: String,
    #[serde(default)]
    pub ssh_host: String,
    pub working_directory: PathBuf,
    #[serde(default = "accounting_default")]
    pub accounting: bool,
}

fn accounting_default() -> bool {
    true
}

fn default_clusters(local_user: &str) -> Vec<ClusterConfig> {
    vec![ClusterConfig {
        name: "local".into(),
        transport: "local".into(),
        user: local_user.into(),
        ssh_host: String::new(),
        working_directory: home(),
        accounting: false,
    }]
}

impl ClusterConfig {
    pub fn remote(&self) -> bool {
        self.transport == "ssh"
    }
}

#[derive(Clone, Debug, Deserialize, serde::Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SbatchBankConfig {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileConfig {
    local_user: Option<String>,
    remote_user: Option<String>,
    ssh_host: Option<String>,
    state_path: Option<PathBuf>,
    sbatch_bank: Option<PathBuf>,
    sbatch_banks: Option<Vec<SbatchBankConfig>>,
    clusters: Option<Vec<ClusterConfig>>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub local_user: String,
    pub remote_user: String,
    pub ssh_host: String,
    pub state_path: PathBuf,
    pub executable: PathBuf,
    pub sbatch_banks: Vec<SbatchBankConfig>,
    pub clusters: Vec<ClusterConfig>,
}

fn home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_path() -> PathBuf {
    env::var_os("SLURM_LOG_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".config"))
                .join("slurm-log/config.json")
        })
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_inner(true)
    }

    pub fn load_for_setup() -> Result<Self> {
        Self::load_inner(false)
    }

    fn load_inner(harden_state: bool) -> Result<Self> {
        let config_path = config_path();
        let file: FileConfig = if config_path.exists() {
            serde_json::from_slice(
                &fs::read(&config_path)
                    .with_context(|| format!("read {}", config_path.display()))?,
            )
            .with_context(|| format!("parse {}", config_path.display()))?
        } else {
            FileConfig::default()
        };
        let local = env::var("SLURM_LOG_LOCAL_USER")
            .ok()
            .or(file.local_user)
            .or_else(|| env::var("USER").ok())
            .unwrap_or_else(|| "unknown".into());
        let remote = env::var("SLURM_LOG_REMOTE_USER")
            .ok()
            .or(file.remote_user)
            .unwrap_or_else(|| local.clone());
        let ssh_host = env::var("SLURM_LOG_SSH_HOST")
            .ok()
            .or(file.ssh_host)
            .unwrap_or_default();
        let state_path = env::var_os("SLURM_LOG_STATE")
            .map(PathBuf::from)
            .or(file.state_path)
            .unwrap_or_else(|| {
                env::var_os("XDG_STATE_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home().join(".local/state"))
                    .join("slurm-log/state.json")
            });
        let sbatch_banks = if let Some(path) = env::var_os("SLURM_LOG_SBATCH_BANK") {
            vec![SbatchBankConfig {
                path: PathBuf::from(path),
                name: None,
            }]
        } else if let Some(banks) = file.sbatch_banks {
            banks
        } else {
            file.sbatch_bank
                .map(|path| vec![SbatchBankConfig { path, name: None }])
                .unwrap_or_default()
        };
        let clusters = file.clusters.unwrap_or_else(|| default_clusters(&local));
        harden_existing(&config_path)?;
        if harden_state {
            harden_existing(&state_path)?;
            harden_existing(&state_path.with_extension("lock"))?;
            harden_existing(&state_path.with_extension("archive-cache.json"))?;
            let state_directory = state_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
            for name in [
                "state.json",
                "state.lock",
                "state.archive-cache.json",
                "queue-sprint-cache.json",
                "queue-cispa-cache.json",
                "recent-cache.json",
                "archive-cache.json",
                "queue-sprint-cache.query.lock",
                "queue-cispa-cache.query.lock",
                "recent-cache.query.lock",
                "archive-cache.query.lock",
                "daemon.lock",
            ] {
                harden_existing(&state_directory.join(name))?;
            }
        }
        let config = Self {
            local_user: local,
            remote_user: remote,
            ssh_host,
            state_path,
            executable: env::current_exe().context("locate slurm-log executable")?,
            sbatch_banks,
            clusters,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("local user", self.local_user.as_str()),
            ("remote user", self.remote_user.as_str()),
        ] {
            if value.is_empty() || value.chars().any(char::is_control) {
                bail!("{name} must be non-empty and contain no control characters");
            }
        }
        if self.ssh_host.starts_with('-') || self.ssh_host.chars().any(char::is_control) {
            bail!("legacy SSH host must not begin with '-' or contain control characters");
        }
        if self.state_path.as_os_str().is_empty() {
            bail!("state path must not be empty");
        }
        if self.clusters.is_empty() {
            bail!("at least one SLURM cluster must be configured");
        }
        if self.sbatch_banks.len() > 64 {
            bail!("at most 64 sbatch banks may be configured");
        }
        for bank in &self.sbatch_banks {
            if bank.path.as_os_str().is_empty() {
                bail!("sbatch bank path must not be empty");
            }
            if let Some(name) = &bank.name
                && (name.trim().is_empty() || name.len() > 80 || name.chars().any(char::is_control))
            {
                bail!(
                    "sbatch bank name must be non-empty, at most 80 characters, and contain no control characters"
                );
            }
        }
        let mut names = std::collections::HashSet::new();
        for cluster in &self.clusters {
            if cluster.name.is_empty()
                || cluster.name.len() > 48
                || !cluster
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
                || ["all", "both"].contains(&cluster.name.as_str())
            {
                bail!("invalid or reserved cluster name {}", cluster.name);
            }
            if !names.insert(&cluster.name) {
                bail!("duplicate cluster name {}", cluster.name);
            }
            if !["local", "ssh"].contains(&cluster.transport.as_str()) {
                bail!("cluster {} transport must be local or ssh", cluster.name);
            }
            if cluster.user.is_empty() || cluster.user.chars().any(char::is_control) {
                bail!("cluster {} has an invalid user", cluster.name);
            }
            if cluster.working_directory.as_os_str().is_empty() {
                bail!("cluster {} has an empty working directory", cluster.name);
            }
            if cluster.remote()
                && (cluster.ssh_host.is_empty()
                    || cluster.ssh_host.starts_with('-')
                    || cluster.ssh_host.chars().any(char::is_control))
            {
                bail!("cluster {} has an invalid SSH host", cluster.name);
            }
        }
        Ok(())
    }

    pub fn cluster(&self, name: &str) -> Result<&ClusterConfig> {
        self.clusters
            .iter()
            .find(|cluster| cluster.name == name)
            .ok_or_else(|| anyhow::anyhow!("unknown cluster {name}"))
    }

    pub fn selected_clusters(&self, selector: &str) -> Result<Vec<&ClusterConfig>> {
        if selector == "all" || selector == "both" {
            return Ok(self.clusters.iter().collect());
        }
        Ok(vec![self.cluster(selector)?])
    }

    pub fn child_args(&self) -> Vec<String> {
        let mut args = vec!["--state-path".into(), self.state_path.display().to_string()];
        let local: Vec<_> = self
            .clusters
            .iter()
            .filter(|cluster| !cluster.remote())
            .collect();
        if !local.is_empty() && local.iter().all(|cluster| cluster.user == self.local_user) {
            args.extend(["--local-user".into(), self.local_user.clone()]);
        }
        let remote: Vec<_> = self
            .clusters
            .iter()
            .filter(|cluster| cluster.remote())
            .collect();
        if !remote.is_empty()
            && remote
                .iter()
                .all(|cluster| cluster.user == self.remote_user)
        {
            args.extend(["--remote-user".into(), self.remote_user.clone()]);
        }
        // Preserve a public --ssh-host override across the daemon boundary,
        // but never flatten a configuration that intentionally uses distinct
        // hosts for different remote clusters.
        if !remote.is_empty()
            && !self.ssh_host.is_empty()
            && remote
                .iter()
                .all(|cluster| cluster.ssh_host == self.ssh_host)
        {
            args.extend(["--ssh-host".into(), self.ssh_host.clone()]);
        }
        args
    }
}

fn harden_existing(path: &std::path::Path) -> Result<()> {
    if path.exists() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("secure private file {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
        let args = value.child_args();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--local-user", "override-local"])
        );

        value.remote_user = "override-remote".into();
        value.ssh_host = "override.invalid".into();
        value.clusters.push(ClusterConfig {
            name: "one".into(),
            transport: "ssh".into(),
            user: "override-remote".into(),
            ssh_host: "one.invalid".into(),
            working_directory: PathBuf::from("/tmp"),
            accounting: true,
        });
        value.clusters.push(ClusterConfig {
            name: "two".into(),
            transport: "ssh".into(),
            user: "override-remote".into(),
            ssh_host: "two.invalid".into(),
            working_directory: PathBuf::from("/tmp"),
            accounting: true,
        });
        let args = value.child_args();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--remote-user", "override-remote"])
        );
        assert!(!args.iter().any(|argument| argument == "--ssh-host"));

        for cluster in value.clusters.iter_mut().filter(|cluster| cluster.remote()) {
            cluster.ssh_host = "override.invalid".into();
        }
        let args = value.child_args();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--ssh-host", "override.invalid"])
        );
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
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].name, "local");
        assert_eq!(clusters[0].transport, "local");
        assert_eq!(clusters[0].user, "alice");
        assert!(!clusters[0].accounting);
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
}
#[cfg(test)]
#[path = "config/tests/extra.rs"]
mod tests_extra;
