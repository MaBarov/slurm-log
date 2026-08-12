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
    let (path, resolved_name) = slurm::terminal_path(config, &job.cluster, &job.id)?;
    if pane && let Ok(pane_id) = env::var("TMUX_PANE") {
        crate::tmux::set_pane_job_name(&pane_id, &resolved_name);
    }
    crate::state::Ledger::mark_opened(&config.state_path, job)?;
    let Some(path) = path else {
        return run_interactive_monitor(config, job, pane);
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
    let mut absent = 0;
    loop {
        if interrupted.load(Ordering::Relaxed) {
            let _ = child.kill();
            if pane && let Ok(pane_id) = env::var("TMUX_PANE") {
                crate::tmux::close_details_for_parent(&pane_id);
                let _ = Command::new("tmux")
                    .args(["kill-pane", "-t", &pane_id])
                    .status();
            }
            return Ok(130);
        }
        if let Some(status) = child.try_wait()? {
            let _ = display.join();
            let code = status.code().unwrap_or(-15);
            if pane {
                let details = slurm::final_details(config, job);
                let final_state = &details.state;
                let still_active = slurm::queued(config, &job.cluster)
                    .is_ok_and(|jobs| jobs.iter().any(|item| item.id == job.id && item.active()));
                let message = if [
                    "FAILED",
                    "TIMEOUT",
                    "OUT_OF_MEMORY",
                    "NODE_FAIL",
                    "CANCELLED",
                ]
                .iter()
                .any(|state| final_state.starts_with(state))
                {
                    let insight = details.insight();
                    format!(
                        "Job {} failed: {}{}",
                        job.id,
                        final_state,
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
                };
                alert(&message);
                println!(
                    "\n[slurm-log] Follower stopped with status {code}. Press Enter to close this pane."
                );
                wait_for_enter();
                if let Ok(pane_id) = env::var("TMUX_PANE") {
                    crate::tmux::close_details_for_parent(&pane_id);
                    let _ = Command::new("tmux")
                        .args(["kill-pane", "-t", &pane_id])
                        .status();
                }
            }
            return Ok(code);
        }
        thread::sleep(Duration::from_secs(15));
        match slurm::queued(config, &job.cluster) {
            Ok(jobs) => {
                let current = jobs.iter().find(|item| item.id == job.id);
                if job.pending() && current.is_some_and(Job::running) {
                    alert(&format!("Job {} started", job.id));
                }
                absent = if current.is_none() { absent + 1 } else { 0 };
                if absent >= 2 {
                    let _ = child.kill();
                }
            }
            Err(_) => absent = 0,
        }
    }
}

fn run_interactive_monitor(config: &Config, job: &Job, pane: bool) -> Result<i32> {
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
            match slurm::all_jobs(config, &job.cluster, "all", false) {
                Ok((jobs, _, _)) => {
                    if let Some(found) = jobs.into_iter().find(|item| item.id == job.id) {
                        current = found;
                        missing = 0;
                    } else {
                        missing = missing.saturating_add(1);
                    }
                }
                Err(_) => missing = 0,
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
                crate::state::Ledger::suppress(&config.state_path, &final_job)?;
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
            crate::state::Ledger::suppress(&config.state_path, &current)?;
            close_monitor_pane(pane);
            return Ok(0);
        }
        if !raw {
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn interactive_frame(job: &Job, ended: bool) -> String {
    let name = if job.name.is_empty() {
        "interactive"
    } else {
        &job.name
    };
    let state = if job.state.is_empty() {
        "ACTIVE"
    } else {
        &job.state
    };
    let elapsed = if job.elapsed.is_empty() {
        "—"
    } else {
        &job.elapsed
    };
    let place = [job.partition.as_str(), job.reason.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty() && *part != "None")
        .collect::<Vec<_>>()
        .join(" · ");
    let prompt = if ended {
        "The allocation has ended. Press Enter to close this pane."
    } else {
        "Ctrl-b i details · Enter closes this monitor (the allocation keeps running)"
    };
    format!(
        "INTERACTIVE ALLOCATION  {}:{}  {}\r\n{}  ·  elapsed {}\r\nPLACE  {}\r\n\r\nSlurm created no stdout log for this interactive allocation (BatchFlag=0).\r\nOutput remains in the terminal or agent session that launched it; another PTY cannot be mirrored here.\r\n\r\n{}\r\n",
        job.cluster,
        job.id,
        name,
        state,
        elapsed,
        if place.is_empty() { "—" } else { &place },
        prompt
    )
}

fn render_monitor(frame: &str, previous: &mut String) -> io::Result<()> {
    if frame == previous {
        return Ok(());
    }
    print!("\x1b[3J\x1b[2J\x1b[H{frame}");
    io::stdout().flush()?;
    previous.clear();
    previous.push_str(frame);
    Ok(())
}

fn close_monitor_pane(pane: bool) {
    if pane && let Ok(pane_id) = env::var("TMUX_PANE") {
        crate::tmux::close_details_for_parent(&pane_id);
        let _ = Command::new("tmux")
            .args(["kill-pane", "-t", &pane_id])
            .status();
    }
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
        let mut input = BufReader::new(tty);
        let mut byte = [0_u8; 1];
        while input.read_exact(&mut byte).is_ok() {
            if enter_byte(byte[0]) {
                return;
            }
        }
    } else {
        let _ = io::stdin().read_line(&mut String::new());
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
mod tests {
    use super::*;
    #[test]
    fn warning_filter_does_not_hide_exceptions() {
        let input = b"FutureWarning: old api\n  warnings.warn(x)\nTraceback (most recent call last):\nValueError: boom\n";
        let mut output = Vec::new();
        filter_log(&input[..], false, false, &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("ValueError: boom"));
        assert!(!text.contains("FutureWarning:"));
        assert!(!text.contains("warnings.warn"));
    }

    #[test]
    fn warning_toggle_shows_warning_records() {
        let input = b"FutureWarning: old api\nValueError: boom\n";
        let mut output = Vec::new();
        filter_log(&input[..], true, false, &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("FutureWarning:"));
        assert!(text.contains("ValueError: boom"));
    }

    #[test]
    fn terminal_output_uses_crlf_to_avoid_staircase_logs() {
        let mut output = Vec::new();
        filter_log(&b"first\nsecond\n"[..], true, true, &mut output).unwrap();
        assert_eq!(output, b"first\r\nsecond\r\n");
    }

    #[test]
    fn enter_accepts_canonical_and_raw_terminal_endings() {
        assert!(enter_byte(b'\n'));
        assert!(enter_byte(b'\r'));
        assert!(!enter_byte(b' '));
    }

    #[test]
    fn close_prompt_accepts_only_enter_key_events() {
        assert!(close_key(KeyCode::Enter));
        assert!(!close_key(KeyCode::Char(' ')));
        assert!(!close_key(KeyCode::Esc));
    }

    #[test]
    fn long_lived_followers_never_consume_query_mux_channels() {
        assert!(FOLLOWER_SSH_OPTIONS.contains(&"ControlMaster=no"));
        assert!(FOLLOWER_SSH_OPTIONS.contains(&"ControlPath=none"));
        assert!(!FOLLOWER_SSH_OPTIONS.contains(&"ControlMaster=auto"));
    }

    #[test]
    fn interactive_monitor_explains_missing_log_and_safe_close() {
        let frame = interactive_frame(
            &Job {
                cluster: "cispa".into(),
                id: "42".into(),
                name: "shell".into(),
                state: "RUNNING".into(),
                elapsed: "00:12".into(),
                partition: "gpu".into(),
                reason: "node-a".into(),
                ..Job::default()
            },
            false,
        );
        assert!(frame.contains("INTERACTIVE ALLOCATION  cispa:42  shell"));
        assert!(frame.contains("BatchFlag=0"));
        assert!(frame.contains("another PTY cannot be mirrored"));
        assert!(frame.contains("allocation keeps running"));
        assert!(frame.contains("Ctrl-b i details"));
        assert!(!frame.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn ended_interactive_monitor_waits_for_enter() {
        let frame = interactive_frame(&Job::default(), true);
        assert!(frame.contains("allocation has ended"));
        assert!(frame.contains("Press Enter"));
    }
}
