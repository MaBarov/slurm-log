use super::super::*;

#[test]
fn corrupt_or_stale_cache_is_a_miss() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache.json");
    fs::write(&path, b"not-json").unwrap();
    assert!(cached_jobs(&path, Duration::from_secs(60)).is_none());
    assert!(cached_jobs(&path, Duration::ZERO).is_none());
    fs::write(&path, [0xdd, 0xff, 0xff, 0xff, 0xff]).unwrap();
    assert!(cached_jobs(&path, Duration::from_secs(60)).is_none());
}

#[test]
fn messagepack_cache_length_guard_covers_all_sequence_headers() {
    assert_eq!(msgpack_sequence_len(&[0x90]), Some(0));
    assert_eq!(msgpack_sequence_len(&[0x9f]), Some(15));
    assert_eq!(msgpack_sequence_len(&[0xdc, 0x01, 0x00]), Some(256));
    assert_eq!(msgpack_sequence_len(&[0xdd, 0, 1, 0, 0]), Some(65_536));
    assert_eq!(msgpack_sequence_len(&[]), None);
    assert_eq!(msgpack_sequence_len(&[0xdc, 0]), None);
    assert_eq!(msgpack_sequence_len(b"not messagepack"), None);
}

#[test]
fn query_dimensions_are_strictly_bounded() {
    assert!(validate_query("both", "all").is_ok());
    assert!(validate_query("../../tmp", "all").is_err());
    assert!(validate_query("cispa", "arbitrary").is_err());
}

#[test]
fn oversized_cache_is_rejected_before_reading() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache.json");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_CACHE_BYTES + 1).unwrap();
    assert!(cached_jobs(&path, Duration::from_secs(60)).is_none());
}

#[test]
fn cache_lock_creates_parents_and_failed_writes_leave_no_file() {
    let directory = tempfile::tempdir().unwrap();
    let nested = directory.path().join("new/cache.json");
    let lock = query_lock(&nested).unwrap();
    assert!(nested.with_extension("query.lock").exists());
    drop(lock);

    let blocked_parent = directory.path().join("blocked");
    fs::write(&blocked_parent, b"not a directory").unwrap();
    let blocked = blocked_parent.join("cache.json");
    store_jobs(&blocked, &[Job::default()]);
    assert!(!blocked.exists());
    assert!(query_lock(&blocked).is_err());
}

#[test]
#[ignore = "release-mode performance budget"]
fn decodes_fifty_thousand_cached_jobs_within_budget() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("archive-cache.json");
    let jobs: Vec<_> = (0..50_000)
        .map(|id| Job {
            cluster: "cispa".into(),
            id: id.to_string(),
            state: "COMPLETED".into(),
            name: "archive-training".into(),
            elapsed: "01:23:45".into(),
            ended: "2026-08-12T00:00:00+02:00".into(),
            alloc_tres: "cpu=8,mem=32G,gres/gpu=2".into(),
            ..Job::default()
        })
        .collect();
    store_jobs(&path, &jobs);
    let started = std::time::Instant::now();
    let decoded = cached_jobs(&path, Duration::from_secs(60)).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(decoded.len(), jobs.len());
    #[cfg(not(coverage))]
    assert!(elapsed < Duration::from_millis(250));
    eprintln!("decode 50k cached jobs: {elapsed:?}");
}
