use std::{
    collections::{HashMap, HashSet},
    env,
    io::{self, Write},
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};

use crate::{config::Config, model::Job, slurm, state::Ledger};

pub struct PickResult {
    pub jobs: Vec<Job>,
    pub show_log_warnings: bool,
}

#[derive(Clone)]
struct Row {
    name: String,
    job: Option<usize>,
    members: Vec<usize>,
    nested: bool,
    expanded: bool,
}

struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn pick(
    config: &Config,
    mut jobs: Vec<Job>,
    mut ledger: Ledger,
    initial: HashSet<String>,
    manage: bool,
    mut history_mode: u8,
    mut live_filter: Option<(String, String)>,
    auto_session: Option<String>,
    mut warnings: Vec<String>,
    refresh_seconds: u64,
    mut blocked_count: usize,
) -> Result<PickResult> {
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
    let _guard = Guard;
    let mut focus = 0usize;
    let mut selected = initial;
    // The pane manager's selection is workspace-wide, while cluster tabs are
    // only views into that workspace. Keep the latest representation of every
    // job we have seen so a selected pane can survive while its cluster is not
    // the active tab and can still be returned to tmux on Enter.
    let mut known_jobs: HashMap<String, Job> =
        jobs.iter().cloned().map(|job| (job.key(), job)).collect();
    let mut expanded = HashSet::new();
    let mut query = String::new();
    let mut show_blocked = live_filter
        .as_ref()
        .is_some_and(|(_, filter)| filter == "blocked");
    let mut show_warnings = false;
    let mut show_log_warnings = false;
    let mut show_help = false;
    let mut help_offset = 0usize;
    let mut selection_dirty = false;
    let mut last_refresh = Instant::now();
    let mut redraw = true;
    let mut focused_key: Option<String> = None;
    let popup = env::var_os("SLURM_LOG_POPUP").is_some();
    let mut popup_frame = Vec::new();
    let mut indices = Vec::new();
    let mut rows = Vec::new();
    let mut view_dirty = true;
    let mut catalog_dirty = false;
    let mut notice: Option<(String, Instant)> = None;
    loop {
        if notice
            .as_ref()
            .is_some_and(|(_, expires)| Instant::now() >= *expires)
        {
            notice = None;
            redraw = true;
        }
        let refresh_interval = if history_mode == 2 {
            refresh_seconds.max(60)
        } else {
            refresh_seconds
        };
        if live_filter.is_some() && last_refresh.elapsed() >= Duration::from_secs(refresh_interval)
        {
            let changed = refresh(
                config,
                &live_filter,
                history_mode,
                show_blocked,
                &mut blocked_count,
                false,
                &mut jobs,
                &mut ledger,
                &mut warnings,
            );
            last_refresh = Instant::now();
            if changed {
                redraw = true;
                view_dirty = true;
                catalog_dirty = true;
            }
        }
        if catalog_dirty {
            known_jobs.extend(jobs.iter().cloned().map(|job| (job.key(), job)));
            catalog_dirty = false;
        }
        if manage && view_dirty {
            let active_cluster = live_filter
                .as_ref()
                .map_or("all", |(cluster, _)| cluster.as_str());
            let every_cluster = matches!(active_cluster, "all" | "both");
            let mut visible: HashSet<_> = jobs.iter().map(Job::key).collect();
            for key in &selected {
                let Some(job) = known_jobs.get(key) else {
                    continue;
                };
                if (every_cluster || job.cluster == active_cluster) && visible.insert(key.clone()) {
                    jobs.push(job.clone());
                }
            }
        }
        if view_dirty {
            let needle = query.to_lowercase();
            indices = jobs
                .iter()
                .enumerate()
                .filter_map(|(index, job)| {
                    if needle.is_empty() {
                        return Some(index);
                    }
                    job_matches(job, &needle).then_some(index)
                })
                .collect();
            rows = grouped_rows(&jobs, &indices, &expanded);
            if let Some(key) = focused_key.take()
                && let Some(position) = rows.iter().position(|row| row_key(row, &jobs) == key)
            {
                focus = position;
            }
            view_dirty = false;
        }
        focus = focus.min(rows.len().saturating_sub(1));
        if redraw {
            if show_help {
                draw_help(help_offset, &mut popup_frame)?;
            } else if popup {
                draw_popup(
                    &jobs,
                    &rows,
                    focus,
                    &selected,
                    manage,
                    history_mode,
                    ledger.auto_add_default,
                    show_blocked,
                    show_log_warnings,
                    &query,
                    &warnings,
                    show_warnings,
                    live_filter.as_ref().map_or("all", |(cluster, _)| cluster),
                    notice.as_ref().map_or("", |(message, _)| message),
                    blocked_count,
                    &mut popup_frame,
                )?;
            } else {
                draw(
                    &jobs,
                    &rows,
                    focus,
                    &selected,
                    manage,
                    history_mode,
                    ledger.auto_add_default,
                    show_blocked,
                    show_warnings,
                    show_log_warnings,
                    &query,
                    &warnings,
                    live_filter.as_ref().map_or("all", |(cluster, _)| cluster),
                    notice.as_ref().map_or("", |(message, _)| message),
                    blocked_count,
                )?;
            }
            redraw = false;
        }
        let poll_for = notice
            .as_ref()
            .map_or(Duration::from_secs(1), |(_, expires)| {
                expires
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(1))
            });
        if !event::poll(poll_for)? {
            continue;
        }
        let event = event::read()?;
        if matches!(event, Event::Resize(_, _)) {
            redraw = true;
            continue;
        }
        let Event::Key(key) = event else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        redraw = true;
        if show_help {
            let (_, height) = terminal::size()?;
            let page = height.saturating_sub(2).max(1) as usize;
            let maximum = help_lines().len().saturating_sub(page);
            match key.code {
                KeyCode::Char('?') | KeyCode::Esc => {
                    show_help = false;
                    popup_frame.clear();
                }
                KeyCode::Up | KeyCode::Char('k') => help_offset = help_offset.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => help_offset = (help_offset + 1).min(maximum),
                KeyCode::PageUp => help_offset = help_offset.saturating_sub(page),
                KeyCode::PageDown => help_offset = (help_offset + page).min(maximum),
                KeyCode::Home | KeyCode::Char('g') => help_offset = 0,
                KeyCode::End | KeyCode::Char('G') => help_offset = maximum,
                KeyCode::Char('q') => {
                    return Ok(PickResult {
                        jobs: Vec::new(),
                        show_log_warnings,
                    });
                }
                _ => {}
            }
            continue;
        }
        // Capture this before an action can replace `jobs`; row indices belong
        // to the current snapshot.
        let prior_focused_key = rows.get(focus).map(|row| row_key(row, &jobs));
        let replaces_jobs = matches!(
            key.code,
            KeyCode::Char('d' | 'o' | 'a' | 'b' | 'r' | 'x') | KeyCode::Tab | KeyCode::BackTab
        );
        match key.code {
            KeyCode::Char('q') => {
                return Ok(PickResult {
                    jobs: Vec::new(),
                    show_log_warnings,
                });
            }
            KeyCode::Esc if !query.is_empty() => {
                query.clear();
                view_dirty = true;
                popup_frame.clear();
            }
            KeyCode::Esc => {
                return Ok(PickResult {
                    jobs: Vec::new(),
                    show_log_warnings,
                });
            }
            KeyCode::Up | KeyCode::Char('k') => focus = focus.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                focus = (focus + 1).min(rows.len().saturating_sub(1))
            }
            KeyCode::Home | KeyCode::Char('g') => focus = 0,
            KeyCode::End | KeyCode::Char('G') => focus = rows.len().saturating_sub(1),
            KeyCode::PageUp => focus = focus.saturating_sub(10),
            KeyCode::PageDown => focus = (focus + 10).min(rows.len().saturating_sub(1)),
            KeyCode::Tab | KeyCode::BackTab if live_filter.is_some() => {
                if let Some((cluster, _)) = &mut live_filter {
                    *cluster = cycle_cluster(config, cluster, matches!(key.code, KeyCode::BackTab));
                }
                query.clear();
                catalog_dirty |= refresh(
                    config,
                    &live_filter,
                    history_mode,
                    show_blocked,
                    &mut blocked_count,
                    false,
                    &mut jobs,
                    &mut ledger,
                    &mut warnings,
                );
                if !manage {
                    let visible: HashSet<_> = jobs.iter().map(Job::key).collect();
                    selected.retain(|key| visible.contains(key));
                }
                view_dirty = true;
                popup_frame.clear();
            }
            KeyCode::Right | KeyCode::Char('l')
                if !rows.is_empty() && rows[focus].job.is_none() =>
            {
                expanded.insert(rows[focus].name.clone());
                view_dirty = true;
            }
            KeyCode::Left | KeyCode::Char('h') if !rows.is_empty() => {
                view_dirty = expanded.remove(&rows[focus].name);
            }
            KeyCode::Char(' ') if !rows.is_empty() => {
                toggle(&mut selected, &jobs, &rows[focus].members);
                selection_dirty = true;
            }
            KeyCode::Char('v') => {
                selected.extend(indices.iter().map(|&index| jobs[index].key()));
                selection_dirty = true;
            }
            KeyCode::Char('c') => {
                selected.clear();
                selection_dirty = true;
            }
            KeyCode::Char('d') if !rows.is_empty() => {
                let targets: Vec<Job> = if selected.is_empty() {
                    rows[focus]
                        .members
                        .iter()
                        .map(|&i| jobs[i].clone())
                        .collect()
                } else {
                    jobs.iter()
                        .filter(|job| selected.contains(&job.key()))
                        .cloned()
                        .collect()
                };
                Ledger::dismiss(&config.state_path, &targets)?;
                ledger = Ledger::load(&config.state_path)?;
                let hidden: HashSet<_> = targets
                    .iter()
                    .map(Job::key)
                    .filter(|key| ledger.dismissed.contains_key(key))
                    .collect();
                jobs.retain(|job| !hidden.contains(&job.key()));
                selected.retain(|key| !hidden.contains(key));
                view_dirty = true;
                popup_frame.clear();
            }
            KeyCode::Char('x') if !rows.is_empty() => {
                let targets: Vec<Job> = if selected.is_empty() {
                    rows[focus]
                        .members
                        .iter()
                        .map(|&index| jobs[index].clone())
                        .filter(Job::active)
                        .collect()
                } else {
                    jobs.iter()
                        .filter(|job| selected.contains(&job.key()) && job.active())
                        .cloned()
                        .collect()
                };
                if targets.is_empty() {
                    set_notice(&mut notice, "No active jobs selected");
                    popup_frame.clear();
                } else if confirm_cancel(&targets)? {
                    let requested = targets.len();
                    let failures = crate::bank::cancel(config, &targets)?;
                    if failures.is_empty() {
                        set_notice(
                            &mut notice,
                            &format!("Stop requested for {requested} job(s)"),
                        );
                    } else {
                        set_notice(&mut notice, "Some stop requests failed — press w");
                        warnings.extend(failures);
                    }
                    if live_filter.is_some() {
                        catalog_dirty |= refresh(
                            config,
                            &live_filter,
                            history_mode,
                            show_blocked,
                            &mut blocked_count,
                            true,
                            &mut jobs,
                            &mut ledger,
                            &mut warnings,
                        );
                        view_dirty = true;
                    }
                    selected.clear();
                    popup_frame.clear();
                }
            }
            KeyCode::Char('s') => {
                terminal::disable_raw_mode()?;
                execute!(io::stdout(), LeaveAlternateScreen, cursor::Show)?;
                let launched = crate::bank::run(config);
                terminal::enable_raw_mode()?;
                execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
                popup_frame.clear();
                if let Some(job) = launched? {
                    if let Some(session) = &auto_session {
                        let mut desired: Vec<Job> = crate::tmux::panes(session)?
                            .into_iter()
                            .map(|pane| Job {
                                cluster: pane.cluster,
                                id: pane.job_id,
                                ..Job::default()
                            })
                            .collect();
                        desired.push(job);
                        crate::tmux::reconcile(config, session, &desired, 50, show_log_warnings)?;
                    } else {
                        return Ok(PickResult {
                            jobs: vec![job],
                            show_log_warnings,
                        });
                    }
                }
            }
            KeyCode::Char('o') if live_filter.is_some() => {
                history_mode = if history_mode == 1 { 0 } else { 1 };
                set_notice(
                    &mut notice,
                    if history_mode == 1 {
                        "Recent completed jobs shown"
                    } else {
                        "Recent completed jobs hidden"
                    },
                );
                query.clear();
                catalog_dirty |= refresh(
                    config,
                    &live_filter,
                    history_mode,
                    show_blocked,
                    &mut blocked_count,
                    false,
                    &mut jobs,
                    &mut ledger,
                    &mut warnings,
                );
                view_dirty = true;
            }
            KeyCode::Char('a') if live_filter.is_some() => {
                history_mode = if history_mode == 2 { 0 } else { 2 };
                set_notice(
                    &mut notice,
                    if history_mode == 2 {
                        "Accounting archive shown"
                    } else {
                        "Live jobs shown"
                    },
                );
                query.clear();
                catalog_dirty |= refresh(
                    config,
                    &live_filter,
                    history_mode,
                    show_blocked,
                    &mut blocked_count,
                    false,
                    &mut jobs,
                    &mut ledger,
                    &mut warnings,
                );
                view_dirty = true;
            }
            KeyCode::Char('b') => {
                show_blocked = !show_blocked;
                set_notice(
                    &mut notice,
                    if show_blocked {
                        "Blocked and interactive jobs shown"
                    } else {
                        "Blocked and interactive jobs hidden"
                    },
                );
                if live_filter.is_some() {
                    catalog_dirty |= refresh(
                        config,
                        &live_filter,
                        history_mode,
                        show_blocked,
                        &mut blocked_count,
                        false,
                        &mut jobs,
                        &mut ledger,
                        &mut warnings,
                    );
                    view_dirty = true;
                }
            }
            KeyCode::Char('w') => {
                show_warnings = !show_warnings;
                set_notice(
                    &mut notice,
                    if show_warnings {
                        "Scheduler notices expanded"
                    } else {
                        "Scheduler notices collapsed"
                    },
                );
            }
            KeyCode::Char('i') if !rows.is_empty() => {
                if let Some(index) = rows[focus].job {
                    terminal::disable_raw_mode()?;
                    execute!(io::stdout(), LeaveAlternateScreen, cursor::Show)?;
                    let result =
                        crate::details::run(config, &jobs[index].cluster, &jobs[index].id, false);
                    terminal::enable_raw_mode()?;
                    execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
                    if let Err(error) = result {
                        warnings.push(format!("details: {error:#}"));
                        set_notice(&mut notice, "Details unavailable; picker kept open");
                    }
                    popup_frame.clear();
                } else {
                    expanded.insert(rows[focus].name.clone());
                    view_dirty = true;
                }
            }
            KeyCode::Char('?') => {
                show_help = true;
                help_offset = 0;
                popup_frame.clear();
            }
            KeyCode::Char('W') => {
                show_log_warnings = !show_log_warnings;
                set_notice(
                    &mut notice,
                    if show_log_warnings {
                        "Warnings included in log panes"
                    } else {
                        "Warnings hidden in log panes"
                    },
                );
            }
            KeyCode::Char('A') => {
                if let Some(session) = &auto_session {
                    crate::tmux::toggle_auto(config, session)?;
                    ledger.auto_add_default = crate::tmux::auto_enabled(session)?;
                } else {
                    ledger.auto_add_default = !ledger.auto_add_default;
                    Ledger::set_auto_add(&config.state_path, ledger.auto_add_default)?;
                }
                set_notice(
                    &mut notice,
                    if ledger.auto_add_default {
                        "Auto-add enabled"
                    } else {
                        "Auto-add disabled"
                    },
                );
            }
            KeyCode::Char('r') if live_filter.is_some() => {
                catalog_dirty |= refresh(
                    config,
                    &live_filter,
                    history_mode,
                    show_blocked,
                    &mut blocked_count,
                    true,
                    &mut jobs,
                    &mut ledger,
                    &mut warnings,
                );
                set_notice(&mut notice, "Scheduler refreshed");
                view_dirty = true;
            }
            KeyCode::Char('/') => {
                if let Some(value) = prompt_search(&query)? {
                    query = value;
                    view_dirty = true;
                }
                // The prompt temporarily occupied the status row, so force it
                // to be restored even when search was cancelled.
                popup_frame.clear();
            }
            KeyCode::Enter if !rows.is_empty() => {
                if manage && !selection_dirty {
                    toggle(&mut selected, &jobs, &rows[focus].members);
                }
                if !selected.is_empty() {
                    known_jobs.extend(jobs.iter().cloned().map(|job| (job.key(), job)));
                    let mut chosen: Vec<_> = selected
                        .iter()
                        .filter_map(|key| known_jobs.get(key).cloned())
                        .collect();
                    chosen.sort_by(|left, right| {
                        left.cluster
                            .cmp(&right.cluster)
                            .then_with(|| job_number(&right.id).cmp(&job_number(&left.id)))
                            .then_with(|| left.id.cmp(&right.id))
                    });
                    return Ok(PickResult {
                        jobs: chosen,
                        show_log_warnings,
                    });
                }
                if rows[focus].job.is_none() {
                    if !expanded.remove(&rows[focus].name) {
                        expanded.insert(rows[focus].name.clone());
                    }
                    view_dirty = true;
                } else {
                    return Ok(PickResult {
                        jobs: rows[focus]
                            .members
                            .iter()
                            .map(|&i| jobs[i].clone())
                            .collect(),
                        show_log_warnings,
                    });
                }
            }
            _ => {}
        }
        focused_key = if replaces_jobs {
            prior_focused_key
        } else {
            rows.get(focus).map(|row| row_key(row, &jobs))
        };
    }
}

