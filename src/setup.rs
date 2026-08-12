use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, IsTerminal, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use crossterm::{
    cursor,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
        MouseEventKind,
    },
    execute,
    style::{Attribute, Print, SetAttribute},
    terminal::{self, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use serde_json::json;

use crate::config::{ClusterConfig, Config, SbatchBankConfig, config_path};

const DISCOVERY_DIRECTORY_LIMIT: usize = 20_000;
const DISCOVERY_DEPTH_LIMIT: usize = 3;
const DISCOVERY_TIME_LIMIT: Duration = Duration::from_secs(3);

fn prompt(label: &str, default: &str) -> Result<String> {
    print!(
        "{label}{}: ",
        if default.is_empty() {
            String::new()
        } else {
            format!(" [{default}]")
        }
    );
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    let value = value.trim();
    Ok(if value.is_empty() {
        default.into()
    } else {
        value.into()
    })
}

fn prompt_yes_no(label: &str, default: bool) -> Result<bool> {
    let answer = prompt(label, if default { "yes" } else { "no" })?;
    match answer.to_ascii_lowercase().as_str() {
        "yes" | "y" | "true" | "1" => Ok(true),
        "no" | "n" | "false" | "0" => Ok(false),
        _ => bail!("answer yes or no"),
    }
}

fn prompt_bank_name(current: Option<&str>) -> Result<Option<String>> {
    print!(
        "  Name (blank = Git repository or directory name){}: ",
        current
            .map(|name| format!(" [currently {name}]"))
            .unwrap_or_default()
    );
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    let value = value.trim();
    Ok((!value.is_empty()).then(|| value.to_string()))
}

fn literal_ssh_alias(alias: &str) -> bool {
    !alias.is_empty()
        && !alias.starts_with('-')
        && !alias.chars().any(char::is_whitespace)
        && !alias.bytes().any(|byte| matches!(byte, b'*' | b'?' | b'!'))
}

fn ssh_aliases_from_text(text: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        if !fields
            .next()
            .is_some_and(|key| key.eq_ignore_ascii_case("host"))
        {
            continue;
        }
        aliases.extend(
            fields
                .take_while(|alias| !alias.starts_with('#'))
                .filter(|alias| literal_ssh_alias(alias))
                .map(str::to_string),
        );
    }
    aliases
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let (mut p, mut v, mut star, mut retry) = (0usize, 0usize, None, 0usize);
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry = v;
        } else if let Some(position) = star {
            p = position + 1;
            retry += 1;
            v = retry;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

fn include_paths(config: &Path, pattern: &str) -> Vec<PathBuf> {
    let expanded = expand_home(pattern);
    let path = if expanded.is_absolute() {
        expanded
    } else {
        config
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(expanded)
    };
    let Some(name) = path.file_name().map(|name| name.to_string_lossy()) else {
        return Vec::new();
    };
    if !name.contains(['*', '?']) {
        return path.is_file().then_some(path).into_iter().collect();
    }
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let mut matches: Vec<_> = fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            wildcard_match(&name, &entry.file_name().to_string_lossy()).then_some(entry.path())
        })
        .filter(|path| path.is_file())
        .collect();
    matches.sort_unstable();
    matches
}

fn ssh_config_aliases() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let mut pending = vec![PathBuf::from(home).join(".ssh/config")];
    let mut seen = BTreeSet::new();
    let mut aliases = BTreeSet::new();
    while let Some(path) = pending.pop() {
        if seen.len() >= 128 {
            break;
        }
        let canonical = fs::canonicalize(&path).unwrap_or(path.clone());
        if !seen.insert(canonical) {
            continue;
        }
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.len() > 1024 * 1024 {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        aliases.extend(ssh_aliases_from_text(&text));
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            if fields
                .next()
                .is_some_and(|key| key.eq_ignore_ascii_case("include"))
            {
                for pattern in fields.take_while(|field| !field.starts_with('#')) {
                    pending.extend(include_paths(&path, pattern));
                }
            }
        }
    }
    aliases.into_iter().collect()
}

struct PickerGuard;
impl Drop for PickerGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
    }
}

struct FolderPickerGuard;
impl Drop for FolderPickerGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            LeaveAlternateScreen,
            cursor::Show
        );
    }
}

