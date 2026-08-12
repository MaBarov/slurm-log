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
    execute!(out, cursor::MoveTo(0, 0))?;
    for (index, line) in header.iter().take(header_rows).enumerate() {
        execute!(
            out,
            SetAttribute(if index == 0 {
                Attribute::Bold
            } else {
                Attribute::Reset
            }),
            Print(clip_line(line, width)),
            terminal::Clear(ClearType::UntilNewLine),
            Print("\r\n")
        )?;
    }
    execute!(
        out,
        SetAttribute(Attribute::Bold),
        Print("    CLUSTER  JOB ID / RUNS   STATE               ELAPSED     NAME"),
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
