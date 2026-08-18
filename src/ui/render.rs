#[allow(clippy::too_many_arguments)]
fn draw(
    jobs: &[Job],
    rows: &[Row],
    focus: usize,
    selected: &HashSet<String>,
    manage: bool,
    history: HistoryMode,
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
    let header = picker_header_lines(
        width,
        height,
        manage,
        history,
        auto_add,
        blocked,
        log_warnings,
        cluster,
        blocked_count,
    );
    if header.too_small {
        return draw_too_small(&header, width, height);
    }
    let header_rows = header.lines.len().min(height.saturating_sub(2) as usize);
    let table = TableLayout::new(width, cluster);
    execute!(out, cursor::MoveTo(0, 0))?;
    for line in header.lines.iter().take(header_rows) {
        execute!(
            out,
            SetAttribute(Attribute::Reset),
            Print(line.render(width.saturating_sub(1) as usize, false)),
            terminal::Clear(ClearType::UntilNewLine),
            Print("\r\n")
        )?;
    }
    execute!(
        out,
        SetAttribute(Attribute::Bold),
        Print(table.header()),
        terminal::Clear(ClearType::UntilNewLine),
        Print("\r\n"),
        SetAttribute(Attribute::Reset)
    )?;
    let available = height.saturating_sub(header_rows as u16 + 2) as usize;
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
                Print(table.job(
                    job,
                    focused,
                    selected.contains(&job.key()),
                    row.nested,
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
                Print(compact_group_row(row, chosen, focused, width)),
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
        SetForegroundColor(Color::DarkGrey),
        Print(footer_line(
            width,
            rows.len(),
            selected.len(),
            query,
            &warning,
            notice,
            blocked_count,
            blocked,
        )),
        ResetColor,
        terminal::Clear(ClearType::UntilNewLine)
    )?;
    let mut stdout = io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;
    Ok(())
}

fn draw_too_small(header: &HeaderLayout, width: u16, height: u16) -> Result<()> {
    let usable = width.saturating_sub(1) as usize;
    let mut out = Vec::new();
    execute!(out, cursor::MoveTo(0, 0))?;
    for row in 0..height {
        execute!(out, cursor::MoveTo(0, row), terminal::Clear(ClearType::CurrentLine))?;
    }
    if let Some(message) = header.lines.first() {
        execute!(
            out,
            cursor::MoveTo(0, 0),
            Print(message.render(usable, false))
        )?;
    }
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
    let selection = if selected > 0 {
        format!("{selected} selected (Enter open · c clear)")
    } else {
        "0 selected".to_string()
    };
    format!(
        "{rows} rows, {selection}{}{}{}",
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

#[allow(clippy::too_many_arguments)]
fn footer_line(
    width: u16,
    rows: usize,
    selected: usize,
    query: &str,
    warning: &str,
    notice: &str,
    blocked_count: usize,
    blocked: bool,
) -> String {
    // Keep the actionable blocked count ahead of verbose scheduler warnings.
    // Most importantly, never write into the terminal's final column: doing so
    // can auto-wrap the last row and scroll the picker header off-screen.
    let mut line = footer_text(rows, selected, query, "", notice);
    line.push_str(&blocked_summary(blocked_count, blocked));
    line.push_str(warning);
    truncate_display(&line, width.saturating_sub(1) as usize)
}

#[cfg(test)]
fn clip_line(text: &str, width: u16) -> String {
    truncate_display(text, width.saturating_sub(1) as usize)
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
