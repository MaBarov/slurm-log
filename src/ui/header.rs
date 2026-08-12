fn span(text: impl Into<String>, tone: HeaderTone) -> HeaderLine {
    let mut line = HeaderLine::default();
    line.push(text, tone);
    line
}

fn join(parts: Vec<HeaderLine>, separator: &str) -> HeaderLine {
    let mut line = HeaderLine::default();
    for (index, part) in parts.into_iter().enumerate() {
        if index > 0 {
            line.push(separator, HeaderTone::Muted);
        }
        line.append(part);
    }
    line
}

fn chip(key: &str, values: &[(&str, HeaderTone)]) -> HeaderLine {
    let mut line = HeaderLine::default();
    line.push("[", HeaderTone::Muted);
    line.push(key, HeaderTone::Key);
    for (value, tone) in values {
        line.push(" ", HeaderTone::Muted);
        line.push(*value, *tone);
    }
    line.push("]", HeaderTone::Muted);
    line
}

fn keyed(key: &str, values: &[(&str, HeaderTone)]) -> HeaderLine {
    let mut line = HeaderLine::default();
    line.push("[", HeaderTone::Muted);
    line.push(key, HeaderTone::Key);
    line.push("]", HeaderTone::Muted);
    for (value, tone) in values {
        line.push(" ", HeaderTone::Muted);
        line.push(*value, *tone);
    }
    line
}

fn labelled(label: &str, content: HeaderLine) -> HeaderLine {
    let mut line = HeaderLine::default();
    line.push(format!("{label:<8}"), HeaderTone::Label);
    line.append(content);
    line
}

fn command(key: &str, action: &str) -> HeaderLine {
    let mut line = HeaderLine::default();
    line.push(key, HeaderTone::Key);
    if !action.is_empty() {
        line.push(format!(" {action}"), HeaderTone::Plain);
    }
    line
}

fn command_rows(width: usize, manage: bool, labelled_rows: bool) -> Vec<HeaderLine> {
    let mut commands = vec![
        command("↑↓", "move"),
        command("Space", "mark"),
        command("Enter", if manage { "apply" } else { "open" }),
        command("/", "find"),
        command("?", "help"),
        command("q", "quit"),
    ];
    let prefix = if labelled_rows { "KEYS    " } else { "" };
    let continuation = " ".repeat(UnicodeWidthStr::width(prefix));
    let separator = if labelled_rows { " · " } else { "  ·  " };
    let full = labelled("KEYS", join(commands.clone(), separator));
    if labelled_rows && full.width() <= width {
        return vec![full];
    }
    if labelled_rows {
        let secondary = commands.split_off(3);
        return vec![
            labelled("KEYS", join(commands, separator)),
            {
                let mut row = span(continuation, HeaderTone::Muted);
                row.append(join(secondary, separator));
                row
            },
        ];
    }
    vec![join(commands, separator)]
}

fn normalized_cluster(cluster: &str, max_width: usize) -> String {
    let cluster = if matches!(cluster, "all" | "both") {
        "ALL"
    } else {
        cluster
    };
    truncate_display(cluster, max_width)
}

fn truncate_display(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".repeat(max_width);
    }
    let mut value = String::new();
    let mut used = 0;
    for character in text.chars() {
        let cells = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + cells >= max_width {
            break;
        }
        value.push(character);
        used += cells;
    }
    value.push('…');
    value
}