fn pick_ssh_alias(aliases: &[String], current: &str) -> Result<Option<String>> {
    if aliases.is_empty() {
        return Ok(None);
    }
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
    let _guard = PickerGuard;
    let mut focus = aliases
        .iter()
        .position(|alias| alias == current)
        .unwrap_or(0);
    loop {
        let (_, height) = terminal::size()?;
        let available = height.saturating_sub(4).max(1) as usize;
        let top = focus.saturating_sub(available.saturating_sub(1));
        let mut out = io::stdout().lock();
        execute!(
            out,
            cursor::MoveTo(0, 0),
            terminal::Clear(ClearType::All),
            SetAttribute(Attribute::Bold),
            Print("Choose an SSH host from ~/.ssh/config\r\n"),
            SetAttribute(Attribute::Reset),
            Print("↑/↓ or j/k move · Enter selects · Esc enters another host\r\n\r\n")
        )?;
        for (index, alias) in aliases.iter().enumerate().skip(top).take(available) {
            if index == focus {
                execute!(out, SetAttribute(Attribute::Reverse))?;
            }
            execute!(
                out,
                Print(format!("  {alias}\r\n")),
                SetAttribute(Attribute::Reset)
            )?;
        }
        out.flush()?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => focus = focus.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                focus = (focus + 1).min(aliases.len().saturating_sub(1));
            }
            KeyCode::Home | KeyCode::Char('g') => focus = 0,
            KeyCode::End | KeyCode::Char('G') => focus = aliases.len() - 1,
            KeyCode::Enter => return Ok(Some(aliases[focus].clone())),
            KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
            _ => {}
        }
    }
}

fn choose_ssh_host(current: &str) -> Result<String> {
    let mut aliases = ssh_config_aliases();
    if literal_ssh_alias(current) && !aliases.iter().any(|alias| alias == current) {
        aliases.push(current.into());
        aliases.sort_unstable();
    }
    if let Some(alias) = pick_ssh_alias(&aliases, current)? {
        println!("Selected SSH host: {alias}");
        return Ok(alias);
    }
    let host = prompt("  SSH host or ~/.ssh/config alias", current)?;
    if !literal_ssh_alias(&host) {
        bail!("SSH host must be non-empty, literal, and must not begin with '-'");
    }
    Ok(host)
}

#[derive(Default, Debug, Eq, PartialEq)]
struct SshProbe {
    cluster: Option<String>,
    user: Option<String>,
    home: Option<String>,
    accounting: Option<bool>,
}

fn parse_ssh_probe(output: &str) -> SshProbe {
    let mut probe = SshProbe::default();
    for line in output.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key {
            "SLURM_LOG_CLUSTER" => probe.cluster = Some(value.into()),
            "SLURM_LOG_USER" => probe.user = Some(value.into()),
            "SLURM_LOG_HOME" => probe.home = Some(value.into()),
            "SLURM_LOG_ACCOUNTING" => probe.accounting = Some(value == "yes"),
            _ => {}
        }
    }
    probe
}

fn probe_ssh(host: &str) -> Result<SshProbe> {
    let remote = r#"printf 'SLURM_LOG_USER=%s\n' "$(id -un)"
printf 'SLURM_LOG_HOME=%s\n' "${HOME:-$(pwd)}"
cluster=$(scontrol show config 2>/dev/null | sed -n 's/^[[:space:]]*ClusterName[[:space:]]*=[[:space:]]*//p' | head -n 1)
printf 'SLURM_LOG_CLUSTER=%s\n' "$cluster"
if command -v sacct >/dev/null 2>&1 && sacct -n -X -S now -u "$(id -un)" -o JobID >/dev/null 2>&1; then
  printf 'SLURM_LOG_ACCOUNTING=yes\n'
else
  printf 'SLURM_LOG_ACCOUNTING=no\n'
fi"#;
    Ok(parse_ssh_probe(&crate::command::ssh(host, remote)?))
}

fn safe_cluster_name(value: &str, fallback: &str) -> String {
    let mut name: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || "._-".contains(character) {
                character
            } else {
                '-'
            }
        })
        .take(48)
        .collect();
    name = name.trim_matches('-').to_string();
    if name.is_empty() || matches!(name.as_str(), "all" | "both") {
        name = fallback
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || "._-".contains(*character))
            .take(48)
            .collect();
    }
    if name.is_empty() || matches!(name.as_str(), "all" | "both") {
        "cluster".into()
    } else {
        name
    }
}

fn has_explicit_clusters() -> bool {
    fs::read(config_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.get("clusters").cloned())
        .is_some_and(|clusters| clusters.as_array().is_some_and(|items| !items.is_empty()))
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

fn ignored_discovery_directory(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".cache"
            | ".cargo"
            | ".local"
            | ".venv"
            | "venv"
            | "__pycache__"
            | "node_modules"
            | "target"
            | "build"
    )
}

fn loose_bank_root(search_root: &Path, script_directory: &Path) -> PathBuf {
    // A broad workspace root is only a discovery boundary, not automatically
    // a bank. Group loose scripts by its first child so scanning `$HOME` does
    // not create a giant home-directory bank.
    script_directory
        .strip_prefix(search_root)
        .ok()
        .and_then(|relative| relative.components().next())
        .map(|component| search_root.join(component.as_os_str()))
        .unwrap_or_else(|| search_root.to_path_buf())
}

