pub fn monitor(config: &Config, session: &str, lines: usize) -> Result<()> {
    let (initial, _, _) = crate::slurm::all_jobs(config, "both", "all", false)?;
    let mut observed: HashMap<_, _> = initial
        .into_iter()
        .map(|job| ((job.cluster.clone(), job.id.clone()), job))
        .collect();
    let mut tracked_pending: HashMap<_, _> = observed
        .iter()
        .filter(|(_, job)| job.pending())
        .map(|(key, job)| (key.clone(), job.clone()))
        .collect();
    let mut missing_pending: HashMap<(String, String), u8> = HashMap::new();
    loop {
        if !auto_enabled(session)? {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(15));
        let (jobs, _, warnings) = crate::slurm::all_jobs(config, "both", "all", false)?;
        let current: HashMap<_, _> = jobs
            .iter()
            .cloned()
            .map(|j| ((j.cluster.clone(), j.id.clone()), j))
            .collect();
        let additions = monitor_additions(&observed, &current);
        if !additions.is_empty() {
            let mut desired: Vec<Job> = panes(session)?
                .into_iter()
                .map(|p| Job {
                    cluster: p.cluster,
                    id: p.job_id,
                    ..Job::default()
                })
                .collect();
            desired.extend(additions);
            reconcile(config, session, &desired, lines, false)?;
        }
        for (key, before) in &observed {
            if before.pending() && current.get(key).is_some_and(Job::running) {
                tmux([
                    "display-message",
                    "-d",
                    "5000",
                    "-t",
                    session,
                    &format!("Job {} started", before.id),
                ])?;
            }
            if before.pending() && current.get(key).is_some_and(Job::failed) {
                tmux([
                    "display-message",
                    "-d",
                    "5000",
                    "-t",
                    session,
                    &format!("Job {} failed before start", before.id),
                ])?;
                close_job_pane(session, &before.cluster, &before.id)?;
                tracked_pending.remove(key);
            }
        }
        for (key, job) in &current {
            if job.pending() {
                tracked_pending.insert(key.clone(), job.clone());
            } else if job.running() {
                tracked_pending.remove(key);
            }
            missing_pending.remove(key);
        }
        for (key, pending) in tracked_pending.clone() {
            if current.contains_key(&key) {
                continue;
            }
            if warnings
                .iter()
                .any(|warning| warning.to_lowercase().contains(&pending.cluster))
            {
                continue;
            }
            let count = missing_pending.entry(key.clone()).or_default();
            *count += 1;
            if *count >= 2 {
                tmux([
                    "display-message",
                    "-d",
                    "5000",
                    "-t",
                    session,
                    &format!("Job {} left the queue before start", pending.id),
                ])?;
                close_job_pane(session, &pending.cluster, &pending.id)?;
                tracked_pending.remove(&key);
                missing_pending.remove(&key);
            }
        }
        observed = current;
    }
}

fn monitor_additions(
    observed: &HashMap<(String, String), Job>,
    current: &HashMap<(String, String), Job>,
) -> Vec<Job> {
    current
        .iter()
        .filter(|(key, job)| {
            job.active()
                && !job.blocked_category()
                && match observed.get(*key) {
                    None => true,
                    Some(before) => before.pending() && job.running(),
                }
        })
        .map(|(_, job)| job.clone())
        .collect()
}

fn close_job_pane(session: &str, cluster: &str, job_id: &str) -> Result<()> {
    for pane in panes(session)? {
        if pane.cluster == cluster && pane.job_id == job_id {
            tmux(["kill-pane", "-t", &pane.id])?;
        }
    }
    Ok(())
}

fn obsolete_panes<'a>(
    current: &'a [Pane],
    desired: &HashSet<(String, String)>,
) -> (Vec<&'a Pane>, Option<&'a Pane>) {
    let mut obsolete: Vec<_> = current
        .iter()
        .filter(|pane| !desired.contains(&(pane.cluster.clone(), pane.job_id.clone())))
        .collect();
    let anchor = (obsolete.len() == current.len())
        .then(|| obsolete.pop())
        .flatten();
    (obsolete, anchor)
}
