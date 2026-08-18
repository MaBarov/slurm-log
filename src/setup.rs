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
    ssh_config_aliases_from(&PathBuf::from(home))
}

fn ssh_config_aliases_from(home: &Path) -> Vec<String> {
    let mut pending = vec![home.join(".ssh/config")];
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

#[derive(Debug, Eq, PartialEq)]
enum PickerKey {
    Continue,
    Select,
    Cancel,
}

fn apply_picker_key(code: KeyCode, focus: &mut usize, count: usize) -> PickerKey {
    match code {
        KeyCode::Up | KeyCode::Char('k') => *focus = focus.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            *focus = (*focus + 1).min(count.saturating_sub(1));
        }
        KeyCode::Home | KeyCode::Char('g') => *focus = 0,
        KeyCode::End | KeyCode::Char('G') => *focus = count.saturating_sub(1),
        KeyCode::Enter => return PickerKey::Select,
        KeyCode::Esc | KeyCode::Char('q') => return PickerKey::Cancel,
        _ => {}
    }
    PickerKey::Continue
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
        match apply_picker_key(key.code, &mut focus, aliases.len()) {
            PickerKey::Select => return Ok(Some(aliases[focus].clone())),
            PickerKey::Cancel => return Ok(None),
            PickerKey::Continue => {}
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

include!("setup/discovery.rs");
include!("setup/folder_browser.rs");
include!("setup/configure.rs");

#[cfg(test)]
mod tests;
#[cfg(test)]
#[path = "setup/tests/coverage.rs"]
mod tests_coverage;
