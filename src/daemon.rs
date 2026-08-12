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
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::{config::Config, details::JobDetails, model::Job, state::Ledger};

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

fn run(config: &Config) -> Result<()> {
    let (socket, lock_path) = paths(config);
    if let Some(parent) = socket.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(());
    }
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    // A blocking accept thread gives immediate wakeups without the old 10 ms
    // polling quantum. recv_timeout provides idle shutdown independently.
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            if sender.send(stream).is_err() {
                break;
            }
        }
    });
    let cache: SharedCache = Arc::new(Mutex::new(HashMap::new()));
    let detail_cache: DetailCache = Arc::new(Mutex::new(HashMap::new()));
    start_refresh_loop(config.clone(), Arc::clone(&cache));
    while let Ok(mut stream) = receiver.recv_timeout(IDLE_TIMEOUT) {
        let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
        let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));
        match handle_stream(config, &cache, &detail_cache, &mut stream) {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) => {
                // A malformed or disconnected client must not terminate the
                // daemon. Return a bounded error when possible, then continue.
                let _ = write_reply(
                    &mut stream,
                    &Reply {
                        error: Some(format!("invalid daemon request: {error:#}")),
                        ..empty_reply()
                    },
                );
            }
        }
    }
    let _ = fs::remove_file(socket);
    Ok(())
}

fn handle_stream(
    config: &Config,
    cache: &SharedCache,
    detail_cache: &DetailCache,
    stream: &mut UnixStream,
) -> Result<bool> {
    let request: Request = read_frame(stream)?;
    let reply = match request {
        Request::Ping => empty_reply(),
        Request::Stop => {
            write_reply(stream, &empty_reply())?;
            return Ok(true);
        }
        Request::Details { cluster, id, force } => {
            crate::details::validate_cluster(config, &cluster)?;
            if !crate::model::valid_job_id(&id) {
                bail!("invalid job ID {id}");
            }
            let key = format!("{cluster}\0{id}");
            let mut previous = None;
            let cached = {
                let mut entries = detail_cache
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                entries.retain(|_, entry| entry.last_access.elapsed() < Duration::from_secs(60));
                entries.get_mut(&key).and_then(|entry| {
                    entry.last_access = Instant::now();
                    previous = Some(entry.details.clone());
                    let base = if entry.details.terminal {
                        DETAIL_TTL
                    } else {
                        ACTIVE_DETAIL_TTL
                    };
                    let normal = base.saturating_mul(1_u32 << entry.failures.min(2));
                    let minimum = if force { FORCED_DETAIL_MINIMUM } else { normal };
                    (entry.details.terminal || entry.created.elapsed() < minimum)
                        .then(|| entry.details.clone())
                })
            };
            let details = if let Some(details) = cached {
                Ok(details)
            } else {
                crate::details::fetch(config, &cluster, &id, previous.as_ref())
            };
            let reply = match details {
                Ok(details) => {
                    let now = Instant::now();
                    let mut entries = detail_cache
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if entries.len() >= 64
                        && !entries.contains_key(&key)
                        && let Some(oldest) = entries
                            .iter()
                            .min_by_key(|(_, value)| value.last_access)
                            .map(|(key, _)| key.clone())
                    {
                        entries.remove(&oldest);
                    }
                    entries.insert(
                        key,
                        DetailEntry {
                            created: now,
                            last_access: now,
                            failures: 0,
                            details: details.clone(),
                        },
                    );
                    Reply {
                        details: Some(details),
                        ..empty_reply()
                    }
                }
                Err(error) => {
                    let mut entries = detail_cache
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if let Some(entry) = entries.get_mut(&key) {
                        entry.failures = entry.failures.saturating_add(1);
                        entry.created = Instant::now();
                        let mut stale = entry.details.clone();
                        stale.stale_error = format!("{error:#}");
                        Reply {
                            details: Some(stale),
                            ..empty_reply()
                        }
                    } else {
                        Reply {
                            error: Some(format!("{error:#}")),
                            ..empty_reply()
                        }
                    }
                }
            };
            write_reply(stream, &reply)?;
            return Ok(false);
        }
        Request::Query {
            cluster,
            filter,
            archive,
            force,
        } => {
            crate::slurm::validate_query(&cluster, &filter)?;
            let key = cache_key(&cluster, archive);
            if force {
                let throttled = {
                    let mut entries = cache.lock().unwrap_or_else(|error| error.into_inner());
                    entries.get_mut(&key).and_then(|entry| {
                        entry.last_access = Instant::now();
                        entry
                            .last_force
                            .is_some_and(|last| last.elapsed() < FORCED_DETAIL_MINIMUM)
                            .then(|| Arc::clone(&entry.reply))
                    })
                };
                if let Some(snapshot) = throttled {
                    let reply = filtered_reply(&snapshot, &cluster, &filter);
                    write_reply(stream, &reply)?;
                    return Ok(false);
                }
            }
            if !force {
                let cached = {
                    let mut entries = cache.lock().unwrap_or_else(|error| error.into_inner());
                    entries.get_mut(&key).map(|entry| {
                        entry.last_access = Instant::now();
                        Arc::clone(&entry.reply)
                    })
                };
                if let Some(snapshot) = cached {
                    // A stale snapshot is deliberately returned immediately.
                    // The refresh loop updates it in the background, so opening
                    // another picker never waits for SSH or scheduler RPCs.
                    let reply = filtered_reply(&snapshot, &cluster, &filter);
                    write_reply(stream, &reply)?;
                    return Ok(false);
                }
            }
            if force {
                crate::slurm::invalidate_caches(config);
                cache
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(&key);
            }
            let canonical = match crate::slurm::all_jobs_direct(config, &cluster, "all", archive) {
                Ok((jobs, ledger, warnings)) => Reply {
                    jobs,
                    ledger,
                    warnings,
                    error: None,
                    details: None,
                },
                Err(error) => Reply {
                    error: Some(format!("{error:#}")),
                    ..empty_reply()
                },
            };
            let reply = filtered_reply(&canonical, &cluster, &filter);
            write_reply(stream, &reply)?;
            let now = Instant::now();
            let mut entries = cache.lock().unwrap_or_else(|error| error.into_inner());
            entries.insert(
                key,
                CachedReply {
                    created: now,
                    last_access: now,
                    refreshing: false,
                    last_force: force.then_some(now),
                    reply: Arc::new(canonical),
                },
            );
            invalidate_older_combined(&mut entries, &cluster, archive, now);
            return Ok(false);
        }
    };
    write_reply(stream, &reply)?;
    Ok(false)
}

