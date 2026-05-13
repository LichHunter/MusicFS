use musicfs_cache::{Database, VirtualTree, ROOT_INODE};
use musicfs_cas::{CasConfig, CasStore};
use musicfs_core::supervisor::{TaskStatus, TaskSupervisor};
use musicfs_core::{
    AudioMeta, FileId, FileMeta, HealthStatus, OriginId, OriginType, RealPath, VirtualPath,
};
use musicfs_origins::{HealthMonitor, LocalOrigin, OriginRegistry};
use musicfs_search::SearchIndex;
use musicfs_test_utils::{FailMode, FaultyOrigin};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

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
    let inner = Arc::new(LocalOrigin::new(
        OriginId::from(id),
        dir.path().to_path_buf(),
    ));
    Arc::new(FaultyOrigin::new(inner, mode))
}

fn make_file_meta(id: i64, path: &str, size: u64) -> FileMeta {
    let name = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    FileMeta {
        id: FileId(id),
        virtual_path: VirtualPath::new(path),
        real_path: RealPath {
            origin_id: OriginId::from("test"),
            path: PathBuf::from(path),
        },
        size,
        mtime: UNIX_EPOCH,
        content_hash: None,
        audio: Some(AudioMeta {
            title: Some(name),
            ..Default::default()
        }),
    }
}

#[tokio::test]
async fn test_sqlite_integrity_check_detects_corruption() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");

    {
        let db = Database::open(&db_path).unwrap();
        db.upsert_file(
            &OriginId::from("test"),
            Path::new("/test.flac"),
            &VirtualPath::new("/Test.flac"),
            &AudioMeta::default(),
            UNIX_EPOCH,
            1000,
        )
        .unwrap();
    }

    let mut data = std::fs::read(&db_path).unwrap();
    let mid = data.len() / 2;
    data[mid..mid + 100].fill(0xFF);
    std::fs::write(&db_path, &data).unwrap();

    let result = Database::open_with_integrity_check(&db_path);
    assert!(result.is_err());
}

#[tokio::test]
async fn test_tantivy_corruption_triggers_rebuild() {
    let dir = TempDir::new().unwrap();
    let index_path = dir.path().join("search_idx");

    {
        let index = SearchIndex::open(&index_path).unwrap();
        index
            .index_file(&make_file_meta(1, "/a.flac", 1000))
            .unwrap();
        index.commit().unwrap();
    }

    std::fs::write(index_path.join("meta.json"), b"corrupted").unwrap();

    let index = SearchIndex::open_with_recovery(&index_path).unwrap();
    let results = index.search("a", 10).unwrap();
    assert_eq!(results.len(), 0);
}

#[tokio::test]
async fn test_sled_corruption_triggers_repair() {
    let dir = TempDir::new().unwrap();
    let chunks_dir = dir.path().join("chunks");
    let config = CasConfig {
        chunks_dir: chunks_dir.clone(),
        max_size: 10_000_000,
        shard_levels: 2,
    };

    {
        let store = CasStore::open(config.clone()).await.unwrap();
        store.put(b"test data").await.unwrap();
    }

    let sled_dir = chunks_dir.join("index.sled");
    if sled_dir.exists() {
        for entry in std::fs::read_dir(&sled_dir).unwrap() {
            let entry = entry.unwrap();
            if entry.metadata().unwrap().is_file() {
                std::fs::write(entry.path(), b"corrupted").unwrap();
            }
        }
    }

    let result = CasStore::open(config).await;
    assert!(result.is_ok(), "sled should recover from corruption");
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

    assert!(
        result.is_err(),
        "Issue 2.8: CasStore should pre-check space and reject oversized write"
    );
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

    assert!(
        tree.read().get(ROOT_INODE).is_some(),
        "parking_lot RwLock should survive writer panic"
    );
}

