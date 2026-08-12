fn run() -> Result<()> {
    let args = parse_args()?;
    if args.mode == "setup-discover-worker" {
        return setup::run_discovery_worker(&args.targets);
    }
    if args.mode == "bank-scan-worker" {
        return bank::run_scan_worker(&args.targets);
    }
    slurm::validate_query(&args.cluster, "all")?;
    if args.refresh == 0 {
        bail!("--refresh must be at least one second");
    }
    let mut config = if args.mode == "setup" {
        Config::load_for_setup()?
    } else {
        Config::load()?
    };
    if let Some(value) = args.local_user {
        config.local_user = value.clone();
        for cluster in config
            .clusters
            .iter_mut()
            .filter(|cluster| !cluster.remote())
        {
            cluster.user = value.clone();
        }
    }
    if let Some(value) = args.remote_user {
        config.remote_user = value.clone();
        for cluster in config
            .clusters
            .iter_mut()
            .filter(|cluster| cluster.remote())
        {
            cluster.user = value.clone();
        }
    }
    if let Some(value) = args.ssh_host {
        config.ssh_host = value.clone();
        for cluster in config
            .clusters
            .iter_mut()
            .filter(|cluster| cluster.remote())
        {
            cluster.ssh_host = value.clone();
        }
    }
    if let Some(value) = args.state_path {
        config.state_path = value.into();
    }
    if let Some(value) = args.bank_dir {
        config.sbatch_banks = vec![SbatchBankConfig {
            path: value.into(),
            name: None,
        }];
    }
    config.validate()?;
    if args.mode == "setup" {
        return setup::run(&config);
    }
    if args.mode == "single-pane" {
        let session = args
            .target
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("session required"))?;
        std::process::exit(if tmux::single_pane(session)? { 0 } else { 1 });
    }
    if ["sessions", "attach", "close"].contains(&args.mode.as_str()) {
        std::process::exit(tmux::session_command(&args.mode, args.target.as_deref())?);
    }
    if args.mode == "daemon" {
        daemon::command(&config, args.target.as_deref())?;
        return Ok(());
    }
    if args.mode == "bank" {
        if let Some(job) = bank::run(&config)? {
            tmux::open(&config, &[job], args.lines, args.show_log_warnings)?;
        }
        return Ok(());
    }
    if args.mode == "submit" {
        if args.cluster == "both" || args.cluster == "all" {
            bail!("submit requires --cluster NAME");
        }
        let relative = args
            .target
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("script path required"))?;
        let (scripts, _) = bank::configured_scripts(&config)?;
        let mut matches = scripts.iter().filter(|script| {
            bank::supports_cluster(script, &args.cluster)
                && (script.relative == std::path::Path::new(relative)
                    || format!("{}/{}", script.bank, script.relative.display()) == relative)
        });
        let script = matches
            .next()
            .ok_or_else(|| anyhow::anyhow!("script is not in a configured bank"))?;
        if matches.next().is_some() {
            bail!("script path is ambiguous; use BANK/{relative}");
        }
        let job = bank::submit(&config, script, &args.cluster)?;
        println!("Submitted {} as {}:{}", job.name, job.cluster, job.id);
        return Ok(());
    }
    if args.mode == "cancel" {
        if args.cluster == "both" || args.cluster == "all" {
            bail!("cancel requires --cluster NAME");
        }
        if args.targets.is_empty() {
            bail!("cancel requires at least one job ID");
        }
        let jobs: Vec<_> = args
            .targets
            .iter()
            .map(|id| Job {
                cluster: args.cluster.clone(),
                id: id.clone(),
                state: "RUNNING".into(),
                ..Job::default()
            })
            .collect();
        let failures = bank::cancel(&config, &jobs)?;
        if !failures.is_empty() {
            bail!("{}", failures.join("; "));
        }
        println!("Cancellation requested for {} job(s)", jobs.len());
        return Ok(());
    }
    if args.mode == "suppress" {
        if args.cluster == "both" || args.cluster == "all" {
            bail!("suppress requires --cluster NAME");
        }
        if args.targets.is_empty() {
            bail!("suppress requires at least one job ID");
        }
        for id in &args.targets {
            if !valid_job_id(id) {
                bail!("invalid job ID {id}");
            }
            crate::state::Ledger::suppress(
                &config.state_path,
                &Job {
                    cluster: args.cluster.clone(),
                    id: id.clone(),
                    state: "RUNNING".into(),
                    ..Job::default()
                },
            )?;
        }
        return Ok(());
    }
    if args.mode == "toggle-details" {
        return tmux::toggle_details(
            &config,
            args.target
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("focused pane required"))?,
        );
    }
    if args.mode == "details" {
        let id = args
            .target
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("job ID required"))?;
        let cluster = resolve_detail_cluster(&config, &args.cluster, id)?;
        let result = details::run(
            &config,
            &cluster,
            id,
            env::var_os("SLURM_LOG_DETAILS_COMPACT").is_some(),
        );
        if env::var_os("SLURM_LOG_DETAILS_PANE").is_some()
            && let Ok(pane) = env::var("TMUX_PANE")
        {
            tmux::close_detail_pane(&pane);
        }
        return result;
    }
    if args.mode == "toggle-auto" {
        tmux::toggle_auto(
            &config,
            args.target
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("session required"))?,
        )?;
        return Ok(());
    }
    if args.mode == "auto-monitor" {
        tmux::monitor(
            &config,
            args.target
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("session required"))?,
            args.lines,
        )?;
        return Ok(());
    }
    if args.mode == "json" {
        let (jobs, _, _) = slurm::all_jobs(&config, &args.cluster, "all", args.archive)?;
        println!("{}", serde_json::to_string_pretty(&jobs)?);
        return Ok(());
    }
    if config.cluster(&args.mode).is_ok() {
        let id = args
            .target
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("job ID required"))?;
        let job = Job {
            cluster: args.mode,
            id: id.into(),
            state: args.initial_state,
            ..Job::default()
        };
        if args.pane_follow {
            let _ = follow::run(&config, &job, args.lines, true, args.show_log_warnings)?;
        } else if args.follow {
            let _ = follow::run(&config, &job, args.lines, false, args.show_log_warnings)?;
        } else {
            let _ = tmux::open(&config, &[job], args.lines, args.show_log_warnings)?;
        }
        return Ok(());
    }
    if ["read", "unread"].contains(&args.mode.as_str()) {
        let id = args
            .target
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("job ID required"))?;
        let changed = state::Ledger::set_read(&config.state_path, id, args.mode == "read")?;
        if changed == 0 {
            bail!("job {id} is not in the tracking ledger");
        }
        println!(
            "Marked job {id} {}",
            if args.mode == "read" {
                "read"
            } else {
                "unread"
            }
        );
        return Ok(());
    }
    if args.mode == "pick-add" {
        let session = args
            .target
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("session required"))?;
        let panes = tmux::panes(session)?;
        let open: HashSet<_> = panes
            .iter()
            .map(|p| format!("{}:{}", p.cluster, p.job_id))
            .collect();
        let (snapshot, ledger, warnings) = slurm::all_jobs(&config, &args.cluster, "all", false)?;
        // Blocked jobs are normally hidden from the live picker, but an
        // already-open pane must keep its real scheduler state. Otherwise it
        // is replaced by the red synthetic OPEN fallback every time Ctrl-b j
        // is reopened.
        let mut open_metadata: HashMap<_, _> = snapshot
            .iter()
            .filter(|job| open.contains(&job.key()))
            .cloned()
            .map(|job| (job.key(), job))
            .collect();
        let blocked_count = slurm::visible_jobs(snapshot.clone(), &ledger, 0, true)
            .iter()
            .filter(|job| job.blocked_category())
            .count();
        let mut jobs = slurm::visible_jobs(snapshot, &ledger, 0, false);
        let mut visible_keys: HashSet<_> = jobs.iter().map(Job::key).collect();
        for pane in panes {
            let key = format!("{}:{}", pane.cluster, pane.job_id);
            if visible_keys.insert(key.clone()) {
                jobs.push(open_metadata.remove(&key).unwrap_or_else(|| Job {
                    cluster: pane.cluster,
                    id: pane.job_id,
                    state: "OPEN".into(),
                    ..Job::default()
                }));
            }
        }
        let chosen = ui::pick(
            &config,
            jobs,
            ledger,
            open,
            true,
            0,
            Some((args.cluster.clone(), "all".into())),
            Some(session.to_string()),
            warnings,
            args.refresh,
            blocked_count,
        )?;
        if !chosen.jobs.is_empty() {
            tmux::reconcile(
                &config,
                session,
                &chosen.jobs,
                args.lines,
                chosen.show_log_warnings,
            )?;
        }
        return Ok(());
    }
    if valid_job_id(&args.mode) {
        for cluster in &config.clusters {
            if slurm::terminal_path(&config, &cluster.name, &args.mode).is_ok() {
                tmux::open(
                    &config,
                    &[Job {
                        cluster: cluster.name.clone(),
                        id: args.mode,
                        ..Job::default()
                    }],
                    args.lines,
                    args.show_log_warnings,
                )?;
                return Ok(());
            }
        }
        bail!("job not found");
    }
    let filter = if ["running", "failed", "blocked"].contains(&args.mode.as_str()) {
        args.mode.as_str()
    } else {
        "all"
    };
    if ![
        "all", "running", "failed", "blocked", "archive", "last", "watch", "fzf",
    ]
    .contains(&args.mode.as_str())
    {
        bail!("unknown mode or invalid job ID: {}", args.mode);
    }
    loop {
        let history_mode = if args.archive {
            2
        } else if ["failed", "blocked"].contains(&filter) {
            1
        } else {
            0
        };
        let (jobs, ledger, warnings) =
            slurm::all_jobs(&config, &args.cluster, filter, args.archive)?;
        let blocked_count = slurm::visible_jobs(jobs.clone(), &ledger, history_mode, true)
            .iter()
            .filter(|job| job.blocked_category())
            .count();
        let jobs = slurm::visible_jobs(jobs, &ledger, history_mode, filter == "blocked");
        if args.mode == "watch" {
            print!("\x1b[H\x1b[2J");
            render(&jobs, &warnings);
            thread::sleep(Duration::from_secs(args.refresh));
            continue;
        }
        if args.fzf || args.mode == "fzf" {
            let selected = choose_fzf(&jobs)?;
            if !selected.is_empty() {
                tmux::open(&config, &selected, args.lines, args.show_log_warnings)?;
            }
            return Ok(());
        }
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            render(&jobs, &warnings);
            return Ok(());
        }
        if args.mode == "last" {
            if let Some(job) = jobs.first() {
                tmux::open(
                    &config,
                    std::slice::from_ref(job),
                    args.lines,
                    args.show_log_warnings,
                )?;
            }
            return Ok(());
        }
        let chosen = ui::pick(
            &config,
            jobs,
            ledger,
            HashSet::new(),
            false,
            history_mode,
            Some((args.cluster.clone(), filter.to_string())),
            None,
            warnings,
            args.refresh,
            blocked_count,
        )?;
        if chosen.jobs.is_empty() {
            return Ok(());
        }
        tmux::open(&config, &chosen.jobs, args.lines, chosen.show_log_warnings)?;
    }
}
