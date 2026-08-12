fn pane_option(pane: &str, option: &str) -> Result<String> {
    let out = tmux(["show-options", "-p", "-v", "-t", pane, option])?;
    Ok(if out.status.success() {
        String::from_utf8_lossy(&out.stdout).trim().into()
    } else {
        String::new()
    })
}
fn detail_panes() -> Result<Vec<(String, String)>> {
    let out = tmux([
        "list-panes",
        "-a",
        "-F",
        "#{pane_id}|#{@slurm_log_detail_parent}",
    ])?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (pane, parent) = line.split_once('|')?;
            (!parent.is_empty()).then(|| (pane.into(), parent.into()))
        })
        .collect())
}

pub fn toggle_details(config: &Config, focused: &str) -> Result<()> {
    let parent = pane_option(focused, "@slurm_log_detail_parent")?;
    if !parent.is_empty() {
        tmux(["kill-pane", "-t", focused])?;
        return Ok(());
    }
    if let Some((pane, _)) = detail_panes()?
        .into_iter()
        .find(|(_, parent)| parent == focused)
    {
        tmux(["kill-pane", "-t", &pane])?;
        return Ok(());
    }
    let cluster = pane_option(focused, "@slurm_log_cluster")?;
    let job_id = pane_option(focused, "@slurm_log_job_id")?;
    let job_name = pane_option(focused, "@slurm_log_job_name")?;
    if cluster.is_empty() || job_id.is_empty() {
        bail!("focused pane is not a slurm-log job");
    }
    let mut args = vec![
        "split-window".into(),
        "-v".into(),
        "-l".into(),
        "38%".into(),
        "-P".into(),
        "-F".into(),
        "#{pane_id}".into(),
        "-t".into(),
        focused.into(),
    ];
    args.extend(detail_watcher(config, &cluster, &job_id));
    let out = tmux(args)?;
    if !out.status.success() {
        let reason = String::from_utf8_lossy(&out.stderr);
        let reason = reason.trim();
        bail!(
            "could not open the details pane{}",
            if reason.is_empty() {
                String::new()
            } else {
                format!(": {reason}")
            }
        );
    }
    let pane = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let mut metadata = vec![
        "set-option",
        "-p",
        "-t",
        &pane,
        "@slurm_log_detail_parent",
        focused,
        ";",
        "set-option",
        "-p",
        "-t",
        &pane,
        "@slurm_log_cluster",
        &cluster,
        ";",
        "set-option",
        "-p",
        "-t",
        &pane,
        "@slurm_log_job_id",
        &job_id,
    ];
    if !job_name.is_empty() {
        metadata.extend([
            ";",
            "set-option",
            "-p",
            "-t",
            &pane,
            "@slurm_log_job_name",
            &job_name,
        ]);
    }
    tmux(metadata)?;
    tmux(["select-pane", "-t", &pane, "-T", "job details"])?;
    tmux(["select-pane", "-t", &pane, "-P", "bg=colour235"])?;
    tmux(["select-pane", "-t", &pane])?;
    Ok(())
}

pub fn close_detail_pane(pane: &str) {
    let _ = tmux(["kill-pane", "-t", pane]);
}

/// Close every auxiliary details pane owned by a log pane. This is called
/// before the owner exits so tmux cannot leave an orphaned details window.
pub fn close_details_for_parent(parent: &str) {
    if let Ok(panes) = detail_panes() {
        for (pane, owner) in panes {
            if owner == parent {
                let _ = tmux(["kill-pane", "-t", &pane]);
            }
        }
    }
}

fn close_details_for_session(session: &str) -> Result<()> {
    let out = tmux([
        "list-panes",
        "-t",
        session,
        "-F",
        "#{pane_id}|#{@slurm_log_detail_parent}",
    ])?;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some((pane, parent)) = line.split_once('|')
            && !parent.is_empty()
        {
            tmux(["kill-pane", "-t", pane])?;
        }
    }
    Ok(())
}

pub fn auto_enabled(session: &str) -> Result<bool> {
    let out = tmux(["show-options", "-v", "-t", session, "@slurm_log_auto_add"])?;
    Ok(out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "on")
}

pub fn start_monitor(config: &Config, session: &str) -> Result<()> {
    let mut command = Command::new(&config.executable);
    command
        .args(config.child_args())
        .args(["auto-monitor", session]);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = command.spawn()?;
    tmux([
        "set-option",
        "-t",
        session,
        "@slurm_log_monitor_pid",
        &child.id().to_string(),
    ])?;
    Ok(())
}

pub fn toggle_auto(config: &Config, session: &str) -> Result<()> {
    let enabled = !auto_enabled(session)?;
    tmux([
        "set-option",
        "-t",
        session,
        "@slurm_log_auto_add",
        if enabled { "on" } else { "off" },
    ])?;
    crate::state::Ledger::set_auto_add(&config.state_path, enabled)?;
    if enabled {
        start_monitor(config, session)?;
    }
    tmux([
        "display-message",
        "-d",
        "5000",
        "-t",
        session,
        if enabled {
            "slurm-log auto-add: on"
        } else {
            "slurm-log auto-add: off"
        },
    ])?;
    Ok(())
}
