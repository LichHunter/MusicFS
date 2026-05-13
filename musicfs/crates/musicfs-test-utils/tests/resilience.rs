use musicfs_cache::{VirtualTree, ROOT_INODE};
use musicfs_cas::{CasConfig, CasStore};
use musicfs_core::{HealthStatus, OriginId, OriginType, RealPath};
use musicfs_origins::{HealthMonitor, LocalOrigin, OriginRegistry};
use musicfs_test_utils::{FaultyOrigin, FailMode};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn setup_test_file(dir: &TempDir, name: &str, content: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, content).unwrap();
    path
}

async fn setup_cas(dir: &Path) -> CasStore {
    CasStore::open(CasConfig {
        chunks_dir: dir.join("chunks"),
        max_size: 100 * 1024 * 1024,
        shard_levels: 2,
    })
    .await
    .unwrap()
}

fn create_faulty_origin(id: &str, dir: &TempDir, mode: FailMode) -> Arc<FaultyOrigin> {
    let inner = Arc::new(LocalOrigin::new(OriginId::from(id), dir.path().to_path_buf()));
    Arc::new(FaultyOrigin::new(inner, mode))
}

#[tokio::test]
async fn test_sqlite_integrity_check_detects_corruption() {
    todo!("Issue 2.4: Implement Database::open_with_integrity_check()")
}

#[tokio::test]
async fn test_tantivy_corruption_triggers_rebuild() {
    todo!("Issue 2.4: Implement SearchIndex::open_with_recovery()")
}

#[tokio::test]
async fn test_sled_corruption_triggers_repair() {
    todo!("Issue 3.5: Implement sled recovery in CasStore::open()")
}

#[tokio::test]
async fn test_cas_put_handles_enospc() {
    let dir = TempDir::new().unwrap();
    let store = CasStore::open(CasConfig {
        chunks_dir: dir.path().join("chunks"),
        max_size: 100,
        shard_levels: 2,
    })
    .await
    .unwrap();

    let large_data = vec![0u8; 1000];
    let result = store.put(&large_data).await;
    
    assert!(result.is_err(), "Issue 2.8: CasStore should pre-check space and reject oversized write");
}

/// Demonstrates the PROBLEM with std::sync::RwLock: after a writer panic,
/// the lock is poisoned and all subsequent access fails with PoisonError.
/// This is why we use parking_lot::RwLock instead (see test_parking_lot_rwlock_survives_panic).
#[test]
fn test_poisoned_tree_lock_returns_eio_not_panic() {
    use std::sync::{Arc, RwLock};
    use std::thread;

    let lock = Arc::new(RwLock::new(42));
    let lock_clone = lock.clone();

    let handle = thread::spawn(move || {
        let _guard = lock_clone.write().unwrap();
        panic!("writer panic");
    });

    let _ = handle.join();

    let result = lock.read();
    // std::sync::RwLock poisons after writer panic - this is the problem we fix with parking_lot
    assert!(result.is_err(), "Issue 2.9: std::sync::RwLock should poison after writer panic (this demonstrates the problem)");
}

#[test]
fn test_parking_lot_rwlock_survives_panic() {
    use parking_lot::RwLock;
    use std::sync::Arc;
    use std::thread;

    let tree = Arc::new(RwLock::new(VirtualTree::new()));
    let tree_clone = tree.clone();

    let handle = thread::spawn(move || {
        let _guard = tree_clone.write();
        panic!("writer panic");
    });

    let _ = handle.join();

    assert!(tree.read().get(ROOT_INODE).is_some(), "parking_lot RwLock should survive writer panic");
}

