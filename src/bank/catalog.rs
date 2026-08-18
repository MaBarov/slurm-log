#[derive(Clone, Debug)]
struct LoadedBank {
    name: String,
    first: usize,
    last: usize,
    path: PathBuf,
    error: Option<String>,
    indexed_at: i64,
    repo_commit: Option<String>,
    fingerprint: u64,
}

#[derive(Clone, Debug)]
pub struct BankHealth {
    pub name: String,
    pub path: PathBuf,
    pub scripts: usize,
    pub indexed_at: i64,
    pub repo_commit: Option<String>,
    pub fingerprint: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CatalogSnapshot {
    pub scripts: Vec<Script>,
    pub warnings: Vec<String>,
    pub banks: Vec<BankHealth>,
    pub catalog_ok: bool,
}

fn epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

fn inferred_name(bank: &SbatchBankConfig) -> Result<String> {
    if let Some(name) = bank
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        return Ok(name.to_string());
    }
    let canonical = bank
        .path
        .canonicalize()
        .with_context(|| format!("open bank {}", bank.path.display()))?;
    if let Some(repository) = canonical
        .ancestors()
        .find(|directory| directory.join(".git").exists())
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
    {
        return Ok(repository.to_string());
    }
    Ok(fallback_name(&canonical))
}

fn fallback_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Sbatch Bank")
        .to_string()
}

fn scan_all(config: &Config) -> Result<(Vec<LoadedBank>, Vec<Script>, Vec<String>)> {
    scan_all_inner(config, false)
}

fn scan_all_fresh(config: &Config) -> Result<(Vec<LoadedBank>, Vec<Script>, Vec<String>)> {
    scan_all_inner(config, true)
}

fn scan_all_inner(
    config: &Config,
    force: bool,
) -> Result<(Vec<LoadedBank>, Vec<Script>, Vec<String>)> {
    if config.sbatch_banks.is_empty() {
        bail!("no sbatch bank configured; run slurm-log setup");
    }
    let mut used = BTreeMap::<String, usize>::new();
    let mut banks = Vec::with_capacity(config.sbatch_banks.len());
    let mut scripts = Vec::new();
    let mut warnings = Vec::new();
    let started = Instant::now();
    for configured in &config.sbatch_banks {
        let (payload, cached_fingerprint) = if cfg!(test) {
            let (scripts, warnings) = scan_direct(&configured.path)?;
            (
                ScanPayload {
                    name: inferred_name(configured)?,
                    scripts,
                    warnings,
                    error: None,
                    indexed_at: epoch_seconds(),
                    repo_commit: repo_head_commit(&configured.path),
                },
                None,
            )
        } else if !force && let Some((payload, fingerprint)) = load_bank_cache(config, &configured.path) {
            (payload, Some(fingerprint))
        } else if let Some(remaining) = BANK_SCAN_TIME_LIMIT.checked_sub(started.elapsed()) {
            match scan_isolated(&configured.path, remaining) {
                Ok(mut payload) => {
                    payload.indexed_at = epoch_seconds();
                    payload.repo_commit = repo_head_commit(&configured.path);
                    store_bank_cache(config, &configured.path, &payload);
                    (payload, None)
                }
                Err(error) => (
                    ScanPayload {
                        name: fallback_name(&configured.path),
                        scripts: Vec::new(),
                        warnings: Vec::new(),
                        error: Some(format!("{error:#}")),
                        indexed_at: epoch_seconds(),
                        repo_commit: None,
                    },
                    None,
                ),
            }
        } else {
            (
                ScanPayload {
                    name: fallback_name(&configured.path),
                    scripts: Vec::new(),
                    warnings: Vec::new(),
                    error: Some("skipped after the 3s total bank-scan limit".into()),
                    indexed_at: epoch_seconds(),
                    repo_commit: None,
                },
                None,
            )
        };
        let base = configured
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .unwrap_or(payload.name);
        let count = used.entry(base.clone()).or_default();
        *count += 1;
        let name = if *count == 1 {
            base
        } else {
            format!("{base} ({count})")
        };
        let first = scripts.len();
        let mut found = payload.scripts;
        let remaining = MAX_SCRIPTS.saturating_sub(first);
        if found.len() > remaining {
            found.truncate(remaining);
            warnings.push(format!(
                "all banks combined are limited to {MAX_SCRIPTS} scripts"
            ));
        }
        let fingerprint = cached_fingerprint
            .unwrap_or_else(|| bank_tree_fingerprint(&configured.path).unwrap_or_default());
        let indexed_at = payload.indexed_at;
        let repo_commit = payload.repo_commit.clone();
        for script in &mut found {
            script.bank.clone_from(&name);
            script.origin = infer_script_origin(script, config);
            script.indexed_at = indexed_at;
            script.repo_commit.clone_from(&repo_commit);
            script.bank_fingerprint = fingerprint;
        }
        scripts.append(&mut found);
        warnings.extend(
            payload
                .warnings
                .into_iter()
                .map(|warning| format!("{name}: {warning}")),
        );
        let bank_error = payload.error;
        if let Some(error) = &bank_error {
            warnings.push(format!("{name}: unavailable ({error})"));
        }
        banks.push(LoadedBank {
            name,
            first,
            last: scripts.len(),
            path: configured.path.clone(),
            error: bank_error,
            indexed_at,
            repo_commit,
            fingerprint,
        });
        if scripts.len() == MAX_SCRIPTS {
            break;
        }
    }
    Ok((banks, scripts, warnings))
}