fn bank_kind(path: &Path) -> &'static str {
    if path.join(".git").exists() {
        "GIT"
    } else {
        "FOLDER"
    }
}

fn suggested_workspace_roots(current: &Config) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut candidates = BTreeSet::new();
    if let Some(path) = &home {
        candidates.insert(path.clone());
    }
    for variable in ["SCRATCH", "WORK", "PROJECT_DIR", "PROJECTS"] {
        if let Some(path) = std::env::var_os(variable).map(PathBuf::from)
            && path.is_dir()
        {
            candidates.insert(path);
        }
    }
    let mut identities = BTreeSet::from([current.local_user.as_str()]);
    if let Some(name) = home
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|v| v.to_str())
    {
        identities.insert(name);
    }
    for identity in &identities {
        // `/storage` and `/storage1` are commonly aliases on cluster login
        // nodes. Prefer `/storage1` so setup does not discover every repo twice
        // or touch a stale compatibility mount.
        let storage1 = Path::new("/storage1").join(identity);
        if storage1.is_dir() {
            candidates.insert(storage1);
        } else {
            let storage = Path::new("/storage").join(identity);
            if storage.is_dir() {
                candidates.insert(storage);
            }
        }
        for parent in ["/scratch", "/work", "/data"] {
            let path = Path::new(parent).join(identity);
            if path.is_dir() {
                candidates.insert(path);
            }
        }
    }
    candidates.into_iter().collect()
}

fn display_workspace_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|path| shell_words::quote(&path.display().to_string()).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Find one bank per Git repository. Loose sbatch files are grouped under the
/// search root, so users enter workspace roots rather than every script folder.
fn discover_banks(roots: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    if cfg!(test) {
        return discover_banks_in_process(roots);
    }
    discover_banks_subprocess(roots)
}

fn discover_banks_in_process(roots: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    let (sender, receiver) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let owned_roots = roots.to_vec();
    if thread::Builder::new()
        .name("sbatch-bank-discovery".into())
        .spawn(move || discover_banks_worker(owned_roots, sender, worker_stop))
        .is_err()
    {
        return (Vec::new(), true);
    }

    collect_discovery(receiver, stop, DISCOVERY_TIME_LIMIT)
}

fn discovery_output_path() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "slurm-log-discovery-{}-{nonce}.jsonl",
        std::process::id()
    ))
}

fn read_discovery_output(path: &Path) -> (Vec<PathBuf>, bool) {
    let Ok(file) = fs::File::open(path) else {
        return (Vec::new(), true);
    };
    let mut banks = BTreeSet::new();
    let mut truncated = true;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(bank) = value.get("bank").and_then(|value| value.as_str()) {
            banks.insert(PathBuf::from(bank));
        }
        if value.get("complete").and_then(|value| value.as_bool()) == Some(true) {
            truncated = value
                .get("truncated")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
        }
    }
    (banks.into_iter().collect(), truncated)
}

fn discover_banks_subprocess(roots: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    let output = discovery_output_path();
    let Ok(file) = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&output)
    else {
        return (Vec::new(), true);
    };
    drop(file);
    let Ok(executable) = std::env::current_exe() else {
        let _ = fs::remove_file(&output);
        return (Vec::new(), true);
    };
    let mut command = Command::new(executable);
    command
        .arg("setup-discover-worker")
        .arg(&output)
        .args(roots)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = command.spawn() else {
        let _ = fs::remove_file(&output);
        return (Vec::new(), true);
    };
    let started = Instant::now();
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) if started.elapsed() < DISCOVERY_TIME_LIMIT => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                break true;
            }
        }
    };
    let (banks, worker_truncated) = read_discovery_output(&output);
    let _ = fs::remove_file(&output);
    (banks, timed_out || worker_truncated)
}

pub fn run_discovery_worker(arguments: &[String]) -> Result<()> {
    let (output, roots) = arguments
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("discovery output path required"))?;
    let output = Path::new(output);
    let file = OpenOptions::new().append(true).mode(0o600).open(output)?;
    let (sender, receiver) = mpsc::channel();
    let writer = thread::spawn(move || {
        let mut file = file;
        while let Ok(event) = receiver.recv() {
            let value = match event {
                DiscoveryEvent::Bank(path) => json!({ "bank": path }),
                DiscoveryEvent::Complete { truncated } => {
                    json!({ "complete": true, "truncated": truncated })
                }
            };
            if serde_json::to_writer(&mut file, &value).is_err()
                || writeln!(file).is_err()
                || file.flush().is_err()
            {
                break;
            }
        }
    });
    discover_banks_worker(
        roots.iter().map(PathBuf::from).collect(),
        sender,
        Arc::new(AtomicBool::new(false)),
    );
    let _ = writer.join();
    Ok(())
}