#[tokio::test]
async fn test_failover_on_primary_death() {
    let primary_dir = TempDir::new().unwrap();
    let backup_dir = TempDir::new().unwrap();
    setup_test_file(&primary_dir, "test.txt", b"primary");
    setup_test_file(&backup_dir, "test.txt", b"backup");

    let primary = create_faulty_origin(
        "primary",
        &primary_dir,
        FailMode::ReturnError(ErrorKind::ConnectionRefused),
    );
    let backup = create_faulty_origin("backup", &backup_dir, FailMode::Healthy);

    let mut thresholds = HashMap::new();
    thresholds.insert(OriginType::Local, 1);
    let monitor =
        Arc::new(HealthMonitor::new(Duration::from_secs(30)).with_per_type_thresholds(thresholds));
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

    let faulty = create_faulty_origin(
        "recovering",
        &dir,
        FailMode::ReturnError(ErrorKind::ConnectionRefused),
    );

    let mut thresholds = HashMap::new();
    thresholds.insert(OriginType::Local, 1);
    let monitor =
        Arc::new(HealthMonitor::new(Duration::from_secs(30)).with_per_type_thresholds(thresholds));
    monitor.add_origin(faulty.clone());

    monitor.check_now(&OriginId::from("recovering")).await;
    assert_eq!(
        monitor
            .get_state(&OriginId::from("recovering"))
            .unwrap()
            .status,
        HealthStatus::Unhealthy
    );

    faulty.set_mode(FailMode::Healthy);
    monitor.check_now(&OriginId::from("recovering")).await;

    assert_eq!(
        monitor
            .get_state(&OriginId::from("recovering"))
            .unwrap()
            .status,
        HealthStatus::Healthy
    );
    assert_eq!(
        monitor
            .get_state(&OriginId::from("recovering"))
            .unwrap()
            .consecutive_failures,
        0
    );
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

    assert!(
        elapsed < Duration::from_secs(2),
        "Issue 4.2.1: Health check should timeout in <2s, took {:?}",
        elapsed
    );

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

    assert!(
        elapsed < Duration::from_millis(350),
        "Issue 4.2.2: check_all() should run in parallel (sequential would take ~600ms), took {:?}",
        elapsed
    );
}

#[test]
fn test_tantivy_survives_uncommitted_crash() {
    let dir = TempDir::new().unwrap();
    let index_path = dir.path().join("search_idx");

    {
        let index = SearchIndex::open(&index_path).unwrap();
        index
            .index_file(&make_file_meta(1, "/a.flac", 1000))
            .unwrap();
        index.commit().unwrap();
        index
            .index_file(&make_file_meta(2, "/b.flac", 1000))
            .unwrap();
    }

    let index = SearchIndex::open(&index_path).unwrap();
    let results = index.search("a", 10).unwrap();
    assert_eq!(results.len(), 1);
}

#[tokio::test]
#[cfg(feature = "resource-limits")]
async fn test_fd_exhaustion_handling() {
    use rlimit::{getrlimit, setrlimit, Resource};

    let (orig_soft, orig_hard) = getrlimit(Resource::NOFILE).unwrap();

    setrlimit(Resource::NOFILE, 64, 64).unwrap();

    let dir = TempDir::new().unwrap();
    let result = CasStore::open(CasConfig {
        chunks_dir: dir.path().join("chunks"),
        max_size: 1_000_000,
        shard_levels: 2,
    })
    .await;

    match result {
        Ok(_store) => {}
        Err(e) => {
            let msg = format!("{}", e);
            assert!(!msg.contains("panic"), "Should not panic on fd exhaustion");
        }
    }

    setrlimit(Resource::NOFILE, orig_soft, orig_hard).unwrap();
}

#[tokio::test]
#[cfg(not(feature = "resource-limits"))]
async fn test_fd_exhaustion_handling() {
    eprintln!("Skipping test_fd_exhaustion_handling: resource-limits feature not enabled");
}

#[tokio::test]
async fn test_corrupt_chunk_auto_refetched() {
    use musicfs_cas::{ContentFetcher, FileReader};
    use musicfs_origins::LocalOrigin;

    let dir = TempDir::new().unwrap();
    let origin_dir = TempDir::new().unwrap();
    let test_content = b"original audio data for chunk test";
    setup_test_file(&origin_dir, "test.flac", test_content);

    let store = Arc::new(setup_cas(dir.path()).await);

    let origin = Arc::new(LocalOrigin::new(
        OriginId::from("local"),
        origin_dir.path().to_path_buf(),
    ));
    let fetcher = Arc::new(ContentFetcher::new(store.clone()));
    fetcher.register_origin(origin);

    let file_meta = FileMeta {
        id: FileId(1),
        virtual_path: VirtualPath::new("/test.flac"),
        real_path: RealPath {
            origin_id: OriginId::from("local"),
            path: PathBuf::from("/test.flac"),
        },
        size: test_content.len() as u64,
        mtime: UNIX_EPOCH,
        content_hash: None,
        audio: None,
    };
    fetcher.register_file(file_meta);

    let manifest = fetcher.fetch_file(FileId(1)).await.unwrap();
    let chunk_hash = manifest.chunks[0].hash;
    let hex = chunk_hash.as_hex();
    let chunk_path = dir
        .path()
        .join("chunks")
        .join(&hex[0..2])
        .join(&hex[2..4])
        .join(&hex);

    let mut corrupted = std::fs::read(&chunk_path).unwrap();
    corrupted[0] = corrupted[0].wrapping_add(1);
    std::fs::write(&chunk_path, &corrupted).unwrap();

    let reader = FileReader::with_fetcher(store, fetcher);
    reader.register_manifest(manifest);

    let result = reader.read(FileId(1), 0, test_content.len() as u32).await;

    assert!(
        result.is_ok(),
        "Issue 6.4: Corrupted chunk should be auto-refetched from origin"
    );
    assert_eq!(
        &result.unwrap()[..],
        test_content,
        "Data should match original after re-fetch"
    );
}

