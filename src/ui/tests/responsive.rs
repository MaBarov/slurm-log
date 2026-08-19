use super::*;

fn plain(layout: &HeaderLayout) -> Vec<String> {
    layout.lines.iter().map(HeaderLine::plain).collect()
}

fn header(width: u16, height: u16) -> HeaderLayout {
    picker_header_lines(
        width,
        height,
        false,
        HistoryMode::Live,
        true,
        false,
        false,
        "all",
        1,
        StateFilter::All,
    )
}

#[test]
fn wide_header_is_the_compact_control_strip() {
    let layout = header(120, 20);
    let lines = plain(&layout);
    assert!(!layout.too_small);
    assert_eq!(
        lines[0],
        "slurm-log  [Tab ALL] [o LIVE ≤2m] [f STATE ALL] [A AUTO ON] [b BLOCKED 1 HIDDEN] [W WARN OFF]"
    );
    assert_eq!(
        lines[1],
        "↑↓ move · Space mark · Enter open · f filter · i details · s submit · d dismiss · / find · ? help · q quit"
    );
    assert!(lines[2].chars().all(|character| character == '─'));
    assert_eq!(UnicodeWidthStr::width(lines[2].as_str()), 119);
    assert!(layout.lines[0].render(119, false).contains("\x1b[1;32m"));
    assert!(layout.lines[0].render(119, false).contains("\x1b[1;36m"));
}

#[test]
fn medium_header_recomposes_into_aligned_rows() {
    for width in [83, 80, 64] {
        let layout = header(width, 20);
        let lines = plain(&layout);
        assert!(lines[0].starts_with("STATUS  [Tab] ALL · [o] LIVE ≤2m · [A] AUTO ON"));
        assert!(
            lines[1].starts_with("FILTER  [f] all states · [b] 1 blocked hidden · [W] warn off")
        );
        assert!(lines.iter().any(|line| line.starts_with("KEYS    ↑↓ move")));
        assert!(
            lines
                .iter()
                .all(|line| UnicodeWidthStr::width(line.as_str()) < width as usize)
        );
    }
}

#[test]
fn narrow_header_has_scope_state_filter_and_key_hierarchy() {
    for width in [63, 48, 44] {
        let layout = header(width, 20);
        let lines = plain(&layout);
        assert!(!layout.too_small, "{width}: {lines:?}");
        assert!(lines[0].starts_with("SCOPE   [Tab] ALL · [o] LIVE ≤2m"));
        assert_eq!(lines[1], "STATE   [A] AUTO ON");
        assert_eq!(lines[2], "FILTER  [b] 1 blocked hidden · [W] warn off");
        assert!(lines.iter().any(|line| line.contains("Enter open")));
        assert!(lines.iter().any(|line| line.contains("? help · q quit")));
        assert!(
            lines
                .iter()
                .all(|line| UnicodeWidthStr::width(line.as_str()) < width as usize)
        );
    }
}

#[test]
fn very_short_or_too_narrow_windows_fail_cleanly() {
    let short = header(48, 7);
    let short_lines = plain(&short);
    assert!(!short.too_small);
    assert_eq!(short_lines.len(), 3);
    assert!(short_lines[0].starts_with("STATUS"));
    for (width, height) in [(43, 20), (80, 6)] {
        let layout = header(width, height);
        assert!(layout.too_small);
        assert_eq!(
            plain(&layout),
            ["Window too small — resize or press q to quit"]
        );
    }
}

#[test]
fn every_state_and_history_mode_stays_explicit() {
    let cases = [
        (HistoryMode::Hours2, "LAST 2h"),
        (HistoryMode::Hours12, "LAST 12h"),
        (HistoryMode::Day1, "LAST 1d"),
        (HistoryMode::Week1, "LAST 1w"),
        (HistoryMode::All, "ALL HISTORY"),
    ];
    for (history, label) in cases {
        let layout = picker_header_lines(
            120,
            20,
            true,
            history,
            false,
            true,
            true,
            "cluster-with-a-name-that-is-far-too-long",
            12_345,
            StateFilter::All,
        );
        let text = plain(&layout).join("\n");
        assert!(text.contains(label));
        assert!(text.contains("AUTO OFF"));
        assert!(text.contains("12345") && text.contains("SHOWN"));
        assert!(text.contains("WARN ON"));
        assert!(text.contains("Enter apply"));
        assert!(text.contains('…'));
        assert!(layout.lines.iter().all(|line| line.width() < 120));
    }
}
#[test]
fn all_state_filters_render_cleanly_across_widths() {
    for filter in [
        StateFilter::All,
        StateFilter::Running,
        StateFilter::Pending,
        StateFilter::Failed,
        StateFilter::Completed,
    ] {
        let wide = picker_header_lines(
            120,
            20,
            false,
            HistoryMode::Live,
            true,
            false,
            false,
            "all",
            0,
            filter,
        );
        let wide_text = plain(&wide).join("\n");
        assert!(wide_text.contains(filter.label()));

        for width in [83, 80, 70] {
            let compact = picker_header_lines(
                width,
                20,
                false,
                HistoryMode::Live,
                true,
                false,
                false,
                "all",
                0,
                filter,
            );
            let lines = plain(&compact);
            assert!(lines[1].contains("[f]"));
            assert!(
                lines
                    .iter()
                    .all(|l| UnicodeWidthStr::width(l.as_str()) < width as usize)
            );
        }
    }
}

