fn configure_clusters(current: &Config) -> Result<Vec<ClusterConfig>> {
    let existing = if has_explicit_clusters() {
        current.clusters.as_slice()
    } else {
        &[]
    };
    if existing.is_empty() {
        println!(
            "No cluster assumptions are made. Add only the local or SSH clusters you actually use."
        );
    } else {
        println!("Existing cluster configuration found; press Enter to keep each value.");
    }
    let count: usize = prompt(
        "Number of SLURM clusters",
        &existing.len().max(1).to_string(),
    )?
    .parse()
    .context("cluster count must be a number")?;
    if !(1..=16).contains(&count) {
        bail!("configure between 1 and 16 clusters");
    }
    let mut clusters = Vec::with_capacity(count);
    for index in 0..count {
        let old = existing.get(index);
        println!("\nCluster {}", index + 1);
        let transport = prompt(
            "  Connection (local/ssh)",
            old.map(|item| item.transport.as_str()).unwrap_or("local"),
        )?;
        if !matches!(transport.as_str(), "local" | "ssh") {
            bail!("cluster connection must be local or ssh");
        }
        let (ssh_host, probe, host_changed) = if transport == "ssh" {
            let previous = old.map(|item| item.ssh_host.as_str()).unwrap_or("");
            let host = choose_ssh_host(previous)?;
            println!("Connecting once to {host} to detect SLURM defaults…");
            let detected = match probe_ssh(&host) {
                Ok(probe) => {
                    println!(
                        "Detected: cluster={} · user={} · home={} · sacct={}",
                        probe.cluster.as_deref().unwrap_or("not reported"),
                        probe.user.as_deref().unwrap_or("not reported"),
                        probe.home.as_deref().unwrap_or("not reported"),
                        probe
                            .accounting
                            .map(|enabled| if enabled { "available" } else { "unavailable" })
                            .unwrap_or("not reported")
                    );
                    Some(probe)
                }
                Err(error) => {
                    println!("Could not probe {host}: {error:#}");
                    println!("Continuing with editable manual defaults.");
                    None
                }
            };
            let changed = old.is_none_or(|item| item.ssh_host != host);
            (host, detected, changed)
        } else {
            (String::new(), None, false)
        };
        let automatic_name = if transport == "local" && index == 0 {
            "local".to_string()
        } else if let Some(detected) = probe.as_ref().and_then(|probe| probe.cluster.as_deref()) {
            safe_cluster_name(detected, &ssh_host)
        } else {
            safe_cluster_name(&ssh_host, &format!("cluster{}", index + 1))
        };
        let old_name = old.filter(|_| !host_changed).map(|item| item.name.as_str());
        let name = prompt("  Short name", old_name.unwrap_or(&automatic_name))?;
        let detected_user = probe.as_ref().and_then(|probe| probe.user.as_deref());
        let user = prompt(
            "  SLURM user",
            old.filter(|_| !host_changed)
                .map(|item| item.user.as_str())
                .or(detected_user)
                .unwrap_or(&current.local_user),
        )?;
        let detected_home = probe.as_ref().and_then(|probe| probe.home.as_deref());
        let directory_default = old
            .filter(|_| !host_changed)
            .map(|item| item.working_directory.display().to_string())
            .or_else(|| detected_home.map(str::to_string))
            .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| ".".into()));
        let directory = prompt("  Default job working directory", &directory_default)?;
        let accounting_default = old
            .filter(|_| !host_changed)
            .map(|item| item.accounting)
            .or_else(|| probe.as_ref().and_then(|probe| probe.accounting))
            .unwrap_or(false);
        let accounting = prompt_yes_no("  Is sacct accounting available", accounting_default)?;
        clusters.push(ClusterConfig {
            name,
            transport,
            user,
            ssh_host,
            working_directory: PathBuf::from(directory),
            accounting,
        });
    }
    Ok(clusters)
}
fn configure_banks(current: &Config) -> Result<Vec<SbatchBankConfig>> {
    println!(
        "\nSBATCH BANKS\nQuick discovery checks at most three directory levels. Missing banks can be selected with the folder browser."
    );
    let mut candidates: BTreeMap<PathBuf, Option<String>> = current
        .sbatch_banks
        .iter()
        .map(|bank| {
            (
                fs::canonicalize(&bank.path).unwrap_or_else(|_| bank.path.clone()),
                bank.name.clone(),
            )
        })
        .collect();
    if prompt_yes_no("Discover repositories containing .sbatch files", true)? {
        let suggested = suggested_workspace_roots(current);
        let default_roots = display_workspace_roots(&suggested);
        if !suggested.is_empty() {
            println!("Suggested local roots: {default_roots}");
        }
        let input = prompt(
            "Workspace roots (space-separated; quote paths containing spaces)",
            &default_roots,
        )?;
        let roots: Vec<_> = shell_words::split(&input)
            .context("parse workspace roots")?
            .iter()
            .map(|path| expand_home(path))
            .collect();
        println!(
            "Scanning locally (up to {}s / {} directories; build/cache directories skipped)…",
            DISCOVERY_TIME_LIMIT.as_secs(),
            DISCOVERY_DIRECTORY_LIMIT
        );
        let (found, truncated) = discover_banks(&roots);
        for path in found {
            candidates.entry(path).or_insert(None);
        }
        if truncated {
            println!(
                "Discovery reached its {}s / {}-directory safety limit; add a narrower root to find anything omitted.",
                DISCOVERY_TIME_LIMIT.as_secs(),
                DISCOVERY_DIRECTORY_LIMIT,
            );
        }
    }

    let candidates: Vec<_> = candidates.into_iter().collect();
    let mut banks = Vec::new();
    if candidates.is_empty() {
        println!("No sbatch banks were found.");
    } else {
        println!("\nDiscovered/existing banks:");
        for (index, (path, name)) in candidates.iter().enumerate() {
            let kind = bank_kind(path);
            let colored_kind = if kind == "GIT" {
                "\x1b[36mGIT   \x1b[0m"
            } else {
                "\x1b[33mFOLDER\x1b[0m"
            };
            println!(
                "  {:>2}. [{}] {}{}",
                index + 1,
                colored_kind,
                path.display(),
                name.as_ref()
                    .map(|name| format!("  ({name})"))
                    .unwrap_or_default()
            );
        }
        let selection = prompt("Banks to use (all, none, or e.g. 1,3-5)", "all")?;
        for index in parse_selection(&selection, candidates.len())? {
            let (path, name) = &candidates[index];
            banks.push(SbatchBankConfig {
                path: path.clone(),
                name: name.clone(),
            });
        }
        if banks.len() > 64 {
            bail!("select at most 64 banks ({} selected)", banks.len());
        }
    }

    while banks.len() < 64 && prompt_yes_no("Add a bank directory manually", false)? {
        let directory = if prompt_yes_no("  Use folder browser", true)? {
            let mut roots: BTreeSet<PathBuf> =
                suggested_workspace_roots(current).into_iter().collect();
            roots.extend(banks.iter().map(|bank| bank.path.clone()));
            if let Ok(directory) = std::env::current_dir() {
                roots.insert(directory);
            }
            let Some(directory) = browse_bank_directory(&roots.into_iter().collect::<Vec<_>>())?
            else {
                println!("  Folder selection cancelled.");
                continue;
            };
            directory
        } else {
            let directory = prompt("  Directory", "")?;
            if directory.is_empty() {
                bail!("sbatch bank directory must not be empty");
            }
            expand_home(&directory)
        };
        let name = prompt_bank_name(None)?;
        banks.push(SbatchBankConfig {
            path: directory,
            name,
        });
    }
    Ok(banks)
}

