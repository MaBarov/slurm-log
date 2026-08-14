use std::collections::HashSet;

use anyhow::Result;
use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::{
    McpServer,
    helpers::*,
    present::{match_field, script_stale},
};
use crate::{bank, slurm};

impl McpServer {
    pub(crate) fn list_clusters(&self) -> Result<Value> {
        let clusters = self
            .config
            .clusters
            .iter()
            .map(|cluster| {
                let connectivity = slurm::all_jobs(&self.config, &cluster.name, "all", false)
                    .map(|(_, _, warnings)| {
                        if warnings.is_empty() {
                            "reachable"
                        } else {
                            "degraded"
                        }
                    })
                    .unwrap_or("unreachable");
                json!({
                    "name": cluster.name,
                    "transport": if cluster.remote() { "ssh" } else { "local" },
                    "accounting": cluster.accounting,
                    "connectivity": connectivity
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"ok":true,"clusters":clusters}))
    }

    pub(crate) fn workspace_context(&self) -> Result<Value> {
        let output = crate::command::output(
            "tmux",
            &[
                "list-panes",
                "-a",
                "-F",
                "#S|#{pane_active}|#{@slurm_log_cluster}|#{@slurm_log_job_id}",
            ],
        );
        let Ok(output) = output else {
            return Ok(json!({"ok":true,"workspaces":[],"focused_jobs":[]}));
        };
        if !output.status.success() {
            return Ok(json!({"ok":true,"workspaces":[],"focused_jobs":[]}));
        }
        let mut workspaces = HashSet::new();
        let mut focused = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let fields: Vec<_> = line.splitn(4, '|').collect();
            if fields.len() != 4 || !fields[0].starts_with("slurm-logs-") {
                continue;
            }
            workspaces.insert(fields[0].to_string());
            if fields[1] == "1" && !fields[2].is_empty() && !fields[3].is_empty() {
                focused.push(json!({"workspace":fields[0],"cluster":fields[2],"job_id":fields[3]}));
            }
        }
        let mut workspaces = workspaces.into_iter().collect::<Vec<_>>();
        workspaces.sort();
        Ok(json!({"ok":true,"workspaces":workspaces,"focused_jobs":focused}))
    }

    pub(crate) fn list_scripts(&self, args: &JsonObject) -> Result<Value> {
        let cluster = optional_string(args, "cluster").unwrap_or("all");
        self.config.selected_clusters(cluster)?;
        let config = self.current_bank_config()?;
        let (scripts, warnings, catalog) = bank::catalog(&config, false)?;
        let search = optional_string(args, "search").map(str::to_lowercase);
        let mut matched = Vec::with_capacity(scripts.len());
        for script in scripts {
            let eligible = cluster == "all" || bank::supports_cluster(&script, cluster);
            if !eligible {
                continue;
            }
            let field = match &search {
                None => None,
                Some(needle) => match_field(&script, needle),
            };
            if search.is_some() && field.is_none() {
                continue;
            }
            matched.push((script, field));
        }
        let (start, limit) = page(args, "s", 50, 200)?;
        let total = matched.len();
        let end = start.saturating_add(limit).min(total);
        let bank_meta = catalog
            .banks
            .iter()
            .map(|bank| (bank.name.clone(), bank))
            .collect::<std::collections::HashMap<_, _>>();
        let items = matched
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .map(|(script, field)| {
                let eligible = config
                    .clusters
                    .iter()
                    .filter(|cluster| bank::supports_cluster(script, &cluster.name))
                    .map(|cluster| cluster.name.clone())
                    .collect::<Vec<_>>();
                let bank = bank_meta.get(&script.bank);
                let mut value = json!({
                    "script": script_id(script),
                    "bank": script.bank,
                    "job_name": script.name,
                    "directives": script.directives,
                    "eligible_clusters": eligible,
                    "script_sha256": sha256(&script.bytes),
                    "indexed_at": bank.and_then(|bank| bank.indexed_at.clone()),
                    "repo_head": bank.and_then(|bank| bank.repo_head.clone()),
                    "dirty": bank.and_then(|bank| bank.dirty),
                    "stale": script_stale(script, bank),
                });
                if let Some(field) = field {
                    value["matched_field"] = Value::String((*field).into());
                }
                value
            })
            .collect::<Vec<_>>();
        let catalog_unavailable = !catalog.available;
        Ok(json!({
            "ok": !catalog_unavailable,
            "cluster": cluster,
            "scripts": items,
            "warnings": warnings,
            "total": total,
            "next_cursor": (end < total).then(|| format!("s:{end}")),
            "catalog": {
                "available": catalog.available,
                "generation": catalog.generation,
                "indexed_at": catalog.indexed_at,
                "bank_count": catalog.banks.len(),
            }
        }))
    }

    pub(crate) fn doctor(&self) -> Result<Value> {
        let config = self.current_bank_config()?;
        let mut clusters = Vec::new();
        let mut scheduler_healthy = true;
        for cluster in &config.clusters {
            let reachable = match slurm::all_jobs_fresh(&config, &cluster.name, "all", false) {
                Ok((_, _, warnings)) => {
                    let reachable = warnings.is_empty();
                    clusters.push(json!({
                        "name": cluster.name,
                        "transport": if cluster.remote() { "ssh" } else { "local" },
                        "accounting": cluster.accounting,
                        "scheduler_reachable": reachable,
                        "warnings": warnings,
                    }));
                    reachable
                }
                Err(error) => {
                    clusters.push(json!({
                        "name": cluster.name,
                        "transport": if cluster.remote() { "ssh" } else { "local" },
                        "accounting": cluster.accounting,
                        "scheduler_reachable": false,
                        "error": format!("{error:#}"),
                    }));
                    false
                }
            };
            scheduler_healthy &= reachable;
        }
        let (scripts, bank_warnings, catalog) = bank::catalog(&config, true)?;
        let mut banks = Vec::new();
        for bank in &catalog.banks {
            banks.push(json!({
                "name": bank.name,
                "path": bank.path.display().to_string(),
                "available": bank.available,
                "script_count": bank.script_count,
                "indexed_at": bank.indexed_at,
                "generation": bank.generation,
                "repo_head": bank.repo_head,
                "dirty": bank.dirty,
            }));
        }
        let bank_healthy = catalog.available;
        Ok(json!({
            "ok": true,
            "bank_healthy": bank_healthy,
            "scheduler_healthy": scheduler_healthy,
            "clusters": clusters,
            "banks": banks,
            "indexed_script_count": scripts.len(),
            "catalog_generation": catalog.generation,
            "last_refresh": catalog.indexed_at,
            "bank_warnings": bank_warnings,
        }))
    }

    pub(crate) fn refresh_bank(&self) -> Result<Value> {
        let config = self.current_bank_config()?;
        let (scripts, warnings, catalog) = bank::catalog(&config, true)?;
        Ok(json!({
            "ok": catalog.available,
            "refreshed": true,
            "script_count": scripts.len(),
            "catalog_generation": catalog.generation,
            "indexed_at": catalog.indexed_at,
            "warnings": warnings,
            "bank_count": catalog.banks.len(),
        }))
    }
}