fn collect_discovery(
    receiver: mpsc::Receiver<DiscoveryEvent>,
    stop: Arc<AtomicBool>,
    time_limit: Duration,
) -> (Vec<PathBuf>, bool) {
    let mut banks = BTreeSet::new();
    let started = Instant::now();
    let truncated = loop {
        let Some(remaining) = time_limit.checked_sub(started.elapsed()) else {
            stop.store(true, Ordering::Relaxed);
            break true;
        };
        match receiver.recv_timeout(remaining) {
            Ok(DiscoveryEvent::Bank(path)) => {
                banks.insert(path);
            }
            Ok(DiscoveryEvent::Complete { truncated }) => break truncated,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                stop.store(true, Ordering::Relaxed);
                break true;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break true,
        }
    };
    (banks.into_iter().collect(), truncated)
}

enum DiscoveryEvent {
    Bank(PathBuf),
    Complete { truncated: bool },
}

fn discover_banks_worker(
    roots: Vec<PathBuf>,
    sender: mpsc::Sender<DiscoveryEvent>,
    stop: Arc<AtomicBool>,
) {
    let mut banks = BTreeSet::new();
    let mut canonical_roots = BTreeSet::new();
    let mut queue = VecDeque::new();
    let mut visited = 0usize;

    for requested_root in &roots {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(root) = fs::canonicalize(requested_root) else {
            continue;
        };
        if !canonical_roots.insert(root.clone()) {
            continue;
        }
        if root.is_file() {
            if root
                .extension()
                .is_some_and(|extension| extension == "sbatch")
                && let Some(parent) = root.parent()
            {
                let path = parent.to_path_buf();
                if banks.insert(path.clone()) && sender.send(DiscoveryEvent::Bank(path)).is_err() {
                    return;
                }
            }
            continue;
        }
        // Interleave broad roots rather than allowing the first large mount to
        // consume the entire budget before later roots are inspected.
        queue.push_back((root.clone(), root, None, 0usize));
    }

    // Never adopt a repository above a requested root: discovery must stay
    // inside the scope the user explicitly selected. Breadth-first traversal
    // also finds repository roots early and avoids diving into one huge tree.
    while let Some((search_root, directory, inherited_repository, depth)) = queue.pop_front() {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if visited >= DISCOVERY_DIRECTORY_LIMIT {
            let _ = sender.send(DiscoveryEvent::Complete { truncated: true });
            return;
        }
        visited += 1;
        let repository = if directory.join(".git").exists() {
            Some(directory.clone())
        } else {
            inherited_repository
        };
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "sbatch")
            {
                let bank = repository
                    .clone()
                    .unwrap_or_else(|| loose_bank_root(&search_root, &directory));
                if banks.insert(bank.clone()) && sender.send(DiscoveryEvent::Bank(bank)).is_err() {
                    return;
                }
            } else if kind.is_dir() && depth < DISCOVERY_DEPTH_LIMIT {
                let name = entry.file_name();
                if !ignored_discovery_directory(&name.to_string_lossy()) {
                    queue.push_back((search_root.clone(), path, repository.clone(), depth + 1));
                }
            }
        }
    }
    let _ = sender.send(DiscoveryEvent::Complete { truncated: false });
}

fn parse_selection(input: &str, count: usize) -> Result<Vec<usize>> {
    let input = input.trim();
    if input.is_empty() || input.eq_ignore_ascii_case("all") {
        return Ok((0..count).collect());
    }
    if input.eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }
    let mut selected = BTreeSet::new();
    for part in input
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let (start, end) = if let Some((start, end)) = part.split_once('-') {
            (start, end)
        } else {
            (part, part)
        };
        let start: usize = start.parse().context("bank selection must use numbers")?;
        let end: usize = end.parse().context("bank selection must use numbers")?;
        if start == 0 || end < start || end > count {
            bail!("bank selection {part} is outside 1-{count}");
        }
        selected.extend((start - 1)..end);
    }
    Ok(selected.into_iter().collect())
}

#[derive(Clone)]
enum DirectoryChoice {
    Select(PathBuf),
    Parent(PathBuf),
    Enter(PathBuf),
}

fn safe_terminal_name(value: &std::ffi::OsStr) -> String {
    value
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

fn directory_children(path: &Path) -> Vec<PathBuf> {
    let mut children: Vec<_> = fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|kind| kind.is_dir() && !kind.is_symlink())
                .map(|_| entry.path())
        })
        .collect();
    children.sort_unstable_by(|left, right| left.file_name().cmp(&right.file_name()));
    children
}

