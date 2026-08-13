use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Component, Path},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{command, config::Config, model::Job, slurm};

pub const MAX_LOG_PAYLOAD: usize = 512 * 1024;
pub const MAX_LOG_WINDOW: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct LogData {
    pub status: String,
    pub cluster: String,
    pub job_id: String,
    pub job_name: String,
    pub state: String,
    pub terminal: bool,
    pub generation: String,
    pub size: u64,
    pub modified: i64,
    pub offset: u64,
    pub bytes: Vec<u8>,
}

impl LogData {
    fn unavailable(cluster: &str, id: &str, job: Option<&Job>, status: &str) -> Self {
        Self {
            status: status.into(),
            cluster: cluster.into(),
            job_id: id.into(),
            job_name: job.map(|value| value.name.clone()).unwrap_or_default(),
            state: job.map(|value| value.state.clone()).unwrap_or_default(),
            terminal: job.is_none_or(|value| !value.active()),
            ..Self::default()
        }
    }

    pub fn metadata_only(mut self) -> Self {
        self.bytes.clear();
        self.offset = self.size;
        self
    }
}

#[derive(Debug)]
struct ResolvedLog {
    cluster: String,
    id: String,
    name: String,
    state: String,
    terminal: bool,
    source: LogSource,
}

#[derive(Debug)]
enum LogSource {
    Local(File),
    Remote(String),
}

pub fn metadata(config: &Config, cluster: &str, id: &str) -> Result<LogData> {
    read(config, cluster, id, ReadMode::Metadata)
}

pub fn recent_window(
    config: &Config,
    cluster: &str,
    id: &str,
    max_bytes: usize,
) -> Result<LogData> {
    let maximum = max_bytes.clamp(1, MAX_LOG_WINDOW);
    read(config, cluster, id, ReadMode::Window(maximum))
}

pub fn range(
    config: &Config,
    cluster: &str,
    id: &str,
    start: u64,
    max_bytes: usize,
) -> Result<LogData> {
    let maximum = max_bytes.clamp(1, MAX_LOG_PAYLOAD);
    read(config, cluster, id, ReadMode::Range(start, maximum))
}

enum ReadMode {
    Metadata,
    Window(usize),
    Range(u64, usize),
}

fn read(config: &Config, cluster: &str, id: &str, mode: ReadMode) -> Result<LogData> {
    let resolved = match resolve(config, cluster, id)? {
        Ok(value) => value,
        Err(value) => return Ok(value),
    };
    let target = config.cluster(cluster)?;
    let result = match &resolved.source {
        LogSource::Remote(path) => {
            remote_read(&target.ssh_host, &target.working_directory, path, &mode)
        }
        LogSource::Local(file) => local_read(file, &mode),
    };
    let (identity, size, modified, offset, bytes) = match result {
        Ok(value) => value,
        Err(_) if !resolved.terminal => {
            return Ok(LogData::unavailable(
                cluster,
                id,
                Some(&resolved_job(&resolved)),
                "pending_log",
            ));
        }
        Err(_) => return Ok(LogData::unavailable(cluster, id, None, "not_found")),
    };
    Ok(LogData {
        status: "available".into(),
        cluster: resolved.cluster,
        job_id: resolved.id,
        job_name: resolved.name,
        state: resolved.state,
        terminal: resolved.terminal,
        generation: generation(cluster, id, &identity),
        size,
        modified,
        offset,
        bytes,
    })
}

fn resolve(config: &Config, cluster: &str, id: &str) -> Result<Result<ResolvedLog, LogData>> {
    let target = config.cluster(cluster)?;
    if !crate::model::valid_job_id(id) {
        bail!("invalid job ID {id}");
    }
    // Do not use the 15-second queue cache for any authorization decision.
    // This fresh result is immediately bound to the metadata request below.
    let authorized = slurm::authorize_exact_job(config, cluster, id)?;
    let terminal = !authorized.active();
    match slurm::terminal_path_authorized(config, cluster, id, &authorized) {
        Ok((Some(path), name)) => match confined_log_source(config, cluster, &path) {
            Ok(source) => Ok(Ok(ResolvedLog {
                cluster: cluster.into(),
                id: id.into(),
                name: if authorized.name.is_empty() {
                    name
                } else {
                    authorized.name.clone()
                },
                state: authorized.state.clone(),
                terminal,
                source,
            })),
            Err(error) => {
                let status = if authorized.active() && crate::secure_open::is_missing(&error) {
                    "pending_log"
                } else {
                    "no_stdout"
                };
                Ok(Err(LogData::unavailable(
                    cluster,
                    id,
                    Some(&authorized),
                    status,
                )))
            }
        },
        Ok((None, _)) => Ok(Err(LogData::unavailable(
            cluster,
            id,
            Some(&authorized),
            "no_stdout",
        ))),
        Err(_) if authorized.active() => Ok(Err(LogData::unavailable(
            cluster,
            id,
            Some(&authorized),
            "pending_log",
        ))),
        Err(_) if !target.accounting => Ok(Err(LogData::unavailable(
            cluster,
            id,
            None,
            "accounting_unavailable",
        ))),
        Err(_) => Ok(Err(LogData::unavailable(cluster, id, None, "not_found"))),
    }
}

