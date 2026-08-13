#[derive(Clone, Debug)]
struct LoadedBank {
    name: String,
    first: usize,
    last: usize,
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
        let payload = if cfg!(test) {
            let (scripts, warnings) = scan_direct(&configured.path)?;
            ScanPayload {
                name: inferred_name(configured)?,
                scripts,
                warnings,
                error: None,
            }
        } else if !force && let Some(payload) = load_bank_cache(config, &configured.path) {
            payload
        } else if let Some(remaining) = BANK_SCAN_TIME_LIMIT.checked_sub(started.elapsed()) {
            match scan_isolated(&configured.path, remaining) {
                Ok(payload) => {
                    store_bank_cache(config, &configured.path, &payload);
                    payload
                }
                Err(error) => ScanPayload {
                    name: fallback_name(&configured.path),
                    scripts: Vec::new(),
                    warnings: Vec::new(),
                    error: Some(format!("{error:#}")),
                },
            }
        } else {
            ScanPayload {
                name: fallback_name(&configured.path),
                scripts: Vec::new(),
                warnings: Vec::new(),
                error: Some("skipped after the 3s total bank-scan limit".into()),
            }
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
        for script in &mut found {
            script.bank.clone_from(&name);
            script.origin = infer_script_origin(script, config);
        }
        scripts.append(&mut found);
        warnings.extend(
            payload
                .warnings
                .into_iter()
                .map(|warning| format!("{name}: {warning}")),
        );
        if let Some(error) = payload.error {
            warnings.push(format!("{name}: unavailable ({error})"));
        }
        banks.push(LoadedBank {
            name,
            first,
            last: scripts.len(),
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
    let (_, scripts, warnings) = scan_all(config)?;
    Ok((scripts, warnings))
}

pub fn configured_scripts_fresh(config: &Config) -> Result<(Vec<Script>, Vec<String>)> {
    let (_, scripts, warnings) = scan_all_fresh(config)?;
    Ok((scripts, warnings))
}

fn directive_job_name(directives: &[String]) -> Option<String> {
    directives.iter().find_map(|line| {
        line.strip_prefix("--job-name=")
            .or_else(|| line.strip_prefix("--job-name "))
            .or_else(|| line.strip_prefix("-J="))
            .map(str::to_string)
            .or_else(|| line.strip_prefix("-J ").map(str::to_string))
    })
}

/// Reject an sbatch directive that would send a previewed script to a
/// controller other than the selected target.  Both long and short Slurm
/// spellings are accepted only when their sole value equals the configured
/// controller identity.  Targets without an explicitly configured controller
/// do not bind scheduler commands, so a routing directive cannot contradict a
/// declared identity and is left to the scheduler to honour or reject.
pub fn validate_script_controller(script: &Script, target: &ClusterConfig) -> Result<()> {
    if !target.binds_controller() {
        return Ok(());
    }
    for directive in &script.directives {
        let Some(value) = routing_directive_value(directive)? else {
            continue;
        };
        if value != target.controller() {
            bail!(
                "script routing directive selects controller {value:?}, not configured controller {:?}",
                target.controller()
            );
        }
    }
    Ok(())
}

fn routing_directive_value(directive: &str) -> Result<Option<&str>> {
    for option in ["--clusters", "--cluster"] {
        if let Some(value) = directive.strip_prefix(&format!("{option}=")) {
            return routing_controller(value).map(Some);
        }
        if directive == option {
            return routing_controller("").map(Some);
        }
        if let Some(value) = directive
            .strip_prefix(option)
            .and_then(|value| value.strip_prefix(char::is_whitespace))
        {
            return routing_controller(value).map(Some);
        }
    }
    let Some(value) = directive.strip_prefix("-M") else {
        return Ok(None);
    };
    // Slurm accepts `-Mcontroller` as well as `-M controller` and
    // `-M=controller`; the attached spelling must not evade target checking.
    let value = value
        .strip_prefix('=')
        .or_else(|| value.strip_prefix(char::is_whitespace))
        .unwrap_or(value);
    routing_controller(value).map(Some)
}

fn routing_controller(value: &str) -> Result<&str> {
    let mut values = value.split_whitespace();
    let controller = values
        .next()
        .filter(|value| !value.is_empty())
        .context("sbatch routing directive requires one controller name")?;
    if values.next().is_some() {
        bail!("sbatch routing directive must contain exactly one controller name");
    }
    Ok(controller)
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
    if target.remote() && target.binds_controller() && actual_controller.is_none() {
        bail!(
            "remote sbatch did not return a controller identity for configured controller {:?}",
            target.controller()
        );
    }
    if target.binds_controller()
        && let Some(actual_controller) = actual_controller
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
