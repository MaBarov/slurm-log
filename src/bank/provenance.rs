/// Resolve the HEAD commit of the repository containing `root` by reading git
/// metadata files directly.  Never shells out, so a hostile worktree cannot
/// select an arbitrary command.
fn repo_head_commit(root: &Path) -> Option<String> {
    let canonical = fs::canonicalize(root).ok()?;
    let repository = canonical.ancestors().find(|dir| dir.join(".git").exists())?;
    let git = repository.join(".git");
    let (git_dir, head) = if git.is_file() {
        let pointer = fs::read_to_string(&git).ok()?;
        let directory = pointer.strip_prefix("gitdir:")?.trim();
        let git_dir = repository.join(directory);
        let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
        (git_dir, head)
    } else {
        let head = fs::read_to_string(git.join("HEAD")).ok()?;
        (git, head)
    };
    resolve_git_ref(&git_dir, head.trim()).map(|hash| hash.chars().take(12).collect())
}

fn resolve_git_ref(git_dir: &Path, head: &str) -> Option<String> {
    if let Some(reference) = head.strip_prefix("ref: ") {
        return fs::read_to_string(git_dir.join(reference))
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| valid_commit_hash(value))
            .or_else(|| {
                fs::read_to_string(git_dir.join("packed-refs")).ok().and_then(|packed| {
                    packed.lines().find_map(|line| {
                        let mut fields = line.split_whitespace();
                        let hash = fields.next()?;
                        (fields.next() == Some(reference) && valid_commit_hash(hash))
                            .then(|| hash.to_string())
                    })
                })
            });
    }
    valid_commit_hash(head).then(|| head.to_string())
}

fn valid_commit_hash(value: &str) -> bool {
    value.len() >= 12 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn directive_job_name(directives: &[String]) -> Option<String> {
    directives.iter().find_map(|line| {
        line.strip_prefix("--job-name=")
            .or_else(|| line.strip_prefix("--job-name "))
            .or_else(|| line.strip_prefix("-J="))
            .map(str::to_string)
            .or_else(|| line.strip_prefix("-J ").map(str::to_string))
    })
}

/// Reject an sbatch directive that would send a previewed script to a
/// controller other than the selected target.  Both long and short Slurm
/// spellings are accepted only when their sole value equals the configured
/// controller identity.
pub fn validate_script_controller(script: &Script, target: &ClusterConfig) -> Result<()> {
    for directive in &script.directives {
        let Some(value) = routing_directive_value(directive)? else {
            continue;
        };
        if value != target.controller() {
            bail!(
                "script routing directive selects controller {value:?}, not configured controller {:?}",
                target.controller()
            );
        }
    }
    Ok(())
}

fn routing_directive_value(directive: &str) -> Result<Option<&str>> {
    for option in ["--clusters", "--cluster"] {
        if let Some(value) = directive.strip_prefix(&format!("{option}=")) {
            return routing_controller(value).map(Some);
        }
        if directive == option {
            return routing_controller("").map(Some);
        }
        if let Some(value) = directive
            .strip_prefix(option)
            .and_then(|value| value.strip_prefix(char::is_whitespace))
        {
            return routing_controller(value).map(Some);
        }
    }
    let Some(value) = directive.strip_prefix("-M") else {
        return Ok(None);
    };
    // Slurm accepts `-Mcontroller` as well as `-M controller` and
    // `-M=controller`; the attached spelling must not evade target checking.
    let value = value
        .strip_prefix('=')
        .or_else(|| value.strip_prefix(char::is_whitespace))
        .unwrap_or(value);
    routing_controller(value).map(Some)
}

fn routing_controller(value: &str) -> Result<&str> {
    let mut values = value.split_whitespace();
    let controller = values
        .next()
        .filter(|value| !value.is_empty())
        .context("sbatch routing directive requires one controller name")?;
    if values.next().is_some() {
        bail!("sbatch routing directive must contain exactly one controller name");
    }
    Ok(controller)
}
