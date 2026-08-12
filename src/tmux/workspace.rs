pub fn open(config: &Config, jobs: &[Job], lines: usize, show_log_warnings: bool) -> Result<i32> {
    if jobs.is_empty() {
        return Ok(0);
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let session = format!("slurm-logs-{stamp}-{}", std::process::id());
    let mut args = vec![
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        session.clone(),
    ];
    // A detached tmux session otherwise starts at tmux's small fallback size
    // (commonly 80x24). Repeated splits then fail before the client attaches,
    // even when the real terminal has ample room.
    if let Ok((columns, rows)) = crossterm::terminal::size() {
        args.extend([
            "-x".into(),
            columns.max(20).to_string(),
            "-y".into(),
            rows.max(5).to_string(),
        ]);
    }
    // Keep the first pane alive while its options and identity are installed.
    // A newly submitted job may not be visible to scontrol yet; starting the
    // follower first lets that transient failure remove the entire session.
    args.extend(["sh".into(), "-c".into(), "while :; do sleep 60; done".into()]);
    let first = tmux(args)?;
    if !first.status.success() {
        bail!("tmux new-session failed");
    }
    let first_pane = String::from_utf8_lossy(
        &tmux(["display-message", "-p", "-t", &session, "#{pane_id}"])?.stdout,
    )
    .trim()
    .to_string();
    label(&first_pane, &jobs[0])?;
    setup(config, &session)?;
    let mut first_watcher = vec!["respawn-pane".into(), "-k".into(), "-t".into(), first_pane];
    first_watcher.extend(watcher(config, &jobs[0], lines, show_log_warnings));
    let started = tmux(first_watcher)?;
    if !started.status.success() {
        let _ = tmux(["kill-session", "-t", &session]);
        return Err(first_watcher_error(&started.stderr));
    }
    for job in &jobs[1..] {
        let args = split_watcher_args(config, &session, job, lines, show_log_warnings);
        let out = tmux(args)?;
        if !out.status.success() {
            let opened = panes(&session).map_or(1, |panes| panes.len());
            let _ = tmux(["kill-session", "-t", &session]);
            return Err(split_watcher_error(&out.stderr, opened, jobs.len()));
        }
        label(String::from_utf8_lossy(&out.stdout).trim(), job)?;
        // Redistribute after every split. Without this, tmux keeps halving the
        // same pane and reaches its minimum height after only a few jobs.
        tmux(["select-layout", "-t", &session, "tiled"])?;
    }
    tmux(["select-layout", "-t", &session, "tiled"])?;
    let action = if env::var_os("TMUX").is_some() {
        "switch-client"
    } else {
        "attach-session"
    };
    Ok(Command::new("tmux")
        .args([action, "-t", &session])
        .status()?
        .code()
        .unwrap_or(1))
}

fn split_watcher_args(
    config: &Config,
    session: &str,
    job: &Job,
    lines: usize,
    show_log_warnings: bool,
) -> Vec<String> {
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
    args
}

fn first_watcher_error(stderr: &[u8]) -> anyhow::Error {
    let reason = String::from_utf8_lossy(stderr).trim().to_string();
    anyhow::anyhow!(
        "could not start the first log follower: {}",
        if reason.is_empty() {
            "tmux respawn failed"
        } else {
            &reason
        }
    )
}

fn split_watcher_error(stderr: &[u8], opened: usize, total: usize) -> anyhow::Error {
    let reason = String::from_utf8_lossy(stderr).trim().to_string();
    anyhow::anyhow!(
        "could not open all selected panels (opened {} of {}): {}",
        opened,
        total,
        if reason.is_empty() {
            "tmux split failed"
        } else {
            &reason
        }
    )
}
fn setup(config: &Config, session: &str) -> Result<()> {
    for args in [
        vec!["set-option", "-t", session, "mouse", "on"],
        vec!["set-option", "-t", session, "history-limit", "50000"],
        vec!["set-option", "-w", "-t", session, "remain-on-exit", "on"],
        vec!["set-option", "-t", session, "bell-action", "any"],
        vec!["set-option", "-t", session, "visual-bell", "off"],
        // Replace tmux's generic `[session] 0:binary*` status with the focused
        // Slurm pane's identity. Pane options make this update locally and
        // immediately on focus without a scheduler query or a shell hook.
        vec!["set-option", "-t", session, "status-left", ""],
        vec!["set-option", "-t", session, "status-right", ""],
        vec!["set-option", "-t", session, "status-justify", "centre"],
        vec![
            "set-option",
            "-t",
            session,
            "status-style",
            "fg=colour0,bg=colour2",
        ],
        vec!["set-option", "-t", session, "window-status-format", ""],
        vec![
            "set-option",
            "-t",
            session,
            "window-status-current-format",
            persistent_job_status_format(),
        ],
        vec![
            "set-option",
            "-t",
            session,
            "window-status-current-style",
            "fg=colour0,bg=colour2",
        ],
    ] {
        tmux(args)?;
    }
    tmux(["set-option", "-s", "set-clipboard", "on"])?;
    for table in ["copy-mode", "copy-mode-vi"] {
        for (key, command) in [
            ("MouseDragEnd1Pane", "send-keys -X stop-selection"),
            ("MouseDown3Pane", "send-keys -X copy-selection-and-cancel"),
            (
                "MouseUp3Pane",
                "display-message -d 1500 'Copied to clipboard'",
            ),
            ("MouseUp1Pane", "send-keys -X cancel"),
        ] {
            tmux(["bind-key", "-T", table, key, command])?;
        }
    }
    tmux([
        "bind-key",
        "-T",
        "root",
        "MouseUp3Pane",
        "display-message -d 1500 'Copied to clipboard'",
    ])?;
    let popup = format!(
        "SLURM_LOG_POPUP=1 {} {} pick-add {}",
        shell_words::quote(&config.executable.display().to_string()),
        config
            .child_args()
            .into_iter()
            .map(|v| shell_words::quote(&v).into_owned())
            .collect::<Vec<_>>()
            .join(" "),
        session
    );
    for key in ["j", "a"] {
        tmux([
            "bind-key",
            key,
            "display-popup",
            "-E",
            "-w",
            "80%",
            "-h",
            "45%",
            &popup,
            "\\;",
            "refresh-client",
        ])?;
    }
    let details = format!(
        "{} {} toggle-details '#{{pane_id}}'",
        shell_words::quote(&config.executable.display().to_string()),
        config
            .child_args()
            .into_iter()
            .map(|v| shell_words::quote(&v).into_owned())
            .collect::<Vec<_>>()
            .join(" "),
    );
    tmux(["bind-key", "i", "run-shell", &details])?;
    let close_pane = format!(
        "{} {} close-pane {} '#{{pane_id}}'",
        shell_words::quote(&config.executable.display().to_string()),
        config
            .child_args()
            .into_iter()
            .map(|value| shell_words::quote(&value).into_owned())
            .collect::<Vec<_>>()
            .join(" "),
        shell_words::quote(session)
    );
    // tmux's default `x` asks for confirmation in the status line, which is
    // easy to miss under the persistent job identity. Use an explicit action:
    // close details/extra logs immediately and protect the final log pane.
    tmux(["bind-key", "x", "run-shell", &close_pane])?;
    let toggle = format!(
        "{} {} toggle-auto {}",
        shell_words::quote(&config.executable.display().to_string()),
        config
            .child_args()
            .into_iter()
            .map(|v| shell_words::quote(&v).into_owned())
            .collect::<Vec<_>>()
            .join(" "),
        session
    );
    tmux(["bind-key", "A", "run-shell", &toggle])?;
    let single = format!(
        "{} single-pane {}",
        shell_words::quote(&config.executable.display().to_string()),
        shell_words::quote(session)
    );
    let close = format!("kill-session -t {session}");
    let confirm = format!(
        "confirm-before -p 'Close the entire slurm-log workspace? (y/n)' '{}'",
        close
    );
    // `q` is the single workspace-close command. Remove the legacy uppercase
    // binding as tmux key tables outlive individual slurm-log sessions.
    tmux(["unbind-key", "-q", "Q"])?;
    tmux(["bind-key", "q", "if-shell", &single, &close, &confirm])?;
    let ledger = crate::state::Ledger::load(&config.state_path)?;
    tmux([
        "set-option",
        "-t",
        session,
        "@slurm_log_auto_add",
        if ledger.auto_add_default { "on" } else { "off" },
    ])?;
    if ledger.auto_add_default {
        start_monitor(config, session)?;
    }
    Ok(())
}