fn browse_bank_directory(starting_roots: &[PathBuf]) -> Result<Option<PathBuf>> {
    if starting_roots.is_empty() {
        return Ok(None);
    }
    terminal::enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        cursor::Hide
    )?;
    let _guard = FolderPickerGuard;
    let mut current: Option<PathBuf> = None;
    let mut focus = 0usize;
    loop {
        let choices: Vec<(String, DirectoryChoice)> = if let Some(directory) = &current {
            let mut rows = vec![(
                "✓ Select this directory".into(),
                DirectoryChoice::Select(directory.clone()),
            )];
            if let Some(parent) = directory.parent()
                && parent != directory
            {
                rows.push(("↰ ..".into(), DirectoryChoice::Parent(parent.to_path_buf())));
            }
            rows.extend(directory_children(directory).into_iter().map(|path| {
                let name = path
                    .file_name()
                    .map(safe_terminal_name)
                    .unwrap_or_else(|| path.display().to_string());
                (format!("▸ {name}"), DirectoryChoice::Enter(path))
            }));
            rows
        } else {
            starting_roots
                .iter()
                .map(|path| {
                    (
                        format!("▸ {}", safe_terminal_name(path.as_os_str())),
                        DirectoryChoice::Enter(path.clone()),
                    )
                })
                .collect()
        };
        focus = focus.min(choices.len().saturating_sub(1));
        let (_, height) = terminal::size()?;
        let available = height.saturating_sub(5).max(1) as usize;
        let top = focus.saturating_sub(available.saturating_sub(1));
        let mut out = io::stdout().lock();
        execute!(
            out,
            cursor::MoveTo(0, 0),
            terminal::Clear(ClearType::All),
            SetAttribute(Attribute::Bold),
            Print("Choose an sbatch bank directory\r\n"),
            SetAttribute(Attribute::Reset),
            Print("↑/↓ move · Enter open/select · ←/Backspace parent · Esc/q cancel\r\n"),
            Print(format!(
                "Location: {}\r\n\r\n",
                current
                    .as_deref()
                    .map(|path| safe_terminal_name(path.as_os_str()))
                    .unwrap_or_else(|| "suggested roots".into())
            ))
        )?;
        for (index, (label, _)) in choices.iter().enumerate().skip(top).take(available) {
            if index == focus {
                execute!(out, SetAttribute(Attribute::Reverse))?;
            }
            execute!(
                out,
                Print(format!("  {label}\r\n")),
                SetAttribute(Attribute::Reset)
            )?;
        }
        out.flush()?;
        let mut activate = false;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Up | KeyCode::Char('k') => focus = focus.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    focus = (focus + 1).min(choices.len().saturating_sub(1));
                }
                KeyCode::Home | KeyCode::Char('g') => focus = 0,
                KeyCode::End | KeyCode::Char('G') => {
                    focus = choices.len().saturating_sub(1);
                }
                KeyCode::Enter | KeyCode::Right => activate = true,
                KeyCode::Left | KeyCode::Backspace => {
                    if let Some(parent) = current.as_deref().and_then(Path::parent) {
                        current = Some(parent.to_path_buf());
                    } else {
                        current = None;
                    }
                    focus = 0;
                }
                KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
                _ => {}
            },
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) if mouse.row >= 4 => {
                    let index = top + usize::from(mouse.row - 4);
                    if index < choices.len() {
                        focus = index;
                        activate = true;
                    }
                }
                MouseEventKind::ScrollUp => focus = focus.saturating_sub(1),
                MouseEventKind::ScrollDown => {
                    focus = (focus + 1).min(choices.len().saturating_sub(1));
                }
                _ => {}
            },
            _ => {}
        }
        if activate {
            match &choices[focus].1 {
                DirectoryChoice::Select(path) => return Ok(Some(path.clone())),
                DirectoryChoice::Parent(path) | DirectoryChoice::Enter(path) => {
                    current = Some(path.clone());
                    focus = 0;
                }
            }
        }
    }
}

