fn set_notice(notice: &mut Option<(String, Instant)>, message: &str) {
    *notice = Some((
        message.to_string(),
        Instant::now() + Duration::from_millis(1_500),
    ));
}
fn prompt_search(initial: &str) -> Result<Option<String>> {
    let mut value = initial.to_string();
    let mut out = io::stdout();
    execute!(out, cursor::Show)?;
    loop {
        let (_, height) = terminal::size()?;
        execute!(
            out,
            cursor::MoveTo(0, height.saturating_sub(1)),
            terminal::Clear(ClearType::CurrentLine),
            Print(format!("Search: {value}  ·  Enter apply  ·  Esc cancel"))
        )?;
        out.flush()?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Enter => {
                    execute!(out, cursor::Hide)?;
                    return Ok(Some(value));
                }
                KeyCode::Esc => {
                    execute!(out, cursor::Hide)?;
                    return Ok(None);
                }
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char(character) if !character.is_control() => value.push(character),
                _ => {}
            },
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

fn row_key(row: &Row, jobs: &[Job]) -> String {
    row.job
        .map(|index| jobs[index].key())
        .unwrap_or_else(|| format!("group:{}", row.name))
}

fn remember_selected(
    known: &mut HashMap<String, Job>,
    jobs: &[Job],
    selected: &HashSet<String>,
) {
    let mut key = String::new();
    for job in jobs {
        job.write_key(&mut key);
        if selected.contains(&key) {
            known.insert(key.clone(), job.clone());
        }
    }
}

fn restore_selected(
    jobs: &mut Vec<Job>,
    known: &HashMap<String, Job>,
    selected: &HashSet<String>,
    active_cluster: &str,
    show_blocked: bool,
) {
    let every_cluster = matches!(active_cluster, "all" | "both");
    let visible: HashSet<_> = jobs
        .iter()
        .map(|job| (job.cluster.as_str(), job.id.as_str()))
        .collect();
    let additions: Vec<_> = selected
        .iter()
        .filter_map(|key| {
            let job = known.get(key)?;
            let in_cluster = every_cluster || job.cluster == active_cluster;
            let category_visible = show_blocked || !job.blocked_category();
            (in_cluster
                && category_visible
                && !visible.contains(&(job.cluster.as_str(), job.id.as_str())))
                .then(|| job.clone())
        })
        .collect();
    jobs.extend(additions);
}

#[allow(clippy::too_many_arguments)]
fn refresh(
    config: &Config,
    live: &Option<(String, String)>,
    history: HistoryMode,
    show_blocked: bool,
    blocked_count: &mut usize,
    force: bool,
    jobs: &mut Vec<Job>,
    ledger: &mut Ledger,
    warnings: &mut Vec<String>,
) -> bool {
    let Some((cluster, filter)) = live else {
        return false;
    };
    let result = if force {
        slurm::all_jobs_fresh(config, cluster, filter, history.scheduler_archive())
    } else {
        slurm::all_jobs(config, cluster, filter, history.scheduler_archive())
    };
    if let Ok((fresh, state, fresh_warnings)) = result {
        let every_cluster = matches!(cluster.as_str(), "all" | "both");
        let pinned: Vec<_> = jobs
            .iter()
            .filter(|job| {
                job.state == "OPEN" && (every_cluster || job.cluster.as_str() == cluster.as_str())
            })
            .cloned()
            .collect();
        let eligible = slurm::visible_jobs(fresh, &state, history, true);
        let next_blocked = eligible.iter().filter(|job| job.blocked_category()).count();
        let mut next_jobs = if show_blocked {
            eligible
        } else {
            eligible
                .into_iter()
                .filter(|job| !job.blocked_category())
                .collect()
        };
        let mut next_keys: HashSet<_> = next_jobs.iter().map(Job::key).collect();
        for job in pinned {
            if next_keys.insert(job.key()) {
                next_jobs.push(job);
            }
        }
        let changed = *jobs != next_jobs
            || *ledger != state
            || *warnings != fresh_warnings
            || *blocked_count != next_blocked;
        *jobs = next_jobs;
        *ledger = state;
        *warnings = fresh_warnings;
        *blocked_count = next_blocked;
        return changed;
    }
    false
}

fn status_rank(job: &Job) -> u8 {
    if job.running() {
        0
    } else if job.pending() {
        1
    } else if job.state.starts_with("COMPLETED") {
        2
    } else if job.state.starts_with("CANCELLED") {
        4
    } else {
        3
    }
}

fn job_number(id: &str) -> u64 {
    id.split('_')
        .next()
        .unwrap_or("0")
        .parse::<u64>()
        .unwrap_or(0)
}

fn job_matches(job: &Job, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let fields = [
        job.cluster.as_str(),
        job.id.as_str(),
        job.state.as_str(),
        job.name.as_str(),
        job.reason.as_str(),
        job.partition.as_str(),
        job.start_time.as_str(),
        job.exit_code.as_str(),
        job.alloc_tres.as_str(),
    ];
    if needle.is_ascii() {
        let needle = needle.as_bytes();
        fields.iter().any(|field| {
            field
                .as_bytes()
                .windows(needle.len())
                .any(|window| window.eq_ignore_ascii_case(needle))
        })
    } else {
        fields
            .iter()
            .any(|field| field.to_lowercase().contains(needle))
    }
}

fn grouped_rows(jobs: &[Job], indices: &[usize], expanded: &HashSet<String>) -> Vec<Row> {
    let mut groups: HashMap<&str, Vec<(usize, u8, u64)>> = HashMap::with_capacity(indices.len());
    for &index in indices {
        let job = &jobs[index];
        let base_id = job_number(&job.id);
        groups
            .entry(&job.name)
            .or_default()
            .push((index, status_rank(job), base_id));
    }
    let mut groups: Vec<_> = groups
        .into_iter()
        .map(|(name, members)| {
            let best_rank = members.iter().map(|member| member.1).min().unwrap_or(9);
            let newest = members.iter().map(|member| member.2).max().unwrap_or(0);
            (name, members, best_rank, newest)
        })
        .collect();
    groups.sort_unstable_by(|left, right| {
        (left.2, std::cmp::Reverse(left.3), left.0).cmp(&(
            right.2,
            std::cmp::Reverse(right.3),
            right.0,
        ))
    });
    let mut rows = Vec::new();
    for (name, mut members, _, _) in groups {
        members.sort_unstable_by_key(|member| (member.1, std::cmp::Reverse(member.2)));
        let members: Vec<_> = members.into_iter().map(|member| member.0).collect();
        let name = name.to_string();
        if members.len() == 1 {
            rows.push(Row {
                name,
                job: Some(members[0]),
                members,
                nested: false,
                expanded: false,
            });
        } else {
            let is_expanded = expanded.contains(&name);
            rows.push(Row {
                name: name.clone(),
                job: None,
                members: members.clone(),
                nested: false,
                expanded: is_expanded,
            });
            if is_expanded {
                for member in members {
                    rows.push(Row {
                        name: name.clone(),
                        job: Some(member),
                        members: vec![member],
                        nested: true,
                        expanded: false,
                    });
                }
            }
        }
    }
    rows
}

fn toggle(selected: &mut HashSet<String>, jobs: &[Job], members: &[usize]) {
    let keys: Vec<_> = members.iter().map(|&i| jobs[i].key()).collect();
    if keys.iter().all(|key| selected.contains(key)) {
        for key in keys {
            selected.remove(&key);
        }
    } else {
        selected.extend(keys);
    }
}

fn cycle_cluster(config: &Config, current: &str, backwards: bool) -> String {
    if config.clusters.len() <= 1 {
        return config.clusters[0].name.clone();
    }
    let choices: Vec<_> = std::iter::once("all")
        .chain(config.clusters.iter().map(|cluster| cluster.name.as_str()))
        .collect();
    let normalized = if current == "both" { "all" } else { current };
    let position = choices
        .iter()
        .position(|choice| *choice == normalized)
        .unwrap_or(0);
    let next = if backwards {
        position.checked_sub(1).unwrap_or(choices.len() - 1)
    } else {
        (position + 1) % choices.len()
    };
    choices[next].to_string()
}
