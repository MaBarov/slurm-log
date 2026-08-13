use anyhow::{Context, Result, bail};
use rmcp::model::{
    JsonObject, ListResourcesResult, PaginatedRequestParams, ReadResourceResponse,
    ReadResourceResult, Resource, ResourceContents,
};
use serde_json::{Value, json};

use super::McpServer;

const RESOURCE_PAGE: usize = 100;

impl McpServer {
    pub(crate) fn resource_list(
        &self,
        request: Option<PaginatedRequestParams>,
    ) -> Result<ListResourcesResult> {
        let start = request
            .and_then(|value| value.cursor)
            .map(|cursor| parse_resource_cursor(&cursor))
            .transpose()?
            .unwrap_or(0);
        let needed = start.saturating_add(RESOURCE_PAGE).saturating_add(1);
        let mut resources = vec![json_resource(
            "slurm-log://clusters",
            "clusters",
            "Configured clusters without SSH credentials",
        )];
        let mut stopped_early = false;
        'clusters: for cluster in &self.config.clusters {
            if resources.len() >= needed {
                stopped_early = true;
                break;
            }
            resources.push(json_resource(
                &format!("slurm-log://clusters/{}/jobs", cluster.name),
                &format!("{} jobs", cluster.name),
                "Active and recent owner-scoped jobs",
            ));
            let Ok((jobs, ledger, _)) =
                crate::slurm::all_jobs(&self.config, &cluster.name, "all", false)
            else {
                continue;
            };
            for job in
                crate::slurm::visible_jobs(jobs, &ledger, crate::slurm::HistoryMode::Live, true)
            {
                if resources.len() >= needed {
                    stopped_early = true;
                    break 'clusters;
                }
                resources.push(json_resource(
                    &format!("slurm-log://jobs/{}/{}", cluster.name, job.id),
                    &format!("{}:{} {}", cluster.name, job.id, job.name),
                    &format!("Slurm job {}", job.state),
                ));
            }
        }
        let start = start.min(resources.len());
        let end = (start + RESOURCE_PAGE).min(resources.len());
        let mut result = ListResourcesResult::with_all_items(resources[start..end].to_vec());
        if stopped_early || end < resources.len() {
            result.next_cursor = Some(format!("r:{end}"));
        }
        Ok(result)
    }

    pub(crate) fn resource_read(&self, uri: &str) -> Result<ReadResourceResponse> {
        let route = ResourceRoute::parse(uri, &self.config)?;
        let value = match route {
            ResourceRoute::Clusters => self.resource_clusters()?,
            ResourceRoute::ClusterJobs(cluster) => {
                let mut args = JsonObject::new();
                args.insert("cluster".into(), Value::String(cluster.into()));
                args.insert("history".into(), Value::String("live".into()));
                args.insert("include_blocked".into(), Value::Bool(true));
                args.insert("limit".into(), Value::from(200));
                self.list_jobs(&args)?
            }
            ResourceRoute::Job(cluster, id) => {
                job_args(cluster, id, |args| self.inspect_job(args))?
            }
            ResourceRoute::Details(cluster, id) => {
                crate::slurm::authorize_exact_job(&self.config, cluster, id)?;
                let details = crate::daemon::job_details(&self.config, cluster, id, false)?;
                json!({"ok":true,"cluster":cluster,"job_id":id,"details":details})
            }
            ResourceRoute::Log(cluster, id) => job_args(cluster, id, |args| self.read_log(args))?,
        };
        let text = serde_json::to_string_pretty(&value)?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, uri).with_mime_type("application/json"),
        ])
        .into())
    }

    fn resource_clusters(&self) -> Result<Value> {
        let clusters = self
            .config
            .clusters
            .iter()
            .map(|cluster| {
                json!({
                    "name":cluster.name,
                    "transport":if cluster.remote() { "ssh" } else { "local" },
                    "accounting":cluster.accounting
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"ok":true,"clusters":clusters}))
    }
}

