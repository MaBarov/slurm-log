use anyhow::{Result, bail};
use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::OsStr,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    config::Config,
    model::{Job, Pane},
};

fn tmux<I, S>(args: I) -> Result<std::process::Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Ok(Command::new("tmux").args(args).output()?)
}

fn watcher(config: &Config, job: &Job, lines: usize, show_log_warnings: bool) -> Vec<String> {
    let mut values = vec![config.executable.display().to_string()];
    values.extend(config.child_args());
    values.extend([
        "--pane-follow".into(),
        "--lines".into(),
        lines.to_string(),
        "--initial-state".into(),
        job.state.clone(),
        job.cluster.clone(),
        job.id.clone(),
    ]);
    if show_log_warnings {
        values.insert(values.len() - 2, "--show-log-warnings".into());
    }
    values
}

fn detail_watcher(config: &Config, cluster: &str, job_id: &str) -> Vec<String> {
    let mut values = vec![
        "env".into(),
        "SLURM_LOG_DETAILS_COMPACT=1".into(),
        "SLURM_LOG_DETAILS_PANE=1".into(),
        config.executable.display().to_string(),
    ];
    values.extend(config.child_args());
    values.extend([
        "details".into(),
        job_id.into(),
        "--cluster".into(),
        cluster.into(),
    ]);
    values
}

pub fn panes(session: &str) -> Result<Vec<Pane>> {
    let out = tmux([
        "list-panes",
        "-t",
        session,
        "-F",
        "#{pane_id}|#{@slurm_log_cluster}|#{@slurm_log_job_id}",
    ])?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let values: Vec<_> = line.splitn(3, '|').collect();
            (values.len() == 3 && !values[1].is_empty() && !values[2].is_empty()).then(|| Pane {
                id: values[0].into(),
                cluster: values[1].into(),
                job_id: values[2].into(),
            })
        })
        .collect())
}

fn label(pane: &str, job: &Job) -> Result<()> {
    tmux(label_args(pane, job))?;
    Ok(())
}

fn label_args(pane: &str, job: &Job) -> Vec<String> {
    let mut args = vec![
        "set-option".into(),
        "-p".into(),
        "-t".into(),
        pane.into(),
        "@slurm_log_cluster".into(),
        job.cluster.clone(),
        ";".into(),
        "set-option".into(),
        "-p".into(),
        "-t".into(),
        pane.into(),
        "@slurm_log_job_id".into(),
        job.id.clone(),
        ";".into(),
    ];
    // A direct CLUSTER JOB_ID open initially has no name. Its follower resolves
    // the name using the scontrol lookup it already needs. Do not race with and
    // overwrite that better value using a fallback from the parent process.
    if !job.name.trim().is_empty() {
        args.extend([
            "set-option".into(),
            "-p".into(),
            "-t".into(),
            pane.into(),
            "@slurm_log_job_name".into(),
            pane_job_name(&job.name),
            ";".into(),
        ]);
    }
    args.extend([
        "select-pane".into(),
        "-t".into(),
        pane.into(),
        "-T".into(),
        format!("{}:{}", job.cluster, job.id),
    ]);
    args
}

fn pane_job_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .filter(|character| !character.is_control())
        .take(100)
        .collect();
    let safe = safe.trim();
    if safe.is_empty() {
        "Slurm job".into()
    } else {
        safe.into()
    }
}

pub fn set_pane_job_name(pane: &str, name: &str) {
    let _ = tmux([
        "set-option",
        "-p",
        "-t",
        pane,
        "@slurm_log_job_name",
        &pane_job_name(name),
    ]);
}

