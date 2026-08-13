use std::{env, path::PathBuf, process::Command};

const TRUSTED_COMMAND_DIRECTORIES: &[&str] = &["/usr/local/bin", "/usr/bin", "/bin"];

/// Resolve every local helper before spawning it. Official builds consult
/// only administrator-controlled directories, so a project/client PATH
/// cannot replace Slurm, SSH, tmux, or the shell. Explicit test builds retain
/// fake-command injection for the hermetic process suite.
pub(super) fn trusted_program(program: &str) -> PathBuf {
    #[cfg(slurm_log_test_build)]
    if let Some(found) = search(env::var_os("PATH"), program) {
        return found;
    }
    search(
        Some(env::join_paths(TRUSTED_COMMAND_DIRECTORIES).expect("trusted command path")),
        program,
    )
    .unwrap_or_else(|| PathBuf::from("/__slurm_log_missing_command__"))
}

fn search(path: Option<impl AsRef<std::ffi::OsStr>>, program: &str) -> Option<PathBuf> {
    if program.is_empty() || program.contains('/') {
        return None;
    }
    env::split_paths(path?.as_ref()).find_map(|directory| {
        let candidate = directory.join(program);
        candidate
            .is_file()
            .then(|| candidate.canonicalize().ok())
            .flatten()
    })
}

pub(super) fn scrub_environment(command: &mut Command) {
    for (key, _) in env::vars_os() {
        let name = key.to_string_lossy();
        if unsafe_variable(&name) {
            command.env_remove(key);
        }
    }
}

fn unsafe_variable(name: &str) -> bool {
    name.starts_with("SBATCH_")
        || name.starts_with("SCANCEL_")
        || name.starts_with("LD_")
        || name.starts_with("DYLD_")
        || name.starts_with("BASH_FUNC_")
        || matches!(
            name,
            "SLURM_CLUSTERS"
                | "SLURM_HINT"
                | "SLURM_EXPORT_ENV"
                | "SLURM_CONF"
                | "SLURM_CONF_SERVER"
                | "BASH_ENV"
                | "ENV"
                | "CDPATH"
                | "GLOBIGNORE"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_resolution_ignores_ambient_path_and_rejects_path_arguments() {
        let shell = trusted_program("sh");
        assert!(shell.is_absolute());
        assert!(shell.is_file());
        assert!(
            TRUSTED_COMMAND_DIRECTORIES
                .iter()
                .any(|directory| shell.starts_with(directory))
        );
        assert_eq!(
            trusted_program("../sh"),
            PathBuf::from("/__slurm_log_missing_command__")
        );
    }

    #[test]
    fn scheduler_and_loader_control_variables_are_scrubbed() {
        for name in [
            "SBATCH_ACCOUNT",
            "SCANCEL_STATE",
            "SLURM_CONF",
            "LD_PRELOAD",
            "BASH_ENV",
        ] {
            assert!(unsafe_variable(name));
        }
        for name in ["HOME", "SSH_AUTH_SOCK", "RESEARCH_DATASET"] {
            assert!(!unsafe_variable(name));
        }
    }
}