pub(crate) enum ResourceRoute<'a> {
    Clusters,
    ClusterJobs(&'a str),
    Job(&'a str, &'a str),
    Details(&'a str, &'a str),
    Log(&'a str, &'a str),
}

impl<'a> ResourceRoute<'a> {
    pub(crate) fn parse(uri: &'a str, config: &crate::config::Config) -> Result<Self> {
        let path = uri
            .strip_prefix("slurm-log://")
            .context("resource URI must use slurm-log://")?;
        let fields: Vec<_> = path.split('/').collect();
        match fields.as_slice() {
            ["clusters"] => Ok(Self::Clusters),
            ["clusters", cluster, "jobs"] => {
                config.cluster(cluster)?;
                Ok(Self::ClusterJobs(cluster))
            }
            ["jobs", cluster, id] => {
                validate_job(config, cluster, id)?;
                Ok(Self::Job(cluster, id))
            }
            ["jobs", cluster, id, "details"] => {
                validate_job(config, cluster, id)?;
                Ok(Self::Details(cluster, id))
            }
            ["jobs", cluster, id, "log"] => {
                validate_job(config, cluster, id)?;
                Ok(Self::Log(cluster, id))
            }
            _ => bail!("unknown or non-concrete slurm-log resource URI"),
        }
    }

    pub(crate) fn is_log(&self) -> bool {
        matches!(self, Self::Log(_, _))
    }

    pub(crate) fn exact_job(&self) -> Option<(&'a str, &'a str)> {
        match self {
            Self::Job(cluster, id) | Self::Details(cluster, id) | Self::Log(cluster, id) => {
                Some((cluster, id))
            }
            Self::Clusters | Self::ClusterJobs(_) => None,
        }
    }
}

fn validate_job(config: &crate::config::Config, cluster: &str, id: &str) -> Result<()> {
    config.cluster(cluster)?;
    if !crate::model::valid_job_id(id) {
        bail!("invalid job ID {id}");
    }
    Ok(())
}

fn job_args(
    cluster: &str,
    id: &str,
    operation: impl FnOnce(&JsonObject) -> Result<Value>,
) -> Result<Value> {
    let mut args = JsonObject::new();
    args.insert("cluster".into(), Value::String(cluster.into()));
    args.insert("job_id".into(), Value::String(id.into()));
    operation(&args)
}

fn json_resource(uri: &str, name: &str, description: &str) -> Resource {
    Resource::new(uri, name)
        .with_description(description)
        .with_mime_type("application/json")
}

fn parse_resource_cursor(value: &str) -> Result<usize> {
    value
        .strip_prefix("r:")
        .and_then(|value| value.parse().ok())
        .context("invalid resource cursor")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn config() -> crate::config::Config {
        crate::config::Config {
            local_user: "a".into(),
            remote_user: "a".into(),
            ssh_host: String::new(),
            state_path: PathBuf::from("/tmp/state"),
            executable: PathBuf::from("/bin/slurm-log"),
            sbatch_banks: Vec::new(),
            clusters: vec![crate::config::ClusterConfig {
                name: "one".into(),
                controller: None,
                transport: "local".into(),
                user: "a".into(),
                ssh_host: String::new(),
                working_directory: PathBuf::from("/tmp"),
                accounting: false,
            }],
        }
    }

    #[test]
    fn resource_parser_rejects_paths_and_ambiguous_jobs() {
        let config = config();
        assert!(matches!(
            ResourceRoute::parse("slurm-log://clusters", &config).unwrap(),
            ResourceRoute::Clusters
        ));
        assert!(ResourceRoute::parse("file:///etc/passwd", &config).is_err());
        assert!(ResourceRoute::parse("slurm-log://jobs/one", &config).is_err());
        assert!(ResourceRoute::parse("slurm-log://jobs/one/not-a-job/log", &config).is_err());
    }
}
