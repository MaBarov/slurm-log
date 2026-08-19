#[allow(clippy::too_many_arguments)]
pub fn pick(
    config: &Config,
    mut jobs: Vec<Job>,
    mut ledger: Ledger,
    initial: HashSet<String>,
    manage: bool,
    mut history_mode: HistoryMode,
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
    let mut show_blocked = live_filter
        .as_ref()
        .is_some_and(|(_, filter)| filter == "blocked");
    // The pane manager's selection is workspace-wide, while cluster tabs are
    // only views into that workspace. Retain metadata only for selected panes;
    // cloning a full accounting archive here made opening Ctrl-b j scale with
    // every historical job instead of the handful of open panes.
    let mut known_jobs = HashMap::with_capacity(selected.len());
    remember_selected(&mut known_jobs, &jobs, &selected);
    // Already-open blocked panes remain in the hidden selection catalog, but
    // the ordinary live view must not bypass its blocked filter to show them.
    if !show_blocked {
        jobs.retain(|job| !job.blocked_category());
    }
    let mut expanded = HashSet::new();
    let mut query = String::new();
    let mut state_filter = StateFilter::All;
    let mut show_warnings = false;
    let mut show_log_warnings = ledger.log_warnings_default;
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
        let key = include!("picker_refresh.rs");
        include!("picker_actions.rs");
    }
}