fn infer_script_origin(script: &Script, config: &Config) -> Option<String> {
    let text = script.relative.to_string_lossy().to_lowercase();
    let tokens: Vec<_> = text
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    let mut matches = config.clusters.iter().filter(|cluster| {
        let name = cluster.name.to_lowercase();
        let host = cluster
            .ssh_host
            .split(['.', '@'])
            .next()
            .unwrap_or("")
            .to_lowercase();
        tokens.iter().any(|token| {
            token_matches_cluster(token, &name)
                || (!host.is_empty() && token_matches_cluster(token, &host))
                || (!cluster.remote() && *token == "local")
        })
    });
    let first = matches.next()?.name.clone();
    matches.next().is_none().then_some(first)
}

fn token_matches_cluster(token: &str, cluster: &str) -> bool {
    token == cluster
        || token.strip_prefix(cluster).is_some_and(|suffix| {
            !suffix.is_empty() && suffix.chars().all(|value| value.is_ascii_digit())
        })
}

pub fn supports_cluster(script: &Script, cluster: &str) -> bool {
    script
        .origin
        .as_deref()
        .is_none_or(|origin| origin == cluster)
}

pub fn configured_scripts(config: &Config) -> Result<(Vec<Script>, Vec<String>)> {
    let snapshot = catalog(config)?;
    Ok((snapshot.scripts, snapshot.warnings))
}

pub fn configured_scripts_fresh(config: &Config) -> Result<(Vec<Script>, Vec<String>)> {
    let snapshot = catalog_fresh(config)?;
    Ok((snapshot.scripts, snapshot.warnings))
}

/// Read-only catalog snapshot.  Uses the per-bank cache when it is fresh.
pub fn catalog(config: &Config) -> Result<CatalogSnapshot> {
    catalog_inner(config, false)
}

/// Forced catalog snapshot with a fresh scan of every bank.
pub fn catalog_fresh(config: &Config) -> Result<CatalogSnapshot> {
    catalog_inner(config, true)
}

