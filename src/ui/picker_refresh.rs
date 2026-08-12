{
        if notice
            .as_ref()
            .is_some_and(|(_, expires)| Instant::now() >= *expires)
        {
            notice = None;
            redraw = true;
        }
        let refresh_interval = if history_mode.scheduler_archive() {
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
            remember_selected(&mut known_jobs, &jobs, &selected);
            catalog_dirty = false;
        }
        if manage && view_dirty {
            let active_cluster = live_filter
                .as_ref()
                .map_or("all", |(cluster, _)| cluster.as_str());
            restore_selected(&mut jobs, &known_jobs, &selected, active_cluster);
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
        key
    }
