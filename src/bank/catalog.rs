#[derive(Clone, Debug)]
struct LoadedBank {
    name: String,
    first: usize,
    last: usize,
    path: PathBuf,
    available: bool,
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
        let available = payload.error.is_none();
        if let Some(error) = payload.error {
            warnings.push(format!("{name}: unavailable ({error})"));
        }
        banks.push(LoadedBank {
            name,
            first,
            last: scripts.len(),
            path: configured.path.clone(),
            available,
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

/// Catalog-level metadata reported alongside script lists and `slurm_doctor`.
/// Distinguishes a healthy catalog with zero matches from a catalog that could
/// not be read at all, and carries provenance the client can bind previews to.
#[derive(Clone, Debug)]
pub struct BankMeta {
    pub name: String,
    pub path: PathBuf,
    pub available: bool,
    pub script_count: usize,
    pub indexed_at: Option<String>,
    pub generation: Option<String>,
    pub repo_head: Option<String>,
    pub dirty: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct CatalogMeta {
    pub available: bool,
    pub generation: String,
    pub indexed_at: Option<String>,
    pub banks: Vec<BankMeta>,
}

pub fn catalog(config: &Config, force: bool) -> Result<(Vec<Script>, Vec<String>, CatalogMeta)> {
    let (banks, scripts, warnings) = if force {
        scan_all_fresh(config)?
    } else {
        scan_all(config)?
    };
    let mut meta = Vec::with_capacity(banks.len());
    let mut latest: Option<SystemTime> = None;
    for bank in &banks {
        let generation = bank_tree_fingerprint(&bank.path).map(|value| format!("{value:016x}"));
        let indexed_at = cache_mtime(config, &bank.path).map(rfc3339);
        if let Some(at) = cache_mtime(config, &bank.path) {
            latest = Some(latest.map_or(at, |previous: SystemTime| previous.max(at)));
        }
        let (repo_head, dirty) = git_provenance(&bank.path);
        meta.push(BankMeta {
            name: bank.name.clone(),
            path: bank.path.clone(),
            available: bank.available,
            script_count: bank.last - bank.first,
            indexed_at,
            generation,
            repo_head,
            dirty,
        });
    }
    Ok((
        scripts,
        warnings,
        CatalogMeta {
            available: meta.iter().any(|bank| bank.available),
            generation: catalog_generation(config),
            indexed_at: latest.map(rfc3339),
            banks: meta,
        },
    ))
}

/// A stable token that changes only when a configured bank's tree fingerprint
/// changes. Cheap (stat-only) and independent of git or a full rescan, so it
/// can bind a submission preview to the exact catalog it was minted from.
pub fn catalog_generation(config: &Config) -> String {
    let mut combined = DefaultHasher::new();
    for configured in &config.sbatch_banks {
        configured.path.hash(&mut combined);
        if let Some(fingerprint) = bank_tree_fingerprint(&configured.path) {
            fingerprint.hash(&mut combined);
        }
    }
    format!("{:016x}", combined.finish())
}

fn cache_mtime(config: &Config, root: &Path) -> Option<SystemTime> {
    fs::metadata(bank_cache_path(config, root))
        .ok()?
        .modified()
        .ok()
}

fn rfc3339(value: SystemTime) -> String {
    let datetime: time::OffsetDateTime = value.into();
    datetime
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

/// Best-effort git provenance for a bank directory. Both probes are local,
/// fixed argv (never a shell), and deadline-bounded; absence of git yields
/// `None` rather than an error.
fn git_provenance(root: &Path) -> (Option<String>, Option<bool>) {
    let Some(repo) = git_root(root) else {
        return (None, None);
    };
    let repo = repo.to_string_lossy().into_owned();
    let head = output_with_timeout("git", &["-C", repo.as_str(), "rev-parse", "HEAD"], Duration::from_secs(3))
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty());
    let dirty = output_with_timeout("git", &["-C", repo.as_str(), "status", "--porcelain"], Duration::from_secs(3))
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !output.stdout.is_empty());
    (head, dirty)
}

fn git_root(path: &Path) -> Option<PathBuf> {
    let canonical = fs::canonicalize(path).ok()?;
    canonical
        .ancestors()
        .find(|directory| directory.join(".git").exists())
        .map(Path::to_path_buf)
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
