use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use musicfs_cache::{Database, TreeBuilder};
use musicfs_cas::{CasConfig, CasStore, ContentFetcher, FileReader};
use musicfs_core::{FileId, FileMeta, LoggingConfig, OriginId, RealPath, VirtualPath};
use musicfs_fuse::MusicFs;
use musicfs_metadata::MetadataParser;
use musicfs_origins::{LocalOrigin, Origin};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use toml::Value;
use tracing::{debug, info, warn};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Layer};

#[derive(Parser)]
#[command(name = "musicfs")]
#[command(about = "Virtual FUSE filesystem for music libraries")]
struct Cli {
    #[arg(short, long, default_value = "info", help = "Log level")]
    log_level: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Mount {
        #[arg(short, long, help = "Config file path")]
        config: Option<PathBuf>,
        #[arg(help = "Mount point (optional if provided in config file)")]
        mountpoint: Option<PathBuf>,
        #[arg(short, long, help = "Source music directory")]
        origin: Option<PathBuf>,
        #[arg(short = 'd', long, help = "Cache directory")]
        cache_dir: Option<PathBuf>,
    },
    Status,
    Cache {
        #[command(subcommand)]
        command: CacheCommands,
    },
    Search {
        query: String,
        #[arg(short, long, default_value = "100")]
        limit: u32,
    },
    Origin {
        #[command(subcommand)]
        command: OriginCommands,
    },
    Events {
        #[arg(short, long, help = "Filter by event type")]
        r#type: Option<String>,
    },
    Shutdown {
        #[arg(short, long, default_value = "true")]
        graceful: bool,
        #[arg(short, long, default_value = "30")]
        timeout: u32,
    },
}

#[derive(Subcommand)]
enum CacheCommands {
    Stats,
    Clear {
        #[arg(help = "Origin to clear cache for")]
        origin: Option<String>,
    },
    Prefetch {
        #[arg(help = "Paths to prefetch")]
        paths: Vec<String>,
    },
}

#[derive(Subcommand)]
enum OriginCommands {
    List,
    Health { origin_id: String },
    Rescan { origin_id: String },
}

struct LockFile {
    _file: File,
}

fn try_acquire_lock(path: &Path) -> Result<LockFile> {
    let file = File::create(path).context("Failed to create lock file")?;
    let fd = file.as_raw_fd();

    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::WouldBlock {
            anyhow::bail!("MusicFS is already running (lock file: {:?})", path);
        }
        return Err(err).context("Failed to acquire lock");
    }

    let mut f = &file;
    writeln!(f, "{}", std::process::id())?;

    Ok(LockFile { _file: file })
}

