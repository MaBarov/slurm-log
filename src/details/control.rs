pub fn run(config: &Config, cluster: &str, id: &str, compact: bool) -> Result<()> {
    validate_cluster(config, cluster)?;
    if !valid_job_id(id) {
        bail!("invalid job ID {id}");
    }
    if !io::stdout().is_terminal() {
        let details = crate::daemon::job_details(config, cluster, id, true)?;
        print_text(&details);
        return Ok(());
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
    let mut paused = false;
    let mut force = true;
    let mut next = Instant::now();
    let mut phase_pending = true;
    let mut manual_refresh = false;
    let mut activity = String::new();
    let mut current: Option<JobDetails> = None;
    let mut cpu = VecDeque::with_capacity(40);
    let mut memory = VecDeque::with_capacity(40);
    loop {
        if !paused && !current.as_ref().is_some_and(|item| item.terminal) && Instant::now() >= next
        {
            let previous_sample = current.as_ref().map(|details| details.sampled_at.clone());
            let requested_manually = manual_refresh;
            manual_refresh = false;
            match crate::daemon::job_details(config, cluster, id, force) {
                Ok(details) => {
                    if let Some(value) = details.cpu_efficiency {
                        push_sample(&mut cpu, value);
                    }
                    if let Some(value) = details.memory_efficiency {
                        push_sample(&mut memory, value);
                    }
                    activity = if requested_manually {
                        if previous_sample.as_deref() == Some(details.sampled_at.as_str())
                            && !details.terminal
                        {
                            // Forced samples are coalesced for ten seconds.
                            // Preserve an early request and retry it instead of
                            // making the key press look as if it was ignored.
                            manual_refresh = true;
                            "refresh queued (10s rate limit)".into()
                        } else {
                            "refreshed".into()
                        }
                    } else {
                        String::new()
                    };
                    current = Some(details);
                }
                Err(error) => {
                    if requested_manually {
                        activity = "refresh failed".into();
                    }
                    if let Some(details) = current.as_mut() {
                        details.stale_error = format!("{error:#}");
                    } else {
                        current = Some(error_details(cluster, id, &format!("{error:#}")));
                    }
                }
            }
            force = manual_refresh;
            let delay = if manual_refresh {
                crate::daemon::FORCED_DETAIL_MINIMUM + Duration::from_millis(250)
            } else if phase_pending {
                phase_pending = false;
                Duration::from_secs(10) + Duration::from_millis(refresh_phase(id))
            } else {
                crate::daemon::ACTIVE_DETAIL_TTL
            };
            next = Instant::now() + delay;
            if let Some(details) = &current {
                draw(details, compact, paused, &activity, &cpu, &memory)?;
            }
        }
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Resize(_, _) => {
                    if let Some(details) = &current {
                        draw(details, compact, paused, &activity, &cpu, &memory)?;
                    }
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => return Ok(()),
                    KeyCode::Char(' ') => {
                        paused = !paused;
                        activity.clear();
                        if !paused {
                            next = Instant::now();
                        }
                        if let Some(details) = &current {
                            draw(details, compact, paused, &activity, &cpu, &memory)?;
                        }
                    }
                    KeyCode::Char('r') => {
                        if current.as_ref().is_some_and(|details| details.terminal) {
                            activity = "final snapshot".into();
                        } else {
                            activity = "refreshing…".into();
                            manual_refresh = true;
                            force = true;
                            next = Instant::now();
                        }
                        if let Some(details) = &current {
                            draw(details, compact, paused, &activity, &cpu, &memory)?;
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
}
fn error_details(cluster: &str, id: &str, error: &str) -> JobDetails {
    JobDetails {
        cluster: cluster.into(),
        id: id.into(),
        state: "UNAVAILABLE".into(),
        source: "retryable error".into(),
        sampled_at: timestamp(),
        stale_error: error.into(),
        ..JobDetails::default()
    }
}

fn refresh_phase(id: &str) -> u64 {
    id.bytes().fold(0_u64, |hash, byte| {
        hash.wrapping_mul(16777619).wrapping_add(byte as u64)
    }) % 10_000
}

fn push_sample(samples: &mut VecDeque<f64>, value: f64) {
    if samples.len() == 40 {
        samples.pop_front();
    }
    samples.push_back(value);
}
