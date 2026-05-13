use std::process::Command;

#[test]
#[ignore]
fn test_mpv_playback() {
    let mountpoint = setup_test_mount();

    let output = Command::new("mpv")
        .args([
            "--no-video",
            "--no-audio",
            "--length=2",
            "--msg-level=all=debug",
            &format!("{}/Artist/Album/01 - Track.flac", mountpoint),
        ])
        .output()
        .expect("mpv must be installed");

    assert!(
        output.status.success(),
        "mpv playback failed: {:?}",
        output
    );
}

#[test]
#[ignore]
fn test_vlc_playback() {
    let mountpoint = setup_test_mount();

    let output = Command::new("cvlc")
        .args([
            "--play-and-exit",
            "--run-time=2",
            &format!("{}/Artist/Album/", mountpoint),
        ])
        .output()
        .expect("vlc must be installed");

    assert!(output.status.success(), "VLC playback failed");
}

#[test]
#[ignore]
fn test_file_manager_operations() {
    let mountpoint = setup_test_mount();

    let entries: Vec<_> = std::fs::read_dir(&mountpoint)
        .expect("read_dir failed")
        .collect();

    assert!(!entries.is_empty(), "mountpoint should have entries");

    for entry in entries {
        let entry = entry.expect("entry should be valid");
        let metadata = entry.metadata().expect("metadata should work");
        assert!(metadata.is_dir() || metadata.is_file());
    }
}

#[test]
#[ignore]
fn test_concurrent_player_access() {
    let mountpoint = setup_test_mount();

    let handles: Vec<_> = (0..3)
        .map(|i| {
            let mp = mountpoint.clone();
            std::thread::spawn(move || {
                Command::new("mpv")
                    .args([
                        "--no-video",
                        "--no-audio",
                        "--length=1",
                        &format!("{}/Artist/Album/0{} - Track.flac", mp, i + 1),
                    ])
                    .output()
            })
        })
        .collect();

    for handle in handles {
        let output = handle.join().unwrap().expect("mpv should run");
        assert!(output.status.success());
    }
}

fn setup_test_mount() -> String {
    std::env::var("MUSICFS_TEST_MOUNT").unwrap_or_else(|_| "/tmp/musicfs-test".to_string())
}
