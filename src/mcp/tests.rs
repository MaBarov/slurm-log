use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use super::*;
use crate::config::ClusterConfig;

fn config() -> Config {
    let directory = tempfile::tempdir().unwrap();
    // Keep the directory alive through the config path only long enough
    // for constructor/schema work; these tests do not invoke a scheduler.
    let state = directory.path().join("state.json");
    Config {
        local_user: "offline".into(),
        remote_user: "offline".into(),
        ssh_host: String::new(),
        state_path: state,
        executable: PathBuf::from("/bin/false"),
        sbatch_banks: Vec::new(),
        clusters: vec![ClusterConfig {
            name: "local".into(),
            controller: None,
            transport: "local".into(),
            user: "offline".into(),
            ssh_host: String::new(),
            working_directory: PathBuf::from("/tmp"),
            accounting: false,
        }],
    }
}

fn server() -> McpServer {
    McpServer::new(config())
}

#[test]
fn subscription_workers_are_globally_capped_and_cooperatively_cancelled() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let server = server();
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(AtomicBool::new(false));
        let mut tasks = Vec::new();
        for _ in 0..MCP_BLOCKING_CONCURRENCY * 2 {
            let server = server.clone();
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            let release = Arc::clone(&release);
            tasks.push(tokio::spawn(async move {
                server
                    .run_subscription_blocking(Arc::new(AtomicBool::new(false)), move || {
                        let now = active.fetch_add(1, Ordering::AcqRel) + 1;
                        maximum.fetch_max(now, Ordering::AcqRel);
                        while !release.load(Ordering::Acquire) {
                            thread::sleep(Duration::from_millis(1));
                        }
                        active.fetch_sub(1, Ordering::AcqRel);
                    })
                    .await
            }));
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while active.load(Ordering::Acquire) < MCP_BLOCKING_CONCURRENCY {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("initial worker permits were not acquired");
        assert!(maximum.load(Ordering::Acquire) <= MCP_BLOCKING_CONCURRENCY);
        release.store(true, Ordering::Release);
        for task in tasks {
            task.await.unwrap();
        }

        let cancelled = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancelled);
        let worker_entered = Arc::clone(&entered);
        let task = tokio::spawn({
            let server = server.clone();
            async move {
                server
                    .run_subscription_blocking(Arc::clone(&worker_cancel), move || {
                        worker_entered.store(true, Ordering::Release);
                        while !worker_cancel.load(Ordering::Acquire) {
                            thread::sleep(Duration::from_millis(1));
                        }
                    })
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !entered.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("cancellation test worker did not start");
        cancelled.store(true, Ordering::Release);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .expect("subscription cancellation did not return")
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn unknown_command_actions_are_rejected() {
    let error = command(&config(), Some("bogus")).unwrap_err();
    assert!(format!("{error:#}").contains("unknown mcp command"));
}

#[test]
fn get_tool_returns_a_matching_tool_or_none() {
    let server = server();
    let found = server.get_tool("slurm_doctor");
    assert!(found.is_some());
    assert_eq!(found.unwrap().name, "slurm_doctor");
    assert!(server.get_tool("slurm_no_such_tool").is_none());
}

#[test]
fn cancelled_error_is_an_internal_error() {
    let error = cancelled_error();
    assert_eq!(error.message, "MCP request cancelled");
}

#[test]
fn subscription_short_circuits_when_already_cancelled() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let server = server();
        let ran = Arc::new(AtomicBool::new(false));
        let worker_ran = Arc::clone(&ran);
        let result = server
            .run_subscription_blocking(Arc::new(AtomicBool::new(true)), move || {
                worker_ran.store(true, Ordering::Release);
            })
            .await;
        assert!(result.is_none());
        assert!(!ran.load(Ordering::Acquire));
    });
}

#[test]
fn subscription_poll_cancels_a_worker_that_ignores_the_flag() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let server = server();
        let cancellation = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(AtomicBool::new(false));
        let worker_entered = Arc::clone(&entered);
        let task = tokio::spawn({
            let server = server.clone();
            let worker_cancellation = Arc::clone(&cancellation);
            async move {
                server
                    .run_subscription_blocking(worker_cancellation, move || {
                        worker_entered.store(true, Ordering::Release);
                        thread::sleep(Duration::from_millis(200));
                    })
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !entered.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("worker did not start");
        cancellation.store(true, Ordering::Release);
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("poll cancellation did not return")
            .unwrap();
        assert!(result.is_none());
    });
}
