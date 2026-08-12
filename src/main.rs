#![forbid(unsafe_code)]

mod bank;
mod command;
mod config;
mod daemon;
mod details;
mod follow;
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
  -h, --help                  Show this help and exit
  -V, --version               Show the version and exit

PICKER ESSENTIALS
  Up/Down or j/k              Move                 Space  Select
  Enter                       Open selection       Tab    Switch cluster
  s                           Open sbatch bank     x      Stop selected jobs
  /                           Search               b      Show blocked jobs
  d                           Dismiss finished     q      Quit
  ?                           Show the complete keyboard reference

EXAMPLES
  slurm-log running --cluster sprint
  slurm-log details 3206932 --cluster cispa
  slurm-log cispa 3206932 --follow --lines 200
  slurm-log submit experiments/train.sbatch --cluster sprint
  slurm-log cancel 3206932 3206933 --cluster sprint

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

fn run() -> Result<()> {
    let args = parse_args()?;
    if args.mode == "setup-discover-worker" {
        return setup::run_discovery_worker(&args.targets);
    }
    if args.mode == "bank-scan-worker" {
        return bank::run_scan_worker(&args.targets);
    }
    slurm::validate_query(&args.cluster, "all")?;
    if args.refresh == 0 {
        bail!("--refresh must be at least one second");
    }
    let mut config = if args.mode == "setup" {
        Config::load_for_setup()?
    } else {
        Config::load()?
    };
    if let Some(value) = args.local_user {
        config.local_user = value.clone();
        for cluster in config
            .clusters
            .iter_mut()
            .filter(|cluster| !cluster.remote())
        {
            cluster.user = value.clone();
        }
    }
    if let Some(value) = args.remote_user {
        config.remote_user = value.clone();
        for cluster in config
            .clusters
            .iter_mut()
            .filter(|cluster| cluster.remote())
        {
            cluster.user = value.clone();
        }
    }
    if let Some(value) = args.ssh_host {
        config.ssh_host = value.clone();
        for cluster in config
            .clusters
            .iter_mut()
            .filter(|cluster| cluster.remote())
        {
            cluster.ssh_host = value.clone();
        }
    }
    if let Some(value) = args.state_path {
        config.state_path = value.into();
    }
    if let Some(value) = args.bank_dir {
        config.sbatch_banks = vec![SbatchBankConfig {
            path: value.into(),
            name: None,
        }];
    }
    config.validate()?;
    if args.mode == "setup" {
        return setup::run(&config);
    }
    if args.mode == "single-pane" {
        let session = args
            .target
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("session required"))?;
        std::process::exit(if tmux::single_pane(session)? { 0 } else { 1 });
    }
    if ["sessions", "attach", "close"].contains(&args.mode.as_str()) {
        std::process::exit(tmux::session_command(&args.mode, args.target.as_deref())?);
    }
    if args.mode == "daemon" {
        daemon::command(&config, args.target.as_deref())?;
        return Ok(());
    }
    if args.mode == "bank" {
        if let Some(job) = bank::run(&config)? {
            tmux::open(&config, &[job], args.lines, args.show_log_warnings)?;
        }
        return Ok(());
    }
    if args.mode == "submit" {
        if args.cluster == "both" || args.cluster == "all" {
            bail!("submit requires --cluster NAME");
        }
        let relative = args
            .target
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("script path required"))?;
        let (scripts, _) = bank::configured_scripts(&config)?;
        let mut matches = scripts.iter().filter(|script| {
            bank::supports_cluster(script, &args.cluster)
                && (script.relative == std::path::Path::new(relative)
                    || format!("{}/{}", script.bank, script.relative.display()) == relative)
        });
        let script = matches
            .next()
            .ok_or_else(|| anyhow::anyhow!("script is not in a configured bank"))?;
        if matches.next().is_some() {
            bail!("script path is ambiguous; use BANK/{relative}");
        }
        let job = bank::submit(&config, script, &args.cluster)?;
        println!("Submitted {} as {}:{}", job.name, job.cluster, job.id);
        return Ok(());
    }
    if args.mode == "cancel" {
        if args.cluster == "both" || args.cluster == "all" {
            bail!("cancel requires --cluster NAME");
        }
        if args.targets.is_empty() {
            bail!("cancel requires at least one job ID");
        }
        let jobs: Vec<_> = args
            .targets
            .iter()
            .map(|id| Job {
                cluster: args.cluster.clone(),
                id: id.clone(),
                state: "RUNNING".into(),
                ..Job::default()
            })
            .collect();
        let failures = bank::cancel(&config, &jobs)?;
        if !failures.is_empty() {
            bail!("{}", failures.join("; "));
        }
        println!("Cancellation requested for {} job(s)", jobs.len());
        return Ok(());
    }
    if args.mode == "suppress" {
        if args.cluster == "both" || args.cluster == "all" {
            bail!("suppress requires --cluster NAME");
        }
        if args.targets.is_empty() {
            bail!("suppress requires at least one job ID");
        }
        for id in &args.targets {
            if !valid_job_id(id) {
                bail!("invalid job ID {id}");
            }
            crate::state::Ledger::suppress(
                &config.state_path,
                &Job {
                    cluster: args.cluster.clone(),
                    id: id.clone(),
                    state: "RUNNING".into(),
                    ..Job::default()
                },
            )?;
        }
        return Ok(());
    }
    if args.mode == "toggle-details" {
        return tmux::toggle_details(
            &config,
            args.target
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("focused pane required"))?,
        );
    }
    if args.mode == "details" {
        let id = args
            .target
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("job ID required"))?;
        let cluster = resolve_detail_cluster(&config, &args.cluster, id)?;
        let result = details::run(
            &config,
            &cluster,
            id,
            env::var_os("SLURM_LOG_DETAILS_COMPACT").is_some(),
        );
        if env::var_os("SLURM_LOG_DETAILS_PANE").is_some()
            && let Ok(pane) = env::var("TMUX_PANE")
        {
            tmux::close_detail_pane(&pane);
        }
        return result;
    }
    if args.mode == "toggle-auto" {
        tmux::toggle_auto(
            &config,
            args.target
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("session required"))?,
        )?;
        return Ok(());
    }
    if args.mode == "auto-monitor" {
        tmux::monitor(
            &config,
            args.target
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("session required"))?,
            args.lines,
        )?;
        return Ok(());
    }
    if args.mode == "json" {
        let (jobs, _, _) = slurm::all_jobs(&config, &args.cluster, "all", args.archive)?;
        println!("{}", serde_json::to_string_pretty(&jobs)?);
        return Ok(());
    }
    if config.cluster(&args.mode).is_ok() {
        let id = args
            .target
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("job ID required"))?;
        let job = Job {
            cluster: args.mode,
            id: id.into(),
            state: args.initial_state,
            ..Job::default()
        };
        if args.pane_follow {
            let _ = follow::run(&config, &job, args.lines, true, args.show_log_warnings)?;
        } else if args.follow {
            let _ = follow::run(&config, &job, args.lines, false, args.show_log_warnings)?;
        } else {
            let _ = tmux::open(&config, &[job], args.lines, args.show_log_warnings)?;
        }
        return Ok(());
    }
    if ["read", "unread"].contains(&args.mode.as_str()) {
        let id = args
            .target
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("job ID required"))?;
        let changed = state::Ledger::set_read(&config.state_path, id, args.mode == "read")?;
        if changed == 0 {
            bail!("job {id} is not in the tracking ledger");
        }
        println!(
            "Marked job {id} {}",
            if args.mode == "read" {
                "read"
            } else {
                "unread"
            }
        );
        return Ok(());
    }
    if args.mode == "pick-add" {
        let session = args
            .target
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("session required"))?;
        let panes = tmux::panes(session)?;
        let open: HashSet<_> = panes
            .iter()
            .map(|p| format!("{}:{}", p.cluster, p.job_id))
            .collect();
        let (snapshot, ledger, warnings) = slurm::all_jobs(&config, &args.cluster, "all", false)?;
        // Blocked jobs are normally hidden from the live picker, but an
        // already-open pane must keep its real scheduler state. Otherwise it
        // is replaced by the red synthetic OPEN fallback every time Ctrl-b j
        // is reopened.
        let mut open_metadata: HashMap<_, _> = snapshot
            .iter()
            .filter(|job| open.contains(&job.key()))
            .cloned()
            .map(|job| (job.key(), job))
            .collect();
        let blocked_count = slurm::visible_jobs(snapshot.clone(), &ledger, 0, true)
            .iter()
            .filter(|job| job.blocked_category())
            .count();
        let mut jobs = slurm::visible_jobs(snapshot, &ledger, 0, false);
        let mut visible_keys: HashSet<_> = jobs.iter().map(Job::key).collect();
        for pane in panes {
            let key = format!("{}:{}", pane.cluster, pane.job_id);
            if visible_keys.insert(key.clone()) {
                jobs.push(open_metadata.remove(&key).unwrap_or_else(|| Job {
                    cluster: pane.cluster,
                    id: pane.job_id,
                    state: "OPEN".into(),
                    ..Job::default()
                }));
            }
        }
        let chosen = ui::pick(
            &config,
            jobs,
            ledger,
            open,
            true,
            0,
            Some((args.cluster.clone(), "all".into())),
            Some(session.to_string()),
            warnings,
            args.refresh,
            blocked_count,
        )?;
        if !chosen.jobs.is_empty() {
            tmux::reconcile(
                &config,
                session,
                &chosen.jobs,
                args.lines,
                chosen.show_log_warnings,
            )?;
        }
        return Ok(());
    }
    if valid_job_id(&args.mode) {
        for cluster in &config.clusters {
            if slurm::terminal_path(&config, &cluster.name, &args.mode).is_ok() {
                tmux::open(
                    &config,
                    &[Job {
                        cluster: cluster.name.clone(),
                        id: args.mode,
                        ..Job::default()
                    }],
                    args.lines,
                    args.show_log_warnings,
                )?;
                return Ok(());
            }
        }
        bail!("job not found");
    }
    let filter = if ["running", "failed", "blocked"].contains(&args.mode.as_str()) {
        args.mode.as_str()
    } else {
        "all"
    };
    if ![
        "all", "running", "failed", "blocked", "archive", "last", "watch", "fzf",
    ]
    .contains(&args.mode.as_str())
    {
        bail!("unknown mode or invalid job ID: {}", args.mode);
    }
    loop {
        let history_mode = if args.archive {
            2
        } else if ["failed", "blocked"].contains(&filter) {
            1
        } else {
            0
        };
        let (jobs, ledger, warnings) =
            slurm::all_jobs(&config, &args.cluster, filter, args.archive)?;
        let blocked_count = slurm::visible_jobs(jobs.clone(), &ledger, history_mode, true)
            .iter()
            .filter(|job| job.blocked_category())
            .count();
        let jobs = slurm::visible_jobs(jobs, &ledger, history_mode, filter == "blocked");
        if args.mode == "watch" {
            print!("\x1b[H\x1b[2J");
            render(&jobs, &warnings);
            thread::sleep(Duration::from_secs(args.refresh));
            continue;
        }
        if args.fzf || args.mode == "fzf" {
            let selected = choose_fzf(&jobs)?;
            if !selected.is_empty() {
                tmux::open(&config, &selected, args.lines, args.show_log_warnings)?;
            }
            return Ok(());
        }
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            render(&jobs, &warnings);
            return Ok(());
        }
        if args.mode == "last" {
            if let Some(job) = jobs.first() {
                tmux::open(
                    &config,
                    std::slice::from_ref(job),
                    args.lines,
                    args.show_log_warnings,
                )?;
            }
            return Ok(());
        }
        let chosen = ui::pick(
            &config,
            jobs,
            ledger,
            HashSet::new(),
            false,
            history_mode,
            Some((args.cluster.clone(), filter.to_string())),
            None,
            warnings,
            args.refresh,
            blocked_count,
        )?;
        if chosen.jobs.is_empty() {
            return Ok(());
        }
        tmux::open(&config, &chosen.jobs, args.lines, chosen.show_log_warnings)?;
    }
}

