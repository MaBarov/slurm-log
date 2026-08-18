fn interactive_frame(job: &Job, ended: bool) -> String {
    let name = if job.name.is_empty() {
        "interactive"
    } else {
        &job.name
    };
    let state = if job.state.is_empty() {
        "ACTIVE"
    } else {
        &job.state
    };
    let elapsed = if job.elapsed.is_empty() {
        "—"
    } else {
        &job.elapsed
    };
    let place = [job.partition.as_str(), job.reason.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty() && *part != "None")
        .collect::<Vec<_>>()
        .join(" · ");
    let prompt = match (ended, job.interactive) {
        (true, _) => "The allocation has ended. Press Enter to close this pane.",
        (false, true) => {
            "Ctrl-b i details · Ctrl-b z zoom · Enter closes this monitor (the allocation keeps running)"
        }
        (false, false) => {
            "Ctrl-b i details · Ctrl-b z zoom · Enter closes this monitor (the Slurm job remains active)"
        }
    };
    if job.interactive {
        format!(
            "INTERACTIVE ALLOCATION  {}:{}  {}\r\n{}  ·  elapsed {}\r\nPLACE  {}\r\n\r\nSlurm created no stdout log for this interactive allocation (BatchFlag=0).\r\nOutput remains in the terminal or agent session that launched it; another PTY cannot be mirrored here.\r\n\r\n{}\r\n",
            job.cluster,
            job.id,
            name,
            state,
            elapsed,
            if place.is_empty() { "—" } else { &place },
            prompt
        )
    } else {
        format!(
            "WAITING FOR LOG  {}:{}  {}\r\n{}  ·  elapsed {}\r\nPLACE  {}\r\n\r\nSlurm has not published this job's stdout path yet.\r\nslurm-log will attach automatically when it becomes available.\r\n\r\n{}\r\n",
            job.cluster,
            job.id,
            name,
            state,
            elapsed,
            if place.is_empty() { "—" } else { &place },
            prompt
        )
    }
}

fn render_monitor(frame: &str, previous: &mut String) -> io::Result<()> {
    if frame == previous {
        return Ok(());
    }
    print!("\x1b[H\x1b[J{frame}");
    io::stdout().flush()?;
    previous.clear();
    previous.push_str(frame);
    Ok(())
}
