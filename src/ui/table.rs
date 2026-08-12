#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TableLayout {
    usable: usize,
    show_cluster: bool,
    show_elapsed: bool,
    cluster_width: usize,
    id_width: usize,
    state_width: usize,
    elapsed_width: usize,
    name_width: usize,
}

impl TableLayout {
    fn new(width: u16, cluster: &str) -> Self {
        let usable = width.saturating_sub(1) as usize;
        let every_cluster = matches!(cluster, "all" | "both");
        let (show_cluster, show_elapsed, cluster_width, id_width, state_width, elapsed_width) =
            if width >= 84 {
                (true, true, 7, 15, 19, 11)
            } else if width >= 64 {
                (every_cluster, true, 7, 13, 13, 9)
            } else {
                (every_cluster, false, 7, 11, 11, 0)
            };
        let fixed = 4
            + id_width
            + 1
            + state_width
            + 1
            + usize::from(show_cluster) * (cluster_width + 1)
            + usize::from(show_elapsed) * (elapsed_width + 1);
        let name_width = usable.saturating_sub(fixed).max(1);
        Self {
            usable,
            show_cluster,
            show_elapsed,
            cluster_width,
            id_width,
            state_width,
            elapsed_width,
            name_width,
        }
    }

    fn header(self) -> String {
        self.row("    ", "CLUSTER", "JOB ID / RUNS", "STATE", "ELAPSED", "NAME")
    }

    fn job(self, job: &Job, focused: bool, selected: bool, nested: bool) -> String {
        let prefix = format!(
            "{}{}{} ",
            if focused { ">" } else { " " },
            if selected { "*" } else { " " },
            if nested { "↳" } else { " " }
        );
        self.row(
            &prefix,
            &job.cluster,
            &job.id,
            &job.state,
            &job.elapsed,
            &display_name(job),
        )
    }

    fn row(
        self,
        prefix: &str,
        cluster: &str,
        id: &str,
        state: &str,
        elapsed: &str,
        name: &str,
    ) -> String {
        let mut row = fit_cell(prefix, 4);
        if self.show_cluster {
            row.push_str(&fit_cell(cluster, self.cluster_width));
            row.push(' ');
        }
        row.push_str(&fit_cell(id, self.id_width));
        row.push(' ');
        row.push_str(&fit_cell(state, self.state_width));
        row.push(' ');
        if self.show_elapsed {
            row.push_str(&fit_cell(elapsed, self.elapsed_width));
            row.push(' ');
        }
        row.push_str(&truncate_display(name, self.name_width));
        truncate_display(&row, self.usable)
    }
}

fn fit_cell(text: &str, width: usize) -> String {
    let mut value = truncate_display(text, width);
    let used = UnicodeWidthStr::width(value.as_str());
    value.extend(std::iter::repeat_n(' ', width.saturating_sub(used)));
    value
}

fn compact_group_row(row: &Row, selected: bool, focused: bool, width: u16) -> String {
    truncate_display(
        &group_row_text(row, selected, focused),
        width.saturating_sub(1) as usize,
    )
}
