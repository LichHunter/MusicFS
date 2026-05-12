use anyhow::{Context, Result};
use clap::Parser;
use musicfs_cache::TreeBuilder;
use musicfs_cas::{CasConfig, CasStore, ContentFetcher, FileReader};
use musicfs_core::{FileId, FileMeta, OriginId, RealPath, VirtualPath};
use musicfs_fuse::MusicFs;
use musicfs_metadata::MetadataParser;
use musicfs_origins::{LocalOrigin, Origin};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;
use tracing::{debug, info};

#[derive(Parser)]
#[command(name = "musicfs")]
#[command(about = "Virtual FUSE filesystem for music libraries")]
struct Cli {
    #[arg(help = "Mount point for the virtual filesystem")]
    mountpoint: PathBuf,

    #[arg(short, long, help = "Source music directory (origin)")]
    origin: PathBuf,

    #[arg(short, long, help = "Cache directory for CAS chunks")]
    cache_dir: Option<PathBuf>,

    #[arg(short, long, default_value = "info", help = "Log level (debug, info, warn, error)")]
    log_level: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    init_logging(&cli.log_level);

    let runtime = tokio::runtime::Runtime::new().context("Failed to create Tokio runtime")?;
    let handle = runtime.handle().clone();

    let (tree, reader) = runtime.block_on(async {
        info!("MusicFS starting...");
        info!("Origin: {:?}", cli.origin);
        info!("Mountpoint: {:?}", cli.mountpoint);

        let cache_dir = cli.cache_dir.unwrap_or_else(|| {
            dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("musicfs")
        });
        info!("Cache directory: {:?}", cache_dir);

        std::fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;
        std::fs::create_dir_all(&cli.mountpoint).context("Failed to create mountpoint")?;

        let cas_config = CasConfig {
            chunks_dir: cache_dir.join("chunks"),
            ..Default::default()
        };
        let store = Arc::new(CasStore::open(cas_config).await.context("Failed to open CAS store")?);
        info!("CAS store initialized");

        let origin_id = OriginId::from("local");
        let origin = Arc::new(LocalOrigin::new(origin_id.clone(), cli.origin.clone()));
        info!("Origin registered: {}", origin.display_name());

        let fetcher = Arc::new(ContentFetcher::new(store.clone()));
        fetcher.register_origin(origin);

        info!("Scanning music files...");
        let files = scan_music_files(&cli.origin, &origin_id).await?;
        info!("Found {} music files", files.len());

        let mut builder = TreeBuilder::new();
        for file in &files {
            builder.add_file(file);
            fetcher.register_file(file.clone());
        }
        let tree = Arc::new(RwLock::new(builder.build()));
        info!("Virtual tree built");

        let reader = Arc::new(FileReader::with_fetcher(store, fetcher));

        Ok::<_, anyhow::Error>((tree, reader))
    })?;

    let fs = MusicFs::with_reader(tree, reader, handle);

    info!("Mounting filesystem at {:?}", cli.mountpoint);
    info!("Press Ctrl+C to unmount");

    fs.mount(&cli.mountpoint).context("Failed to mount filesystem")?;

    Ok(())
}

fn init_logging(level: &str) {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(filter)
        .init();
}

async fn scan_music_files(dir: &Path, origin_id: &OriginId) -> Result<Vec<FileMeta>> {
    let parser = MetadataParser::new();
    let mut files = Vec::new();
    let mut file_id_counter = 1i64;

    scan_dir_recursive(dir, dir, origin_id, &parser, &mut files, &mut file_id_counter).await?;

    Ok(files)
}

async fn scan_dir_recursive(
    base: &Path,
    dir: &Path,
    origin_id: &OriginId,
    parser: &MetadataParser,
    files: &mut Vec<FileMeta>,
    id_counter: &mut i64,
) -> Result<()> {
    let mut entries = tokio::fs::read_dir(dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let metadata = entry.metadata().await?;

        if metadata.is_dir() {
            Box::pin(scan_dir_recursive(base, &path, origin_id, parser, files, id_counter)).await?;
        } else if is_audio_file(&path) {
            let relative_path = path.strip_prefix(base).unwrap_or(&path);

            let audio_meta = match parser.parse_file(&path) {
                Ok(meta) => Some(meta),
                Err(e) => {
                    debug!("Failed to parse metadata for {:?}: {}", path, e);
                    None
                }
            };

            let virtual_path = build_virtual_path(&path, audio_meta.as_ref());

            let file_meta = FileMeta {
                id: FileId(*id_counter),
                virtual_path,
                real_path: RealPath {
                    origin_id: origin_id.clone(),
                    path: PathBuf::from("/").join(relative_path),
                },
                size: metadata.len(),
                mtime: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                content_hash: None,
                audio: audio_meta,
            };

            debug!("Found: {:?} -> {:?}", file_meta.real_path.path, file_meta.virtual_path);
            files.push(file_meta);
            *id_counter += 1;
        }
    }

    Ok(())
}

fn is_audio_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).as_deref(),
        Some("flac" | "mp3" | "ogg" | "wav" | "m4a" | "aac" | "opus")
    )
}

fn build_virtual_path(path: &Path, audio: Option<&musicfs_core::AudioMeta>) -> VirtualPath {
    if let Some(meta) = audio {
        let artist = meta.artist.as_deref().unwrap_or("Unknown Artist");
        let album = meta.album.as_deref().unwrap_or("Unknown Album");
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("track");

        VirtualPath::new(&format!("/{}/{}/{}", sanitize(artist), sanitize(album), filename))
    } else {
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
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
