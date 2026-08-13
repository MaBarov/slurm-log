use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::Path,
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

#[derive(Clone, Debug)]
struct ResolvedLog {
    cluster: String,
    id: String,
    name: String,
    state: String,
    terminal: bool,
    path: String,
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
    let result = if target.remote() {
        remote_read(&target.ssh_host, &resolved.path, &mode)
    } else {
        local_read(Path::new(&resolved.path), &mode)
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
    let queued = slurm::queued(config, cluster);
    let queue_known = queued.is_ok();
    let active = queued
        .unwrap_or_default()
        .into_iter()
        .find(|job| job.id == id);
    let terminal = queue_known && active.as_ref().is_none_or(|job| !job.active());
    match slurm::terminal_path(config, cluster, id) {
        Ok((Some(path), name)) => Ok(Ok(ResolvedLog {
            cluster: cluster.into(),
            id: id.into(),
            name: active.as_ref().map_or(name, |job| job.name.clone()),
            state: active
                .as_ref()
                .map(|job| job.state.clone())
                .unwrap_or_default(),
            terminal,
            path,
        })),
        Ok((None, _)) => Ok(Err(LogData::unavailable(
            cluster,
            id,
            active.as_ref(),
            "no_stdout",
        ))),
        Err(_) if active.as_ref().is_some_and(Job::active) => Ok(Err(LogData::unavailable(
            cluster,
            id,
            active.as_ref(),
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

fn local_read(path: &Path, mode: &ReadMode) -> Result<ReadResult> {
    let mut file = File::open(path).with_context(|| format!("open job log {}", path.display()))?;
    let metadata = file.metadata()?;
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

fn remote_read(host: &str, path: &str, mode: &ReadMode) -> Result<ReadResult> {
    let quoted = command::shell_quote(path);
    let body = match mode {
        ReadMode::Metadata => String::new(),
        ReadMode::Window(maximum) => format!("tail -c {maximum} -- {quoted}"),
        ReadMode::Range(start, maximum) => {
            let first = start.saturating_add(1);
            format!("tail -c +{first} -- {quoted} | head -c {maximum}")
        }
    };
    let script = format!("set -e; stat -Lc 'SLURMLOG|%d|%i|%s|%Y' -- {quoted}; {body}");
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
mod tests {
    use super::*;

    #[test]
    fn bounds_are_bounded_and_range_starts_are_clamped() {
        assert_eq!(read_bounds(100, &ReadMode::Metadata), (100, 0));
        assert_eq!(read_bounds(100, &ReadMode::Window(20)), (80, 20));
        assert_eq!(read_bounds(10, &ReadMode::Window(20)), (0, 20));
        assert_eq!(read_bounds(10, &ReadMode::Range(99, 4)), (10, 4));
    }

    #[test]
    fn generation_is_stable_but_cluster_and_inode_scoped() {
        let value = generation("one", "123", "1:2");
        assert_eq!(value.len(), 64);
        assert_eq!(value, generation("one", "123", "1:2"));
        assert_ne!(value, generation("two", "123", "1:2"));
        assert_ne!(value, generation("one", "123", "1:3"));
    }

    #[test]
    fn local_reads_cover_metadata_tail_ranges_and_missing_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("job.log");
        std::fs::write(&path, b"0123456789").unwrap();
        let metadata = local_read(&path, &ReadMode::Metadata).unwrap();
        assert_eq!((metadata.1, metadata.3, metadata.4), (10, 10, Vec::new()));
        let window = local_read(&path, &ReadMode::Window(4)).unwrap();
        assert_eq!((window.3, window.4), (6, b"6789".to_vec()));
        let range = local_read(&path, &ReadMode::Range(2, 3)).unwrap();
        assert_eq!((range.3, range.4), (2, b"234".to_vec()));
        assert!(local_read(&directory.path().join("missing"), &ReadMode::Metadata).is_err());
    }
}
