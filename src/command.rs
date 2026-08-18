use anyhow::{Context, Result, bail};
use std::{
    cell::RefCell,
    env, fs,
    io::{Read, Result as IoResult, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::UnixStream,
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

include!("command/mux.rs");

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
    let mut command = Command::new(program);
    // A child owns its own process group so deadline/cancellation can tear
    // down shell/SSH descendants that retain stdout or stderr pipes.
    command.process_group(0);
    scrub_scheduler_environment(&mut command);
    command
}

fn scrub_scheduler_environment(command: &mut Command) {
    // sbatch/scancel honour several inherited option variables. They are not
    // part of an MCP preview and therefore must not silently alter a submitted
    // script, its target, or a cancellation request.
    for (key, _) in env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("SBATCH_")
            || name.starts_with("SCANCEL_")
            || matches!(
                name.as_ref(),
                "SLURM_CLUSTERS" | "SLURM_HINT" | "SLURM_EXPORT_ENV"
            )
        {
            command.env_remove(key);
        }
    }
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

pub fn shell_quote(value: &str) -> String {
    shell_words::quote(value).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_round_trips_adversarial_values() {
        for value in [
            "simple",
            "with spaces",
            "single'quote",
            "$(touch /tmp/never)",
            "; rm -rf nope",
            "line1\nline2",
            "unicode-λ",
        ] {
            let script = format!("printf %s {}", shell_quote(value));
            let output = text("sh", &["-c", &script]).unwrap();
            assert_eq!(output, value);
        }
    }

    #[test]
    fn remote_scheduler_wrapper_uses_fixed_paths_and_quotes_arguments() {
        let command = remote_scheduler_command(
            "sbatch",
            &[
                "--parsable",
                "--clusters",
                "controller-a",
                "$(not-expanded)",
            ],
            Some(Path::new("/work space")),
        );
        assert!(
            command.starts_with(
                "/usr/bin/env -i PATH=/usr/local/bin:/usr/bin:/bin HOME=/ /bin/sh -c "
            )
        );
        assert!(command.contains("/bin/sh"));
        assert!(!command.contains("${PATH"));
        assert!(!command.contains("$PATH"));
        assert!(command.contains("'$(not-expanded)'"));
    }

    #[test]
    fn failed_commands_return_stderr() {
        let error = text("sh", &["-c", "printf denied >&2; exit 7"]).unwrap_err();
        assert!(format!("{error:#}").contains("denied"));
    }

    #[test]
    fn oversized_stdout_and_stderr_are_drained_then_rejected() {
        for script in [
            "i=0; while [ $i -lt 200 ]; do printf 0123456789; i=$((i+1)); done",
            "i=0; while [ $i -lt 200 ]; do printf 0123456789 >&2; i=$((i+1)); done",
        ] {
            let error = output_with_limit("sh", &["-c", script], 1024).unwrap_err();
            assert!(format!("{error:#}").contains("safety limit"));
        }
    }

    #[test]
    fn bounded_input_command_preserves_bytes_and_working_directory() {
        let directory = tempfile::tempdir().unwrap();
        let output = text_with_input(
            "sh",
            &["-c", "printf '%s|' \"$PWD\"; cat"],
            b"exact\0bytes\n",
            Some(directory.path()),
        )
        .unwrap();
        assert_eq!(
            output.as_bytes(),
            [
                directory.path().as_os_str().as_encoded_bytes(),
                b"|exact\0bytes\n"
            ]
            .concat()
        );
    }

    #[test]
    fn bounded_input_command_reports_failures_and_output_overflow() {
        let error = text_with_input_limit(
            "sh",
            &["-c", "cat >/dev/null; printf rejected >&2; exit 9"],
            b"input",
            None,
            1024,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("rejected"));

        let error = text_with_input_limit(
            "sh",
            &["-c", "cat >/dev/null; printf 0123456789"],
            b"input",
            None,
            4,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("safety limit"));
    }

    #[test]
    fn deadline_kills_descendants_that_hold_the_output_pipe() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("descendant.pid");
        let started = Instant::now();
        let output = output_with_limit_and_timeout(
            "sh",
            &[
                "-c",
                "sleep 30 & printf '%s' \"$!\" > \"$1\"; printf ready",
                "sh",
                pid_file.to_str().unwrap(),
            ],
            1024,
            Duration::from_millis(250),
        )
        .unwrap();
        assert_eq!(output.stdout, b"ready");
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid = std::fs::read_to_string(pid_file).unwrap();
        let process = Path::new("/proc").join(pid.trim());
        for _ in 0..20 {
            if !process.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!process.exists(), "background descendant survived cleanup");
    }

    #[test]
    fn ssh_control_path_uses_private_runtime_and_reproducible_tokens() {
        let path1 = ssh_control_path("login.cluster.local");
        let path2 = ssh_control_path("login.cluster.local");
        assert_eq!(path1, path2);
        assert!(path1.to_string_lossy().contains("slurm-log-"));
    }
}