fn configure_state_path(current: &Config) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let local_default = home.join(".local/state/slurm-log/state.json");
    let default = if current.state_path.starts_with(&home) {
        current.state_path.clone()
    } else {
        local_default
    };
    println!(
        "\nLOCAL STATE\nThe small UI ledger and daemon socket should live on responsive local storage, not a cluster mount."
    );
    let value = prompt("State file", &default.display().to_string())?;
    Ok(expand_home(&value))
}

pub fn run(current: &Config) -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!("setup requires an interactive terminal");
    }
    println!("slurm-log setup — these settings are private to this user\n");
    let clusters = configure_clusters(current)?;
    let sbatch_banks = configure_banks(current)?;
    let state_path = configure_state_path(current)?;
    let proposed = Config {
        clusters: clusters.clone(),
        sbatch_banks: sbatch_banks.clone(),
        state_path: state_path.clone(),
        ..current.clone()
    };
    proposed.validate()?;
    let value =
        json!({ "clusters": clusters, "sbatchBanks": sbatch_banks, "statePath": state_path });
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    serde_json::to_writer_pretty(&mut file, &value)?;
    writeln!(file)?;
    file.sync_all()?;
    fs::rename(&temporary, &path)?;
    println!("\nSaved {}", path.display());
    Ok(())
}
