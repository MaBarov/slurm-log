/// The MCP alias is never a Slurm federation name; SSH options stay neutral
/// and only the explicit controller binding reaches the remote scheduler.
pub fn ssh(host: &str, remote: &str) -> Result<String> {
    ssh_retry(host, remote, None)
}

fn ssh_control_path(host: &str) -> PathBuf {
    let digest = Sha256::digest(host.as_bytes());
    let token: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    home_directory()
        .join(".ssh")
        .join(format!("slurm-log-{token}"))
}

fn home_directory() -> PathBuf {
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home);
    }
    let uid = rustix::process::getuid().as_raw();
    let user_run = PathBuf::from(format!("/run/user/{uid}"));
    if user_run.is_dir() {
        return user_run;
    }
    let tmp_user = PathBuf::from(format!("/tmp/slurm-log-runtime-{uid}"));
    let _ = fs::create_dir_all(&tmp_user);
    let _ = fs::set_permissions(&tmp_user, fs::Permissions::from_mode(0o700));
    tmp_user
}

/// A socket is stale when this user owns it but no master answers a connect.
/// Connecting without speaking is harmless: the master accepts, sees EOF
/// before any multiplexed message, and logs a benign reset for the client.
fn control_socket_is_stale(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata.uid() != rustix::process::getuid().as_raw() {
        return false;
    }
    UnixStream::connect(path).is_err()
}

fn remove_stale_control_socket(host: &str) {
    let path = ssh_control_path(host);
    if control_socket_is_stale(&path) {
        let _ = fs::remove_file(&path);
    }
}

fn ssh_retry(host: &str, remote: &str, input: Option<&[u8]>) -> Result<String> {
    remove_stale_control_socket(host);
    let attempt = |with_mux: bool| -> Result<String> {
        let control_path = ssh_control_path(host).display().to_string();
        let mut args: Vec<String> = vec![
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ConnectTimeout=8".into(),
        ];
        if with_mux {
            args.extend([
                "-o".into(),
                "ControlMaster=auto".into(),
                "-o".into(),
                "ControlPersist=120".into(),
                "-o".into(),
                format!("ControlPath={control_path}"),
            ]);
        }
        args.push(host.into());
        args.push(remote.into());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        match input {
            None => text("ssh", &refs),
            Some(bytes) => text_with_input("ssh", &refs, bytes, None),
        }
    };
    match attempt(true) {
        Ok(value) => Ok(value),
        Err(first) => {
            remove_stale_control_socket(host);
            match attempt(true) {
                Ok(value) => Ok(value),
                Err(_second) => attempt(false).context(first),
            }
        }
    }
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

pub fn ssh_with_input(host: &str, remote: &str, input: &[u8]) -> Result<String> {
    ssh_retry(host, remote, Some(input))
}
