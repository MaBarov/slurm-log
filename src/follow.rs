use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal,
};
use std::{
    env,
    io::{self, BufRead, BufReader, IsTerminal, Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{command::shell_quote, config::Config, model::Job, slurm};

// Log followers are long-lived SSH sessions. Sharing the scheduler-query
// master would consume one mux channel per pane and eventually hit sshd's
// MaxSessions limit, so followers deliberately use dedicated connections.
const FOLLOWER_SSH_OPTIONS: &[&str] = &[
    "-o",
    "BatchMode=yes",
    "-o",
    "ControlMaster=no",
    "-o",
    "ControlPath=none",
    "-o",
    "ServerAliveInterval=30",
    "-o",
    "ServerAliveCountMax=3",
];
const FOLLOWER_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const FOLLOWER_SCHEDULER_POLL_INTERVAL: Duration = Duration::from_secs(15);

fn alert(message: &str) {
    print!("\x07");
    let _ = io::stdout().flush();
    if let Ok(pane) = env::var("TMUX_PANE") {
        let _ = Command::new("tmux")
            .args(["display-message", "-d", "5000", "-t", &pane, message])
            .status();
    }
}

pub fn run(
    config: &Config,
    job: &Job,
    lines: usize,
    pane: bool,
    show_log_warnings: bool,
) -> Result<i32> {
    let (path, resolved_name) = match slurm::terminal_path(config, &job.cluster, &job.id) {
        Ok(found) => found,
        // sbatch can return before a new pending job is registered in
        // scontrol. Keep monitoring the queue instead of killing its pane.
        Err(_) if job.pending() => {
            return run_interactive_monitor(config, job, lines, pane, show_log_warnings);
        }
        Err(error) => return Err(error),
    };
    if pane && let Ok(pane_id) = env::var("TMUX_PANE") {
        crate::tmux::set_pane_job_name(&pane_id, &resolved_name);
    }
    crate::state::Ledger::mark_opened(&config.state_path, job)?;
    let Some(path) = path else {
        return run_interactive_monitor(config, job, lines, pane, show_log_warnings);
    };
    print!("\x1b[3J\x1b[2J\x1b[H");
    io::stdout().flush()?;
    let mut command;
    let cluster = config.cluster(&job.cluster)?;
    if !cluster.remote() {
        if !Path::new(&path).exists() {
            println!(
                "[{}] job {} is pending; waiting for its log file…",
                job.cluster, job.id
            );
        }
        while !Path::new(&path).exists() {
            thread::sleep(Duration::from_secs(1));
        }
        command = Command::new("tail");
        command.args(["-n", &lines.to_string(), "-F", "--", &path]);
    } else {
        let qp = shell_quote(&path);
        let qj = shell_quote(&job.id);
        let qc = shell_quote(&job.cluster);
        let remote = format!(
            "if [ ! -e {qp} ]; then printf '[%s] job %s is pending; waiting for its log file…\\n' {qc} {qj}; fi; while [ ! -e {qp} ]; do sleep 1; done; printf '\\033[3J\\033[2J\\033[H[%s] %s\\n' {qc} {qp}; exec tail -n {lines} -F -- {qp}"
        );
        command = Command::new("ssh");
        // `tail` is non-interactive. Allocating an SSH TTY lets a forced SSH
        // shutdown strand the tmux pane in raw mode, where Enter is `\r` and
        // `read_line` waits forever for `\n`.
        command
            .args(FOLLOWER_SSH_OPTIONS)
            .args([&cluster.ssh_host, &remote]);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("start log follower")?;
    let output = child.stdout.take().expect("piped follower stdout");
    let display = thread::spawn(move || display_log(output, show_log_warnings));
    let interrupted = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, interrupted.clone())?;
    supervise_follower(
        config,
        job,
        pane,
        child,
        display,
        interrupted,
        FOLLOWER_SCHEDULER_POLL_INTERVAL,
        || slurm::queued(config, &job.cluster),
    )
}

#[allow(clippy::too_many_arguments)]
fn supervise_follower(
    config: &Config,
    job: &Job,
    pane: bool,
    mut child: std::process::Child,
    display: thread::JoinHandle<()>,
    interrupted: Arc<AtomicBool>,
    scheduler_interval: Duration,
    mut queued: impl FnMut() -> Result<Vec<Job>>,
) -> Result<i32> {
    let mut absent = 0;
    let mut next_scheduler_poll = Instant::now() + scheduler_interval;
    loop {
        if interrupted.load(Ordering::Relaxed) {
            let _ = child.kill();
            if pane && let Ok(pane_id) = env::var("TMUX_PANE") {
                crate::tmux::close_details_for_parent(&pane_id);
                defer_pane_close(&pane_id);
            }
            return Ok(130);
        }
        if let Some(status) = child.try_wait()? {
            let _ = display.join();
            let code = status.code().unwrap_or(-15);
            if pane {
                let details = slurm::final_details(config, job);
                let still_active = slurm::queued(config, &job.cluster)
                    .is_ok_and(|jobs| jobs.iter().any(|item| item.id == job.id && item.active()));
                let message = completion_message(job, &details, still_active, code);
                alert(&message);
                println!(
                    "\n[slurm-log] Follower stopped with status {code}. Press Enter to close this pane."
                );
                wait_for_enter();
                if let Ok(pane_id) = env::var("TMUX_PANE") {
                    crate::tmux::close_details_for_parent(&pane_id);
                    defer_pane_close(&pane_id);
                }
            }
            return Ok(code);
        }
        let now = Instant::now();
        if now < next_scheduler_poll {
            thread::sleep(FOLLOWER_EXIT_POLL_INTERVAL.min(next_scheduler_poll - now));
            continue;
        }
        next_scheduler_poll = now + scheduler_interval;
        match queued() {
            Ok(jobs) => {
                let (next_absent, started, stop) = observe_queue(job, &jobs, absent);
                absent = next_absent;
                if started {
                    alert(&format!("Job {} started", job.id));
                }
                if stop {
                    let _ = child.kill();
                }
            }
            Err(_) => absent = 0,
        }
    }
}

fn completion_message(job: &Job, details: &Job, still_active: bool, code: i32) -> String {
    if details.failed() {
        let insight = details.insight();
        format!(
            "Job {} failed: {}{}",
            job.id,
            details.state,
            if insight.is_empty() {
                String::new()
            } else {
                format!(" ({insight})")
            }
        )
    } else if still_active {
        format!("Job {} log follower stopped (status {code})", job.id)
    } else {
        format!("Job {} finished", job.id)
    }
}

fn observe_queue(job: &Job, jobs: &[Job], absent: u8) -> (u8, bool, bool) {
    let current = jobs.iter().find(|item| item.id == job.id);
    let started = job.pending() && current.is_some_and(Job::running);
    let absent = if current.is_none() {
        absent.saturating_add(1)
    } else {
        0
    };
    (absent, started, absent >= 2)
}

fn run_interactive_monitor(
    config: &Config,
    job: &Job,
    lines: usize,
    pane: bool,
    show_log_warnings: bool,
) -> Result<i32> {
    struct RawModeGuard(bool);
    impl Drop for RawModeGuard {
        fn drop(&mut self) {
            if self.0 {
                let _ = terminal::disable_raw_mode();
            }
        }
    }

    let raw = io::stdin().is_terminal() && terminal::enable_raw_mode().is_ok();
    let _raw_guard = RawModeGuard(raw);
    let mut current = job.clone();
    let mut rendered = String::new();
    let mut missing = 0_u8;
    let mut refresh_at = Instant::now();
    loop {
        if Instant::now() >= refresh_at {
            refresh_at = Instant::now() + Duration::from_secs(3);
            apply_monitor_snapshot(
                &mut current,
                &job.id,
                &mut missing,
                slurm::all_jobs(config, &job.cluster, "all", false),
            );

            // A freshly returned sbatch ID may briefly be absent from
            // scontrol. As soon as Slurm publishes its stdout path, replace
            // this lightweight monitor with the normal log follower.
            if current.active() && !current.interactive {
                match slurm::terminal_path(config, &current.cluster, &current.id) {
                    Ok((Some(_), _)) => {
                        return run(config, &current, lines, pane, show_log_warnings);
                    }
                    Ok((None, _)) => current.interactive = true,
                    Err(_) => {}
                }
            }

            if missing >= 2 || !current.active() {
                let final_job = slurm::final_details(config, &current);
                alert(&if final_job.failed() {
                    format!("Job {} failed: {}", job.id, final_job.state)
                } else {
                    format!("Job {} finished", job.id)
                });
                render_monitor(&interactive_frame(&final_job, true), &mut rendered)?;
                wait_for_enter();
                crate::state::Ledger::mark_opened(&config.state_path, &final_job)?;
                close_monitor_pane(pane);
                return Ok(0);
            }

            render_monitor(&interactive_frame(&current, false), &mut rendered)?;
        }

        if raw
            && event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && close_key(key.code)
        {
            crate::state::Ledger::mark_opened(&config.state_path, &current)?;
            close_monitor_pane(pane);
            return Ok(0);
        }
        if !raw {
            thread::sleep(Duration::from_millis(100));
        }
    }
}

include!("follow/monitor_frame.rs");

fn close_monitor_pane(pane: bool) {
    if pane && let Ok(pane_id) = env::var("TMUX_PANE") {
        crate::tmux::close_details_for_parent(&pane_id);
        defer_pane_close(&pane_id);
    }
}

fn defer_pane_close(pane: &str) {
    // Killing the pane synchronously terminates this process before normal
    // destructors, buffered output, and coverage profiles can be flushed.
    // Let tmux perform the visual close immediately after the follower exits.
    let command = format!("sleep 0.05; tmux kill-pane -t {}", shell_quote(pane));
    let _ = Command::new("tmux")
        .args(["run-shell", "-b", &command])
        .status();
}

fn wait_for_enter() {
    // Crossterm normalizes Enter across tmux, canonical, application-cursor,
    // and raw terminal modes. Always restore the prior mode before tmux kills
    // the pane so a failed kill cannot leave an unusable terminal behind.
    if io::stdin().is_terminal() && terminal::enable_raw_mode().is_ok() {
        struct RawModeGuard;
        impl Drop for RawModeGuard {
            fn drop(&mut self) {
                let _ = terminal::disable_raw_mode();
            }
        }
        let _guard = RawModeGuard;
        while let Ok(input) = event::read() {
            if let Event::Key(key) = input
                && key.kind == KeyEventKind::Press
                && close_key(key.code)
            {
                return;
            }
        }
    }
    // Open the controlling terminal afresh: the follower child deliberately
    // has no stdin. This is the fallback for unusual non-Crossterm terminals.
    if let Ok(tty) = std::fs::File::open("/dev/tty") {
        read_until_enter(tty);
    } else {
        let _ = io::stdin().read_line(&mut String::new());
    }
}

fn read_until_enter(reader: impl Read) {
    let mut input = BufReader::new(reader);
    let mut byte = [0_u8; 1];
    while input.read_exact(&mut byte).is_ok() {
        if enter_byte(byte[0]) {
            return;
        }
    }
}

fn apply_monitor_snapshot(
    current: &mut Job,
    job_id: &str,
    missing: &mut u8,
    snapshot: Result<(Vec<Job>, crate::state::Ledger, Vec<String>)>,
) {
    match snapshot {
        Ok((jobs, _, _)) => {
            if let Some(found) = jobs.into_iter().find(|item| item.id == job_id) {
                *current = found;
                *missing = 0;
            } else {
                *missing = missing.saturating_add(1);
            }
        }
        Err(_) => *missing = 0,
    }
}

fn close_key(code: KeyCode) -> bool {
    matches!(code, KeyCode::Enter)
}

fn enter_byte(byte: u8) -> bool {
    matches!(byte, b'\n' | b'\r')
}

fn display_log(reader: impl Read, show_warnings: bool) {
    let stdout = io::stdout();
    let terminal = stdout.is_terminal();
    let mut output = stdout.lock();
    let _ = filter_log(reader, show_warnings, terminal, &mut output);
}

fn filter_log(
    reader: impl Read,
    show_warnings: bool,
    terminal: bool,
    output: &mut impl Write,
) -> io::Result<()> {
    let warning = [
        "FutureWarning:",
        "UserWarning:",
        "DeprecationWarning:",
        "RuntimeWarning:",
        "PendingDeprecationWarning:",
        "ResourceWarning:",
        "Warning:",
    ];
    let mut summary = false;
    let mut continuation = false;
    for line in BufReader::new(reader).lines().map_while(Result::ok) {
        let plain = line.trim_end_matches('\r');
        if !show_warnings {
            if plain.starts_with("===") && plain.contains("warnings summary") {
                summary = true;
                continue;
            }
            if summary {
                if plain.starts_with("-- Docs:") {
                    summary = false;
                }
                continue;
            }
            if warning.iter().any(|marker| plain.contains(marker))
                || plain.starts_with("There are modules in ") && plain.contains("kept in float32")
            {
                continuation = true;
                continue;
            }
            if continuation
                && (plain.starts_with(char::is_whitespace) || plain.contains("warnings.warn("))
            {
                continue;
            }
            continuation = false;
        }
        write!(output, "{plain}{}", if terminal { "\r\n" } else { "\n" })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