#[allow(clippy::too_many_arguments)]
fn picker_header_lines(
    width: u16,
    height: u16,
    manage: bool,
    history: HistoryMode,
    auto_add: bool,
    blocked: bool,
    log_warnings: bool,
    cluster: &str,
    blocked_count: usize,
) -> HeaderLayout {
    let usable = width.saturating_sub(1) as usize;
    if width < 44 || height < 7 {
        return HeaderLayout {
            lines: vec![span(
                "Window too small — resize or press q to quit",
                HeaderTone::Amber,
            )],
            too_small: true,
        };
    }

    let cluster = normalized_cluster(cluster, if width >= 84 { 18 } else { 12 });
    let mode = history.label();
    let cluster_chip = chip("Tab", &[(&cluster, HeaderTone::Cyan)]);
    let window_chip = chip(
        "o",
        &[(
            mode,
            if history == HistoryMode::Live {
                HeaderTone::Green
            } else {
                HeaderTone::Cyan
            },
        )],
    );
    let auto_chip = chip(
        "A",
        &[
            ("AUTO", HeaderTone::Plain),
            (
                if auto_add { "ON" } else { "OFF" },
                if auto_add {
                    HeaderTone::Green
                } else {
                    HeaderTone::Muted
                },
            ),
        ],
    );
    let blocked_number = blocked_count.to_string();
    let blocked_chip = chip(
        "b",
        &[
            (
                "BLOCKED",
                if blocked_count > 0 {
                    HeaderTone::Amber
                } else {
                    HeaderTone::Plain
                },
            ),
            (
                &blocked_number,
                if blocked_count > 0 {
                    HeaderTone::Amber
                } else {
                    HeaderTone::Muted
                },
            ),
            (
                if blocked { "SHOWN" } else { "HIDDEN" },
                if blocked {
                    HeaderTone::Cyan
                } else {
                    HeaderTone::Muted
                },
            ),
        ],
    );
    let warning_chip = chip(
        "W",
        &[
            ("WARN", HeaderTone::Plain),
            (
                if log_warnings { "ON" } else { "OFF" },
                if log_warnings {
                    HeaderTone::Amber
                } else {
                    HeaderTone::Muted
                },
            ),
        ],
    );

    let wide_status = {
        let mut line = span("slurm-log  ", HeaderTone::Label);
        line.append(join(
            vec![
                cluster_chip.clone(),
                window_chip.clone(),
                auto_chip.clone(),
                blocked_chip.clone(),
                warning_chip.clone(),
            ],
            "  ",
        ));
        line
    };
    if width >= 84 && wide_status.width() <= usable {
        let mut lines = vec![wide_status];
        lines.extend(command_rows(usable, manage, false));
        if lines.len() + 4 <= height as usize {
            lines.push(span("─".repeat(usable), HeaderTone::Muted));
        }
        return HeaderLayout {
            lines,
            too_small: false,
        };
    }

    let compact_cluster = keyed("Tab", &[(&cluster, HeaderTone::Cyan)]);
    let compact_window = keyed(
        "o",
        &[(
            mode,
            if history == HistoryMode::Live {
                HeaderTone::Green
            } else {
                HeaderTone::Cyan
            },
        )],
    );
    let compact_auto = keyed(
        "A",
        &[
            ("AUTO", HeaderTone::Plain),
            (
                if auto_add { "ON" } else { "OFF" },
                if auto_add {
                    HeaderTone::Green
                } else {
                    HeaderTone::Muted
                },
            ),
        ],
    );
    let compact_blocked_text = format!(
        "{blocked_count} blocked {}",
        if blocked { "shown" } else { "hidden" }
    );
    let compact_blocked = keyed(
        "b",
        &[(&compact_blocked_text, if blocked_count > 0 { HeaderTone::Amber } else { HeaderTone::Muted })],
    );
    let compact_warning = keyed(
        "W",
        &[(
            if log_warnings { "warnings on" } else { "warnings off" },
            if log_warnings { HeaderTone::Amber } else { HeaderTone::Muted },
        )],
    );
    let compact_status = labelled(
        "STATUS",
        join(
            vec![
                compact_cluster.clone(),
                compact_window.clone(),
                compact_auto.clone(),
            ],
            " · ",
        ),
    );
    let compact_filter = labelled(
        "FILTER",
        join(
            vec![compact_blocked.clone(), compact_warning.clone()],
            " · ",
        ),
    );
    let mut compact = vec![compact_status, compact_filter];
    compact.extend(command_rows(usable, manage, true));
    if width >= 64
        && compact.iter().all(|line| line.width() <= usable)
        && compact.len() + 4 <= height as usize
    {
        return HeaderLayout {
            lines: compact,
            too_small: false,
        };
    }

    let mut narrow = vec![
        labelled(
            "SCOPE",
            join(vec![compact_cluster, compact_window], " · "),
        ),
        labelled("STATE", compact_auto),
        labelled(
            "FILTER",
            join(
                vec![
                    compact_blocked,
                    keyed(
                        "W",
                        &[(
                            if log_warnings { "warn on" } else { "warn off" },
                            if log_warnings {
                                HeaderTone::Amber
                            } else {
                                HeaderTone::Muted
                            },
                        )],
                    ),
                ],
                " · ",
            ),
        ),
    ];
    narrow.extend(command_rows(usable, manage, true));
    if narrow.iter().all(|line| line.width() <= usable) && narrow.len() + 4 <= height as usize {
        return HeaderLayout {
            lines: narrow,
            too_small: false,
        };
    }

    let short_cluster = normalized_cluster(&cluster, 8);
    let lines = vec![
        labelled(
            "STATUS",
            join(
                vec![
                    span(short_cluster, HeaderTone::Cyan),
                    span(mode, HeaderTone::Green),
                    span(
                        format!("AUTO {}", if auto_add { "ON" } else { "OFF" }),
                        if auto_add {
                            HeaderTone::Green
                        } else {
                            HeaderTone::Muted
                        },
                    ),
                ],
                " · ",
            ),
        ),
        labelled(
            "FILTER",
            span(
                format!(
                    "{blocked_count} blocked {} · warn {}",
                    if blocked { "shown" } else { "hidden" },
                    if log_warnings { "on" } else { "off" }
                ),
                HeaderTone::Plain,
            ),
        ),
        labelled(
            "KEYS",
            join(
                vec![
                    span("↑↓", HeaderTone::Key),
                    span("Space", HeaderTone::Key),
                    span("Enter", HeaderTone::Key),
                    span("/", HeaderTone::Key),
                    span("?", HeaderTone::Key),
                    span("q", HeaderTone::Key),
                ],
                " · ",
            ),
        ),
    ];
    HeaderLayout {
        too_small: lines.iter().any(|line| line.width() > usable),
        lines,
    }
}
