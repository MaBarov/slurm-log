use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use rmcp::{
    ErrorData as McpError,
    model::ResourceUpdatedNotificationParam,
    service::{Peer, RequestContext, RoleServer},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{McpServer, resources::ResourceRoute};

const MAX_SUBSCRIPTIONS: usize = 32;
const MAX_LOG_SUBSCRIPTIONS: usize = 8;

pub struct Subscription {
    cancel: Arc<AtomicBool>,
    log: bool,
}

impl McpServer {
    pub async fn subscribe_resource(
        &self,
        uri: String,
        peer: Peer<RoleServer>,
        context: &RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        let route = ResourceRoute::parse(&uri, &self.config)
            .map_err(|error| McpError::invalid_params(format!("{error:#}"), None))?;
        let log = route.is_log();
        // Reject excess subscriptions before doing an expensive scheduler RPC.
        {
            let entries = self
                .subscriptions
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !entries.contains_key(&uri) {
                check_capacity(&entries, log)?;
            }
        }
        if let Some((cluster, id)) = route.exact_job() {
            let config = Arc::clone(&self.config);
            let cluster = cluster.to_string();
            let id = id.to_string();
            self.run_blocking(context, move || {
                crate::slurm::authorize_exact_job(&config, &cluster, &id)
            })
            .await?
            .map_err(|error| McpError::invalid_params(format!("{error:#}"), None))?;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut entries = self
                .subscriptions
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if entries.contains_key(&uri) {
                return Ok(());
            }
            check_capacity(&entries, log)?;
            entries.insert(
                uri.clone(),
                Subscription {
                    cancel: Arc::clone(&cancel),
                    log,
                },
            );
        }
        let server = self.clone();
        tokio::spawn(async move {
            monitor(server, uri, log, cancel, peer).await;
        });
        Ok(())
    }

    pub fn unsubscribe_resource(&self, uri: &str) -> Result<(), McpError> {
        let entry = self
            .subscriptions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(uri)
            .ok_or_else(|| McpError::invalid_params("resource is not subscribed", None))?;
        entry.cancel.store(true, Ordering::Release);
        Ok(())
    }

    fn subscription_fingerprint(&self, uri: &str) -> Result<(String, bool)> {
        match ResourceRoute::parse(uri, &self.config)? {
            ResourceRoute::Clusters => Ok((
                hash(&self.config.clusters.iter().map(|cluster| {
                    json!({"name":cluster.name,"transport":cluster.transport,"accounting":cluster.accounting})
                }).collect::<Vec<_>>())?,
                false,
            )),
            ResourceRoute::ClusterJobs(cluster) => {
                let (jobs, ledger, _) =
                    crate::slurm::all_jobs(&self.config, cluster, "all", false)?;
                let jobs = crate::slurm::visible_jobs(
                    jobs,
                    &ledger,
                    crate::slurm::HistoryMode::Live,
                    true,
                )
                .into_iter()
                .map(|job| json!({"id":job.id,"state":job.state}))
                .collect::<Vec<_>>();
                Ok((hash(&jobs)?, false))
            }
            ResourceRoute::Job(cluster, id) => {
                let (jobs, _, warnings) =
                    crate::slurm::all_jobs(&self.config, cluster, "all", false)?;
                let job = jobs.into_iter().find(|job| job.id == id);
                if job.is_none() && !warnings.is_empty() {
                    anyhow::bail!("scheduler state is temporarily unavailable");
                }
                let terminal = job.as_ref().is_none_or(|job| !job.active());
                let value = job.map(|job| json!({
                    "state":job.state,"reason":job.reason,
                    "allocation":job.alloc_tres,"terminal":terminal
                }));
                Ok((hash(&value)?, terminal))
            }
            ResourceRoute::Details(cluster, id) => {
                crate::slurm::authorize_exact_job(&self.config, cluster, id)?;
                let details = crate::daemon::job_details(&self.config, cluster, id, false)?;
                let terminal = details.terminal;
                let mut value = serde_json::to_value(details)?;
                if let Value::Object(object) = &mut value {
                    object.remove("sampled_at");
                    object.remove("stale_error");
                }
                Ok((hash(&value)?, terminal))
            }
            ResourceRoute::Log(cluster, id) => {
                crate::slurm::authorize_exact_job(&self.config, cluster, id)?;
                let log = crate::daemon::log_metadata(&self.config, cluster, id)?;
                let value = json!({
                    "status":log.status,"generation":log.generation,"size":log.size,
                    "terminal":log.terminal
                });
                Ok((hash(&value)?, log.terminal))
            }
        }
    }
}

async fn monitor(
    server: McpServer,
    uri: String,
    log: bool,
    cancel: Arc<AtomicBool>,
    peer: Peer<RoleServer>,
) {
    let mut previous = fingerprint(&server, &uri, Arc::clone(&cancel)).await;
    if previous.as_ref().is_some_and(|value| value.1) {
        let _ = peer
            .notify_resource_updated(ResourceUpdatedNotificationParam::new(uri.clone()))
            .await;
        remove_subscription(&server, &uri, &cancel);
        return;
    }
    let mut quiet_seconds = 0_u64;
    loop {
        let interval = poll_interval(log, quiet_seconds);
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let current = fingerprint(&server, &uri, Arc::clone(&cancel)).await;
        if current.is_none() {
            quiet_seconds = quiet_seconds.saturating_add(interval);
            continue;
        }
        let changed =
            current.as_ref().map(|value| &value.0) != previous.as_ref().map(|value| &value.0);
        if changed {
            quiet_seconds = 0;
            if peer
                .notify_resource_updated(ResourceUpdatedNotificationParam::new(uri.clone()))
                .await
                .is_err()
            {
                break;
            }
        } else {
            quiet_seconds = quiet_seconds.saturating_add(interval);
        }
        let terminal = current.as_ref().is_some_and(|value| value.1);
        previous = current;
        if terminal {
            break;
        }
    }
    remove_subscription(&server, &uri, &cancel);
}

fn check_capacity(
    entries: &std::collections::HashMap<String, Subscription>,
    log: bool,
) -> Result<(), McpError> {
    if entries.len() >= MAX_SUBSCRIPTIONS {
        return Err(McpError::invalid_params(
            "at most 32 resource subscriptions are allowed",
            None,
        ));
    }
    if log && entries.values().filter(|entry| entry.log).count() >= MAX_LOG_SUBSCRIPTIONS {
        return Err(McpError::invalid_params(
            "at most eight log subscriptions are allowed",
            None,
        ));
    }
    Ok(())
}

fn poll_interval(log: bool, quiet_seconds: u64) -> u64 {
    if log && quiet_seconds >= 5 * 60 {
        30
    } else if log && quiet_seconds >= 60 {
        15
    } else {
        5
    }
}

fn remove_subscription(server: &McpServer, uri: &str, cancel: &Arc<AtomicBool>) {
    let mut entries = server
        .subscriptions
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if entries
        .get(uri)
        .is_some_and(|entry| Arc::ptr_eq(&entry.cancel, cancel))
    {
        entries.remove(uri);
    }
}

async fn fingerprint(
    server: &McpServer,
    uri: &str,
    cancel: Arc<AtomicBool>,
) -> Option<(String, bool)> {
    let server = server.clone();
    let uri = uri.to_string();
    let worker = server.clone();
    server
        .run_subscription_blocking(cancel, move || worker.subscription_fingerprint(&uri))
        .await
        .and_then(Result::ok)
}

fn hash(value: &impl serde::Serialize) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{ClusterConfig, Config},
        mcp::schema,
    };
    use std::{collections::HashMap, path::PathBuf, sync::Mutex};

    fn server() -> McpServer {
        let config = Config {
            local_user: "offline".into(),
            remote_user: "offline".into(),
            ssh_host: String::new(),
            state_path: PathBuf::from("/tmp/slurm-log-subscription-test-state.json"),
            executable: PathBuf::from("/bin/false"),
            sbatch_banks: Vec::new(),
            clusters: vec![ClusterConfig {
                name: "alpha".into(),
                controller: None,
                transport: "local".into(),
                user: "offline".into(),
                ssh_host: String::new(),
                working_directory: PathBuf::from("/tmp"),
                accounting: false,
            }],
        };
        McpServer {
            tools: Arc::new(schema::tools(&config)),
            config: Arc::new(config),
            previews: Arc::new(Mutex::new(HashMap::new())),
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
            work: Arc::new(tokio::sync::Semaphore::new(4)),
        }
    }

    #[test]
    fn fingerprints_are_deterministic_and_sensitive() {
        assert_eq!(
            hash(&json!({"a":1})).unwrap(),
            hash(&json!({"a":1})).unwrap()
        );
        assert_ne!(
            hash(&json!({"a":1})).unwrap(),
            hash(&json!({"a":2})).unwrap()
        );
    }

    #[test]
    fn log_backoff_and_connection_caps_are_exact() {
        assert_eq!(poll_interval(true, 0), 5);
        assert_eq!(poll_interval(true, 60), 15);
        assert_eq!(poll_interval(true, 300), 30);
        assert_eq!(poll_interval(false, 999), 5);

        let entries = (0..MAX_LOG_SUBSCRIPTIONS)
            .map(|index| {
                (
                    index.to_string(),
                    Subscription {
                        cancel: Arc::new(AtomicBool::new(false)),
                        log: true,
                    },
                )
            })
            .collect();
        assert!(check_capacity(&entries, true).is_err());
        assert!(check_capacity(&entries, false).is_ok());

        let full = (0..MAX_SUBSCRIPTIONS)
            .map(|index| {
                (
                    index.to_string(),
                    Subscription {
                        cancel: Arc::new(AtomicBool::new(false)),
                        log: false,
                    },
                )
            })
            .collect();
        assert!(check_capacity(&full, false).is_err());
    }

    #[test]
    fn cluster_fingerprint_and_unsubscribe_are_scoped_to_the_exact_entry() {
        let server = server();
        let (fingerprint, terminal) = server
            .subscription_fingerprint("slurm-log://clusters")
            .unwrap();
        assert_eq!(fingerprint.len(), 64);
        assert!(!terminal);

        let first = Arc::new(AtomicBool::new(false));
        server.subscriptions.lock().unwrap().insert(
            "slurm-log://clusters".into(),
            Subscription {
                cancel: Arc::clone(&first),
                log: false,
            },
        );
        let unrelated = Arc::new(AtomicBool::new(false));
        remove_subscription(&server, "slurm-log://clusters", &unrelated);
        assert_eq!(server.subscriptions.lock().unwrap().len(), 1);

        server.unsubscribe_resource("slurm-log://clusters").unwrap();
        assert!(first.load(Ordering::Acquire));
        assert!(server.unsubscribe_resource("slurm-log://clusters").is_err());
    }
}
