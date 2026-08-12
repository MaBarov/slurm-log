#[derive(Clone)]
enum BankRow {
    Bank(usize, bool, usize),
    Directory(usize, PathBuf, usize, bool),
    File(usize, usize),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Expanded {
    Bank(usize),
    Directory(usize, PathBuf),
}

fn row_indent(row: &BankRow) -> usize {
    match row {
        BankRow::Bank(_, _, _) => 0,
        BankRow::Directory(_, _, depth, _) | BankRow::File(_, depth) => depth * 2,
    }
}

fn cluster_tabs(config: &Config, selected: usize) -> String {
    config
        .clusters
        .iter()
        .enumerate()
        .map(|(index, cluster)| {
            if index == selected {
                format!("[{}]", cluster.name)
            } else {
                cluster.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("  ")
}

fn rows(
    banks: &[LoadedBank],
    scripts: &[Script],
    expanded: &HashSet<Expanded>,
    query: &str,
    cluster: &str,
) -> Vec<BankRow> {
    if !query.is_empty() {
        let needle = query.to_lowercase();
        return scripts
            .iter()
            .enumerate()
            .filter(|(_, script)| {
                supports_cluster(script, cluster)
                    && format!("{}/{}", script.bank, script.relative.display())
                        .to_lowercase()
                        .contains(&needle)
            })
            .map(|(index, _)| BankRow::File(index, 1))
            .collect();
    }
    let mut result = Vec::new();
    for (bank_index, bank) in banks.iter().enumerate() {
        let eligible: Vec<_> = (bank.first..bank.last)
            .filter(|&index| supports_cluster(&scripts[index], cluster))
            .collect();
        if eligible.is_empty() {
            continue;
        }
        let bank_open = expanded.contains(&Expanded::Bank(bank_index));
        result.push(BankRow::Bank(bank_index, bank_open, eligible.len()));
        if !bank_open {
            continue;
        }
        let mut directories = BTreeSet::new();
        let mut children: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
        for index in eligible {
            let script = &scripts[index];
            let direct_parent = script.relative.parent().unwrap_or_else(|| Path::new(""));
            children
                .entry(direct_parent.to_path_buf())
                .or_default()
                .push(index);
            let mut parent = script.relative.parent();
            while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
                directories.insert(path.to_path_buf());
                parent = path.parent();
            }
        }
        let visible = |path: &Path| {
            path.ancestors()
                .skip(1)
                .filter(|ancestor| !ancestor.as_os_str().is_empty())
                .all(|ancestor| {
                    expanded.contains(&Expanded::Directory(bank_index, ancestor.to_path_buf()))
                })
        };
        for directory in directories {
            let key = Expanded::Directory(bank_index, directory.clone());
            if visible(&directory) {
                result.push(BankRow::Directory(
                    bank_index,
                    directory.clone(),
                    directory.components().count() + 1,
                    expanded.contains(&key),
                ));
            }
            if expanded.contains(&key) && visible(&directory) {
                for &index in children.get(&directory).into_iter().flatten() {
                    result.push(BankRow::File(index, directory.components().count() + 1));
                }
            }
        }
        for &index in children.get(Path::new("")).into_iter().flatten() {
            result.push(BankRow::File(index, 1));
        }
    }
    result
}

pub fn run(config: &Config) -> Result<Option<Job>> {
    let (mut banks, mut scripts, mut warnings) = scan_all(config)?;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        for script in scripts {
            println!("{}/{}", script.bank, script.relative.display());
        }
        return Ok(None);
    }
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = terminal::disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
        }
    }
    let _guard = Guard;
    let mut expanded = HashSet::new();
    let mut focus = 0_usize;
    let mut query = String::new();
    let mut searching = false;
    let mut selected_cluster = 0_usize;
    let mut previous_frame = Vec::new();
    let mut current = Vec::new();
    let mut visible_scripts = 0_usize;
    let mut view_dirty = true;
    loop {
        if view_dirty {
            let cluster = &config.clusters[selected_cluster].name;
            current = rows(&banks, &scripts, &expanded, &query, cluster);
            visible_scripts = scripts
                .iter()
                .filter(|script| supports_cluster(script, cluster))
                .count();
            view_dirty = false;
        }
        focus = focus.min(current.len().saturating_sub(1));
        draw_bank(
            &banks,
            &scripts,
            &current,
            focus,
            &query,
            &warnings,
            config,
            selected_cluster,
            visible_scripts,
            &mut previous_frame,
        )?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if searching {
            match key.code {
                KeyCode::Esc => {
                    searching = false;
                    query.clear();
                    view_dirty = true;
                }
                KeyCode::Enter => searching = false,
                KeyCode::Backspace => {
                    query.pop();
                    view_dirty = true;
                }
                KeyCode::Char(character) if !character.is_control() => {
                    query.push(character);
                    view_dirty = true;
                }
                _ => {}
            }
            continue;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | 's') if query.is_empty() => return Ok(None),
            KeyCode::Esc => {
                query.clear();
                view_dirty = true;
            }
            KeyCode::Up | KeyCode::Char('k') => focus = focus.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                focus = (focus + 1).min(current.len().saturating_sub(1))
            }
            KeyCode::Tab => {
                selected_cluster = (selected_cluster + 1) % config.clusters.len();
                view_dirty = true;
                previous_frame.clear();
            }
            KeyCode::BackTab => {
                selected_cluster = selected_cluster
                    .checked_sub(1)
                    .unwrap_or(config.clusters.len() - 1);
                view_dirty = true;
                previous_frame.clear();
            }
            KeyCode::Char('r') => {
                (banks, scripts, warnings) = scan_all_fresh(config)?;
                expanded.extend((0..banks.len()).map(Expanded::Bank));
                view_dirty = true;
            }
            KeyCode::Char('/') => {
                query.clear();
                searching = true;
                view_dirty = true;
            }
            KeyCode::Enter | KeyCode::Right if !current.is_empty() => match &current[focus] {
                BankRow::Bank(index, _, _) => {
                    let key = Expanded::Bank(*index);
                    if !expanded.remove(&key) {
                        expanded.insert(key);
                    }
                    view_dirty = true;
                }
                BankRow::Directory(bank, path, _, _) => {
                    let key = Expanded::Directory(*bank, path.clone());
                    if !expanded.remove(&key) {
                        expanded.insert(key);
                    }
                    view_dirty = true;
                }
                BankRow::File(index, _) => {
                    match submit_flow(config, &scripts[*index], selected_cluster) {
                        Ok(Some(job)) => return Ok(Some(job)),
                        Ok(None) => {}
                        Err(error) => warnings.push(format!("submission failed: {error:#}")),
                    }
                    previous_frame.clear();
                }
            },
            KeyCode::Left if !current.is_empty() => match &current[focus] {
                BankRow::Bank(index, _, _) => {
                    view_dirty = expanded.remove(&Expanded::Bank(*index));
                }
                BankRow::Directory(bank, path, _, _) => {
                    view_dirty = expanded.remove(&Expanded::Directory(*bank, path.clone()));
                }
                BankRow::File(_, _) => {}
            },
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_bank(
    banks: &[LoadedBank],
    scripts: &[Script],
    rows: &[BankRow],
    focus: usize,
    query: &str,
    warnings: &[String],
    config: &Config,
    selected_cluster: usize,
    visible_scripts: usize,
    previous: &mut Vec<String>,
) -> Result<()> {
    let (width, height) = terminal::size()?;
    let mut frame = vec![String::new(); height as usize];
    if height == 0 {
        return Ok(());
    }
    let tabs = cluster_tabs(config, selected_cluster);
    frame[0] = format!("SBATCH BANK  ·  SUBMIT TO  {tabs}");
    if height > 1 {
        frame[1] =
            "Tab / Shift-Tab target  ·  ↑↓ move  ·  Enter run  ·  ←→ folders  ·  / search  ·  Esc return"
                .into();
    }
    let top = focus.saturating_sub(height.saturating_sub(5) as usize);
    for (screen, (position, row)) in rows
        .iter()
        .enumerate()
        .skip(top)
        .take(height.saturating_sub(4) as usize)
        .enumerate()
    {
        let text = match row {
            BankRow::Bank(index, expanded, visible) => {
                format!(
                    "{} {}  ({} scripts here / {} total)",
                    if *expanded { "▾" } else { "▸" },
                    banks[*index].name,
                    visible,
                    banks[*index].last - banks[*index].first
                )
            }
            BankRow::Directory(_, path, _, expanded) => {
                format!(
                    "{} {}/",
                    if *expanded { "▾" } else { "▸" },
                    path.file_name().unwrap_or_default().to_string_lossy()
                )
            }
            BankRow::File(index, _) => {
                format!(
                    "{}{}  [{}]",
                    if query.is_empty() {
                        String::new()
                    } else {
                        format!("{}/", scripts[*index].bank)
                    },
                    scripts[*index]
                        .relative
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                    scripts[*index].name
                ) + &format!(
                    "  @{}",
                    scripts[*index].origin.as_deref().unwrap_or("shared")
                )
            }
        };
        frame[screen + 2] = format!(
            "{}{}{}",
            if position == focus { "> " } else { "  " },
            " ".repeat(row_indent(row)),
            text
        );
    }
    frame[height as usize - 1] = format!(
        "{} shown / {} scripts · submit target={} · search={query:?}{}",
        visible_scripts,
        scripts.len(),
        config.clusters[selected_cluster].name,
        if warnings.is_empty() {
            String::new()
        } else {
            format!(" · ⚠ {}", warnings.len())
        }
    );
    let mut out = Vec::new();
    for (line, content) in frame.iter().enumerate() {
        if previous.get(line) == Some(content) {
            continue;
        }
        let fitted: String = content.chars().take(width as usize).collect();
        execute!(
            out,
            cursor::MoveTo(0, line as u16),
            SetAttribute(Attribute::Reset),
            Print(fitted),
            terminal::Clear(ClearType::UntilNewLine)
        )?;
    }
    io::stdout().write_all(&out)?;
    io::stdout().flush()?;
    *previous = frame;
    Ok(())
}

fn submit_flow(config: &Config, script: &Script, selected_cluster: usize) -> Result<Option<Job>> {
    let cluster = &config.clusters[selected_cluster];
    execute!(
        io::stdout(),
        cursor::MoveTo(0, 0),
        terminal::Clear(ClearType::All),
        Print(submit_confirmation(script, cluster))
    )?;
    io::stdout().flush()?;
    let confirmed = loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if let Some(choice) = confirmation_choice(key.code) {
                    break choice;
                }
            }
            _ => {}
        }
    };
    if !confirmed {
        return Ok(None);
    }
    Ok(Some(submit(config, script, &cluster.name)?))
}

fn confirmation_choice(code: KeyCode) -> Option<bool> {
    match code {
        KeyCode::Char('y' | 'Y') => Some(true),
        KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => Some(false),
        _ => None,
    }
}

fn submit_confirmation(script: &Script, cluster: &crate::config::ClusterConfig) -> String {
    let arguments = script
        .directives
        .iter()
        .map(|directive| format!("    {directive}\r\n"))
        .collect::<String>();
    format!(
        "SUBMIT?\r\n  Script: {}/{}\r\n  Name: {}\r\n  Cluster: {}\r\n  Directory: {}\r\n  Arguments:\r\n{}\r\nPress y to submit and open its pane · n/Esc cancels",
        script.bank,
        script.relative.display(),
        script.name,
        cluster.name,
        cluster.working_directory.display(),
        arguments
    )
}