fn set_notice(notice: &mut Option<(String, Instant)>, message: &str) {
    *notice = Some((
        message.to_string(),
        Instant::now() + Duration::from_millis(1_500),
    ));
}

fn prompt_search(initial: &str) -> Result<Option<String>> {
    let mut value = initial.to_string();
    let mut out = io::stdout();
    execute!(out, cursor::Show)?;
    loop {
        let (_, height) = terminal::size()?;
        execute!(
            out,
            cursor::MoveTo(0, height.saturating_sub(1)),
            terminal::Clear(ClearType::CurrentLine),
            Print(format!("Search: {value}  ·  Enter apply  ·  Esc cancel"))
        )?;
        out.flush()?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Enter => {
                    execute!(out, cursor::Hide)?;
                    return Ok(Some(value));
                }
                KeyCode::Esc => {
                    execute!(out, cursor::Hide)?;
                    return Ok(None);
                }
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char(character) if !character.is_control() => value.push(character),
                _ => {}
            },
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

fn row_key(row: &Row, jobs: &[Job]) -> String {
    row.job
        .map(|index| jobs[index].key())
        .unwrap_or_else(|| format!("group:{}", row.name))
}

#[allow(clippy::too_many_arguments)]
fn refresh(
    config: &Config,
    live: &Option<(String, String)>,
    history: u8,
    show_blocked: bool,
    blocked_count: &mut usize,
    force: bool,
    jobs: &mut Vec<Job>,
    ledger: &mut Ledger,
    warnings: &mut Vec<String>,
) -> bool {
    let Some((cluster, filter)) = live else {
        return false;
    };
    let result = if force {
        slurm::all_jobs_fresh(config, cluster, filter, history == 2)
    } else {
        slurm::all_jobs(config, cluster, filter, history == 2)
    };
    if let Ok((fresh, state, fresh_warnings)) = result {
        let every_cluster = matches!(cluster.as_str(), "all" | "both");
        let pinned: Vec<_> = jobs
            .iter()
            .filter(|job| {
                job.state == "OPEN" && (every_cluster || job.cluster.as_str() == cluster.as_str())
            })
            .cloned()
            .collect();
        let eligible = slurm::visible_jobs(fresh, &state, history, true);
        let next_blocked = eligible.iter().filter(|job| job.blocked_category()).count();
        let mut next_jobs = if show_blocked {
            eligible
        } else {
            eligible
                .into_iter()
                .filter(|job| !job.blocked_category())
                .collect()
        };
        let mut next_keys: HashSet<_> = next_jobs.iter().map(Job::key).collect();
        for job in pinned {
            if next_keys.insert(job.key()) {
                next_jobs.push(job);
            }
        }
        let changed = *jobs != next_jobs
            || *ledger != state
            || *warnings != fresh_warnings
            || *blocked_count != next_blocked;
        *jobs = next_jobs;
        *ledger = state;
        *warnings = fresh_warnings;
        *blocked_count = next_blocked;
        return changed;
    }
    false
}

fn status_rank(job: &Job) -> u8 {
    if job.running() {
        0
    } else if job.pending() {
        1
    } else if job.state.starts_with("COMPLETED") {
        2
    } else if job.state.starts_with("CANCELLED") {
        4
    } else {
        3
    }
}

fn job_number(id: &str) -> u64 {
    id.split('_')
        .next()
        .unwrap_or("0")
        .parse::<u64>()
        .unwrap_or(0)
}

fn job_matches(job: &Job, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let fields = [
        job.cluster.as_str(),
        job.id.as_str(),
        job.state.as_str(),
        job.name.as_str(),
        job.reason.as_str(),
        job.partition.as_str(),
        job.start_time.as_str(),
        job.exit_code.as_str(),
        job.alloc_tres.as_str(),
    ];
    if needle.is_ascii() {
        let needle = needle.as_bytes();
        fields.iter().any(|field| {
            field
                .as_bytes()
                .windows(needle.len())
                .any(|window| window.eq_ignore_ascii_case(needle))
        })
    } else {
        fields
            .iter()
            .any(|field| field.to_lowercase().contains(needle))
    }
}

fn grouped_rows(jobs: &[Job], indices: &[usize], expanded: &HashSet<String>) -> Vec<Row> {
    let mut groups: HashMap<&str, Vec<(usize, u8, u64)>> = HashMap::with_capacity(indices.len());
    for &index in indices {
        let job = &jobs[index];
        let base_id = job_number(&job.id);
        groups
            .entry(&job.name)
            .or_default()
            .push((index, status_rank(job), base_id));
    }
    let mut groups: Vec<_> = groups
        .into_iter()
        .map(|(name, members)| {
            let best_rank = members.iter().map(|member| member.1).min().unwrap_or(9);
            let newest = members.iter().map(|member| member.2).max().unwrap_or(0);
            (name, members, best_rank, newest)
        })
        .collect();
    groups.sort_unstable_by(|left, right| {
        (left.2, std::cmp::Reverse(left.3), left.0).cmp(&(
            right.2,
            std::cmp::Reverse(right.3),
            right.0,
        ))
    });
    let mut rows = Vec::new();
    for (name, mut members, _, _) in groups {
        members.sort_unstable_by_key(|member| (member.1, std::cmp::Reverse(member.2)));
        let members: Vec<_> = members.into_iter().map(|member| member.0).collect();
        let name = name.to_string();
        if members.len() == 1 {
            rows.push(Row {
                name,
                job: Some(members[0]),
                members,
                nested: false,
                expanded: false,
            });
        } else {
            let is_expanded = expanded.contains(&name);
            rows.push(Row {
                name: name.clone(),
                job: None,
                members: members.clone(),
                nested: false,
                expanded: is_expanded,
            });
            if is_expanded {
                for member in members {
                    rows.push(Row {
                        name: name.clone(),
                        job: Some(member),
                        members: vec![member],
                        nested: true,
                        expanded: false,
                    });
                }
            }
        }
    }
    rows
}

fn toggle(selected: &mut HashSet<String>, jobs: &[Job], members: &[usize]) {
    let keys: Vec<_> = members.iter().map(|&i| jobs[i].key()).collect();
    if keys.iter().all(|key| selected.contains(key)) {
        for key in keys {
            selected.remove(&key);
        }
    } else {
        selected.extend(keys);
    }
}

fn cycle_cluster(config: &Config, current: &str, backwards: bool) -> String {
    if config.clusters.len() <= 1 {
        return config.clusters[0].name.clone();
    }
    let choices: Vec<_> = std::iter::once("all")
        .chain(config.clusters.iter().map(|cluster| cluster.name.as_str()))
        .collect();
    let normalized = if current == "both" { "all" } else { current };
    let position = choices
        .iter()
        .position(|choice| *choice == normalized)
        .unwrap_or(0);
    let next = if backwards {
        position.checked_sub(1).unwrap_or(choices.len() - 1)
    } else {
        (position + 1) % choices.len()
    };
    choices[next].to_string()
}

fn help_lines() -> &'static [&'static str] {
    &[
        "NAVIGATION",
        "  ↑↓ / j k          Move one row",
        "  PgUp / PgDn       Move ten rows",
        "  g / G             First / last row",
        "  → / l, ← / h      Expand / collapse group",
        "",
        "SELECTION & ACTIONS",
        "  Space             Toggle focused job; on a group, toggle every run",
        "  v                 Select every filtered job",
        "  c                 Clear the selection",
        "  Enter             Open selection; otherwise open the focused job/group",
        "  d                 Dismiss selected finished jobs (active jobs stay visible)",
        "  x                 Stop focused/selected active jobs after confirmation",
        "  s                 Open the recursive sbatch script bank",
        "",
        "VIEWS & FILTERS",
        "  Tab / Shift-Tab   Cycle configured clusters forward / backward",
        "  o                 Toggle jobs ended within 20 minutes",
        "  a                 Toggle bounded archive (dismissed jobs included)",
        "  b                 Toggle blocked and interactive jobs",
        "  r                 Refresh the scheduler now",
        "  /                 Search all displayed fields; Enter applies, Esc cancels",
        "  Esc               Clear search; press again to leave the picker",
        "  w                 Toggle scheduler notices",
        "  W                 Include warnings in opened log panes",
        "  i                 Open live details; on a collapsed group, expand it",
        "  A                 Auto-add submitted jobs when they start",
        "  ?                 Open / close this reference",
        "  q                 Quit without opening anything",
        "",
        "WORKSPACE (tmux prefix: Ctrl-b)",
        "  j                 Manage panes: selection becomes the exact open pane set",
        "  i                 Toggle details for the focused log",
        "  A                 Toggle auto-add",
        "  x                 Close the current pane",
        "  z                 Zoom / restore the current pane",
        "  q                 Close the workspace",
        "  Mouse drag        Keep a text selection",
        "  Right-click       Copy the selection",
        "  Left-click        Clear the selection",
    ]
}