#[test]
fn unicode_cell_width_and_ellipsis_are_correct() {
    assert_eq!(truncate_display("cluster界name", 10), "cluster界…");
    assert_eq!(truncate_display("cluster界name", 8), "cluster…");
    let line = span("界", HeaderTone::Cyan);
    assert_eq!(line.width(), 2);
    let rendered = line.render(4, true);
    assert!(rendered.contains("界\x1b[0m  "));
}

#[test]
fn truncate_display_handles_ascii_fast_path_and_boundary_widths() {
    assert_eq!(truncate_display("hello", 10), "hello");
    assert_eq!(truncate_display("hello", 5), "hello");
    assert_eq!(truncate_display("hello", 4), "hel…");
    assert_eq!(truncate_display("hello", 2), "h…");
    assert_eq!(truncate_display("hello", 1), "…");
    assert_eq!(truncate_display("hello", 0), "");
    assert_eq!(truncate_display("🦀crab🦀", 5), "🦀cr…");
    assert_eq!(truncate_display("München", 5), "Münc…");
}
fn table_job() -> Job {
    Job {
        cluster: "cispa".into(),
        id: "3212645_123456".into(),
        state: "OUT_OF_MEMORY".into(),
        elapsed: "20:54".into(),
        name: "experiment-with-a-very-long-name".into(),
        ..Job::default()
    }
}

#[test]
fn table_columns_follow_scope_and_width_priority() {
    let job = table_job();
    let wide = TableLayout::new(120, "cispa");
    assert!(wide.show_cluster && wide.show_elapsed);
    assert!(wide.header().contains("CLUSTER"));
    assert!(
        wide.job(&job, true, true, false)
            .contains("experiment-with")
    );

    let medium_single = TableLayout::new(80, "cispa");
    assert!(!medium_single.show_cluster && medium_single.show_elapsed);
    assert!(!medium_single.header().contains("CLUSTER"));
    let medium_all = TableLayout::new(80, "all");
    assert!(medium_all.show_cluster && medium_all.show_elapsed);
    assert!(medium_all.job(&job, false, false, false).contains("cispa"));

    let narrow = TableLayout::new(44, "all");
    assert!(narrow.show_cluster && !narrow.show_elapsed);
    let row = narrow.job(&job, false, false, true);
    assert!(row.contains("cispa"));
    assert!(!row.contains("20:54"));
    assert!(row.contains('…'));
    assert!(UnicodeWidthStr::width(row.as_str()) <= 43);
}

#[test]
fn grouped_rows_and_cells_never_exceed_the_terminal() {
    let row = Row {
        name: "界".repeat(50),
        job: None,
        members: vec![0, 1, 2],
        nested: false,
        expanded: false,
    };
    let rendered = compact_group_row(&row, false, true, 48);
    assert!(UnicodeWidthStr::width(rendered.as_str()) <= 47);
    assert!(rendered.ends_with('…'));
    assert_eq!(UnicodeWidthStr::width(fit_cell("界", 4).as_str()), 4);
}

#[test]
fn verbose_footer_stays_on_one_row_and_prioritizes_blocked_status() {
    let warning = " | sprint: completed jobs unavailable because sacct/accounting is disabled; \
                   only active squeue jobs can be listed";
    let line = footer_line(120, 120, 0, "", warning, "", 18, false);

    assert!(line.starts_with("120 rows, 0 selected | blocked: 18 (b to show)"));
    assert!(line.contains("sprint: completed jobs unavailable"));
    assert!(line.ends_with('…'));
    assert!(!line.contains(['\r', '\n']));
    assert_eq!(UnicodeWidthStr::width(line.as_str()), 119);
}

#[test]
fn footer_reserves_the_terminal_final_column_for_wrap_safety() {
    for width in [44, 80, 120] {
        let line = footer_line(
            width,
            100_000,
            42,
            "界".repeat(100).as_str(),
            " | a very long warning",
            "a very long notice",
            9_999,
            true,
        );
        assert!(UnicodeWidthStr::width(line.as_str()) < width as usize);
        assert!(!line.contains(['\r', '\n']));
    }
}

