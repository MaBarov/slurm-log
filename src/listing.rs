fn resolve_detail_cluster(config: &Config, requested: &str, id: &str) -> Result<String> {
    if requested != "both" {
        details::validate_cluster(config, requested)?;
        return Ok(requested.into());
    }
    let (jobs, _, _) = slurm::all_jobs(config, "both", "all", false)?;
    let mut matches: Vec<_> = jobs
        .iter()
        .filter(|job| job.id == id)
        .map(|job| job.cluster.as_str())
        .collect();
    matches.sort_unstable();
    matches.dedup();
    let choices = config
        .clusters
        .iter()
        .map(|cluster| cluster.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    match matches.as_slice() {
        [cluster] => Ok((*cluster).into()),
        [] => bail!("job {id} is not in the live/recent cache; specify --cluster NAME ({choices})"),
        _ => bail!("job {id} exists on multiple clusters; specify --cluster NAME ({choices})"),
    }
}

fn render(jobs: &[Job], warnings: &[String]) {
    println!(" #  CLUSTER  JOB ID          STATE                ELAPSED     NAME / REASON");
    for (index, job) in jobs.iter().enumerate() {
        println!(
            "{:2}  {:<7}  {:<15} {:<20} {:<11} {} {}",
            index + 1,
            job.cluster,
            job.id,
            job.state,
            job.elapsed,
            job.name,
            if job.insight().is_empty() {
                job.reason.clone()
            } else {
                job.insight()
            }
        );
    }
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}

fn choose_fzf(jobs: &[Job]) -> Result<Vec<Job>> {
    use std::io::Write;
    let mut child = Command::new("fzf")
        .args(["-m", "--delimiter=\\t", "--with-nth=2..", "--prompt=logs> "])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    if let Some(input) = child.stdin.as_mut() {
        for (index, job) in jobs.iter().enumerate() {
            writeln!(
                input,
                "{index}\t{}\t{}\t{}\t{}",
                job.cluster, job.id, job.state, job.name
            )?;
        }
    }
    let output = child.wait_with_output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .filter_map(|line| line.split('\t').next()?.parse::<usize>().ok())
        .filter_map(|index| jobs.get(index).cloned())
        .collect())
}
