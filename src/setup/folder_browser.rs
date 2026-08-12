#[derive(Clone)]
enum DirectoryChoice {
    Select(PathBuf),
    Parent(PathBuf),
    Enter(PathBuf),
}

fn safe_terminal_name(value: &std::ffi::OsStr) -> String {
    value
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

fn directory_children(path: &Path) -> Vec<PathBuf> {
    let mut children: Vec<_> = fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|kind| kind.is_dir() && !kind.is_symlink())
                .map(|_| entry.path())
        })
        .collect();
    children.sort_unstable_by(|left, right| left.file_name().cmp(&right.file_name()));
    children
}

fn browse_bank_directory(starting_roots: &[PathBuf]) -> Result<Option<PathBuf>> {
    if starting_roots.is_empty() {
        return Ok(None);
    }
    terminal::enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        cursor::Hide
    )?;
    let _guard = FolderPickerGuard;
    let mut current: Option<PathBuf> = None;
    let mut focus = 0usize;
    loop {
        let choices: Vec<(String, DirectoryChoice)> = if let Some(directory) = &current {
            let mut rows = vec![(
                "✓ Select this directory".into(),
                DirectoryChoice::Select(directory.clone()),
            )];
            if let Some(parent) = directory.parent()
                && parent != directory
            {
                rows.push(("↰ ..".into(), DirectoryChoice::Parent(parent.to_path_buf())));
            }
            rows.extend(directory_children(directory).into_iter().map(|path| {
                let name = path
                    .file_name()
                    .map(safe_terminal_name)
                    .unwrap_or_else(|| path.display().to_string());
                (format!("▸ {name}"), DirectoryChoice::Enter(path))
            }));
            rows
        } else {
            starting_roots
                .iter()
                .map(|path| {
                    (
                        format!("▸ {}", safe_terminal_name(path.as_os_str())),
                        DirectoryChoice::Enter(path.clone()),
                    )
                })
                .collect()
        };
        focus = focus.min(choices.len().saturating_sub(1));
        let (_, height) = terminal::size()?;
        let available = height.saturating_sub(5).max(1) as usize;
        let top = focus.saturating_sub(available.saturating_sub(1));
        let mut out = io::stdout().lock();
        execute!(
            out,
            cursor::MoveTo(0, 0),
            terminal::Clear(ClearType::All),
            SetAttribute(Attribute::Bold),
            Print("Choose an sbatch bank directory\r\n"),
            SetAttribute(Attribute::Reset),
            Print("↑/↓ move · Enter open/select · ←/Backspace parent · Esc/q cancel\r\n"),
            Print(format!(
                "Location: {}\r\n\r\n",
                current
                    .as_deref()
                    .map(|path| safe_terminal_name(path.as_os_str()))
                    .unwrap_or_else(|| "suggested roots".into())
            ))
        )?;
        for (index, (label, _)) in choices.iter().enumerate().skip(top).take(available) {
            if index == focus {
                execute!(out, SetAttribute(Attribute::Reverse))?;
            }
            execute!(
                out,
                Print(format!("  {label}\r\n")),
                SetAttribute(Attribute::Reset)
            )?;
        }
        out.flush()?;
        let mut activate = false;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Up | KeyCode::Char('k') => focus = focus.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    focus = (focus + 1).min(choices.len().saturating_sub(1));
                }
                KeyCode::Home | KeyCode::Char('g') => focus = 0,
                KeyCode::End | KeyCode::Char('G') => {
                    focus = choices.len().saturating_sub(1);
                }
                KeyCode::Enter | KeyCode::Right => activate = true,
                KeyCode::Left | KeyCode::Backspace => {
                    if let Some(parent) = current.as_deref().and_then(Path::parent) {
                        current = Some(parent.to_path_buf());
                    } else {
                        current = None;
                    }
                    focus = 0;
                }
                KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
                _ => {}
            },
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) if mouse.row >= 4 => {
                    let index = top + usize::from(mouse.row - 4);
                    if index < choices.len() {
                        focus = index;
                        activate = true;
                    }
                }
                MouseEventKind::ScrollUp => focus = focus.saturating_sub(1),
                MouseEventKind::ScrollDown => {
                    focus = (focus + 1).min(choices.len().saturating_sub(1));
                }
                _ => {}
            },
            _ => {}
        }
        if activate {
            match &choices[focus].1 {
                DirectoryChoice::Select(path) => return Ok(Some(path.clone())),
                DirectoryChoice::Parent(path) | DirectoryChoice::Enter(path) => {
                    current = Some(path.clone());
                    focus = 0;
                }
            }
        }
    }
}
