/// Ordinary list paths validate the scheduler-returned owner before any row
/// reaches a cache or MCP resource. `-u` remains a load reduction, not an
/// authorization assumption.
fn parse_owned_queue(input: &str, cluster: &str, owner: &str) -> Vec<Job> {
    let mut canonical = String::new();
    for line in input.lines() {
        let fields: Vec<_> = line.split('|').map(str::trim).collect();
        if fields.len() != 10 || fields[9] != owner {
            continue;
        }
        canonical.push_str(fields[0]);
        for field in &fields[1..9] {
            canonical.push('|');
            canonical.push_str(field);
        }
        canonical.push('\n');
    }
    parse_queue(&canonical, cluster)
}

/// Accounting additionally reports the real controller. Bound cluster labels
/// must match it; legacy unbound local labels still validate the exact owner.
fn parse_owned_recent(
    input: &str,
    cluster: &str,
    owner: &str,
    controller: Option<&str>,
) -> Vec<Job> {
    let mut canonical = String::new();
    for line in input.lines() {
        let fields: Vec<_> = line.split('|').map(str::trim).collect();
        if fields.len() != 11
            || fields[9] != owner
            || controller.is_some_and(|expected| fields[10] != expected)
        {
            continue;
        }
        canonical.push_str(fields[0]);
        for field in &fields[1..9] {
            canonical.push('|');
            canonical.push_str(field);
        }
        canonical.push('\n');
    }
    parse_recent(&canonical, cluster)
}

#[cfg(test)]
mod listing_auth_tests {
    use super::*;

    #[test]
    fn list_rows_require_returned_owner_and_bound_controller() {
        let queue = "1|RUNNING|kept|00:01|node|cpu|now|1|run.sbatch|owner\n\
                     2|RUNNING|foreign|00:01|node|cpu|now|1|run.sbatch|other\n";
        assert_eq!(parse_owned_queue(queue, "label", "owner").len(), 1);

        let recent = "3|COMPLETED|kept|00:02|now|0:0|1G|cpu=1|cpu|owner|ctrl\n\
                      4|FAILED|foreign|00:02|now|1:0|1G|cpu=1|cpu|other|ctrl\n\
                      5|FAILED|wrong-cluster|00:02|now|1:0|1G|cpu=1|cpu|owner|wrong\n";
        let jobs = parse_owned_recent(recent, "label", "owner", Some("ctrl"));
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "3");
    }
}