fn draw_help(offset: usize, previous: &mut Vec<String>) -> Result<()> {
    let (_, height) = terminal::size()?;
    let mut frame = vec![String::new(); height as usize];
    if height == 0 {
        return Ok(());
    }
    frame[0] = "slurm-log command reference".into();
    let body_height = height.saturating_sub(2) as usize;
    for (slot, line) in frame
        .iter_mut()
        .skip(1)
        .take(body_height)
        .zip(help_lines().iter().skip(offset))
    {
        *slot = (*line).into();
    }
    frame[height as usize - 1] = "↑↓/PgUp/PgDn scroll  ·  ? or Esc return  ·  q quit".into();
    let mut out = Vec::new();
    for (line, content) in frame.iter().enumerate() {
        if previous.get(line) == Some(content) {
            continue;
        }
        execute!(
            out,
            cursor::MoveTo(0, line as u16),
            SetAttribute(Attribute::Reset),
            Print(content),
            terminal::Clear(ClearType::UntilNewLine)
        )?;
    }
    let mut stdout = io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;
    *previous = frame;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_popup(
    jobs: &[Job],
    rows: &[Row],
    focus: usize,
    selected: &HashSet<String>,
    manage: bool,
    history: u8,
    auto_add: bool,
    blocked: bool,
    log_warnings: bool,
    query: &str,
    warnings: &[String],
    show_warnings: bool,
    cluster: &str,
    notice: &str,
    blocked_count: usize,
    previous: &mut Vec<String>,
) -> Result<()> {
    let (width, height) = terminal::size()?;
    let available = height.saturating_sub(4) as usize;
    let top = focus.saturating_sub(available.saturating_sub(1));
    let (header, _, _) = header_lines(manage, history, auto_add, blocked, log_warnings, cluster);
    let shortcuts = compact_commands(manage).0;
    // Collapsing a group turns its former child rows into blanks. Those blanks
    // still need to overwrite the full popup width; an empty write leaves the
    // previous terminal cells behind, while EL makes tmux visibly flicker.
    let mut frame = vec![fit_popup_line("", width); height as usize];
    frame[0] = header;
    frame[1] = shortcuts;
    frame[2] = "    CLUSTER  JOB ID / RUNS   STATE               ELAPSED     NAME".into();
    for (screen_row, (row_index, row)) in rows
        .iter()
        .enumerate()
        .skip(top)
        .take(available)
        .enumerate()
    {
        frame[screen_row + 3] = if let Some(index) = row.job {
            let job = &jobs[index];
            let text = format!(
                "{}{}{}  {:<7} {:<15} {:<19} {:<11} {}",
                if row_index == focus { ">" } else { " " },
                if selected.contains(&job.key()) {
                    "*"
                } else {
                    " "
                },
                if row.nested { "  " } else { "" },
                job.cluster,
                job.id,
                job.state,
                job.elapsed,
                display_name(job)
            );
            let color = if job.running() {
                32
            } else if job.pending() {
                33
            } else if job.state.starts_with("COMPLETED") {
                36
            } else {
                31
            };
            popup_styled(
                fit_popup_line(&text, width),
                Some(color),
                row_index == focus,
            )
        } else {
            let chosen = row
                .members
                .iter()
                .all(|&i| selected.contains(&jobs[i].key()));
            popup_styled(
                fit_popup_line(&group_row_text(row, chosen, row_index == focus), width),
                Some(90),
                row_index == focus,
            )
        };
    }
    let warning = if warnings.is_empty() {
        String::new()
    } else if show_warnings {
        format!(" | {}", warnings.join("; "))
    } else {
        format!(" | ⚠ {} warning(s) — press w", warnings.len())
    };
    if height > 0 {
        frame[height as usize - 1] = popup_styled(
            fit_popup_line(
                &format!(
                    "{}{}",
                    footer_text(rows.len(), selected.len(), query, &warning, notice),
                    blocked_summary(blocked_count, blocked)
                ),
                width,
            ),
            Some(90),
            false,
        );
    }
    let mut out = Vec::new();
    for (line, content) in frame.iter().enumerate() {
        if previous.get(line) == Some(content) {
            continue;
        }
        execute!(
            out,
            cursor::MoveTo(0, line as u16),
            SetAttribute(Attribute::Reset),
            Print(content)
        )?;
        // The first frame may inherit contents from the popup's terminal.
        // Later frames are already padded to the full width: avoiding EL here
        // prevents tmux from repainting the popup surface on every key press.
        if previous.get(line).is_none() {
            execute!(out, terminal::Clear(ClearType::UntilNewLine))?;
        }
    }
    let mut stdout = io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;
    *previous = frame;
    Ok(())
}

fn fit_popup_line(text: &str, width: u16) -> String {
    let width = width as usize;
    let mut line: String = text.chars().take(width).collect();
    let used = line.chars().count();
    line.extend(std::iter::repeat_n(' ', width.saturating_sub(used)));
    line
}

fn popup_styled(text: String, color: Option<u8>, focused: bool) -> String {
    let mut prefix = String::new();
    if focused {
        prefix.push_str("\x1b[7m");
    }
    if let Some(color) = color {
        prefix.push_str(&format!("\x1b[{color}m"));
    }
    if prefix.is_empty() {
        text
    } else {
        format!("{prefix}{text}\x1b[0m")
    }
}

fn group_row_text(row: &Row, selected: bool, focused: bool) -> String {
    format!(
        "{}{}  {}  {:>3} runs  ·  {}",
        if focused { ">" } else { " " },
        if selected { "*" } else { " " },
        if row.expanded { "▾" } else { "▸" },
        row.members.len(),
        row.name
    )
}

fn header_lines(
    manage: bool,
    history: u8,
    auto_add: bool,
    blocked: bool,
    log_warnings: bool,
    cluster: &str,
) -> (String, String, String) {
    let mode = ["LIVE ≤2m", "OLD ≤20m", "ARCHIVE"][history as usize];
    (
        format!(
            "slurm-log  ·  {mode}  ·  cluster {}  ·  auto {}  ·  blocked {}  ·  log warnings {}",
            if matches!(cluster, "all" | "both") {
                "ALL"
            } else {
                cluster
            },
            if auto_add { "on" } else { "off" },
            if blocked { "shown" } else { "hidden" },
            if log_warnings { "shown" } else { "hidden" }
        ),
        format!(
            "MOVE ↑↓/j/k  ·  MARK Space  ·  {} Enter  ·  CLUSTER Tab/Shift-Tab  ·  SEARCH /  ·  HELP ?",
            if manage { "APPLY" } else { "OPEN" }
        ),
        "VIEWS o recent · a archive · b blocked  ·  ACTIONS A auto-add · s scripts · x stop · d dismiss · q quit".into(),
    )
}

fn confirm_cancel(jobs: &[Job]) -> Result<bool> {
    let (_, height) = terminal::size()?;
    let mut out = Vec::new();
    execute!(
        out,
        cursor::MoveTo(0, 0),
        terminal::Clear(ClearType::All),
        SetAttribute(Attribute::Bold),
        Print(format!("STOP {} ACTIVE JOB(S)?\r\n\r\n", jobs.len())),
        SetAttribute(Attribute::Reset)
    )?;
    for job in jobs.iter().take(height.saturating_sub(5) as usize) {
        execute!(
            out,
            Print(format!(
                "  {}:{}  {:<12} {}\r\n",
                job.cluster, job.id, job.state, job.name
            ))
        )?;
    }
    if jobs.len() > height.saturating_sub(5) as usize {
        execute!(
            out,
            Print(format!(
                "  … and {} more\r\n",
                jobs.len() - height.saturating_sub(5) as usize
            ))
        )?;
    }
    execute!(out, Print("\r\nPress y to request scancel · n/Esc returns"))?;
    io::stdout().write_all(&out)?;
    io::stdout().flush()?;
    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if let Some(choice) = cancel_confirmation_choice(key.code) {
                    return Ok(choice);
                }
            }
            _ => {}
        }
    }
}

