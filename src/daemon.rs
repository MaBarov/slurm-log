use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{config::Config, details::JobDetails, log_service::LogData, model::Job, state::Ledger};

const IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MEMORY_TTL: Duration = Duration::from_secs(5);
const DETAIL_TTL: Duration = Duration::from_secs(60);
pub(crate) const ACTIVE_DETAIL_TTL: Duration = Duration::from_secs(30);
pub(crate) const FORCED_DETAIL_MINIMUM: Duration = Duration::from_secs(10);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Serialize, Deserialize)]
enum Request {
    Query {
        cluster: String,
        filter: String,
        archive: bool,
        force: bool,
    },
    // Keep control variants before newly added requests: rmp-serde encodes
    // enum indexes, so their positions are part of the daemon upgrade wire
    // protocol used while an older process may still be shutting down.
    Ping,
    Stop,
    Details {
        cluster: String,
        id: String,
        force: bool,
    },
    LogMetadata {
        cluster: String,
        id: String,
    },
    LogWindow {
        cluster: String,
        id: String,
        max_bytes: usize,
    },
    LogRead {
        cluster: String,
        id: String,
        start: u64,
        max_bytes: usize,
    },
}

#[derive(Clone, Serialize, Deserialize)]
struct Reply {
    jobs: Vec<Job>,
    ledger: Ledger,
    warnings: Vec<String>,
    error: Option<String>,
    #[serde(default)]
    details: Option<JobDetails>,
}

struct CachedReply {
    created: Instant,
    last_access: Instant,
    refreshing: bool,
    last_force: Option<Instant>,
    // Immutable snapshots release the cache mutex after an O(1) Arc clone.
    // Large archive filtering and serialization no longer block background
    // refreshes or cache bookkeeping.
    reply: Arc<Reply>,
}

type SharedCache = Arc<Mutex<HashMap<String, CachedReply>>>;

struct DetailEntry {
    created: Instant,
    last_access: Instant,
    failures: u32,
    details: JobDetails,
}
type DetailCache = Arc<Mutex<HashMap<String, DetailEntry>>>;

struct LogEntry {
    created: Instant,
    last_access: Instant,
    base_generation: String,
    generation_epoch: u64,
    data: LogData,
}
type LogCache = Arc<Mutex<HashMap<String, LogEntry>>>;

fn paths(config: &Config) -> (PathBuf, PathBuf) {
    let directory = config
        .state_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    (directory.join("daemon.sock"), directory.join("daemon.lock"))
}

fn exchange(socket: &PathBuf, request: &Request) -> Result<Reply> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
    stream.set_write_timeout(Some(CLIENT_TIMEOUT))?;
    write_frame(&mut stream, request)?;
    read_frame(&mut stream)
}