fn cache_key(cluster: &str, archive: bool) -> String {
    format!("{cluster}\0{archive}")
}

fn invalidate_older_combined(
    entries: &mut HashMap<String, CachedReply>,
    cluster: &str,
    archive: bool,
    cluster_created: Instant,
) {
    if matches!(cluster, "all" | "both") {
        return;
    }
    for combined in ["all", "both"] {
        let key = cache_key(combined, archive);
        if entries
            .get(&key)
            .is_some_and(|entry| entry.created <= cluster_created)
        {
            entries.remove(&key);
        }
    }
}

fn filtered_reply(reply: &Reply, cluster: &str, filter: &str) -> Reply {
    Reply {
        jobs: reply
            .jobs
            .iter()
            .filter(|job| {
                (matches!(cluster, "all" | "both") || job.cluster == cluster)
                    && match filter {
                        "running" => job.running(),
                        "failed" => job.failed(),
                        "blocked" => job.blocked_category(),
                        _ => true,
                    }
            })
            .cloned()
            .collect(),
        ledger: reply.ledger.clone(),
        warnings: reply.warnings.clone(),
        error: reply.error.clone(),
        details: reply.details.clone(),
    }
}

fn start_refresh_loop(config: Config, cache: SharedCache) {
    thread::spawn(move || {
        loop {
            thread::sleep(MEMORY_TTL);
            let due: Vec<(String, String, bool)> = {
                let mut entries = cache.lock().unwrap_or_else(|error| error.into_inner());
                entries
                    .iter_mut()
                    .filter_map(|(key, entry)| {
                        let archive = key.ends_with("\0true");
                        let ttl = if archive {
                            Duration::from_secs(60)
                        } else {
                            MEMORY_TTL
                        };
                        let active_window = if archive {
                            Duration::from_secs(70)
                        } else {
                            Duration::from_secs(20)
                        };
                        if !entry.refreshing
                            && entry.created.elapsed() >= ttl
                            && entry.last_access.elapsed() < active_window
                        {
                            entry.refreshing = true;
                            let cluster = key.split('\0').next().unwrap_or("both").to_string();
                            Some((key.clone(), cluster, archive))
                        } else {
                            None
                        }
                    })
                    .collect()
            };
            for (key, cluster, archive) in due {
                let config = config.clone();
                let cache = Arc::clone(&cache);
                thread::spawn(move || refresh_cached(config, cache, key, cluster, archive));
            }
        }
    });
}

