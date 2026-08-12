{
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
                // Stopping is deliberately cursor-scoped. Space marks are a
                // pane-opening selection and must never broaden a destructive
                // scheduler action. A collapsed group has no single focused
                // job, so require the user to expand it first.
                let targets: Vec<Job> = rows[focus]
                    .job
                    .into_iter()
                    .map(|index| jobs[index].clone())
                    .filter(Job::active)
                    .collect();
                if targets.is_empty() {
                    set_notice(
                        &mut notice,
                        if rows[focus].job.is_none() {
                            "Expand the group and focus one job to stop it"
                        } else {
                            "Focused job is not active"
                        },
                    );
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