fn main() -> Result<()> {
    musicfs_core::install_panic_hook();
    let cli = Cli::parse();

    match cli.command {
        Commands::Mount {
            config,
            mountpoint,
            origin,
            cache_dir,
        } => {
            let mut config = if let Some(config_path) = config {
                musicfs_core::Config::from_file(&config_path)?
            } else {
                let origin_path = origin
                    .context("--origin is required for mount if no config file is provided")?;
                let mp = mountpoint
                    .clone()
                    .context("mount point is required if no config file is provided")?;
                let cache_dir = cache_dir.clone().unwrap_or_else(|| {
                    dirs::cache_dir()
                        .unwrap_or_else(|| PathBuf::from("/tmp"))
                        .join("musicfs")
                });

                let mut settings = HashMap::new();
                settings.insert(
                    "path".to_string(),
                    Value::String(origin_path.to_string_lossy().into_owned()),
                );

                musicfs_core::Config {
                    mount_point: mp,
                    cache_dir: cache_dir.clone(),
                    origins: vec![musicfs_core::OriginConfig {
                        id: "local".to_string(),
                        origin_type: musicfs_core::OriginType::Local,
                        priority: 1,
                        enabled: true,
                        settings,
                    }],
                    cache: Default::default(),
                    health: Default::default(),
                    logging: LoggingConfig {
                        level: cli.log_level.clone(),
                        ..Default::default()
                    },
                }
            };

            if let Some(c_dir) = cache_dir {
                config.cache_dir = c_dir;
            }
            if let Some(cli_mountpoint) = mountpoint {
                config.mount_point = cli_mountpoint;
            }

            let _guard = init_logging(&config.logging)?;
            run_mount(config)
        }
        Commands::Status => {
            init_basic_logging(&cli.log_level);
            run_status()
        }
        Commands::Cache { command } => {
            init_basic_logging(&cli.log_level);
            run_cache(command)
        }
        Commands::Search { query, limit } => {
            init_basic_logging(&cli.log_level);
            run_search(&query, limit)
        }
        Commands::Origin { command } => {
            init_basic_logging(&cli.log_level);
            run_origin(command)
        }
        Commands::Events { r#type } => {
            init_basic_logging(&cli.log_level);
            run_events(r#type)
        }
        Commands::Shutdown { graceful, timeout } => {
            init_basic_logging(&cli.log_level);
            run_shutdown(graceful, timeout)
        }
    }
}

fn run_mount(config: musicfs_core::Config) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("Failed to create Tokio runtime")?;
    let handle = runtime.handle().clone();

    let (tree, reader, db) = runtime.block_on(async {
        info!(mountpoint = ?config.mount_point, "Mount configuration");
        info!("Cache directory: {:?}", config.cache_dir);

        std::fs::create_dir_all(&config.cache_dir).context("Failed to create cache directory")?;
        std::fs::create_dir_all(&config.mount_point).context("Failed to create mountpoint")?;

        let db_path = config.cache_dir.join("musicfs.db");
        let db = Arc::new(Database::open(&db_path).context("Failed to open metadata database")?);
        info!("Metadata database opened at {:?}", db_path);

        let cas_config = CasConfig {
            chunks_dir: config.cache_dir.join("chunks"),
            ..Default::default()
        };
        let store = Arc::new(
            CasStore::open(cas_config)
                .await
                .context("Failed to open CAS store")?,
        );
        info!("CAS store initialized");

        let fetcher = Arc::new(ContentFetcher::new(store.clone()));
        let mut files = Vec::new();

        for origin_cfg in &config.origins {
            if !origin_cfg.enabled {
                continue;
            }

            let origin_id = OriginId::from(origin_cfg.id.as_str());
            let origin: Arc<dyn Origin> = match origin_cfg.origin_type {
                musicfs_core::OriginType::Local => {
                    let path_str = origin_cfg
                        .settings
                        .get("path")
                        .and_then(|v| v.as_str())
                        .context("path required for local origin")?;
                    Arc::new(LocalOrigin::new(origin_id.clone(), PathBuf::from(path_str)))
                }
                _ => {
                    warn!(
                        "Origin type {:?} not supported in CLI yet, skipping",
                        origin_cfg.origin_type
                    );
                    continue;
                }
            };

            info!("Origin registered: {}", origin.display_name());
            fetcher.register_origin(origin.clone());

            if origin_cfg.origin_type == musicfs_core::OriginType::Local {
                let path_str = origin_cfg
                    .settings
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap();
                let origin_path = PathBuf::from(path_str);
                info!("Scanning music files for origin {}...", origin_cfg.id);
                let origin_files = scan_music_files(&origin_path, &origin_id, db.as_ref()).await?;
                info!(
                    "Found {} music files for origin {}",
                    origin_files.len(),
                    origin_cfg.id
                );
                files.extend(origin_files);
            }
        }

        let mut builder = TreeBuilder::new();
        for file in &files {
            builder.add_file(file);
            fetcher.register_file(file.clone());
        }
        let mut tree = builder.build();

        let dirs = db.list_directories().unwrap_or_default();
        for dir_path in &dirs {
            if tree.get_by_path(dir_path).is_none() {
                if let Err(e) = tree.mkdir(dir_path) {
                    debug!("Could not restore directory {:?}: {:?}", dir_path, e);
                }
            }
        }
        info!(
            "Virtual tree built ({} files, {} user directories)",
            tree.file_count(),
            dirs.len()
        );

        let tree = Arc::new(RwLock::new(tree));

        let reader = Arc::new(FileReader::with_fetcher(store, fetcher));

        Ok::<_, anyhow::Error>((tree, reader, db))
    })?;

    check_stale_mount(&config.mount_point)?;

    let lock_path = config.cache_dir.join("musicfs.lock");
    let _lock = try_acquire_lock(&lock_path)
        .context("Failed to acquire lock — is another instance running?")?;
    info!(lock_path = ?lock_path, "Lock acquired");

    let fs = MusicFs::with_reader(tree, reader, handle.clone()).with_db(db);

    info!("Mounting filesystem at {:?}", config.mount_point);

    let session = fs
        .spawn_mount(&config.mount_point)
        .context("Failed to mount filesystem")?;

    #[cfg(target_os = "linux")]
    {
        if let Err(e) = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]) {
            debug!("sd_notify not available (not running under systemd): {}", e);
        }
    }
    info!("MusicFS ready, PID {}", std::process::id());

    let shutdown_token = tokio_util::sync::CancellationToken::new();

    runtime.block_on(async {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

        tokio::select! {
            _ = sigterm.recv() => {
                info!("Received SIGTERM, shutting down");
            }
            _ = sigint.recv() => {
                info!("Received SIGINT, shutting down");
            }
        }

        info!("Beginning ordered shutdown");
        shutdown_token.cancel();

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        info!("Background tasks stopped");

        Ok::<_, anyhow::Error>(())
    })?;

    #[cfg(target_os = "linux")]
    {
        let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Stopping]);
    }
    info!("Unmounting filesystem");
    drop(session);
    info!("Shutdown complete");

    Ok(())
}

