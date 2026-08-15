use anyhow::{Context, Result, bail};
use std::{
    cell::RefCell,
    fs,
    io::{Read, Result as IoResult, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

mod resolver;

const MAX_COMMAND_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// The remote scheduler invocation never inherits a login-shell PATH.  Sites
/// that install Slurm elsewhere should expose an explicit trusted wrapper in
/// one of these administrator-controlled directories.
const REMOTE_SCHEDULER_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

thread_local! {
    static REQUEST_CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

struct CancellationScope {
    previous: Option<Arc<AtomicBool>>,
}

impl Drop for CancellationScope {
    fn drop(&mut self) {
        REQUEST_CANCEL.with(|current| {
            current.replace(self.previous.take());
        });
    }
}

/// Bind an MCP request cancellation flag to every child command issued by the
/// current blocking worker.
pub fn with_cancellation<T>(cancel: Arc<AtomicBool>, action: impl FnOnce() -> T) -> T {
    let scope = REQUEST_CANCEL.with(|current| CancellationScope {
        previous: current.replace(Some(cancel)),
    });
    let value = action();
    drop(scope);
    value
}

fn request_cancelled() -> bool {
    REQUEST_CANCEL.with(|current| {
        current
            .borrow()
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Acquire))
    })
}

fn command(program: &str) -> Command {
    let mut command = Command::new(resolver::trusted_program(program));
    // A child owns its own process group so deadline/cancellation can tear
    // down shell/SSH descendants that retain stdout or stderr pipes.
    command.process_group(0);
    resolver::scrub_environment(&mut command);
    command
}

fn wait_with_deadline(child: &mut Child, program: &str, deadline: Duration) -> Result<ExitStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("check {program}"))?
        {
            // A shell can exit while a background descendant still owns one
            // of the pipes. Kill that group before joining reader threads.
            terminate_process_group(child);
            return Ok(status);
        }
        if request_cancelled() {
            terminate_process_group(child);
            bail!("{program} was cancelled");
        }
        if started.elapsed() >= deadline {
            terminate_process_group(child);
            bail!(
                "{program} exceeded the {}s command deadline",
                deadline.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn terminate_process_group(child: &mut Child) {
    if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::Kill);
    }
    // Fallback if process-group creation failed before the first poll.
    let _ = child.kill();
    let _ = child.wait();
}

pub fn output(program: &str, args: &[&str]) -> Result<Output> {
    output_with_limit(program, args, MAX_COMMAND_OUTPUT_BYTES)
}

/// Like `output`, but with an explicit deadline. Used for best-effort metadata
/// probes (for example git provenance) that must never stall the MCP worker.
pub fn output_with_timeout(program: &str, args: &[&str], deadline: Duration) -> Result<Output> {
    output_with_limit_and_timeout(program, args, MAX_COMMAND_OUTPUT_BYTES, deadline)
}

fn output_with_limit(program: &str, args: &[&str], limit: usize) -> Result<Output> {
    output_with_limit_and_timeout(program, args, limit, COMMAND_TIMEOUT)
}

fn output_with_limit_and_timeout(
    program: &str,
    args: &[&str],
    limit: usize,
    deadline: Duration,
) -> Result<Output> {
    let mut child = command(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("run {program}"))?;
    let stdout = child.stdout.take().context("capture command stdout")?;
    let stderr = child.stderr.take().context("capture command stderr")?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout, limit));
    let stderr_reader = thread::spawn(move || read_bounded(stderr, limit));
    let status = wait_with_deadline(&mut child, program, deadline);
    let (stdout, stdout_overflow) = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
    let (stderr, stderr_overflow) = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;
    let status = status?;
    if stdout_overflow || stderr_overflow {
        bail!(
            "{program} output exceeded {} MiB safety limit",
            limit / 1024 / 1024
        );
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn read_bounded(mut reader: impl Read, limit: usize) -> IoResult<(Vec<u8>, bool)> {
    let mut stored = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut overflow = false;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(stored.len());
        stored.extend_from_slice(&buffer[..count.min(remaining)]);
        overflow |= count > remaining;
    }
    Ok((stored, overflow))
}

