pub fn reconcile(
    config: &Config,
    session: &str,
    jobs: &[Job],
    lines: usize,
    show_log_warnings: bool,
) -> Result<()> {
    if jobs.is_empty() {
        bail!("at least one log must remain open");
    }
    // Auxiliary details panes are paired with the current layout. Close them
    // before a structural add/remove so tmux cannot tile them as log panes.
    close_details_for_session(session)?;
    let current = panes(session)?;
    let desired: HashMap<_, _> = jobs
        .iter()
        .map(|job| ((job.cluster.clone(), job.id.clone()), job))
        .collect();
    let mut current_keys: HashSet<_> = current
        .iter()
        .map(|pane| (pane.cluster.clone(), pane.job_id.clone()))
        .collect();

    // Remove panels that are no longer selected before adding replacements.
    // Keeping the old set around consumed all available layout space, so only
    // the first few newly marked jobs could be split in. If every old panel is
    // being replaced, retain one temporary anchor because tmux cannot remove
    // the final pane of a window before its replacement exists.
    let desired_keys = desired.keys().cloned().collect();
    let (obsolete, anchor) = obsolete_panes(&current, &desired_keys);
    for pane in obsolete {
        tmux(["kill-pane", "-t", &pane.id])?;
        current_keys.remove(&(pane.cluster.clone(), pane.job_id.clone()));
    }

    for job in jobs {
        let key = (job.cluster.clone(), job.id.clone());
        if current_keys.contains(&key) {
            continue;
        }
        let mut args = vec![
            "split-window".into(),
            "-d".into(),
            "-P".into(),
            "-F".into(),
            "#{pane_id}".into(),
            "-t".into(),
            session.into(),
        ];
        args.extend(watcher(config, job, lines, show_log_warnings));
        let out = tmux(args)?;
        if !out.status.success() {
            let reason = String::from_utf8_lossy(&out.stderr).trim().to_string();
            bail!(
                "could not open all marked panels: {}",
                if reason.is_empty() {
                    "tmux split failed"
                } else {
                    &reason
                }
            );
        }
        label(String::from_utf8_lossy(&out.stdout).trim(), job)?;
        current_keys.insert(key);
    }
    if let Some(anchor) = anchor {
        tmux(["kill-pane", "-t", &anchor.id])?;
    }
    tmux(["select-layout", "-t", session, "tiled"])?;
    Ok(())
}

pub fn session_command(action: &str, target: Option<&str>) -> Result<i32> {
    let out = tmux(["list-sessions", "-F", "#S"])?;
    let sessions: Vec<_> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|name| name.starts_with("slurm-logs-"))
        .map(str::to_string)
        .collect();
    if action == "sessions" {
        println!("{}", sessions.join("\n"));
        return Ok(if sessions.is_empty() { 1 } else { 0 });
    }
    if action == "close" && target == Some("all") {
        for session in sessions {
            tmux(["kill-session", "-t", &session])?;
        }
        return Ok(0);
    }
    let name = target
        .map(str::to_string)
        .or_else(|| sessions.last().cloned())
        .ok_or_else(|| anyhow::anyhow!("no slurm-log session"))?;
    if action == "close" {
        tmux(["kill-session", "-t", &name])?;
        return Ok(0);
    }
    let command = if env::var_os("TMUX").is_some() {
        "switch-client"
    } else {
        "attach-session"
    };
    Ok(Command::new("tmux")
        .args([command, "-t", &name])
        .status()?
        .code()
        .unwrap_or(1))
}

pub fn single_pane(session: &str) -> Result<bool> {
    Ok(!confirmation_needed(panes(session)?.len()))
}

fn confirmation_needed(log_panes: usize) -> bool {
    log_panes > 1
}