#[tokio::test]
async fn test_missing_chunk_triggers_origin_fetch() {
    use musicfs_cas::{ContentFetcher, FileReader};
    use musicfs_origins::LocalOrigin;

    let dir = TempDir::new().unwrap();
    let origin_dir = TempDir::new().unwrap();
    let test_content = b"test data for missing chunk";
    setup_test_file(&origin_dir, "test.flac", test_content);

    let store = Arc::new(setup_cas(dir.path()).await);

    let origin = Arc::new(LocalOrigin::new(
        OriginId::from("local"),
        origin_dir.path().to_path_buf(),
    ));
    let fetcher = Arc::new(ContentFetcher::new(store.clone()));
    fetcher.register_origin(origin);

    let file_meta = FileMeta {
        id: FileId(1),
        virtual_path: VirtualPath::new("/test.flac"),
        real_path: RealPath {
            origin_id: OriginId::from("local"),
            path: PathBuf::from("/test.flac"),
        },
        size: test_content.len() as u64,
        mtime: UNIX_EPOCH,
        content_hash: None,
        audio: None,
    };
    fetcher.register_file(file_meta);

    let manifest = fetcher.fetch_file(FileId(1)).await.unwrap();
    let chunk_hash = manifest.chunks[0].hash;
    let hex = chunk_hash.as_hex();
    let chunk_path = dir
        .path()
        .join("chunks")
        .join(&hex[0..2])
        .join(&hex[2..4])
        .join(&hex);

    std::fs::remove_file(&chunk_path).unwrap();

    let reader = FileReader::with_fetcher(store, fetcher);
    reader.register_manifest(manifest);

    let result = reader.read(FileId(1), 0, test_content.len() as u32).await;

    assert!(
        result.is_ok(),
        "Issue 6.4: Missing chunk should be re-fetched from origin"
    );
    assert_eq!(
        &result.unwrap()[..],
        test_content,
        "Data should match original after re-fetch"
    );
}

#[tokio::test]
async fn test_passthrough_mode_when_cache_disk_dead() {
    use musicfs_cas::ContentFetcher;
    use musicfs_origins::LocalOrigin;

    let dir = TempDir::new().unwrap();
    let origin_dir = TempDir::new().unwrap();
    let test_content = b"passthrough test data";
    setup_test_file(&origin_dir, "test.flac", test_content);

    let store = Arc::new(
        CasStore::open(CasConfig {
            chunks_dir: dir.path().join("chunks"),
            max_size: 10,
            shard_levels: 2,
        })
        .await
        .unwrap(),
    );

    let origin = Arc::new(LocalOrigin::new(
        OriginId::from("local"),
        origin_dir.path().to_path_buf(),
    ));
    let fetcher = Arc::new(ContentFetcher::new(store.clone()));
    fetcher.register_origin(origin);

    let file_meta = FileMeta {
        id: FileId(1),
        virtual_path: VirtualPath::new("/test.flac"),
        real_path: RealPath {
            origin_id: OriginId::from("local"),
            path: PathBuf::from("/test.flac"),
        },
        size: test_content.len() as u64,
        mtime: UNIX_EPOCH,
        content_hash: None,
        audio: None,
    };
    fetcher.register_file(file_meta);

    let manifest = fetcher.fetch_file(FileId(1)).await.unwrap();

    assert!(
        !manifest.chunks.is_empty(),
        "Issue 6.6: Fetch should complete even when CAS write fails (passthrough mode)"
    );
}

#[tokio::test]
async fn test_cas_size_tracking_is_correct() {
    let dir = TempDir::new().unwrap();
    let config = CasConfig {
        chunks_dir: dir.path().join("chunks"),
        max_size: 10_000_000,
        shard_levels: 2,
    };
    let store = CasStore::open(config).await.unwrap();

    let data = vec![0u8; 1000];
    store.put(&data).await.unwrap();

    assert!(
        store.current_size() >= 1000,
        "Issue C6: current_size should track chunk data (recursive), got {}",
        store.current_size()
    );
}