fn persistent_job_status_format() -> &'static str {
    "#{?@slurm_log_job_id,#{?@slurm_log_job_name,#{@slurm_log_job_name},Slurm job} · job #{@slurm_log_job_id},slurm-log}"
}

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
    args.extend(watcher(config, &jobs[0], lines, show_log_warnings));
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
    for job in &jobs[1..] {
        let mut args = vec![
            "split-window".into(),
            "-d".into(),
            "-P".into(),
            "-F".into(),
            "#{pane_id}".into(),
            "-t".into(),
            session.clone(),
        ];
        args.extend(watcher(config, job, lines, show_log_warnings));
        let out = tmux(args)?;
        if !out.status.success() {
            let reason = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let opened = panes(&session).map_or(1, |panes| panes.len());
            let _ = tmux(["kill-session", "-t", &session]);
            bail!(
                "could not open all selected panels (opened {} of {}): {}",
                opened,
                jobs.len(),
                if reason.is_empty() {
                    "tmux split failed"
                } else {
                    &reason
                }
            );
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
        "-p".into(),
        "38".into(),
        "-P".into(),
        "-F".into(),
        "#{pane_id}".into(),
        "-t".into(),
        focused.into(),
    ];
    args.extend(detail_watcher(config, &cluster, &job_id));
    let out = tmux(args)?;
    if !out.status.success() {
        bail!("could not open the details pane");
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

pub fn monitor(config: &Config, session: &str, lines: usize) -> Result<()> {
    let (initial, _, _) = crate::slurm::all_jobs(config, "both", "all", false)?;
    let mut observed: HashMap<_, _> = initial
        .into_iter()
        .map(|job| ((job.cluster.clone(), job.id.clone()), job))
        .collect();
    let mut tracked_pending: HashMap<_, _> = observed
        .iter()
        .filter(|(_, job)| job.pending())
        .map(|(key, job)| (key.clone(), job.clone()))
        .collect();
    let mut missing_pending: HashMap<(String, String), u8> = HashMap::new();
    loop {
        if !auto_enabled(session)? {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(15));
        let (jobs, _, warnings) = crate::slurm::all_jobs(config, "both", "all", false)?;
        let current: HashMap<_, _> = jobs
            .iter()
            .cloned()
            .map(|j| ((j.cluster.clone(), j.id.clone()), j))
            .collect();
        let additions = monitor_additions(&observed, &current);
        if !additions.is_empty() {
            let mut desired: Vec<Job> = panes(session)?
                .into_iter()
                .map(|p| Job {
                    cluster: p.cluster,
                    id: p.job_id,
                    ..Job::default()
                })
                .collect();
            desired.extend(additions);
            reconcile(config, session, &desired, lines, false)?;
        }
        for (key, before) in &observed {
            if before.pending() && current.get(key).is_some_and(Job::running) {
                tmux([
                    "display-message",
                    "-d",
                    "5000",
                    "-t",
                    session,
                    &format!("Job {} started", before.id),
                ])?;
            }
            if before.pending() && current.get(key).is_some_and(Job::failed) {
                tmux([
                    "display-message",
                    "-d",
                    "5000",
                    "-t",
                    session,
                    &format!("Job {} failed before start", before.id),
                ])?;
                close_job_pane(session, &before.cluster, &before.id)?;
                tracked_pending.remove(key);
            }
        }
        for (key, job) in &current {
            if job.pending() {
                tracked_pending.insert(key.clone(), job.clone());
            } else if job.running() {
                tracked_pending.remove(key);
            }
            missing_pending.remove(key);
        }
        for (key, pending) in tracked_pending.clone() {
            if current.contains_key(&key) {
                continue;
            }
            if warnings
                .iter()
                .any(|warning| warning.to_lowercase().contains(&pending.cluster))
            {
                continue;
            }
            let count = missing_pending.entry(key.clone()).or_default();
            *count += 1;
            if *count >= 2 {
                tmux([
                    "display-message",
                    "-d",
                    "5000",
                    "-t",
                    session,
                    &format!("Job {} left the queue before start", pending.id),
                ])?;
                close_job_pane(session, &pending.cluster, &pending.id)?;
                tracked_pending.remove(&key);
                missing_pending.remove(&key);
            }
        }
        observed = current;
    }
}

fn monitor_additions(
    observed: &HashMap<(String, String), Job>,
    current: &HashMap<(String, String), Job>,
) -> Vec<Job> {
    current
        .iter()
        .filter(|(key, job)| {
            job.active()
                && !job.blocked_category()
                && match observed.get(*key) {
                    None => true,
                    Some(before) => before.pending() && job.running(),
                }
        })
        .map(|(_, job)| job.clone())
        .collect()
}

fn close_job_pane(session: &str, cluster: &str, job_id: &str) -> Result<()> {
    for pane in panes(session)? {
        if pane.cluster == cluster && pane.job_id == job_id {
            tmux(["kill-pane", "-t", &pane.id])?;
        }
    }
    Ok(())
}

fn obsolete_panes<'a>(
    current: &'a [Pane],
    desired: &HashSet<(String, String)>,
) -> (Vec<&'a Pane>, Option<&'a Pane>) {
    let mut obsolete: Vec<_> = current
        .iter()
        .filter(|pane| !desired.contains(&(pane.cluster.clone(), pane.job_id.clone())))
        .collect();
    let anchor = (obsolete.len() == current.len())
        .then(|| obsolete.pop())
        .flatten();
    (obsolete, anchor)
}

#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;
    fn job(id: &str, state: &str) -> Job {
        Job {
            cluster: "cispa".into(),
            id: id.into(),
            state: state.into(),
            ..Job::default()
        }
    }
    fn map(jobs: Vec<Job>) -> HashMap<(String, String), Job> {
        jobs.into_iter()
            .map(|job| ((job.cluster.clone(), job.id.clone()), job))
            .collect()
    }
    #[test]
    fn auto_add_baselines_existing_running_jobs() {
        let observed = map(vec![job("1", "RUNNING"), job("2", "PENDING")]);
        assert!(monitor_additions(&observed, &observed).is_empty());
        let current = map(vec![
            job("1", "RUNNING"),
            job("2", "RUNNING"),
            job("3", "PENDING"),
        ]);
        let additions = monitor_additions(&observed, &current);
        assert_eq!(
            additions
                .iter()
                .map(|job| job.id.as_str())
                .collect::<HashSet<_>>(),
            HashSet::from(["2", "3"])
        );
        let mut interactive = job("4", "RUNNING");
        interactive.interactive = true;
        let current = map(vec![interactive]);
        assert!(monitor_additions(&HashMap::new(), &current).is_empty());
    }

    #[test]
    fn one_or_zero_log_panes_are_safe_to_close_without_confirmation() {
        assert!(!confirmation_needed(0));
        assert!(!confirmation_needed(1));
        assert!(confirmation_needed(2));
    }

    #[test]
    fn pane_labels_are_one_batched_tmux_transaction() {
        let mut named = job("42", "RUNNING");
        named.name = "training-run".into();
        let args = label_args("%7", &named);
        assert_eq!(args.iter().filter(|value| value.as_str() == ";").count(), 3);
        assert!(
            args.windows(2)
                .any(|pair| { pair[0] == "@slurm_log_job_name" && pair[1] == "training-run" })
        );
        assert_eq!(args.last().map(String::as_str), Some("cispa:42"));
        let unresolved = label_args("%8", &job("43", "RUNNING"));
        assert!(
            !unresolved
                .iter()
                .any(|value| value == "@slurm_log_job_name")
        );
    }

    #[test]
    fn pane_job_names_sanitize_metadata() {
        assert_eq!(pane_job_name("  train\u{1b}[2J\nrun  "), "train[2Jrun");
        assert_eq!(pane_job_name("\n\t"), "Slurm job");
    }

    #[test]
    fn persistent_status_names_the_focused_job() {
        let format = persistent_job_status_format();
        assert!(format.contains("#{@slurm_log_job_name}"));
        assert!(format.contains("#{@slurm_log_job_id}"));
        assert!(format.contains("Slurm job"));
        assert!(!format.contains("pane_title"));
        assert!(!format.contains("window_name"));
    }

    #[test]
    fn reconciliation_removes_old_panes_before_additions() {
        let current = vec![
            Pane {
                id: "%1".into(),
                cluster: "cispa".into(),
                job_id: "old".into(),
            },
            Pane {
                id: "%2".into(),
                cluster: "cispa".into(),
                job_id: "keep".into(),
            },
        ];
        let desired = HashSet::from([
            ("cispa".into(), "keep".into()),
            ("cispa".into(), "new".into()),
        ]);
        let (remove_first, anchor) = obsolete_panes(&current, &desired);
        assert_eq!(
            remove_first
                .iter()
                .map(|pane| pane.id.as_str())
                .collect::<Vec<_>>(),
            ["%1"]
        );
        assert!(anchor.is_none());
    }

    #[test]
    fn reconciliation_keeps_one_anchor_for_a_total_replacement() {
        let current = vec![Pane {
            id: "%1".into(),
            cluster: "cispa".into(),
            job_id: "old".into(),
        }];
        let desired = HashSet::from([("cispa".into(), "new".into())]);
        let (remove_first, anchor) = obsolete_panes(&current, &desired);
        assert!(remove_first.is_empty());
        assert_eq!(anchor.map(|pane| pane.id.as_str()), Some("%1"));
    }
}

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
        tmux(["select-layout", "-t", session, "tiled"])?;
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