fn catalog_inner(config: &Config, force: bool) -> Result<CatalogSnapshot> {
    let (banks, scripts, warnings) = if force {
        scan_all_fresh(config)?
    } else {
        scan_all(config)?
    };
    let health = banks
        .iter()
        .map(|bank| BankHealth {
            name: bank.name.clone(),
            path: bank.path.clone(),
            scripts: bank.last.saturating_sub(bank.first),
            indexed_at: bank.indexed_at,
            repo_commit: bank.repo_commit.clone(),
            fingerprint: bank.fingerprint,
            error: bank.error.clone(),
        })
        .collect::<Vec<_>>();
    let catalog_ok = !health.is_empty()
        && health.iter().any(|bank| bank.error.is_none());
    Ok(CatalogSnapshot {
        scripts,
        warnings,
        banks: health,
        catalog_ok,
    })
}


pub fn submit(config: &Config, script: &Script, cluster: &str) -> Result<Job> {
    let target = config.cluster(cluster)?;
    validate_script_controller(script, target)?;
    let mut args = vec!["--parsable"];
    if target.binds_controller() {
        args.extend(["--clusters", target.controller()]);
    }
    let output = if target.remote() {
        let remote = remote_scheduler_command("sbatch", &args, Some(&target.working_directory));
        ssh_with_input(&target.ssh_host, &remote, &script.bytes)?
    } else {
        text_with_input(
            "sbatch",
            &args,
            &script.bytes,
            Some(&target.working_directory),
        )?
    };
    let mut parts = output.trim().split(';');
    let id = parts.next().unwrap_or("");
    if !valid_job_id(id) {
        bail!("sbatch returned an invalid job ID: {:?}", output.trim());
    }
    let actual_controller = parts.next();
    if target.remote() && actual_controller.is_none() {
        bail!(
            "remote sbatch did not return a controller identity for configured controller {:?}",
            target.controller()
        );
    }
    if let Some(actual_controller) = actual_controller
        && actual_controller != target.controller()
    {
        bail!(
            "sbatch submitted to controller {actual_controller:?}, not configured controller {:?}",
            target.controller()
        );
    }
    if parts.next().is_some() {
        bail!("sbatch returned malformed parsable output: {:?}", output.trim());
    }
    crate::slurm::invalidate_caches(config);
    Ok(Job {
        cluster: cluster.into(),
        id: id.into(),
        state: "PENDING".into(),
        name: script.name.clone(),
        ..Job::default()
    })
}

pub fn cancel(config: &Config, jobs: &[Job]) -> Result<Vec<String>> {
    let mut checked = Vec::new();
    for job in jobs.iter().filter(|job| job.active()) {
        if !valid_job_id(&job.id) {
            bail!("invalid job ID {}", job.id);
        }
        let fresh = crate::slurm::fresh_cancellable_job(config, &job.cluster, &job.id)?;
        if !job.name.is_empty() && job.name != fresh.name {
            bail!(
                "job {}:{} changed name before cancellation",
                job.cluster,
                job.id
            );
        }
        checked.push(fresh);
    }
    cancel_verified(config, &checked)
}

/// Dispatch only jobs that have just passed `fresh_cancellable_job`.  MCP
/// cancellation invokes that check itself to compare the user-supplied name,
/// while the CLI and picker enter through `cancel` above.
pub(crate) fn cancel_verified(config: &Config, jobs: &[Job]) -> Result<Vec<String>> {
    let mut grouped: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for job in jobs.iter().filter(|job| job.active()) {
        if !valid_job_id(&job.id) {
            bail!("invalid job ID {}", job.id);
        }
        grouped.entry(&job.cluster).or_default().push(&job.id);
    }
    let mut failures = Vec::new();
    for (cluster, ids) in grouped {
        let target = config.cluster(cluster)?;
        let mut args = Vec::new();
        if target.binds_controller() {
            args.extend(["--clusters", target.controller()]);
        }
        args.extend(ids);
        let result = if target.remote() {
            let command = remote_scheduler_command("scancel", &args, None);
            crate::command::ssh(&target.ssh_host, &command).map(|_| ())
        } else {
            text("scancel", &args).map(|_| ())
        };
        if let Err(error) = result {
            failures.push(format!("{cluster}: {error:#}"));
        }
    }
    crate::slurm::invalidate_caches(config);
    Ok(failures)
}