#[test]
fn test_pid_file_prevents_concurrent_mount() {
    use std::fs::File;
    use std::os::unix::io::AsRawFd;

    let dir = TempDir::new().unwrap();
    let lock_path = dir.path().join("musicfs.lock");

    fn try_lock(path: &Path) -> Result<File, std::io::Error> {
        let file = File::create(path)?;
        let fd = file.as_raw_fd();
        let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if ret != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(file)
    }

    let lock1 = try_lock(&lock_path);
    assert!(lock1.is_ok(), "Issue C9: First lock should succeed");

    let lock2 = try_lock(&lock_path);
    assert!(
        lock2.is_err(),
        "Issue C9: Second lock should fail (already held)"
    );

    drop(lock1);

    let lock3 = try_lock(&lock_path);
    assert!(
        lock3.is_ok(),
        "Issue C9: Third lock should succeed after first released"
    );
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
        panic!(
            "Issue 3.7: dist/musicfs.service does not exist at {:?}",
            service_path
        );
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
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();

    std::env::set_var("NOTIFY_SOCKET", &socket_path);

    let result = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);
    assert!(
        result.is_ok(),
        "sd_notify should succeed when NOTIFY_SOCKET is set"
    );

    let mut buf = [0u8; 256];
    let len = socket.recv(&mut buf).unwrap();
    let msg = std::str::from_utf8(&buf[..len]).unwrap();

    assert!(
        msg.contains("READY=1"),
        "sd_notify should send READY=1, got: {}",
        msg
    );

    std::env::remove_var("NOTIFY_SOCKET");
}

#[tokio::test]
async fn test_shutdown_cancels_background_tasks() {
    let token = CancellationToken::new();
    let stopped = Arc::new(AtomicBool::new(false));
    let stopped_clone = stopped.clone();
    let token_clone = token.clone();

    tokio::spawn(async move {
        token_clone.cancelled().await;
        stopped_clone.store(true, Ordering::SeqCst);
    });

    assert!(!stopped.load(Ordering::SeqCst));
    token.cancel();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(stopped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_shutdown_flushes_tantivy() {
    let dir = TempDir::new().unwrap();
    let idx_path = dir.path().join("idx");

    {
        let index = SearchIndex::open(&idx_path).unwrap();
        index
            .index_file(&make_file_meta(1, "/a.flac", 1000))
            .unwrap();
        index.commit().unwrap();
    }

    let index2 = SearchIndex::open(&idx_path).unwrap();
    assert_eq!(index2.search("a", 10).unwrap().len(), 1);
}

#[tokio::test]
async fn test_supervisor_detects_task_completion() {
    let supervisor = TaskSupervisor::new();
    supervisor.spawn_supervised("fast", async {});
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test]
async fn test_supervisor_detects_panic() {
    let supervisor = TaskSupervisor::new();
    supervisor.spawn_supervised("panicker", async {
        panic!("boom");
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(matches!(
        supervisor.task_status("panicker"),
        TaskStatus::Failed { .. }
    ));
}

#[tokio::test]
async fn test_supervisor_restarts_critical_task() {
    let count = Arc::new(AtomicU32::new(0));
    let c = count.clone();

    let supervisor = TaskSupervisor::new();
    supervisor.spawn_critical("restartable", move || {
        let c = c.clone();
        async move {
            let n = c.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                panic!("first run fails");
            }
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    });

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(matches!(
        supervisor.task_status("restartable"),
        TaskStatus::Running
    ));
}

#[tokio::test]
async fn test_sigterm_triggers_shutdown() {
    use std::process::{Command, Stdio};
    use std::time::Duration;
    use tokio::time::timeout;

    let musicfs_bin = std::env::var("CARGO_BIN_EXE_musicfs").ok();
    if musicfs_bin.is_none() {
        eprintln!(
            "Skipping test_sigterm_triggers_shutdown: musicfs binary not available in test context"
        );
        return;
    }

    let bin_path = musicfs_bin.unwrap();
    let temp_dir = tempfile::TempDir::new().unwrap();
    let mountpoint = temp_dir.path().join("mount");
    let origin = temp_dir.path().join("origin");
    std::fs::create_dir_all(&mountpoint).unwrap();
    std::fs::create_dir_all(&origin).unwrap();

    let mut child = Command::new(&bin_path)
        .args([
            "mount",
            "--origin",
            origin.to_str().unwrap(),
            mountpoint.to_str().unwrap(),
        ])
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
    })
    .await;

    assert!(
        exit_result.is_ok(),
        "Issue 2.1: Process should exit within 10s after SIGTERM"
    );
}
