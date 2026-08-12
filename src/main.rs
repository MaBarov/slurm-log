#![forbid(unsafe_code)]

mod bank;
mod command;
mod config;
mod daemon;
mod details;
mod follow;
mod lifecycle;
mod model;
mod setup;
mod slurm;
mod state;
mod tmux;
mod ui;

use anyhow::{Result, bail};
use config::{Config, SbatchBankConfig};
use model::{Job, valid_job_id};
use std::{
    collections::{HashMap, HashSet},
    env,
    io::IsTerminal,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

#[derive(Debug)]
struct Args {
    mode: String,
    target: Option<String>,
    cluster: String,
    lines: usize,
    pane_follow: bool,
    show_log_warnings: bool,
    follow: bool,
    fzf: bool,
    refresh: u64,
    archive: bool,
    initial_state: String,
    local_user: Option<String>,
    remote_user: Option<String>,
    ssh_host: Option<String>,
    state_path: Option<String>,
    bank_dir: Option<String>,
    update_binary: Option<String>,
    purge: bool,
    targets: Vec<String>,
}

fn parse_args() -> Result<Args> {
    let mut values = env::args().skip(1).peekable();
    let mut positional = Vec::new();
    let mut cluster = "both".into();
    let mut lines = 50;
    let mut pane_follow = false;
    let mut show_log_warnings = false;
    let mut follow = false;
    let mut fzf = false;
    let mut refresh = 3;
    let mut initial_state = String::new();
    let mut local_user = None;
    let mut remote_user = None;
    let mut ssh_host = None;
    let mut state_path = None;
    let mut bank_dir = None;
    let mut update_binary = None;
    let mut purge = false;
    while let Some(value) = values.next() {
        match value.as_str() {
            "-h" | "--help" => {
                help();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("slurm-log {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--cluster" => {
                cluster = values
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--cluster needs a value"))?
            }
            "-n" | "--lines" => {
                lines = values
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--lines needs a value"))?
                    .parse()?
            }
            "--pane-follow" => pane_follow = true,
            "--show-log-warnings" => show_log_warnings = true,
            "--follow" => follow = true,
            "--fzf" => fzf = true,
            "--refresh" => {
                refresh = values
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("refresh required"))?
                    .parse()?
            }
            "--initial-state" => {
                initial_state = values
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("state required"))?
            }
            "--local-user" => local_user = values.next(),
            "--remote-user" => remote_user = values.next(),
            "--ssh-host" => ssh_host = values.next(),
            "--state-path" => state_path = values.next(),
            "--bank-dir" => bank_dir = values.next(),
            "--binary" => {
                update_binary = Some(
                    values
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--binary needs a value"))?,
                )
            }
            "--purge" => purge = true,
            "--me" => {}
            option if option.starts_with('-') => bail!("unknown option {option}"),
            _ => positional.push(value),
        }
    }
    let mode = positional.first().cloned().unwrap_or_else(|| "all".into());
    let target = positional.get(1).cloned();
    let targets = positional.iter().skip(1).cloned().collect();
    let archive = mode == "archive";
    Ok(Args {
        mode,
        target,
        cluster,
        lines,
        pane_follow,
        show_log_warnings,
        follow,
        fzf,
        refresh,
        archive,
        initial_state,
        local_user,
        remote_user,
        ssh_host,
        state_path,
        bank_dir,
        update_binary,
        purge,
        targets,
    })
}

fn help() {
    print!("{}", help_text());
}

fn help_text() -> &'static str {
    r#"slurm-log — fast, owner-scoped Slurm job and log browser

USAGE
  slurm-log [VIEW] [OPTIONS]
  slurm-log JOB_ID [OPTIONS]
  slurm-log CLUSTER JOB_ID [--follow] [OPTIONS]
  slurm-log COMMAND [ARGUMENTS] [OPTIONS]

START HERE
  slurm-log                   Browse live and recent jobs in the interactive picker
  slurm-log setup             Configure local/SSH clusters and sbatch script banks
  slurm-log JOB_ID            Open one job's log in a tmux workspace
  slurm-log details JOB_ID    Show allocation, placement, and live resource usage

VIEWS
  all                         Live and recent jobs (default)
  running                     Running jobs only
  failed                      Failed jobs and recent failures
  blocked                     Blocked jobs and interactive shell allocations
  archive                     Bounded accounting history, including dismissed jobs
  last                        Open the newest visible job immediately
  watch                       Print and periodically refresh the job table
  fzf                         Select jobs with fzf instead of the built-in picker

JOB & SCRIPT COMMANDS
  details JOB_ID              Show details; use --cluster if the ID is ambiguous
  bank                        Browse configured .sbatch files and submit one
  submit SCRIPT --cluster C   Submit a script from a configured bank
  cancel JOB_ID... --cluster C
                              Request cancellation of one or more jobs
  read JOB_ID                 Mark a tracked job as read
  unread JOB_ID               Mark a tracked job as unread
  json                        Print the selected job view as JSON

WORKSPACE & CACHE
  sessions                    List slurm-log tmux workspaces
  attach [SESSION]            Attach to a workspace
  close [SESSION]             Close a workspace
  daemon start|status|stop    Manage the per-user snapshot cache

INSTALLATION
  update                      Download, verify, and install the latest release
  update --binary FILE        Atomically install a local release binary
  uninstall                   Remove the binary; preserve configuration and history
  uninstall --purge           Also remove this user's configuration and history

OPTIONS
  -n, --lines N               Initial log lines per pane (default: 50)
      --cluster NAME|all      Query one cluster or all configured clusters
      --refresh SECONDS       Picker/watch refresh interval (default: 3)
      --bank-dir DIR          Temporarily use one sbatch bank directory
      --follow                Follow a direct CLUSTER JOB_ID in this terminal
      --fzf                   Use fzf for job selection
      --show-log-warnings     Include warning lines in opened log panes
      --local-user USER       Override the configured local Slurm user
      --remote-user USER      Override the configured remote Slurm user
      --ssh-host HOST         Override the configured SSH host
      --state-path PATH       Override the tracking ledger path
      --binary FILE           Use a local binary with `update`
      --purge                 Remove user data with `uninstall`
  -h, --help                  Show this help and exit
  -V, --version               Show the version and exit

PICKER ESSENTIALS
  Up/Down or j/k              Move                 Space  Select
  Enter                       Open selection       Tab    Switch cluster
  s                           Open sbatch bank     x      Stop focused job
  /                           Search               b      Show blocked jobs
  d                           Dismiss finished     q      Quit
  o                           Cycle 2h/12h/1d/1w/all history
  a                           Toggle all history / live jobs
  ?                           Show the complete keyboard reference

EXAMPLES
  slurm-log running --cluster local
  slurm-log details 12345 --cluster research
  slurm-log research 12345 --follow --lines 200
  slurm-log submit experiments/train.sbatch --cluster local
  slurm-log cancel 12345 12346 --cluster local

Configuration: ~/.config/slurm-log/config.json
Run `slurm-log setup` for guided configuration.
"#
}

fn main() {
    if let Err(error) = run() {
        eprintln!("slurm-log: {error:#}");
        std::process::exit(1);
    }
}

include!("app.rs");
include!("listing.rs");

#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;
