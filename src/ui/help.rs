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
    if height == 0 {
        return Ok(());
    }
    let header = picker_header_lines(
        width,
        manage,
        history,
        auto_add,
        blocked,
        log_warnings,
        cluster,
        blocked_count,
    );
    let header_rows = header.len().min(height.saturating_sub(2) as usize);
    let table_row = header_rows;
    let data_row = table_row + 1;
    let available = height.saturating_sub(data_row as u16 + 1) as usize;
    let top = focus.saturating_sub(available.saturating_sub(1));
    // Collapsing a group turns its former child rows into blanks. Those blanks
    // still need to overwrite the full popup width; an empty write leaves the
    // previous terminal cells behind, while EL makes tmux visibly flicker.
    let mut frame = vec![fit_popup_line("", width); height as usize];
    for (target, line) in frame.iter_mut().zip(header).take(header_rows) {
        *target = fit_popup_line(&line, width);
    }
    frame[table_row] = fit_popup_line(
        "    CLUSTER  JOB ID / RUNS   STATE               ELAPSED     NAME",
        width,
    );
    for (screen_row, (row_index, row)) in rows
        .iter()
        .enumerate()
        .skip(top)
        .take(available)
        .enumerate()
    {
        frame[screen_row + data_row] = popup_row(jobs, row, row_index, focus, selected, width);
    }
    let warning = popup_warning(warnings, show_warnings);
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

fn popup_row(
    jobs: &[Job],
    row: &Row,
    row_index: usize,
    focus: usize,
    selected: &HashSet<String>,
    width: u16,
) -> String {
    if let Some(index) = row.job {
        let job = &jobs[index];
        let text = format!(
            "{}{}{}  {:<7} {:<15} {:<19} {:<11} {}",
            if row_index == focus { ">" } else { " " },
            if selected.contains(&job.key()) { "*" } else { " " },
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
        popup_styled(fit_popup_line(&text, width), Some(color), row_index == focus)
    } else {
        let chosen = row
            .members
            .iter()
            .all(|&index| selected.contains(&jobs[index].key()));
        popup_styled(
            fit_popup_line(&group_row_text(row, chosen, row_index == focus), width),
            Some(90),
            row_index == focus,
        )
    }
}

fn popup_warning(warnings: &[String], show: bool) -> String {
    if warnings.is_empty() {
        String::new()
    } else if show {
        format!(" | {}", warnings.join("; "))
    } else {
        format!(" | ⚠ {} warning(s) — press w", warnings.len())
    }
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

include!("header.rs");

fn confirm_cancel(jobs: &[Job]) -> Result<bool> {
    let (_, height) = terminal::size()?;
    let out = cancel_frame(jobs, height)?;
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

fn cancel_frame(jobs: &[Job], height: u16) -> Result<Vec<u8>> {
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
    Ok(out)
}

fn cancel_confirmation_choice(code: KeyCode) -> Option<bool> {
    match code {
        KeyCode::Char('y' | 'Y') => Some(true),
        KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => Some(false),
        _ => None,
    }
}