fn run_status() -> Result<()> {
    println!("Status: Not connected to daemon");
    println!("Hint: gRPC client integration pending");
    Ok(())
}

fn run_cache(command: CacheCommands) -> Result<()> {
    match command {
        CacheCommands::Stats => {
            println!("Cache stats: gRPC client integration pending");
        }
        CacheCommands::Clear { origin } => {
            println!("Clearing cache for: {}", origin.as_deref().unwrap_or("all"));
            println!("gRPC client integration pending");
        }
        CacheCommands::Prefetch { paths } => {
            println!("Prefetching {} paths", paths.len());
            println!("gRPC client integration pending");
        }
    }
    Ok(())
}

fn run_search(query: &str, limit: u32) -> Result<()> {
    println!("Searching for: {} (limit: {})", query, limit);
    println!("gRPC client integration pending");
    Ok(())
}

fn run_origin(command: OriginCommands) -> Result<()> {
    match command {
        OriginCommands::List => {
            println!("Origins: gRPC client integration pending");
        }
        OriginCommands::Health { origin_id } => {
            println!("Health for {}: gRPC client integration pending", origin_id);
        }
        OriginCommands::Rescan { origin_id } => {
            println!("Rescanning {}: gRPC client integration pending", origin_id);
        }
    }
    Ok(())
}

fn run_events(event_type: Option<String>) -> Result<()> {
    println!(
        "Subscribing to events: {}",
        event_type.as_deref().unwrap_or("all")
    );
    println!("gRPC client integration pending");
    Ok(())
}

fn run_shutdown(graceful: bool, timeout: u32) -> Result<()> {
    println!(
        "Shutdown requested (graceful: {}, timeout: {}s)",
        graceful, timeout
    );
    println!("gRPC client integration pending");
    Ok(())
}

fn init_logging(config: &LoggingConfig) -> Result<WorkerGuard> {
    std::fs::create_dir_all(&config.log_dir)?;

    let file_appender = tracing_appender::rolling::daily(&config.log_dir, "musicfs.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let file_layer = if config.json_output {
        fmt::layer()
            .json()
            .with_writer(non_blocking)
            .with_ansi(false)
            .boxed()
    } else {
        fmt::layer()
            .with_writer(non_blocking)
            .with_ansi(false)
            .boxed()
    };

    let stderr_layer = fmt::layer().with_writer(std::io::stderr).compact();

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.level));

    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer);

    #[cfg(target_os = "linux")]
    let subscriber = {
        let journald_layer = if config.journald {
            tracing_journald::layer()
                .ok()
                .map(|l| l.with_syslog_identifier("musicfs".to_string()))
        } else {
            None
        };
        subscriber.with(journald_layer)
    };

    subscriber.init();

    info!(version = env!("CARGO_PKG_VERSION"), "MusicFS starting");
    Ok(guard)
}

