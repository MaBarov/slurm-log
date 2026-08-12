pub fn monitor(config: &Config, session: &str, lines: usize, refresh_seconds: u64) -> Result<()> {
    let (initial, _, _) = crate::slurm::all_jobs(config, "both", "all", false)?;
    let mut state = MonitorState::new(initial);
    loop {
        if !auto_enabled(session)? {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(refresh_seconds));
        // Ask the daemon for a fresh frame. Its force-ratelimit coalesces this
        // with other clients and prevents every monitor from issuing its own
        // scheduler RPC while still avoiding indefinitely stale transitions.
        let (jobs, _, warnings) = crate::slurm::all_jobs_fresh(config, "both", "all", false)?;
        let update = state.update(jobs, &warnings);
        if !update.additions.is_empty() {
            let mut desired: Vec<Job> = panes(session)?
                .into_iter()
                .map(|p| Job {
                    cluster: p.cluster,
                    id: p.job_id,
                    ..Job::default()
                })
                .collect();
            desired.extend(update.additions);
            reconcile(config, session, &desired, lines, false)?;
        }
        for event in update.events {
            let (job, message, close) = match event {
                MonitorEvent::Started(job) => (job, "started", false),
                MonitorEvent::Failed(job) => (job, "failed before start", true),
                MonitorEvent::Vanished(job) => (job, "left the queue before start", true),
            };
            tmux([
                "display-message",
                "-d",
                "5000",
                "-t",
                session,
                &format!("Job {} {message}", job.id),
            ])?;
            if close {
                close_job_pane(session, &job.cluster, &job.id)?;
            }
        }
    }
}

type JobKey = (String, String);

struct MonitorState {
    observed: HashMap<JobKey, Job>,
    tracked_pending: HashMap<JobKey, Job>,
    missing_pending: HashMap<JobKey, u8>,
}

struct MonitorUpdate {
    additions: Vec<Job>,
    events: Vec<MonitorEvent>,
}

#[derive(Debug)]
enum MonitorEvent {
    Started(Job),
    Failed(Job),
    Vanished(Job),
}

impl MonitorState {
    fn new(jobs: Vec<Job>) -> Self {
        let observed: HashMap<_, _> = jobs
            .into_iter()
            .map(|job| ((job.cluster.clone(), job.id.clone()), job))
            .collect();
        let tracked_pending = observed
            .iter()
            .filter(|(_, job)| job.pending())
            .map(|(key, job)| (key.clone(), job.clone()))
            .collect();
        Self {
            observed,
            tracked_pending,
            missing_pending: HashMap::new(),
        }
    }

    fn update(&mut self, jobs: Vec<Job>, warnings: &[String]) -> MonitorUpdate {
        let current: HashMap<_, _> = jobs
            .into_iter()
            .map(|job| ((job.cluster.clone(), job.id.clone()), job))
            .collect();
        let additions = monitor_additions(&self.observed, &current);
        let mut events = Vec::new();
        for (key, before) in &self.observed {
            if before.pending() && current.get(key).is_some_and(Job::running) {
                events.push(MonitorEvent::Started(before.clone()));
            }
            if before.pending() && current.get(key).is_some_and(Job::failed) {
                events.push(MonitorEvent::Failed(before.clone()));
                self.tracked_pending.remove(key);
            }
        }
        for (key, job) in &current {
            if job.pending() {
                self.tracked_pending.insert(key.clone(), job.clone());
            } else if job.running() {
                self.tracked_pending.remove(key);
            }
            self.missing_pending.remove(key);
        }
        let warnings: Vec<_> = warnings.iter().map(|warning| warning.to_lowercase()).collect();
        for (key, pending) in self.tracked_pending.clone() {
            if current.contains_key(&key)
                || warnings
                    .iter()
                    .any(|warning| warning.contains(&pending.cluster))
            {
                continue;
            }
            let count = self.missing_pending.entry(key.clone()).or_default();
            *count += 1;
            if *count >= 2 {
                events.push(MonitorEvent::Vanished(pending));
                self.tracked_pending.remove(&key);
                self.missing_pending.remove(&key);
            }
        }
        self.observed = current;
        MonitorUpdate { additions, events }
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