fn refresh_cached(config: Config, cache: SharedCache, key: String, cluster: String, archive: bool) {
    let result = crate::slurm::all_jobs_direct(&config, &cluster, "all", archive);
    let mut entries = cache.lock().unwrap_or_else(|error| error.into_inner());
    let Some(entry) = entries.get_mut(&key) else {
        return;
    };
    entry.refreshing = false;
    if let Ok((jobs, ledger, warnings)) = result {
        let refreshed_at = Instant::now();
        entry.reply = Arc::new(Reply {
            jobs,
            ledger,
            warnings,
            error: None,
            details: None,
        });
        entry.created = refreshed_at;
        invalidate_older_combined(&mut entries, &cluster, archive, refreshed_at);
    }
}

fn encode_reply(reply: &Reply) -> Result<Vec<u8>> {
    encode_frame(reply)
}

fn write_reply(stream: &mut UnixStream, reply: &Reply) -> Result<()> {
    stream.write_all(&encode_reply(reply)?)?;
    Ok(())
}

fn encode_frame(value: &impl Serialize) -> Result<Vec<u8>> {
    let payload = rmp_serde::to_vec(value)?;
    let length = u32::try_from(payload.len()).context("daemon message too large")?;
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn write_frame(stream: &mut UnixStream, value: &impl Serialize) -> Result<()> {
    stream.write_all(&encode_frame(value)?)?;
    Ok(())
}

fn read_frame<T: for<'de> Deserialize<'de>>(stream: &mut UnixStream) -> Result<T> {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header)?;
    let length = u32::from_le_bytes(header) as usize;
    if length > 64 * 1024 * 1024 {
        bail!("daemon message exceeds 64 MiB limit");
    }
    let mut payload = vec![0_u8; length];
    stream.read_exact(&mut payload)?;
    Ok(rmp_serde::from_slice(&payload)?)
}