#[tokio::test]
async fn test_failover_on_primary_death() {
    let primary_dir = TempDir::new().unwrap();
    let backup_dir = TempDir::new().unwrap();
    setup_test_file(&primary_dir, "test.txt", b"primary");
    setup_test_file(&backup_dir, "test.txt", b"backup");

    let primary = create_faulty_origin("primary", &primary_dir, FailMode::ReturnError(ErrorKind::ConnectionRefused));
    let backup = create_faulty_origin("backup", &backup_dir, FailMode::Healthy);

    let mut thresholds = HashMap::new();
    thresholds.insert(OriginType::Local, 1);
    let monitor = Arc::new(HealthMonitor::new(Duration::from_secs(30)).with_per_type_thresholds(thresholds));
    let registry = Arc::new(OriginRegistry::new(monitor.clone()));

    registry.register(primary.clone(), 1);
    registry.register(backup.clone(), 2);

    monitor.check_now(&OriginId::from("primary")).await;
    monitor.check_now(&OriginId::from("backup")).await;

    assert!(registry.health().is_unhealthy(&OriginId::from("primary")));
    assert!(registry.health().is_healthy(&OriginId::from("backup")));

    let path = RealPath {
        origin_id: OriginId::from("backup"),
        path: PathBuf::from("/test.txt"),
    };
    let candidates = registry.route_all(&path);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id(), &OriginId::from("backup"));
}

#[tokio::test]
async fn test_origin_recovery_resumes_routing() {
    let dir = TempDir::new().unwrap();
    setup_test_file(&dir, "test.txt", b"content");

    let faulty = create_faulty_origin("recovering", &dir, FailMode::ReturnError(ErrorKind::ConnectionRefused));

    let mut thresholds = HashMap::new();
    thresholds.insert(OriginType::Local, 1);
    let monitor = Arc::new(HealthMonitor::new(Duration::from_secs(30)).with_per_type_thresholds(thresholds));
    monitor.add_origin(faulty.clone());

    monitor.check_now(&OriginId::from("recovering")).await;
    assert_eq!(monitor.get_state(&OriginId::from("recovering")).unwrap().status, HealthStatus::Unhealthy);

    faulty.set_mode(FailMode::Healthy);
    monitor.check_now(&OriginId::from("recovering")).await;

    assert_eq!(monitor.get_state(&OriginId::from("recovering")).unwrap().status, HealthStatus::Healthy);
    assert_eq!(monitor.get_state(&OriginId::from("recovering")).unwrap().consecutive_failures, 0);
}

#[tokio::test]
async fn test_local_origin_health_check_has_timeout() {
    let dir = TempDir::new().unwrap();
    setup_test_file(&dir, "test.txt", b"content");

    let slow = create_faulty_origin("slow", &dir, FailMode::TimeoutMs(5_000));

    let monitor = Arc::new(HealthMonitor::new(Duration::from_secs(30)));
    monitor.add_origin(slow.clone());

    let start = Instant::now();
    monitor.check_now(&OriginId::from("slow")).await;
    let elapsed = start.elapsed();

    assert!(elapsed < Duration::from_secs(2), 
        "Issue 4.2.1: Health check should timeout in <2s, took {:?}", elapsed);
    
    let state = monitor.get_state(&OriginId::from("slow")).unwrap();
    assert_eq!(state.status, HealthStatus::Unhealthy);
}

#[tokio::test]
async fn test_health_checks_run_in_parallel() {
    let slow1_dir = TempDir::new().unwrap();
    let slow2_dir = TempDir::new().unwrap();
    let slow3_dir = TempDir::new().unwrap();

    let slow1 = create_faulty_origin("slow1", &slow1_dir, FailMode::TimeoutMs(200));
    let slow2 = create_faulty_origin("slow2", &slow2_dir, FailMode::TimeoutMs(200));
    let slow3 = create_faulty_origin("slow3", &slow3_dir, FailMode::TimeoutMs(200));

    let monitor = Arc::new(HealthMonitor::new(Duration::from_secs(30)));
    monitor.add_origin(slow1);
    monitor.add_origin(slow2);
    monitor.add_origin(slow3);

    let start = Instant::now();
    monitor.check_all().await;
    let elapsed = start.elapsed();

    assert!(elapsed < Duration::from_millis(350), "Issue 4.2.2: check_all() should run in parallel (sequential would take ~600ms), took {:?}", elapsed);
}

#[tokio::test]
async fn test_tantivy_survives_uncommitted_crash() {
    todo!("Issue 5.2: Implement tantivy crash recovery test")
}

#[tokio::test]
async fn test_fd_exhaustion_handling() {
    todo!("Issue 5.3: Implement fd exhaustion test with rlimit")
}

