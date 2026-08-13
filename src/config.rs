use std::{env, fs, os::unix::fs::PermissionsExt, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, serde::Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClusterConfig {
    pub name: String,
    /// Slurm controller identity.  `name` is a local display label and older
    /// configurations used it for both purposes, so an absent value retains
    /// that behaviour while allowing a label to differ from the controller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<String>,
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
        controller: None,
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

    /// The scheduler-controller name used for an exact target binding.
    ///
    /// Existing configurations did not have a separate controller field, so
    /// their display name remains the compatibility fallback.
    pub fn controller(&self) -> &str {
        self.controller.as_deref().unwrap_or(&self.name)
    }

    /// Whether this target can be passed explicitly to Slurm commands.
    ///
    /// `name` is a display label, so a local target needs an explicit
    /// controller before it can be passed to Slurm. Remote targets still bind
    /// explicitly: when their controller is absent, the legacy name fallback
    /// remains their controller identity.
    pub fn binds_controller(&self) -> bool {
        self.remote() || self.controller.is_some()
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
            secure_state_directory(&state_path)?;
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
                "mcp-audit.jsonl",
                "mcp-audit.jsonl.1",
                "mcp-audit.jsonl.lock",
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
            if bank.path.as_os_str().is_empty()
                || bank.path.to_string_lossy().chars().any(char::is_control)
            {
                bail!("sbatch bank path must be non-empty and contain no control characters");
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
            if let Some(controller) = &cluster.controller
                && (controller.is_empty()
                    || controller.len() > 48
                    || !controller
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
                    || ["all", "both"].contains(&controller.as_str()))
            {
                bail!("invalid or reserved controller name {controller}");
            }
            if !["local", "ssh"].contains(&cluster.transport.as_str()) {
                bail!("cluster {} transport must be local or ssh", cluster.name);
            }
            if cluster.user.is_empty() || cluster.user.chars().any(char::is_control) {
                bail!("cluster {} has an invalid user", cluster.name);
            }
            if cluster.working_directory.as_os_str().is_empty()
                || cluster
                    .working_directory
                    .to_string_lossy()
                    .chars()
                    .any(char::is_control)
            {
                bail!("cluster {} has an invalid working directory", cluster.name);
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

fn secure_state_directory(state_path: &std::path::Path) -> Result<()> {
    let directory = state_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    fs::create_dir_all(directory)
        .with_context(|| format!("create private state directory {}", directory.display()))?;
    let metadata = fs::symlink_metadata(directory)
        .with_context(|| format!("inspect private state directory {}", directory.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "state parent must be a real directory: {}",
            directory.display()
        );
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("secure private state directory {}", directory.display()))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "config/tests/base.rs"]
mod tests;
#[cfg(test)]
#[path = "config/tests/extra.rs"]
mod tests_extra;
