use anyhow::{Context, Result, bail};
use std::{
    io::{Read, Result as IoResult, Write},
    path::Path,
    process::{Command, Output, Stdio},
    thread,
};

const MAX_COMMAND_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

pub fn output(program: &str, args: &[&str]) -> Result<Output> {
    output_with_limit(program, args, MAX_COMMAND_OUTPUT_BYTES)
}

fn output_with_limit(program: &str, args: &[&str], limit: usize) -> Result<Output> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("run {program}"))?;
    let stdout = child.stdout.take().context("capture command stdout")?;
    let stderr = child.stderr.take().context("capture command stderr")?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout, limit));
    let stderr_reader = thread::spawn(move || read_bounded(stderr, limit));
    let status = child
        .wait()
        .with_context(|| format!("wait for {program}"))?;
    let (stdout, stdout_overflow) = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
    let (stderr, stderr_overflow) = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;
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
    let mut command = Command::new(program);
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
    child
        .stdin
        .take()
        .context("open command stdin")?
        .write_all(input)?;
    let status = child.wait()?;
    let (stdout, stdout_overflow) = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
    let (stderr, stderr_overflow) = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;
    if stdout_overflow || stderr_overflow {
        bail!("{program} output exceeded safety limit");
    }
    if !status.success() {
        bail!("{}", String::from_utf8_lossy(&stderr).trim());
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

pub fn ssh(host: &str, remote: &str) -> Result<String> {
    text(
        "ssh",
        &[
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=120",
            "-o",
            "ControlPath=~/.ssh/slurm-log-%C",
            host,
            remote,
        ],
    )
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
}