fn init_basic_logging(level: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    tracing_subscriber::registry()
        .with(fmt::layer().compact())
        .with(filter)
        .init();
}

async fn scan_music_files(
    dir: &Path,
    origin_id: &OriginId,
    db: &Database,
) -> Result<Vec<FileMeta>> {
    let parser = MetadataParser::new();
    let mut files = Vec::new();
    let mut file_id_counter = 1i64;

    scan_dir_recursive(
        dir,
        dir,
        origin_id,
        &parser,
        db,
        &mut files,
        &mut file_id_counter,
    )
    .await?;

    Ok(files)
}

async fn scan_dir_recursive(
    base: &Path,
    dir: &Path,
    origin_id: &OriginId,
    parser: &MetadataParser,
    db: &Database,
    files: &mut Vec<FileMeta>,
    id_counter: &mut i64,
) -> Result<()> {
    let mut entries = tokio::fs::read_dir(dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let metadata = entry.metadata().await?;

        if metadata.is_dir() {
            Box::pin(scan_dir_recursive(
                base, &path, origin_id, parser, db, files, id_counter,
            ))
            .await?;
        } else if is_audio_file(&path) {
            let relative_path = path.strip_prefix(base).unwrap_or(&path);
            let real_path_for_db = PathBuf::from("/").join(relative_path);

            let audio_meta = match parser.parse_file(&path) {
                Ok(meta) => Some(meta),
                Err(e) => {
                    debug!("Failed to parse metadata for {:?}: {}", path, e);
                    None
                }
            };

            let virtual_path = if let Ok(Some(stored_path)) =
                db.get_file_by_real_path(origin_id, &real_path_for_db)
            {
                stored_path
            } else {
                build_virtual_path(&path, audio_meta.as_ref())
            };

            let real_path = RealPath {
                origin_id: origin_id.clone(),
                path: real_path_for_db.clone(),
            };

            let file_id = db
                .upsert_file(
                    origin_id,
                    &real_path.path,
                    &virtual_path,
                    audio_meta.as_ref().unwrap_or(&Default::default()),
                    metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    metadata.len(),
                )
                .unwrap_or_else(|e| {
                    debug!("Failed to upsert file to DB: {}", e);
                    FileId(*id_counter)
                });

            let file_meta = FileMeta {
                id: file_id,
                virtual_path,
                real_path,
                size: metadata.len(),
                mtime: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                content_hash: None,
                audio: audio_meta,
            };

            debug!(
                "Found: {:?} -> {:?}",
                file_meta.real_path.path, file_meta.virtual_path
            );
            files.push(file_meta);
            *id_counter += 1;
        }
    }

    Ok(())
}

fn is_audio_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .as_deref(),
        Some("flac" | "mp3" | "ogg" | "wav" | "m4a" | "aac" | "opus")
    )
}

fn build_virtual_path(path: &Path, audio: Option<&musicfs_core::AudioMeta>) -> VirtualPath {
    if let Some(meta) = audio {
        let artist = meta.artist.as_deref().unwrap_or("Unknown Artist");
        let album = meta.album.as_deref().unwrap_or("Unknown Album");
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("track");

        VirtualPath::new(&format!(
            "/{}/{}/{}",
            sanitize(artist),
            sanitize(album),
            filename
        ))
    } else {
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        VirtualPath::new(&format!("/Unknown Artist/Unknown Album/{}", filename))
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

fn check_stale_mount(mountpoint: &Path) -> Result<()> {
    if let Ok(mounts) = std::fs::read_to_string("/proc/mounts") {
        for line in mounts.lines() {
            if line.contains(mountpoint.to_string_lossy().as_ref()) && line.contains("fuse") {
                warn!(
                    "Stale FUSE mount detected at {:?}, attempting cleanup",
                    mountpoint
                );
                let status = std::process::Command::new("fusermount")
                    .args(["-uz", &mountpoint.to_string_lossy()])
                    .status();
                match status {
                    Ok(s) if s.success() => info!("Stale mount cleaned up"),
                    Ok(s) => warn!("fusermount exited with: {}", s),
                    Err(e) => warn!("Failed to run fusermount: {}", e),
                }
            }
        }
    }
    Ok(())
}
