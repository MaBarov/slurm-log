const HEADER_CELL_WIDTH: usize = 25;

fn header_lines(
    manage: bool,
    history: HistoryMode,
    auto_add: bool,
    blocked: bool,
    log_warnings: bool,
    cluster: &str,
    blocked_count: usize,
) -> Vec<String> {
    let mode = history.label();
    let cluster = if matches!(cluster, "all" | "both") {
        "ALL"
    } else {
        cluster
    };
    vec![
        " slurm-log".into(),
        grid_row(
            "Status",
            &[
                chip("Cluster", cluster),
                chip("Monitor", mode),
                chip("Auto-add", if auto_add { "ON" } else { "OFF" }),
            ],
        ),
        grid_row(
            "Views",
            &[
                chip("Window", history.label()),
                chip(
                    "Archive",
                    if history.scheduler_archive() { "ON" } else { "OFF" },
                ),
                chip(
                    "Blocked",
                    &format!(
                        "{blocked_count} {}",
                        if blocked { "SHOWN" } else { "HIDDEN" }
                    ),
                ),
            ],
        ),
        grid_row(
            "Output",
            &[chip(
                "Warnings",
                if log_warnings { "SHOWN" } else { "HIDDEN" },
            )],
        ),
        grid_row(
            "Keys",
            &[
                "↑↓ Move".into(),
                "Space Select".into(),
                format!("Enter {}", if manage { "Apply" } else { "Open" }),
            ],
        ),
        grid_row(
            "",
            &["Tab Cluster".into(), "/ Search".into(), "? Commands   q Quit".into()],
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn picker_header_lines(
    width: u16,
    manage: bool,
    history: HistoryMode,
    auto_add: bool,
    blocked: bool,
    log_warnings: bool,
    cluster: &str,
    blocked_count: usize,
) -> Vec<String> {
    if width >= 92 {
        return header_lines(
            manage,
            history,
            auto_add,
            blocked,
            log_warnings,
            cluster,
            blocked_count,
        );
    }
    let mode = history.label();
    let cluster = if matches!(cluster, "all" | "both") {
        "ALL"
    } else {
        cluster
    };
    let (primary, secondary) = compact_commands(manage);
    vec![
        format!(
            "slurm-log  [ {cluster} ]  [ {mode} ]  [ AUTO {} ]",
            if auto_add { "ON" } else { "OFF" }
        ),
        format!(
            "[ WINDOW {mode} ]  [ ARCHIVE {} ]  [ BLOCKED {blocked_count} {} ]  [ WARNINGS {} ]",
            if history.scheduler_archive() { "ON" } else { "OFF" },
            if blocked { "SHOWN" } else { "HIDDEN" },
            if log_warnings { "SHOWN" } else { "HIDDEN" }
        ),
        primary,
        secondary,
    ]
}

fn chip(label: &str, value: &str) -> String {
    format!("{label} [ {value} ]")
}

fn grid_row(label: &str, cells: &[String]) -> String {
    let mut row = format!(" {label:<10}");
    for cell in cells {
        let mut fitted: String = cell.chars().take(HEADER_CELL_WIDTH).collect();
        fitted.extend(std::iter::repeat_n(
            ' ',
            HEADER_CELL_WIDTH.saturating_sub(fitted.chars().count()),
        ));
        row.push_str(&fitted);
    }
    row.trim_end().to_string()
}