fn start(config: &Config) -> Result<()> {
    let mut command = Command::new(&config.executable);
    command.args(config.child_args()).args(["daemon", "run"]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn().context("start slurm-log daemon")?;
    Ok(())
}

pub fn query(
    config: &Config,
    cluster: &str,
    filter: &str,
    archive: bool,
    force: bool,
) -> Result<(Vec<Job>, Ledger, Vec<String>)> {
    let (socket, _) = paths(config);
    let request = Request::Query {
        cluster: cluster.into(),
        filter: filter.into(),
        archive,
        force,
    };
    let mut reply = exchange(&socket, &request).or_else(|_| {
        start(config)?;
        for _ in 0..50 {
            thread::sleep(Duration::from_millis(10));
            if let Ok(reply) = exchange(&socket, &request) {
                return Ok(reply);
            }
        }
        bail!("daemon did not start")
    })?;
    if let Some(error) = reply.error.take() {
        bail!(error);
    }
    Ok((reply.jobs, reply.ledger, reply.warnings))
}

pub fn job_details(config: &Config, cluster: &str, id: &str, force: bool) -> Result<JobDetails> {
    crate::details::validate_cluster(config, cluster)?;
    if !crate::model::valid_job_id(id) {
        bail!("invalid job ID {id}");
    }
    // The daemon can answer from a short-lived cache; bind even that cache
    // read to a fresh exact-owner lookup so a reused ID cannot surface data
    // for a now-foreign job.
    crate::slurm::authorize_exact_job(config, cluster, id)?;
    let (socket, _) = paths(config);
    let request = Request::Details {
        cluster: cluster.into(),
        id: id.into(),
        force,
    };
    let mut reply = exchange(&socket, &request).or_else(|_| {
        start(config)?;
        for _ in 0..50 {
            thread::sleep(Duration::from_millis(10));
            if let Ok(reply) = exchange(&socket, &request) {
                return Ok(reply);
            }
        }
        bail!("daemon did not start")
    })?;
    if let Some(error) = reply.error.take() {
        bail!(error);
    }
    reply
        .details
        .ok_or_else(|| anyhow::anyhow!("daemon returned no job details"))
}

pub fn log_metadata(config: &Config, cluster: &str, id: &str) -> Result<LogData> {
    crate::slurm::authorize_exact_job(config, cluster, id)?;
    log_request(
        config,
        Request::LogMetadata {
            cluster: cluster.into(),
            id: id.into(),
        },
        || crate::log_service::metadata(config, cluster, id),
    )
}

pub fn log_window(config: &Config, cluster: &str, id: &str, max_bytes: usize) -> Result<LogData> {
    crate::slurm::authorize_exact_job(config, cluster, id)?;
    log_request(
        config,
        Request::LogWindow {
            cluster: cluster.into(),
            id: id.into(),
            max_bytes,
        },
        || crate::log_service::recent_window(config, cluster, id, max_bytes),
    )
}

pub fn log_range(
    config: &Config,
    cluster: &str,
    id: &str,
    start: u64,
    max_bytes: usize,
) -> Result<LogData> {
    crate::slurm::authorize_exact_job(config, cluster, id)?;
    log_request(
        config,
        Request::LogRead {
            cluster: cluster.into(),
            id: id.into(),
            start,
            max_bytes,
        },
        || crate::log_service::range(config, cluster, id, start, max_bytes),
    )
}

fn log_request(
    config: &Config,
    request: Request,
    direct: impl FnOnce() -> Result<LogData>,
) -> Result<LogData> {
    let (socket, _) = paths(config);
    match log_exchange(&socket, &request).or_else(|_| {
        start(config)?;
        for _ in 0..50 {
            thread::sleep(Duration::from_millis(10));
            if let Ok(reply) = log_exchange(&socket, &request) {
                return Ok(reply);
            }
        }
        bail!("daemon did not start")
    }) {
        Ok(value) => Ok(value),
        Err(_) => direct(),
    }
}

fn log_exchange(socket: &PathBuf, request: &Request) -> Result<LogData> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
    stream.set_write_timeout(Some(CLIENT_TIMEOUT))?;
    write_frame(&mut stream, request)?;
    read_frame(&mut stream)
}

pub fn command(config: &Config, action: Option<&str>) -> Result<()> {
    let (socket, _) = paths(config);
    match action.unwrap_or("status") {
        "run" => run(config),
        "start" => {
            if exchange(&socket, &Request::Ping).is_err() {
                start(config)?;
            }
            println!("slurm-log daemon started");
            Ok(())
        }
        "status" => {
            if exchange(&socket, &Request::Ping).is_ok() {
                println!("slurm-log daemon is running");
                Ok(())
            } else {
                bail!("slurm-log daemon is stopped")
            }
        }
        "stop" => {
            exchange(&socket, &Request::Stop)?;
            println!("slurm-log daemon stopped");
            Ok(())
        }
        other => bail!("unknown daemon command: {other}"),
    }
}

pub(crate) fn is_running(config: &Config) -> bool {
    let (socket, _) = paths(config);
    exchange(&socket, &Request::Ping).is_ok()
}

pub(crate) fn ensure_running(config: &Config) -> Result<()> {
    if is_running(config) {
        Ok(())
    } else {
        start_for_lifecycle(config)
    }
}

pub(crate) fn stop_for_lifecycle(config: &Config) -> Result<bool> {
    if !is_running(config) {
        return Ok(false);
    }
    let (socket, _) = paths(config);
    exchange(&socket, &Request::Stop)?;
    Ok(true)
}

pub(crate) fn start_for_lifecycle(config: &Config) -> Result<()> {
    start(config)?;
    let (socket, _) = paths(config);
    for _ in 0..50 {
        thread::sleep(Duration::from_millis(10));
        if exchange(&socket, &Request::Ping).is_ok() {
            return Ok(());
        }
    }
    bail!("daemon did not restart after lifecycle operation")
}

include!("daemon/server.rs");
include!("daemon/cache.rs");
include!("daemon/log.rs");

#[cfg(test)]
mod tests;
#[cfg(test)]
#[path = "daemon/tests/cache.rs"]
mod tests_cache;
#[cfg(test)]
#[path = "daemon/tests/server.rs"]
mod tests_server;