fn cancel_confirmation_choice(code: KeyCode) -> Option<bool> {
    match code {
        KeyCode::Char('y' | 'Y') => Some(true),
        KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => Some(false),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn draw(
    jobs: &[Job],
    rows: &[Row],
    focus: usize,
    selected: &HashSet<String>,
    manage: bool,
    history: u8,
    auto_add: bool,
    blocked: bool,
    show_warnings: bool,
    log_warnings: bool,
    query: &str,
    warnings: &[String],
    cluster: &str,
    notice: &str,
    blocked_count: usize,
) -> Result<()> {
    // Assemble the frame before writing it. Do not use synchronized-update
    // escapes here: tmux popups can repaint their border for those sequences.
    let mut out = Vec::new();
    let (width, height) = terminal::size()?;
    let (header, mut primary, mut secondary) =
        header_lines(manage, history, auto_add, blocked, log_warnings, cluster);
    if width < 110 {
        (primary, secondary) = compact_commands(manage);
    }
    execute!(
        out,
        cursor::MoveTo(0, 0),
        SetAttribute(Attribute::Bold),
        Print(clip_line(&header, width)),
        terminal::Clear(ClearType::UntilNewLine),
        Print("\r\n"),
        Print(clip_line(&primary, width)),
        terminal::Clear(ClearType::UntilNewLine),
        Print("\r\n"),
        Print(clip_line(&secondary, width)),
        terminal::Clear(ClearType::UntilNewLine),
        Print("\r\n"),
        SetAttribute(Attribute::Reset),
        Print("    CLUSTER  JOB ID / RUNS   STATE               ELAPSED     NAME"),
        terminal::Clear(ClearType::UntilNewLine),
        Print("\r\n")
    )?;
    let available = height.saturating_sub(5) as usize;
    let top = focus.saturating_sub(available.saturating_sub(1));
    for (row_index, row) in rows.iter().enumerate().skip(top).take(available) {
        let focused = row_index == focus;
        if focused {
            execute!(out, SetAttribute(Attribute::Reverse))?;
        }
        if let Some(index) = row.job {
            let job = &jobs[index];
            let color = if job.running() {
                Color::Green
            } else if job.pending() {
                Color::Yellow
            } else if job.state.starts_with("COMPLETED") {
                Color::Cyan
            } else {
                Color::Red
            };
            execute!(
                out,
                SetForegroundColor(color),
                Print(format!(
                    "{}{}{}  {:<7} {:<15} {:<19} {:<11} {}",
                    if focused { ">" } else { " " },
                    if selected.contains(&job.key()) {
                        "*"
                    } else {
                        " "
                    },
                    if row.nested { "  " } else { "" },
                    job.cluster,
                    job.id,
                    job.state,
                    job.elapsed,
                    display_name(job)
                )),
                ResetColor,
                terminal::Clear(ClearType::UntilNewLine),
                Print("\r\n")
            )?;
        } else {
            let chosen = row
                .members
                .iter()
                .all(|&i| selected.contains(&jobs[i].key()));
            execute!(
                out,
                SetForegroundColor(Color::DarkGrey),
                Print(group_row_text(row, chosen, focused)),
                ResetColor,
                terminal::Clear(ClearType::UntilNewLine),
                Print("\r\n")
            )?;
        }
        execute!(out, SetAttribute(Attribute::Reset))?;
    }
    // Remove rows left behind when collapsing a group or switching to a
    // shorter result set, without blanking rows that are still visible.
    for _ in rows.iter().skip(top).take(available).count()..available {
        execute!(out, terminal::Clear(ClearType::UntilNewLine), Print("\r\n"))?;
    }
    let warning = if warnings.is_empty() {
        String::new()
    } else if show_warnings {
        format!(" | {}", warnings.join("; "))
    } else {
        format!(" | ⚠ {} warning(s) — press w", warnings.len())
    };
    execute!(
        out,
        cursor::MoveTo(0, height.saturating_sub(1)),
        Print(footer_text(
            rows.len(),
            selected.len(),
            query,
            &warning,
            notice,
        )),
        SetForegroundColor(Color::DarkGrey),
        Print(blocked_summary(blocked_count, blocked)),
        ResetColor,
        terminal::Clear(ClearType::UntilNewLine)
    )?;
    let mut stdout = io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;
    Ok(())
}

fn blocked_summary(count: usize, shown: bool) -> String {
    format!(
        " | blocked: {count} ({})",
        if shown { "shown" } else { "b to show" }
    )
}

fn footer_text(rows: usize, selected: usize, query: &str, warning: &str, notice: &str) -> String {
    format!(
        "{rows} rows, {selected} selected{}{}{}",
        if query.is_empty() {
            String::new()
        } else {
            format!(" | search={query:?}")
        },
        warning,
        if notice.is_empty() {
            String::new()
        } else {
            format!(" | {notice}")
        }
    )
}

fn compact_commands(manage: bool) -> (String, String) {
    (
        format!(
            "↑↓ move · Space mark · Enter {} · Tab cluster · / search · ? help",
            if manage { "apply" } else { "open" }
        ),
        "o recent · a archive · b blocked · A auto · s scripts · x stop · d dismiss · q quit"
            .into(),
    )
}

fn clip_line(text: &str, width: u16) -> String {
    text.chars()
        .take(width.saturating_sub(1) as usize)
        .collect()
}

fn display_name(job: &Job) -> String {
    // Pending explanations can include a reason and a full timestamp. They are
    // useful in details, but make the picker row look like part of the job name.
    if job.pending() {
        return job.name.clone();
    }
    let insight = job.insight();
    if insight.is_empty() {
        job.name.clone()
    } else {
        format!("{} — {insight}", job.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClusterConfig;
    use std::path::PathBuf;
    fn job(id: &str, name: &str) -> Job {
        Job {
            cluster: "cispa".into(),
            id: id.into(),
            state: "COMPLETED".into(),
            name: name.into(),
            ..Job::default()
        }
    }

    fn multi_cluster_config() -> Config {
        Config {
            local_user: "alice".into(),
            remote_user: "alice".into(),
            ssh_host: String::new(),
            state_path: PathBuf::from("/tmp/slurm-log-ui-test.json"),
            executable: PathBuf::from("slurm-log"),
            sbatch_banks: Vec::new(),
            clusters: ["sprint", "cispa"]
                .into_iter()
                .map(|name| ClusterConfig {
                    name: name.into(),
                    transport: "local".into(),
                    user: "alice".into(),
                    ssh_host: String::new(),
                    working_directory: PathBuf::from("/tmp"),
                    accounting: true,
                })
                .collect(),
        }
    }

    #[test]
    fn cluster_cycle_includes_all_and_wraps_in_both_directions() {
        let config = multi_cluster_config();
        assert_eq!(cycle_cluster(&config, "both", false), "sprint");
        assert_eq!(cycle_cluster(&config, "sprint", false), "cispa");
        assert_eq!(cycle_cluster(&config, "cispa", false), "all");
        assert_eq!(cycle_cluster(&config, "all", true), "cispa");
    }
    #[test]
    fn groups_collapse_and_expand_by_job_name() {
        let jobs = vec![job("12", "train"), job("11", "train"), job("10", "eval")];
        let indices = vec![0, 1, 2];
        let collapsed = grouped_rows(&jobs, &indices, &HashSet::new());
        assert_eq!(collapsed.len(), 2);
        assert!(
            collapsed
                .iter()
                .any(|row| row.name == "train" && row.job.is_none() && row.members.len() == 2)
        );
        let expanded = grouped_rows(&jobs, &indices, &HashSet::from(["train".into()]));
        assert_eq!(expanded.len(), 4);
        assert!(
            expanded
                .iter()
                .any(|row| row.name == "train" && row.expanded)
        );
        assert_eq!(expanded.iter().filter(|row| row.nested).count(), 2);
        assert!(
            expanded
                .iter()
                .filter(|row| row.nested)
                .all(|row| row.job.is_some())
        );
    }

    #[test]
    fn popup_styles_keep_color_and_focus_in_one_incremental_line() {
        assert_eq!(
            popup_styled("job".into(), Some(32), false),
            "\x1b[32mjob\x1b[0m"
        );
        assert_eq!(
            popup_styled("job".into(), Some(33), true),
            "\x1b[7m\x1b[33mjob\x1b[0m"
        );
    }

    #[test]
    fn compact_rows_overwrite_the_width_without_clear_sequences() {
        assert_eq!(fit_popup_line("job", 6), "job   ");
        assert_eq!(fit_popup_line("archive", 4), "arch");
        assert_eq!(fit_popup_line("", 3), "   ");
        assert_eq!(clip_line("abcdef", 5), "abcd");
        let (primary, secondary) = compact_commands(true);
        assert!(primary.contains("Enter apply"));
        assert!(primary.contains("? help"));
        assert!(secondary.contains("b blocked"));
        assert!(secondary.contains("A auto"));
    }

    #[test]
    fn pending_picker_name_does_not_append_scheduler_explanation() {
        let pending = Job {
            state: "PENDING".into(),
            name: "training".into(),
            reason: "Resources".into(),
            start_time: "2026-08-11T18:00:00".into(),
            ..Job::default()
        };
        assert_eq!(display_name(&pending), "training");
        assert!(pending.insight().contains("estimated start"));
    }

    #[test]
    fn field_search_is_case_insensitive_without_joining_fields() {
        let mut value = job("42", "MixedCaseExperiment");
        value.partition = "GPU-Long".into();
        assert!(job_matches(&value, "mixedcase"));
        assert!(job_matches(&value, "gpu-long"));
        assert!(!job_matches(&value, "not-present"));
    }

    #[test]
    #[ignore = "release-mode performance budget"]
    fn searches_one_hundred_thousand_jobs_within_budget() {
        let jobs: Vec<_> = (0..100_000)
            .map(|id| job(&id.to_string(), &format!("experiment-{}", id % 500)))
            .collect();
        let started = Instant::now();
        let matches = jobs
            .iter()
            .filter(|job| job_matches(job, "experiment-499"))
            .count();
        assert_eq!(matches, 200);
        assert!(started.elapsed() < Duration::from_millis(150));
    }

    #[test]
    fn compact_header_points_to_static_shortcuts_page() {
        let (header, primary, secondary) = header_lines(false, 2, true, false, false, "cispa");
        assert!(header.contains("ARCHIVE"));
        assert!(header.contains("cluster cispa"));
        assert!(primary.contains("CLUSTER Tab/Shift-Tab"));
        assert!(primary.contains("HELP ?"));
        assert!(secondary.contains("b blocked"));
        assert!(secondary.contains("A auto-add"));
        assert!(secondary.contains("d dismiss"));
    }

    #[test]
    fn toggle_notices_are_explicit_and_expire_after_about_1500ms() {
        let before = Instant::now();
        let mut notice = None;
        set_notice(&mut notice, "Blocked and interactive jobs shown");
        let (message, expires) = notice.unwrap();
        assert_eq!(message, "Blocked and interactive jobs shown");
        let lifetime = expires.saturating_duration_since(before);
        assert!(lifetime >= Duration::from_millis(1_500));
        assert!(lifetime < Duration::from_millis(1_550));
        assert!(footer_text(4, 1, "", "", &message).contains(&message));
    }

    #[test]
    fn blocked_count_summary_is_quiet_and_actionable() {
        assert_eq!(blocked_summary(3, false), " | blocked: 3 (b to show)");
        assert_eq!(blocked_summary(3, true), " | blocked: 3 (shown)");
    }

    #[test]
    fn stop_confirmation_ignores_non_decision_events() {
        assert_eq!(cancel_confirmation_choice(KeyCode::Char('y')), Some(true));
        assert_eq!(cancel_confirmation_choice(KeyCode::Esc), Some(false));
        assert_eq!(cancel_confirmation_choice(KeyCode::Char('a')), None);
    }

    #[test]
    fn group_rows_are_quiet_and_do_not_use_the_old_group_badge() {
        let row = Row {
            name: "training".into(),
            job: None,
            members: vec![0, 1, 2],
            nested: false,
            expanded: true,
        };
        let text = group_row_text(&row, false, false);
        assert_eq!(text, "    ▾    3 runs  ·  training");
        assert!(!text.contains("GROUP"));
    }

    #[test]
    fn help_reference_covers_picker_and_workspace_controls() {
        let help = help_lines().join("\n").to_ascii_lowercase();
        for topic in [
            "navigation",
            "selection",
            "views",
            "workspace",
            "auto-add",
            "right-click",
        ] {
            assert!(help.contains(topic), "missing help topic: {topic}");
        }
    }

    #[test]
    fn help_reference_aligns_every_command_description() {
        for line in help_lines().iter().filter(|line| line.starts_with("  ")) {
            assert_eq!(
                line.chars().nth(19),
                Some(' '),
                "missing key/description gutter: {line:?}"
            );
            assert!(
                line.chars()
                    .nth(20)
                    .is_some_and(|character| character != ' '),
                "description does not begin in column 21: {line:?}"
            );
        }
    }

    #[test]
    fn help_reference_does_not_merge_distinct_actions() {
        let help = help_lines().join("\n");
        for command in [
            "  b                 Toggle blocked and interactive jobs",
            "  r                 Refresh the scheduler now",
            "  w                 Toggle scheduler notices",
            "  W                 Include warnings in opened log panes",
            "  x                 Close the current pane",
            "  z                 Zoom / restore the current pane",
        ] {
            assert!(help.contains(command), "missing command: {command}");
        }
    }

    #[test]
    fn grouping_order_is_deterministic_for_equal_statuses() {
        let jobs = vec![job("10", "zeta"), job("10", "alpha"), job("10", "middle")];
        let indices = vec![0, 1, 2];
        let first: Vec<_> = grouped_rows(&jobs, &indices, &HashSet::new())
            .into_iter()
            .map(|row| row.name)
            .collect();
        let second: Vec<_> = grouped_rows(&jobs, &indices, &HashSet::new())
            .into_iter()
            .map(|row| row.name)
            .collect();
        assert_eq!(first, second);
        assert_eq!(first, ["alpha", "middle", "zeta"]);
    }

    #[test]
    #[ignore = "release-mode performance budget"]
    fn groups_twenty_thousand_archive_jobs_within_budget() {
        let jobs: Vec<_> = (0..20_000)
            .map(|id| job(&id.to_string(), &format!("experiment-{}", id % 200)))
            .collect();
        let indices: Vec<_> = (0..jobs.len()).collect();
        let started = Instant::now();
        let rows = grouped_rows(&jobs, &indices, &HashSet::new());
        assert_eq!(rows.len(), 200);
        assert!(started.elapsed() < Duration::from_millis(100));
    }
}