fn configure_clusters(current: &Config) -> Result<Vec<ClusterConfig>> {
    let existing = if has_explicit_clusters() {
        current.clusters.as_slice()
    } else {
        &[]
    };
    if existing.is_empty() {
        println!(
            "No cluster assumptions are made. Add only the local or SSH clusters you actually use."
        );
    } else {
        println!("Existing cluster configuration found; press Enter to keep each value.");
    }
    let count: usize = prompt(
        "Number of SLURM clusters",
        &existing.len().max(1).to_string(),
    )?
    .parse()
    .context("cluster count must be a number")?;
    if !(1..=16).contains(&count) {
        bail!("configure between 1 and 16 clusters");
    }
    let mut clusters = Vec::with_capacity(count);
    for index in 0..count {
        let old = existing.get(index);
        println!("\nCluster {}", index + 1);
        let transport = prompt(
            "  Connection (local/ssh)",
            old.map(|item| item.transport.as_str()).unwrap_or("local"),
        )?;
        if !matches!(transport.as_str(), "local" | "ssh") {
            bail!("cluster connection must be local or ssh");
        }
        let (ssh_host, probe, host_changed) = if transport == "ssh" {
            let previous = old.map(|item| item.ssh_host.as_str()).unwrap_or("");
            let host = choose_ssh_host(previous)?;
            println!("Connecting once to {host} to detect SLURM defaults…");
            let detected = match probe_ssh(&host) {
                Ok(probe) => {
                    println!(
                        "Detected: cluster={} · user={} · home={} · sacct={}",
                        probe.cluster.as_deref().unwrap_or("not reported"),
                        probe.user.as_deref().unwrap_or("not reported"),
                        probe.home.as_deref().unwrap_or("not reported"),
                        probe
                            .accounting
                            .map(|enabled| if enabled { "available" } else { "unavailable" })
                            .unwrap_or("not reported")
                    );
                    Some(probe)
                }
                Err(error) => {
                    println!("Could not probe {host}: {error:#}");
                    println!("Continuing with editable manual defaults.");
                    None
                }
            };
            let changed = old.is_none_or(|item| item.ssh_host != host);
            (host, detected, changed)
        } else {
            (String::new(), None, false)
        };
        let automatic_name = if transport == "local" && index == 0 {
            "local".to_string()
        } else if let Some(detected) = probe.as_ref().and_then(|probe| probe.cluster.as_deref()) {
            safe_cluster_name(detected, &ssh_host)
        } else {
            safe_cluster_name(&ssh_host, &format!("cluster{}", index + 1))
        };
        let old_name = old.filter(|_| !host_changed).map(|item| item.name.as_str());
        let name = prompt("  Short name", old_name.unwrap_or(&automatic_name))?;
        let detected_user = probe.as_ref().and_then(|probe| probe.user.as_deref());
        let user = prompt(
            "  SLURM user",
            old.filter(|_| !host_changed)
                .map(|item| item.user.as_str())
                .or(detected_user)
                .unwrap_or(&current.local_user),
        )?;
        let detected_home = probe.as_ref().and_then(|probe| probe.home.as_deref());
        let directory_default = old
            .filter(|_| !host_changed)
            .map(|item| item.working_directory.display().to_string())
            .or_else(|| detected_home.map(str::to_string))
            .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| ".".into()));
        let directory = prompt("  Default job working directory", &directory_default)?;
        let accounting_default = old
            .filter(|_| !host_changed)
            .map(|item| item.accounting)
            .or_else(|| probe.as_ref().and_then(|probe| probe.accounting))
            .unwrap_or(false);
        let accounting = prompt_yes_no("  Is sacct accounting available", accounting_default)?;
        clusters.push(ClusterConfig {
            name,
            transport,
            user,
            ssh_host,
            working_directory: PathBuf::from(directory),
            accounting,
        });
    }
    Ok(clusters)
}