fn empty_reply() -> Reply {
    Reply {
        jobs: Vec::new(),
        ledger: Ledger::default(),
        warnings: Vec::new(),
        error: None,
        details: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn protocol_round_trips_query_and_reply() {
        let request = Request::Query {
            cluster: "both".into(),
            filter: "all".into(),
            archive: false,
            force: true,
        };
        let encoded = rmp_serde::to_vec(&request).unwrap();
        assert!(matches!(
            rmp_serde::from_slice(&encoded).unwrap(),
            Request::Query { force: true, .. }
        ));
        let reply = Reply {
            jobs: vec![Job {
                id: "42".into(),
                ..Job::default()
            }],
            ..empty_reply()
        };
        let decoded: Reply = rmp_serde::from_slice(&rmp_serde::to_vec(&reply).unwrap()).unwrap();
        assert_eq!(decoded.jobs[0].id, "42");
    }

    #[test]
    fn control_requests_remain_compatible_with_pre_details_daemon() {
        #[derive(Serialize)]
        enum LegacyRequest {
            Query {
                cluster: String,
                filter: String,
                archive: bool,
                force: bool,
            },
            Ping,
            Stop,
        }
        for (legacy, expected_stop) in [(LegacyRequest::Ping, false), (LegacyRequest::Stop, true)] {
            let encoded = rmp_serde::to_vec(&legacy).unwrap();
            let decoded: Request = rmp_serde::from_slice(&encoded).unwrap();
            assert_eq!(matches!(decoded, Request::Stop), expected_stop);
        }
        // Exercise the legacy Query variant as well so its field shape stays
        // pinned even though this test only needs the control requests.
        let query = LegacyRequest::Query {
            cluster: "both".into(),
            filter: "all".into(),
            archive: false,
            force: false,
        };
        assert!(matches!(
            rmp_serde::from_slice::<Request>(&rmp_serde::to_vec(&query).unwrap()).unwrap(),
            Request::Query { .. }
        ));
    }

    fn config(path: PathBuf) -> Config {
        Config {
            local_user: "alice".into(),
            remote_user: "alice".into(),
            ssh_host: "cluster".into(),
            state_path: path,
            executable: PathBuf::from("slurm-log"),
            sbatch_banks: Vec::new(),
            clusters: vec![crate::config::ClusterConfig {
                name: "cispa".into(),
                transport: "ssh".into(),
                user: "alice".into(),
                ssh_host: "cluster".into(),
                working_directory: PathBuf::from("/tmp"),
                accounting: false,
            }],
        }
    }

    #[test]
    fn oversized_and_truncated_frames_are_rejected_without_allocation() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        client
            .write_all(&(64_u32 * 1024 * 1024 + 1).to_le_bytes())
            .unwrap();
        assert!(read_frame::<Request>(&mut server).is_err());

        let (mut client, mut server) = UnixStream::pair().unwrap();
        client.write_all(&10_u32.to_le_bytes()).unwrap();
        client.write_all(&[1, 2]).unwrap();
        drop(client);
        assert!(read_frame::<Request>(&mut server).is_err());
    }

    #[test]
    fn cached_query_is_served_without_scheduler_access() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path().join("state.json"));
        let reply = Reply {
            jobs: vec![Job {
                id: "cached".into(),
                ..Job::default()
            }],
            ..empty_reply()
        };
        let now = Instant::now();
        let cache = Arc::new(Mutex::new(HashMap::from([(
            cache_key("both", false),
            CachedReply {
                created: now - Duration::from_secs(30),
                last_access: now,
                refreshing: false,
                last_force: None,
                reply: Arc::new(reply),
            },
        )])));
        let (mut client, mut server) = UnixStream::pair().unwrap();
        write_frame(
            &mut client,
            &Request::Query {
                cluster: "both".into(),
                filter: "all".into(),
                archive: false,
                force: false,
            },
        )
        .unwrap();
        let details = Arc::new(Mutex::new(HashMap::new()));
        assert!(!handle_stream(&config, &cache, &details, &mut server).unwrap());
        let received: Reply = read_frame(&mut client).unwrap();
        assert_eq!(received.jobs[0].id, "cached");
    }

    #[test]
    fn repeated_forced_query_is_rate_limited_without_scheduler_access() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path().join("state.json"));
        let now = Instant::now();
        let cache = Arc::new(Mutex::new(HashMap::from([(
            cache_key("both", false),
            CachedReply {
                created: now,
                last_access: now,
                refreshing: false,
                last_force: Some(now),
                reply: Arc::new(Reply {
                    jobs: vec![Job {
                        id: "rate-limited".into(),
                        ..Job::default()
                    }],
                    ..empty_reply()
                }),
            },
        )])));
        let details = Arc::new(Mutex::new(HashMap::new()));
        let (mut client, mut server) = UnixStream::pair().unwrap();
        write_frame(
            &mut client,
            &Request::Query {
                cluster: "both".into(),
                filter: "all".into(),
                archive: false,
                force: true,
            },
        )
        .unwrap();
        assert!(!handle_stream(&config, &cache, &details, &mut server).unwrap());
        let reply: Reply = read_frame(&mut client).unwrap();
        assert_eq!(reply.jobs[0].id, "rate-limited");
    }

    #[test]
    fn canonical_snapshot_derives_filters_without_scheduler_access() {
        let reply = Reply {
            jobs: vec![
                Job {
                    cluster: "sprint".into(),
                    id: "1".into(),
                    state: "RUNNING".into(),
                    ..Job::default()
                },
                Job {
                    cluster: "cispa".into(),
                    id: "2".into(),
                    state: "FAILED".into(),
                    ..Job::default()
                },
            ],
            ..empty_reply()
        };
        assert_eq!(filtered_reply(&reply, "sprint", "running").jobs.len(), 1);
        assert_eq!(filtered_reply(&reply, "cispa", "failed").jobs[0].id, "2");
        assert!(filtered_reply(&reply, "sprint", "failed").jobs.is_empty());
        assert_eq!(filtered_reply(&reply, "all", "all").jobs.len(), 2);
        assert_eq!(filtered_reply(&reply, "both", "all").jobs.len(), 2);
    }

    #[test]
    #[ignore = "release-mode performance budget"]
    fn sparse_filter_does_not_clone_an_entire_large_snapshot() {
        let reply = Reply {
            jobs: (0..100_000)
                .map(|id| Job {
                    cluster: if id % 2 == 0 { "sprint" } else { "cispa" }.into(),
                    id: id.to_string(),
                    state: if id % 100 == 0 {
                        "RUNNING".into()
                    } else {
                        "COMPLETED".into()
                    },
                    name: "large-archive-entry".into(),
                    ..Job::default()
                })
                .collect(),
            ..empty_reply()
        };
        let started = Instant::now();
        let filtered = filtered_reply(&reply, "sprint", "running");
        let optimized = started.elapsed();
        assert_eq!(filtered.jobs.len(), 1_000);
        assert!(optimized < Duration::from_millis(100));

        // Retain the former clone-everything approach as an offline benchmark
        // oracle so a future refactor cannot silently lose the sparse-filter
        // algorithmic advantage.
        let baseline_started = Instant::now();
        let mut baseline = reply.clone();
        baseline
            .jobs
            .retain(|job| job.cluster == "sprint" && job.running());
        let baseline_elapsed = baseline_started.elapsed();
        assert_eq!(baseline.jobs.len(), filtered.jobs.len());
        assert!(optimized < baseline_elapsed);
        eprintln!("sparse archive filter: optimized={optimized:?}, clone-all={baseline_elapsed:?}");
    }

    #[test]
    fn newer_cluster_snapshot_invalidates_only_older_combined_snapshots() {
        let now = Instant::now();
        let cached = |created| CachedReply {
            created,
            last_access: now,
            refreshing: false,
            last_force: None,
            reply: Arc::new(empty_reply()),
        };
        let mut entries = HashMap::from([
            (
                cache_key("all", false),
                cached(now - Duration::from_secs(2)),
            ),
            (
                cache_key("both", false),
                cached(now - Duration::from_secs(1)),
            ),
            (cache_key("all", true), cached(now)),
        ]);

        invalidate_older_combined(&mut entries, "cispa", false, now);
        assert!(!entries.contains_key(&cache_key("all", false)));
        assert!(!entries.contains_key(&cache_key("both", false)));
        assert!(entries.contains_key(&cache_key("all", true)));

        entries.insert(cache_key("all", false), cached(now));
        invalidate_older_combined(&mut entries, "sprint", false, now - Duration::from_secs(1));
        assert!(entries.contains_key(&cache_key("all", false)));
    }

    #[test]
    fn terminal_detail_snapshot_is_served_without_scheduler_access() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path().join("state.json"));
        let cache = Arc::new(Mutex::new(HashMap::new()));
        let now = Instant::now();
        let details = Arc::new(Mutex::new(HashMap::from([(
            format!("cispa\0{}", "42"),
            DetailEntry {
                created: now - Duration::from_secs(30),
                last_access: now,
                failures: 0,
                details: JobDetails {
                    cluster: "cispa".into(),
                    id: "42".into(),
                    state: "COMPLETED".into(),
                    terminal: true,
                    ..JobDetails::default()
                },
            },
        )])));
        let (mut client, mut server) = UnixStream::pair().unwrap();
        write_frame(
            &mut client,
            &Request::Details {
                cluster: "cispa".into(),
                id: "42".into(),
                force: false,
            },
        )
        .unwrap();
        assert!(!handle_stream(&config, &cache, &details, &mut server).unwrap());
        let reply: Reply = read_frame(&mut client).unwrap();
        assert_eq!(reply.details.unwrap().state, "COMPLETED");
    }

    #[test]
    fn socket_and_lock_are_scoped_to_the_private_state_directory() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path().join("nested/state.json"));
        let (socket, lock) = paths(&config);
        assert_eq!(socket, directory.path().join("nested/daemon.sock"));
        assert_eq!(lock, directory.path().join("nested/daemon.lock"));
    }

    #[test]
    #[ignore = "release-mode performance budget"]
    fn binary_protocol_handles_large_archive_within_budget() {
        let reply = Reply {
            jobs: (0..10_000)
                .map(|id| Job {
                    cluster: "cispa".into(),
                    id: id.to_string(),
                    state: "COMPLETED".into(),
                    name: "long-training-name".into(),
                    elapsed: "01:23:45".into(),
                    ended: "2026-08-11T12:00:00+02:00".into(),
                    ..Job::default()
                })
                .collect(),
            ..empty_reply()
        };
        let started = Instant::now();
        let encoded = encode_reply(&reply).unwrap();
        let payload: Reply = rmp_serde::from_slice(&encoded[4..]).unwrap();
        assert_eq!(payload.jobs.len(), 10_000);
        assert!(started.elapsed() < Duration::from_millis(100));
    }
}