fn resolved_job(value: &ResolvedLog) -> Job {
    Job {
        cluster: value.cluster.clone(),
        id: value.id.clone(),
        name: value.name.clone(),
        state: value.state.clone(),
        ..Job::default()
    }
}

type ReadResult = (String, u64, i64, u64, Vec<u8>);

fn local_read(opened: &File, mode: &ReadMode) -> Result<ReadResult> {
    // `opened` was produced by secure_open with openat2 resolution beneath a
    // pinned directory descriptor. Re-check the descriptor, not a pathname.
    let mut file = opened
        .try_clone()
        .context("clone secured job log descriptor")?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        bail!("job log descriptor is not a single-link regular file");
    }
    let size = metadata.len();
    let (offset, maximum) = read_bounds(size, mode);
    let mut bytes = Vec::with_capacity(maximum.min(size.saturating_sub(offset) as usize));
    if maximum > 0 {
        file.seek(SeekFrom::Start(offset))?;
        file.take(maximum as u64).read_to_end(&mut bytes)?;
    }
    Ok((
        format!("{}:{}", metadata.dev(), metadata.ino()),
        size,
        metadata.mtime(),
        offset,
        bytes,
    ))
}

fn remote_read(host: &str, root: &Path, relative: &str, mode: &ReadMode) -> Result<ReadResult> {
    let relative = command::shell_quote(relative);
    let root = command::shell_quote(&root.display().to_string());
    let body = match mode {
        ReadMode::Metadata => String::new(),
        ReadMode::Window(maximum) => format!("tail -c {maximum} -- /proc/self/fd/3"),
        ReadMode::Range(start, maximum) => {
            let first = start.saturating_add(1);
            format!("tail -c +{first} -- /proc/self/fd/3 | head -c {maximum}")
        }
    };
    // The remote helper rejects a resolved symlink path, binds reads to fd 3,
    // and then validates that opened descriptor again. A parent swap cannot
    // redirect `tail` outside the configured root; `%h == 1` rejects hard
    // links. Unlike local openat2 this remains a bounded shell equivalent,
    // so the descriptor revalidation is the final authority.
    let script = format!(
        "set -efu; root=$(readlink -f -- {root}); path=\"$root\"/{relative}; resolved=$(readlink -f -- \"$path\"); [ \"$resolved\" = \"$path\" ] || exit 2; exec 3<\"$path\"; opened=$(readlink -f -- /proc/self/fd/3); [ \"$opened\" = \"$path\" ] || exit 2; metadata=$(stat -Lc 'SLURMLOG|%h|%F|%d|%i|%s|%Y' -- /proc/self/fd/3); case \"$metadata\" in 'SLURMLOG|1|regular file|'*) ;; *) exit 2;; esac; printf '%s\\n' \"$metadata\" | awk -F'|' '{{print $1 \"|\" $4 \"|\" $5 \"|\" $6 \"|\" $7}}'; {body}"
    );
    let output = command::output(
        "ssh",
        &[
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=120",
            "-o",
            "ControlPath=~/.ssh/slurm-log-%C",
            host,
            &script,
        ],
    )?;
    if !output.status.success() {
        bail!("remote job log is unavailable");
    }
    let newline = output
        .stdout
        .iter()
        .position(|byte| *byte == b'\n')
        .context("invalid remote log metadata")?;
    let header = std::str::from_utf8(&output.stdout[..newline])?;
    let fields: Vec<_> = header.split('|').collect();
    if fields.len() != 5 || fields[0] != "SLURMLOG" {
        bail!("invalid remote log metadata");
    }
    let size: u64 = fields[3].parse()?;
    let modified: i64 = fields[4].parse()?;
    let offset = match mode {
        ReadMode::Window(maximum) => size.saturating_sub(*maximum as u64),
        ReadMode::Range(start, _) => (*start).min(size),
        ReadMode::Metadata => size,
    };
    Ok((
        format!("{}:{}", fields[1], fields[2]),
        size,
        modified,
        offset,
        output.stdout[newline + 1..].to_vec(),
    ))
}

fn confined_log_source(config: &Config, cluster: &str, value: &str) -> Result<LogSource> {
    let target = config.cluster(cluster)?;
    let root = &target.working_directory;
    let raw = Path::new(value);
    if raw
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("job stdout path contains parent traversal");
    }
    let relative = if raw.is_absolute() {
        raw.strip_prefix(root)
            .map(Path::to_path_buf)
            .context("job stdout path is outside the configured working directory")?
    } else {
        raw.to_path_buf()
    };
    if target.remote() {
        return relative
            .to_str()
            .map(str::to_string)
            .map(LogSource::Remote)
            .context("job stdout path is not valid UTF-8");
    }
    crate::secure_open::open_regular_file_beneath(root, &relative).map(LogSource::Local)
}

fn read_bounds(size: u64, mode: &ReadMode) -> (u64, usize) {
    match mode {
        ReadMode::Metadata => (size, 0),
        ReadMode::Window(maximum) => (size.saturating_sub(*maximum as u64), *maximum),
        ReadMode::Range(start, maximum) => ((*start).min(size), *maximum),
    }
}

fn generation(cluster: &str, id: &str, identity: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"slurm-log-generation-v1\0");
    digest.update(cluster.as_bytes());
    digest.update(b"\0");
    digest.update(id.as_bytes());
    digest.update(b"\0");
    digest.update(identity.as_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
#[path = "log_service/tests.rs"]
mod tests;