fn configure_banks(current: &Config) -> Result<Vec<SbatchBankConfig>> {
    println!(
        "\nSBATCH BANKS\nQuick discovery checks at most three directory levels. Missing banks can be selected with the folder browser."
    );
    let mut candidates: BTreeMap<PathBuf, Option<String>> = current
        .sbatch_banks
        .iter()
        .map(|bank| {
            (
                fs::canonicalize(&bank.path).unwrap_or_else(|_| bank.path.clone()),
                bank.name.clone(),
            )
        })
        .collect();
    if prompt_yes_no("Discover repositories containing .sbatch files", true)? {
        let suggested = suggested_workspace_roots(current);
        let default_roots = display_workspace_roots(&suggested);
        if !suggested.is_empty() {
            println!("Suggested local roots: {default_roots}");
        }
        let input = prompt(
            "Workspace roots (space-separated; quote paths containing spaces)",
            &default_roots,
        )?;
        let roots: Vec<_> = shell_words::split(&input)
            .context("parse workspace roots")?
            .iter()
            .map(|path| expand_home(path))
            .collect();
        println!(
            "Scanning locally (up to {}s / {} directories; build/cache directories skipped)…",
            DISCOVERY_TIME_LIMIT.as_secs(),
            DISCOVERY_DIRECTORY_LIMIT
        );
        let (found, truncated) = discover_banks(&roots);
        for path in found {
            candidates.entry(path).or_insert(None);
        }
        if truncated {
            println!(
                "Discovery reached its {}s / {}-directory safety limit; add a narrower root to find anything omitted.",
                DISCOVERY_TIME_LIMIT.as_secs(),
                DISCOVERY_DIRECTORY_LIMIT,
            );
        }
    }

    let candidates: Vec<_> = candidates.into_iter().collect();
    let mut banks = Vec::new();
    if candidates.is_empty() {
        println!("No sbatch banks were found.");
    } else {
        println!("\nDiscovered/existing banks:");
        for (index, (path, name)) in candidates.iter().enumerate() {
            let kind = bank_kind(path);
            let colored_kind = if kind == "GIT" {
                "\x1b[36mGIT   \x1b[0m"
            } else {
                "\x1b[33mFOLDER\x1b[0m"
            };
            println!(
                "  {:>2}. [{}] {}{}",
                index + 1,
                colored_kind,
                path.display(),
                name.as_ref()
                    .map(|name| format!("  ({name})"))
                    .unwrap_or_default()
            );
        }
        let selection = prompt("Banks to use (all, none, or e.g. 1,3-5)", "all")?;
        for index in parse_selection(&selection, candidates.len())? {
            let (path, name) = &candidates[index];
            banks.push(SbatchBankConfig {
                path: path.clone(),
                name: name.clone(),
            });
        }
        if banks.len() > 64 {
            bail!("select at most 64 banks ({} selected)", banks.len());
        }
    }

    while banks.len() < 64 && prompt_yes_no("Add a bank directory manually", false)? {
        let directory = if prompt_yes_no("  Use folder browser", true)? {
            let mut roots: BTreeSet<PathBuf> =
                suggested_workspace_roots(current).into_iter().collect();
            roots.extend(banks.iter().map(|bank| bank.path.clone()));
            if let Ok(directory) = std::env::current_dir() {
                roots.insert(directory);
            }
            let Some(directory) = browse_bank_directory(&roots.into_iter().collect::<Vec<_>>())?
            else {
                println!("  Folder selection cancelled.");
                continue;
            };
            directory
        } else {
            let directory = prompt("  Directory", "")?;
            if directory.is_empty() {
                bail!("sbatch bank directory must not be empty");
            }
            expand_home(&directory)
        };
        let name = prompt_bank_name(None)?;
        banks.push(SbatchBankConfig {
            path: directory,
            name,
        });
    }
    Ok(banks)
}

fn configure_state_path(current: &Config) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let local_default = home.join(".local/state/slurm-log/state.json");
    let default = if current.state_path.starts_with(&home) {
        current.state_path.clone()
    } else {
        local_default
    };
    println!(
        "\nLOCAL STATE\nThe small UI ledger and daemon socket should live on responsive local storage, not a cluster mount."
    );
    let value = prompt("State file", &default.display().to_string())?;
    Ok(expand_home(&value))
}