fn resolve_detail_cluster(config: &Config, requested: &str, id: &str) -> Result<String> {
    if requested != "both" {
        details::validate_cluster(config, requested)?;
        return Ok(requested.into());
    }
    let (jobs, _, _) = slurm::all_jobs(config, "both", "all", false)?;
    let mut matches: Vec<_> = jobs
        .iter()
        .filter(|job| job.id == id)
        .map(|job| job.cluster.as_str())
        .collect();
    matches.sort_unstable();
    matches.dedup();
    let choices = config
        .clusters
        .iter()
        .map(|cluster| cluster.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    match matches.as_slice() {
        [cluster] => Ok((*cluster).into()),
        [] => bail!("job {id} is not in the live/recent cache; specify --cluster NAME ({choices})"),
        _ => bail!("job {id} exists on multiple clusters; specify --cluster NAME ({choices})"),
    }
}

fn render(jobs: &[Job], warnings: &[String]) {
    println!(" #  CLUSTER  JOB ID          STATE                ELAPSED     NAME / REASON");
    for (index, job) in jobs.iter().enumerate() {
        println!(
            "{:2}  {:<7}  {:<15} {:<20} {:<11} {} {}",
            index + 1,
            job.cluster,
            job.id,
            job.state,
            job.elapsed,
            job.name,
            if job.insight().is_empty() {
                job.reason.clone()
            } else {
                job.insight()
            }
        );
    }
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}

fn choose_fzf(jobs: &[Job]) -> Result<Vec<Job>> {
    use std::io::Write;
    let mut child = Command::new("fzf")
        .args(["-m", "--delimiter=\\t", "--with-nth=2..", "--prompt=logs> "])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    if let Some(input) = child.stdin.as_mut() {
        for (index, job) in jobs.iter().enumerate() {
            writeln!(
                input,
                "{index}\t{}\t{}\t{}\t{}",
                job.cluster, job.id, job.state, job.name
            )?;
        }
    }
    let output = child.wait_with_output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .filter_map(|line| line.split('\t').next()?.parse::<usize>().ok())
        .filter_map(|index| jobs.get(index).cloned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::help_text;

    #[test]
    fn cli_help_is_scannable_and_documents_public_workflows() {
        let help = help_text();
        for section in [
            "USAGE",
            "START HERE",
            "VIEWS",
            "JOB & SCRIPT COMMANDS",
            "WORKSPACE & CACHE",
            "OPTIONS",
            "PICKER ESSENTIALS",
            "EXAMPLES",
        ] {
            assert!(
                help.lines().any(|line| line == section),
                "missing {section}"
            );
        }
        for workflow in [
            "slurm-log setup",
            "slurm-log JOB_ID",
            "details JOB_ID",
            "submit SCRIPT --cluster C",
            "cancel JOB_ID... --cluster C",
            "daemon start|status|stop",
            "--cluster NAME|all",
            "--show-log-warnings",
            "-h, --help",
            "-V, --version",
        ] {
            assert!(help.contains(workflow), "missing workflow: {workflow}");
        }
        assert!(help.ends_with('\n'));
        assert!(
            help.lines().all(|line| line.chars().count() <= 88),
            "help should fit comfortably in a standard terminal"
        );
    }

    #[test]
    fn cli_help_hides_internal_worker_commands() {
        let help = help_text();
        for internal in [
            "setup-discover-worker",
            "bank-scan-worker",
            "pick-add",
            "toggle-details",
            "auto-monitor",
            "pane-follow",
            "initial-state",
            "suppress",
        ] {
            assert!(
                !help.contains(internal),
                "leaked internal command: {internal}"
            );
        }
    }
}
