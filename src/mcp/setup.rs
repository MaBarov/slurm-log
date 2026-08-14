use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::{bank, config::Config, daemon};

const SERVER_NAME: &str = "slurm-log";

#[derive(Clone, Copy)]
enum ClientKind {
    Codex,
    Claude,
}

struct Client {
    kind: ClientKind,
    program: PathBuf,
}

impl Client {
    fn name(&self) -> &'static str {
        match self.kind {
            ClientKind::Codex => "Codex",
            ClientKind::Claude => "Claude Code",
        }
    }

    fn get_args(&self) -> Vec<String> {
        match self.kind {
            ClientKind::Codex => vec!["mcp", "get", SERVER_NAME, "--json"],
            ClientKind::Claude => vec!["mcp", "get", SERVER_NAME],
        }
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    fn add_args(&self, server: &Path) -> Vec<String> {
        let binary = server.display().to_string();
        match self.kind {
            ClientKind::Codex => vec!["mcp", "add", SERVER_NAME, "--", &binary, "mcp"],
            ClientKind::Claude => vec![
                "mcp",
                "add",
                "--scope",
                "user",
                SERVER_NAME,
                "--",
                &binary,
                "mcp",
            ],
        }
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    fn remove_args(&self) -> Vec<String> {
        match self.kind {
            ClientKind::Codex => vec!["mcp", "remove", SERVER_NAME],
            ClientKind::Claude => vec!["mcp", "remove", "--scope", "user", SERVER_NAME],
        }
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    fn run(&self, args: &[String]) -> Result<Output> {
        Command::new(&self.program)
            .args(args)
            .output()
            .with_context(|| format!("run {}", self.name()))
    }

    fn exists(&self) -> bool {
        self.run(&self.get_args())
            .is_ok_and(|output| output.status.success())
    }

    fn display(&self, args: &[String]) -> String {
        std::iter::once(self.program.display().to_string())
            .chain(args.iter().cloned())
            .map(|field| shell_quote(&field))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub fn run(config: &Config) -> Result<()> {
    let server = absolute_executable(config)?;
    let clients = clients();
    if clients.is_empty() {
        println!("No supported command-line MCP client was found on PATH.");
    }
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    for client in clients {
        println!("\n{}:", client.name());
        if client.exists() {
            println!("  Existing `{SERVER_NAME}` registration left unchanged.");
            if interactive && confirm("  Replace it? [y/N] ")? {
                remove(&client)?;
                register(&client, &server)?;
            }
        } else {
            let args = client.add_args(&server);
            println!("  {}", client.display(&args));
            if interactive && confirm("  Run this user-scoped command? [y/N] ")? {
                register(&client, &server)?;
            } else if !interactive {
                println!("  Not run because setup is non-interactive.");
            }
        }
    }
    println!("\nGeneric stdio configuration (Cursor, VS Code, Windsurf, Cline, and others):");
    println!("{}", generic_json(&server)?);
    println!("Mutation tools are registered without automatic approval.");
    Ok(())
}

pub fn unregister(_config: &Config) -> Result<()> {
    let mut found = false;
    for client in clients() {
        if client.exists() {
            found = true;
            remove(&client)?;
        }
    }
    if !found {
        println!("No supported client has a `{SERVER_NAME}` registration.");
    }
    Ok(())
}

pub fn doctor(config: &Config) -> Result<()> {
    config.validate().context("configuration")?;
    let tools = super::schema::tools(config);
    if tools.len() != 20 || tools.iter().any(|tool| tool.output_schema.is_none()) {
        bail!("MCP tool schema validation failed");
    }
    let (_, warnings) = bank::configured_scripts_fresh(config).context("scan sbatch banks")?;
    daemon::ensure_running(config).context("private daemon access")?;
    println!("configuration: ok ({} cluster(s))", config.clusters.len());
    println!("tool schemas: ok ({} tools)", tools.len());
    println!("sbatch banks: ok ({} warning(s))", warnings.len());
    println!("private daemon: ok");
    Ok(())
}

fn register(client: &Client, server: &Path) -> Result<()> {
    let args = client.add_args(server);
    println!("  Running: {}", client.display(&args));
    checked(client, &args)?;
    if !client.exists() {
        bail!("{} registration could not be verified", client.name());
    }
    println!("  Verified `{SERVER_NAME}` registration.");
    Ok(())
}

fn remove(client: &Client) -> Result<()> {
    let args = client.remove_args();
    println!("  Running: {}", client.display(&args));
    checked(client, &args)?;
    if client.exists() {
        bail!("{} registration still exists after removal", client.name());
    }
    println!("  Removed `{SERVER_NAME}` registration.");
    Ok(())
}

fn checked(client: &Client, args: &[String]) -> Result<()> {
    let output = client.run(args)?;
    if output.status.success() {
        return Ok(());
    }
    let error = String::from_utf8_lossy(&output.stderr);
    bail!("{} command failed: {}", client.name(), error.trim())
}

fn clients() -> Vec<Client> {
    [("codex", ClientKind::Codex), ("claude", ClientKind::Claude)]
        .into_iter()
        .filter_map(|(name, kind)| find_on_path(name).map(|program| Client { kind, program }))
        .collect()
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .find_map(|directory| {
            let path = Path::new(directory).join(name);
            let metadata = fs::metadata(&path).ok()?;
            (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0).then_some(path)
        })
}

fn absolute_executable(config: &Config) -> Result<PathBuf> {
    config
        .executable
        .canonicalize()
        .with_context(|| format!("resolve {}", config.executable.display()))
}

fn generic_json(server: &Path) -> Result<String> {
    serde_json::to_string_pretty(&json!({
        "mcpServers": {
            SERVER_NAME: {
                "type": "stdio",
                "command": server,
                "args": ["mcp"]
            }
        }
    }))
    .context("render generic MCP configuration")
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_+-./:=,@".contains(&byte))
    {
        value.into()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_configuration_is_portable_stdio() {
        let text = generic_json(Path::new("/opt/slurm log")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["mcpServers"][SERVER_NAME]["type"], "stdio");
        assert_eq!(value["mcpServers"][SERVER_NAME]["args"], json!(["mcp"]));
    }

    #[test]
    fn displayed_commands_quote_without_shell_execution() {
        assert_eq!(shell_quote("/tmp/slurm log"), "'/tmp/slurm log'");
        assert_eq!(shell_quote("slurm-log"), "slurm-log");
    }

    #[test]
    fn codex_registration_and_removal_are_verified_with_argument_vectors() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("registered");
        let program = directory.path().join("codex");
        fs::write(
            &program,
            format!(
                "#!/bin/sh\ncase \"$*\" in\n'mcp get slurm-log --json') test -f '{}' ;;\n'mcp add slurm-log -- '*) touch '{}' ;;\n'mcp remove slurm-log') rm -f '{}' ;;\n*) exit 2 ;;\nesac\n",
                state.display(),
                state.display(),
                state.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let client = Client {
            kind: ClientKind::Codex,
            program,
        };
        assert!(!client.exists());
        register(&client, Path::new("/opt/slurm-log")).unwrap();
        assert!(client.exists());
        remove(&client).unwrap();
        assert!(!client.exists());
    }

    #[test]
    fn claude_registration_is_explicitly_user_scoped() {
        let client = Client {
            kind: ClientKind::Claude,
            program: PathBuf::from("claude"),
        };
        assert_eq!(
            client.add_args(Path::new("/opt/slurm-log")),
            [
                "mcp",
                "add",
                "--scope",
                "user",
                "slurm-log",
                "--",
                "/opt/slurm-log",
                "mcp"
            ]
        );
    }
}