pub fn run(current: &Config) -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!("setup requires an interactive terminal");
    }
    println!("slurm-log setup — these settings are private to this user\n");
    let clusters = configure_clusters(current)?;
    let sbatch_banks = configure_banks(current)?;
    let state_path = configure_state_path(current)?;
    let proposed = Config {
        clusters: clusters.clone(),
        sbatch_banks: sbatch_banks.clone(),
        state_path: state_path.clone(),
        ..current.clone()
    };
    proposed.validate()?;
    let value =
        json!({ "clusters": clusters, "sbatchBanks": sbatch_banks, "statePath": state_path });
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    serde_json::to_writer_pretty(&mut file, &value)?;
    writeln!(file)?;
    file.sync_all()?;
    fs::rename(&temporary, &path)?;
    println!("\nSaved {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_accepts_ranges_and_rejects_invalid_indices() {
        assert_eq!(parse_selection("1,3-4", 5).unwrap(), vec![0, 2, 3]);
        assert_eq!(parse_selection("all", 3).unwrap(), vec![0, 1, 2]);
        assert!(parse_selection("0", 3).is_err());
        assert!(parse_selection("2-5", 3).is_err());
    }

    #[test]
    fn discovery_groups_scripts_by_repository_and_search_root() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("project");
        fs::create_dir_all(repository.join(".git")).unwrap();
        fs::create_dir_all(repository.join("cluster/nested")).unwrap();
        fs::write(repository.join("cluster/a.sbatch"), "#!/bin/sh").unwrap();
        fs::write(repository.join("cluster/nested/b.sbatch"), "#!/bin/sh").unwrap();
        let loose = temporary.path().join("loose/jobs");
        fs::create_dir_all(&loose).unwrap();
        fs::write(loose.join("c.sbatch"), "#!/bin/sh").unwrap();

        let (banks, truncated) = discover_banks(&[repository.clone(), loose.clone()]);
        assert!(!truncated);
        assert_eq!(banks, vec![loose, repository]);
        assert_eq!(bank_kind(&banks[0]), "FOLDER");
        assert_eq!(bank_kind(&banks[1]), "GIT");
    }

    #[test]
    fn broad_roots_do_not_become_giant_loose_script_banks() {
        let temporary = tempfile::tempdir().unwrap();
        for directory in ["experiment-a", "experiment-b/sbatch"] {
            let directory = temporary.path().join(directory);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("job.sbatch"), "#!/bin/sh").unwrap();
        }
        let (banks, truncated) = discover_banks(&[temporary.path().into()]);
        assert!(!truncated);
        assert_eq!(
            banks,
            vec![
                temporary.path().join("experiment-a"),
                temporary.path().join("experiment-b")
            ]
        );
        assert!(!banks.contains(&temporary.path().to_path_buf()));
        assert_eq!(bank_kind(&banks[0]), "FOLDER");
    }

    #[test]
    fn automatic_discovery_never_descends_beyond_three_levels() {
        let temporary = tempfile::tempdir().unwrap();
        let shallow = temporary.path().join("shallow/one/two");
        let deep = temporary.path().join("deep/one/two/three");
        fs::create_dir_all(&shallow).unwrap();
        fs::create_dir_all(&deep).unwrap();
        fs::write(shallow.join("found.sbatch"), "#!/bin/sh").unwrap();
        fs::write(deep.join("ignored.sbatch"), "#!/bin/sh").unwrap();

        let (banks, truncated) = discover_banks(&[temporary.path().into()]);
        assert!(!truncated);
        assert_eq!(banks, vec![temporary.path().join("shallow")]);
    }

    #[test]
    fn discovery_worker_streams_safe_results_to_its_output_file() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("project");
        fs::create_dir_all(repository.join(".git")).unwrap();
        fs::write(repository.join("job.sbatch"), "#!/bin/sh").unwrap();
        let output = temporary.path().join("results.jsonl");
        fs::write(&output, "").unwrap();

        run_discovery_worker(&[
            output.display().to_string(),
            temporary.path().display().to_string(),
        ])
        .unwrap();
        let (banks, truncated) = read_discovery_output(&output);
        assert!(!truncated);
        assert_eq!(banks, vec![repository]);
    }

    #[test]
    fn folder_browser_names_cannot_inject_terminal_controls() {
        assert_eq!(
            safe_terminal_name(std::ffi::OsStr::new("bad\u{1b}[2J\nname")),
            "bad�[2J�name"
        );
    }

    #[test]
    fn discovery_deadline_does_not_wait_for_a_blocked_worker() {
        let (sender, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let keep_open = thread::spawn(move || {
            thread::sleep(Duration::from_millis(200));
            drop(sender);
        });
        let started = Instant::now();
        let (banks, truncated) =
            collect_discovery(receiver, Arc::clone(&stop), Duration::from_millis(10));
        assert!(truncated);
        assert!(banks.is_empty());
        assert!(stop.load(Ordering::Relaxed));
        assert!(started.elapsed() < Duration::from_millis(100));
        keep_open.join().unwrap();
    }

    #[test]
    fn ssh_config_parser_keeps_only_selectable_literal_aliases() {
        let aliases = ssh_aliases_from_text(
            "Host *\nHost cispa sprint *.internal !blocked\nHOST gpu-box # comment\n",
        );
        assert_eq!(aliases, vec!["cispa", "sprint", "gpu-box"]);
        assert!(wildcard_match("*.conf", "cluster.conf"));
        assert!(!wildcard_match("*.conf", "cluster.txt"));
    }

    #[test]
    fn ssh_probe_values_become_safe_editable_defaults() {
        let probe = parse_ssh_probe(
            "SLURM_LOG_USER=alice\nSLURM_LOG_HOME=/remote/home/alice\nSLURM_LOG_CLUSTER=gpu lab\nSLURM_LOG_ACCOUNTING=yes\n",
        );
        assert_eq!(probe.user.as_deref(), Some("alice"));
        assert_eq!(probe.home.as_deref(), Some("/remote/home/alice"));
        assert_eq!(probe.accounting, Some(true));
        assert_eq!(safe_cluster_name("gpu lab", "remote"), "gpu-lab");
        assert_eq!(safe_cluster_name("all", "remote-alias"), "remote-alias");
    }

    #[test]
    fn suggested_workspace_root_text_round_trips_paths_with_spaces() {
        let roots = vec![
            PathBuf::from("/home/alice"),
            PathBuf::from("/work/my project"),
        ];
        let displayed = display_workspace_roots(&roots);
        let parsed: Vec<PathBuf> = shell_words::split(&displayed)
            .unwrap()
            .into_iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(parsed, roots);
    }
}