#[test]
fn blocked_status_chip_and_footer_toggle_accurately_reflect_visibility_state() {
    // 1. When blocked jobs are HIDDEN (show_blocked = false):
    let hidden_header = picker_header_lines(
        120,
        20,
        false,
        HistoryMode::Live,
        true,
        false,
        false,
        "all",
        5,
        StateFilter::All,
    );
    let hidden_plain = plain(&hidden_header);
    assert!(
        hidden_plain[0].contains("[b BLOCKED 5 HIDDEN]"),
        "Header must indicate 5 blocked jobs are hidden: {}",
        hidden_plain[0]
    );

    let hidden_footer = footer_line(120, 10, 0, "", "", "", 5, false);
    assert!(
        hidden_footer.contains("blocked: 5 (b to show)"),
        "Footer must show '(b to show)' when blocked jobs are hidden: {hidden_footer}"
    );

    // 2. When blocked jobs are SHOWN (show_blocked = true):
    let shown_header = picker_header_lines(
        120,
        20,
        false,
        HistoryMode::Live,
        true,
        true,
        false,
        "all",
        5,
        StateFilter::All,
    );
    let shown_plain = plain(&shown_header);
    assert!(
        shown_plain[0].contains("[b BLOCKED 5 SHOWN]"),
        "Header must indicate 5 blocked jobs are shown: {}",
        shown_plain[0]
    );

    let shown_footer = footer_line(120, 15, 0, "", "", "", 5, true);
    assert!(
        shown_footer.contains("blocked: 5 (shown)"),
        "Footer must show 'blocked: 5 (shown)' when blocked jobs are shown: {shown_footer}"
    );

    // 3. When there are 0 blocked jobs:
    let zero_header = picker_header_lines(
        120,
        20,
        false,
        HistoryMode::Live,
        true,
        false,
        false,
        "all",
        0,
        StateFilter::All,
    );
    let zero_plain = plain(&zero_header);
    assert!(
        zero_plain[0].contains("[b BLOCKED 0 HIDDEN]"),
        "Header must show '[b BLOCKED 0 HIDDEN]' when 0 blocked jobs exist: {}",
        zero_plain[0]
    );

    let zero_footer = footer_line(120, 10, 0, "", "", "", 0, false);
    assert!(
        zero_footer.contains("blocked: 0 (b to show)"),
        "Footer must show 'blocked: 0 (b to show)' when count is 0: {zero_footer}"
    );
}

#[test]
fn adaptive_command_hints_scale_gracefully_with_terminal_width() {
    // 1. Extra-wide terminal (>= 115 cols): shows details, submit, and dismiss
    let wide = header(120, 20);
    let wide_plain = plain(&wide);
    assert!(wide_plain[1].contains("i details"));
    assert!(wide_plain[1].contains("s submit"));
    assert!(wide_plain[1].contains("d dismiss"));

    // 2. Wide terminal (100 cols): shows details and submit
    let mid_wide = header(100, 20);
    let mid_wide_plain = plain(&mid_wide);
    assert!(mid_wide_plain[1].contains("i details"));
    assert!(mid_wide_plain[1].contains("s submit"));
    assert!(!mid_wide_plain[1].contains("d dismiss"));

    // 3. Standard terminal (84 cols): shows details
    let std = header(84, 20);
    let std_plain = plain(&std);
    assert!(std_plain.iter().any(|line| line.contains("i details")));
    assert!(!std_plain.iter().any(|line| line.contains("s submit")));
    // 4. Footer selection hint when items are selected
    let selected_footer = footer_line(120, 10, 3, "", "", "", 0, false);
    assert!(
        selected_footer.contains("3 selected"),
        "Footer must show '3 selected' when items are selected: {selected_footer}"
    );
}

#[test]
fn interactive_toast_hints_teach_command_shortcuts_and_expire_cleanly() {
    let mut notice = None;

    // 1. Selection toast includes open and clear hints
    set_notice(&mut notice, "2 selected (Enter open · c clear)");
    assert_eq!(
        notice.as_ref().unwrap().0,
        "2 selected (Enter open · c clear)"
    );
    let line = footer_text(10, 2, "", "", &notice.as_ref().unwrap().0);
    assert!(line.contains("2 selected (Enter open · c clear)"));

    // 2. Cluster switch toast includes navigation hints
    set_notice(&mut notice, "Cluster: sprint (Tab next · Shift-Tab prev)");
    assert_eq!(
        notice.as_ref().unwrap().0,
        "Cluster: sprint (Tab next · Shift-Tab prev)"
    );

    // 3. Group expand toast includes collapse and details hints
    set_notice(
        &mut notice,
        "Group expanded (← collapse · i details on job)",
    );
    assert_eq!(
        notice.as_ref().unwrap().0,
        "Group expanded (← collapse · i details on job)"
    );

    // 4. Group collapse toast includes expand and mark hints
    set_notice(&mut notice, "Group collapsed (→ expand · Space mark all)");
    assert_eq!(
        notice.as_ref().unwrap().0,
        "Group collapsed (→ expand · Space mark all)"
    );

    // 5. Refresh toast includes history hints
    set_notice(&mut notice, "Scheduler refreshed (r refresh · o history)");
    assert_eq!(
        notice.as_ref().unwrap().0,
        "Scheduler refreshed (r refresh · o history)"
    );
}