pub fn text(program: &str, args: &[&str]) -> Result<String> {
    let out = output(program, args)?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn text_with_input(
    program: &str,
    args: &[&str],
    input: &[u8],
    directory: Option<&Path>,
) -> Result<String> {
    text_with_input_limit(program, args, input, directory, MAX_COMMAND_OUTPUT_BYTES)
}

fn text_with_input_limit(
    program: &str,
    args: &[&str],
    input: &[u8],
    directory: Option<&Path>,
    limit: usize,
) -> Result<String> {
    let mut command = command(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    let mut child = command.spawn().with_context(|| format!("run {program}"))?;
    let stdout = child.stdout.take().context("capture command stdout")?;
    let stderr = child.stderr.take().context("capture command stderr")?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout, limit));
    let stderr_reader = thread::spawn(move || read_bounded(stderr, limit));
    let mut stdin = child.stdin.take().context("open command stdin")?;
    let input = input.to_vec();
    let input_writer = thread::spawn(move || stdin.write_all(&input));
    let status = wait_with_deadline(&mut child, program, COMMAND_TIMEOUT);
    let (stdout, stdout_overflow) = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
    let (stderr, stderr_overflow) = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;
    input_writer
        .join()
        .map_err(|_| anyhow::anyhow!("stdin writer panicked"))??;
    let status = status?;
    if stdout_overflow || stderr_overflow {
        bail!("{program} output exceeded safety limit");
    }
    if !status.success() {
        bail!("{}", String::from_utf8_lossy(&stderr).trim());
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

pub fn ssh(host: &str, remote: &str) -> Result<String> {
    ssh_retry(host, remote, None)
}

pub fn ssh_with_input(host: &str, remote: &str, input: &[u8]) -> Result<String> {
    ssh_retry(host, remote, Some(input))
}

/// A failed multiplexed connection often means the `ControlPath` socket is
/// stale (for example a previous daemon exited without closing its master).
/// Retry once after removing the exact socket this tool owns, then fall back
/// to a plain connection without multiplexing so a lingering bad socket can
/// never wedge every subsequent scheduler query.
fn ssh_retry(host: &str, remote: &str, input: Option<&[u8]>) -> Result<String> {
    match ssh_text(host, remote, input, true) {
        Ok(value) => Ok(value),
        Err(first) => {
            if let Some(socket) = control_socket_path(host) {
                let _ = fs::remove_file(&socket);
            }
            match ssh_text(host, remote, input, true) {
                Ok(value) => Ok(value),
                Err(_) => ssh_text(host, remote, input, false).map_err(|_| first),
            }
        }
    }
}

fn ssh_text(host: &str, remote: &str, input: Option<&[u8]>, multiplex: bool) -> Result<String> {
    let mut args = ssh_args(multiplex);
    args.extend([host, remote]);
    match input {
        Some(input) => text_with_input("ssh", &args, input, None),
        None => text("ssh", &args),
    }
}

fn ssh_args(multiplex: bool) -> Vec<&'static str> {
    let mut args = vec!["-o", "BatchMode=yes", "-o", "ConnectTimeout=8"];
    if multiplex {
        args.extend([
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=120",
            "-o",
            "ControlPath=~/.ssh/slurm-log-%C",
        ]);
    }
    args
}

/// Resolve the expanded `ControlPath` this tool would use for `host` without
/// opening a connection (`ssh -G` only dumps the effective configuration).
/// Only sockets owned by slurm-log (the `slurm-log-` prefix) are ever removed.
fn control_socket_path(host: &str) -> Option<std::path::PathBuf> {
    let out = output_with_timeout(
        "ssh",
        &["-G", "-o", "ControlPath=~/.ssh/slurm-log-%C", host],
        Duration::from_secs(5),
    )
    .ok()?;
    let raw = out
        .status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout))?;
    let path = raw.lines().find_map(|line| {
        line.strip_prefix("controlpath ")
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "none")
    })?;
    let expanded = path.replace('~', &std::env::var("HOME").unwrap_or_default());
    let path = std::path::PathBuf::from(expanded);
    let name = path.file_name().and_then(|value| value.to_str())?;
    name.starts_with("slurm-log-").then_some(path)
}

/// Run a scheduler command through a deliberately small remote environment.
/// The outer paths are absolute so a hostile remote login PATH cannot select a
/// different `env` or shell; the inner scheduler lookup uses only the fixed
/// administrator-controlled search path.
pub fn remote_scheduler_command(program: &str, args: &[&str], directory: Option<&Path>) -> String {
    let invocation = std::iter::once(shell_quote(program))
        .chain(args.iter().map(|argument| shell_quote(argument)))
        .collect::<Vec<_>>()
        .join(" ");
    let script = if let Some(directory) = directory {
        format!(
            "cd {} && exec {invocation}",
            shell_quote(&directory.display().to_string())
        )
    } else {
        format!("exec {invocation}")
    };
    format!(
        "/usr/bin/env -i PATH={} HOME=/ /bin/sh -c {}",
        shell_quote(REMOTE_SCHEDULER_PATH),
        shell_quote(&script)
    )
}

pub fn shell_quote(value: &str) -> String {
    shell_words::quote(value).into_owned()
}

#[cfg(test)]
#[path = "command/tests.rs"]
mod tests;