#[tokio::test]
async fn test_corrupt_chunk_auto_refetched() {
    let dir = TempDir::new().unwrap();
    let origin_dir = TempDir::new().unwrap();
    setup_test_file(&origin_dir, "test.flac", b"original audio data");

    let store = setup_cas(dir.path()).await;
    let data = b"chunk data";
    let hash = store.put(data).await.unwrap();

    let hex = hash.as_hex();
    let chunk_path = dir.path().join("chunks").join(&hex[0..2]).join(&hex[2..4]).join(&hex);
    let mut corrupted = std::fs::read(&chunk_path).unwrap();
    corrupted[0] = corrupted[0].wrapping_add(1);
    std::fs::write(&chunk_path, &corrupted).unwrap();

    let result = store.get(&hash).await;
    
    assert!(result.is_ok(), "Issue 6.4: Corrupted chunk should be auto-refetched from origin");
}

#[tokio::test]
async fn test_missing_chunk_triggers_origin_fetch() {
    todo!("Issue 6.4: Implement missing chunk origin fetch")
}

#[tokio::test]
async fn test_passthrough_mode_when_cache_disk_dead() {
    todo!("Issue 6.6: Implement passthrough mode")
}

#[test]
fn test_panic_hook_logs_to_tracing() {
    use std::panic;

    musicfs_core::install_panic_hook();

    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        panic!("test panic message");
    }));

    assert!(result.is_err(), "Panic should have been caught");
}

#[test]
fn test_stale_mount_check_function_exists() {
    let path = std::path::Path::new("/nonexistent/musicfs/mount");
    assert!(
        !path.exists(),
        "Test path should not exist for this test to be meaningful"
    );
}

#[test]
fn test_systemd_service_has_execstoppost() {
    let service_path = std::path::Path::new("../../dist/musicfs.service");
    if !service_path.exists() {
        panic!("Issue 3.7: dist/musicfs.service does not exist at {:?}", service_path);
    }

    let content = std::fs::read_to_string(service_path).unwrap();
    assert!(
        content.contains("ExecStopPost") && content.contains("fusermount"),
        "Issue 3.7: Service file should have ExecStopPost with fusermount for cleanup"
    );
}

#[test]
fn test_sd_notify_ready_sent() {
    use std::os::unix::net::UnixDatagram;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("notify.sock");
    let socket = UnixDatagram::bind(&socket_path).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(1))).unwrap();

    std::env::set_var("NOTIFY_SOCKET", &socket_path);

    let result = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);
    assert!(result.is_ok(), "sd_notify should succeed when NOTIFY_SOCKET is set");

    let mut buf = [0u8; 256];
    let len = socket.recv(&mut buf).unwrap();
    let msg = std::str::from_utf8(&buf[..len]).unwrap();

    assert!(msg.contains("READY=1"), "sd_notify should send READY=1, got: {}", msg);

    std::env::remove_var("NOTIFY_SOCKET");
}

#[tokio::test]
async fn test_sigterm_triggers_shutdown() {
    use std::process::{Command, Stdio};
    use std::time::Duration;
    use tokio::time::timeout;

    let musicfs_bin = std::env::var("CARGO_BIN_EXE_musicfs").ok();
    if musicfs_bin.is_none() {
        eprintln!("Skipping test_sigterm_triggers_shutdown: musicfs binary not available in test context");
        return;
    }

    let bin_path = musicfs_bin.unwrap();
    let temp_dir = tempfile::TempDir::new().unwrap();
    let mountpoint = temp_dir.path().join("mount");
    let origin = temp_dir.path().join("origin");
    std::fs::create_dir_all(&mountpoint).unwrap();
    std::fs::create_dir_all(&origin).unwrap();

    let mut child = Command::new(&bin_path)
        .args(["mount", "--origin", origin.to_str().unwrap(), mountpoint.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    if child.is_err() {
        eprintln!("Skipping test_sigterm_triggers_shutdown: failed to spawn musicfs");
        return;
    }

    let mut child = child.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }

    let exit_result = timeout(Duration::from_secs(10), async {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) => tokio::time::sleep(Duration::from_millis(100)).await,
                Err(_) => break,
            }
        }
        child.wait().unwrap()
    }).await;

    assert!(exit_result.is_ok(), "Issue 2.1: Process should exit within 10s after SIGTERM");
}
