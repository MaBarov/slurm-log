use super::*;

#[test]
fn detail_resolution_caches_success_and_preserves_failures_offline() {
    let cache: DetailCache = Arc::new(Mutex::new(HashMap::new()));
    let key = format!("cispa\0{}", 42);
    let reply = resolve_detail_reply(&cache, key.clone(), false, |_| {
        Ok(JobDetails {
            cluster: "cispa".into(),
            id: "42".into(),
            state: "RUNNING".into(),
            ..JobDetails::default()
        })
    });
    assert_eq!(reply.details.unwrap().state, "RUNNING");

    cache.lock().unwrap().get_mut(&key).unwrap().created =
        Instant::now() - FORCED_DETAIL_MINIMUM - Duration::from_secs(1);
    let stale = resolve_detail_reply(&cache, key, true, |previous| {
        assert_eq!(previous.unwrap().id, "42");
        Err(anyhow::anyhow!("temporary accounting outage"))
    });
    let stale = stale.details.unwrap();
    assert_eq!(stale.state, "RUNNING");
    assert!(stale.stale_error.contains("temporary accounting outage"));

    let empty: DetailCache = Arc::new(Mutex::new(HashMap::new()));
    let failure = resolve_detail_reply(&empty, format!("cispa\0{}", 99), false, |_| {
        Err(anyhow::anyhow!("not found"))
    });
    assert!(failure.error.unwrap().contains("not found"));
}
