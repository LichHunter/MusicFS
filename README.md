# MusicFS

> A read-only FUSE filesystem that presents your music library organized by metadata — artist, album, track — regardless of how files are stored on disk.

Browse `/Artist/Album/Track.flac` in any media player or file manager. Original files are never touched.

---

## What It Does

MusicFS mounts as a virtual filesystem. Point it at your music storage (local drive, NFS share, S3 bucket, SFTP server) and it exposes a clean metadata-based directory tree:

```
/mnt/music/
├── Metallica/
│   └── 72 Seasons (2023) [FLAC]/
│       ├── 01 - 72 Seasons.flac
│       ├── 02 - Shadows Follow.flac
│       └── cover.jpg
├── Pink Floyd/
│   └── The Wall (1979) [FLAC]/
│       ├── 01 - In the Flesh?.flac
│       └── ...
└── .search/
    └── (full-text search — see Search section)
```

Files are read directly from origin storage with local chunk caching. Once cached, playback works entirely offline. Write operations return `EROFS` — origin files are always safe.

---

## Features

| Feature | Details |
|---------|---------|
| **Instant mount** | O(1) regardless of library size (<500ms) |
| **Metadata-organized paths** | Configurable path templates via `$artist`, `$album`, `$year`, etc. |
| **Multi-origin federation** | Local, NFS, SMB, S3, SFTP — automatic failover by priority |
| **Content-addressable cache** | Chunk-level deduplication, LRU eviction, delta sync (>90% bandwidth savings) |
| **Full-text search** | `/.search/metallica/` returns instant results across 1M+ tracks |
| **Metadata overlay** | Set/override tags in the virtual layer without modifying originals |
| **Album art** | Virtual `cover.jpg` per album, extracted from embedded tags |
| **Plugin system** | Native `.so` and WASM plugins for custom origins, formats, metadata sources |
| **gRPC control API** | Cache stats, origin health, live event streaming, metadata management |
| **systemd integration** | `sd_notify` ready, journald logging, clean SIGTERM handling |

**Supported formats:** FLAC, MP3, OGG, WAV, M4A, AAC, Opus

---

## Quick Start

```bash
# 1. Enter dev environment (provides Rust, FUSE3, SQLite, everything)
nix develop

# 2. Build
cargo build

# 3. Mount your music library
./target/debug/musicfs mount /mnt/music --origin /path/to/your/music

# 4. Browse
ls /mnt/music
mpv /mnt/music/Artist/Album/01\ -\ Track.flac

# 5. Unmount
fusermount -u /mnt/music
```

No `rustup`, no `apt install`. The Nix flake provides the full toolchain.

---

## Installation

### From Nix (recommended)

```bash
# Development shell — everything you need
nix develop

# Or install the binary into your profile
nix profile install .#musicfs
```

### From Source

**Prerequisites (non-Nix):**
- Rust 1.75+
- `libfuse3-dev` / `fuse3` (package name varies by distro)
- `libsqlite3-dev`
- `libssl-dev`
- `protobuf-compiler` (for gRPC)
- `clang` + `lld`

```bash
git clone https://github.com/user/musicfs
cd musicfs/musicfs
cargo build --release
sudo cp target/release/musicfs /usr/local/bin/
```

### System Requirements

| Resource | Minimum | Recommended |
|----------|---------|-------------|
| CPU | 1 core | 4 cores |
| RAM | 256 MB | 2 GB |
| Disk (cache) | 1 GB | 50 GB |
| Linux kernel | 4.x+ | 5.x+ |
| FUSE module | required | — |

---

## Configuration

MusicFS can be configured via file (`--config`), CLI flags, or environment variables (`RUST_LOG` for log level).

### Minimal Config

```toml
mount_point = "/mnt/music"
cache_dir   = "/home/user/.cache/musicfs"

[[origins]]
id          = "local"
origin_type = "local"
priority    = 1
path        = "/mnt/nas/music"
```

```bash
musicfs mount --config /etc/musicfs/config.toml
```

### Full Config Reference

<!-- embedme config.example.toml -->
```toml
# MusicFS Configuration
# Copy to /etc/musicfs/config.toml or ~/.config/musicfs/config.toml

# Required: where to mount the virtual filesystem
mount_point = "/mnt/music"

# Required: directory for cache data (CAS chunks, metadata, search index)
cache_dir = "/var/cache/musicfs"

# ------------------------------------------------------------------------------
# Origins - music sources (at least one required)
# Supported types: local, nfs, smb, s3, sftp
# Lower priority number = preferred source for failover
# ------------------------------------------------------------------------------

[[origins]]
id = "local-music"
origin_type = "local"
priority = 1
enabled = true
path = "/home/user/Music"

[[origins]]
id = "nas-nfs"
origin_type = "nfs"
priority = 2
enabled = true
path = "/mnt/nas/music"

[[origins]]
id = "nas-smb"
origin_type = "smb"
priority = 3
enabled = false
path = "/mnt/smb/music"

[[origins]]
id = "cloud-backup"
origin_type = "s3"
priority = 10
enabled = false
bucket = "my-music-backup"
region = "us-east-1"

[[origins]]
id = "remote-server"
origin_type = "sftp"
priority = 10
enabled = false
host = "music.example.com"
port = 22
user = "musicfs"
path = "/srv/music"

# ------------------------------------------------------------------------------
# Cache settings
# ------------------------------------------------------------------------------

[cache]
# In-memory metadata cache size (artist/album/track info)
metadata_cache_mb = 100

# On-disk content cache size (audio chunks)
content_cache_gb = 10

# ------------------------------------------------------------------------------
# Health monitoring for origin failover
# ------------------------------------------------------------------------------

[health]
# How often to check origin health
check_interval_secs = 30

# Timeout for health check probes
timeout_ms = 5000

# Consecutive failures before marking origin unhealthy
unhealthy_threshold = 3

# Per-origin type thresholds (overrides unhealthy_threshold)
[health.per_origin_thresholds]
local = 1
nfs = 3
smb = 3
s3 = 3
sftp = 3

# ------------------------------------------------------------------------------
# Logging
# ------------------------------------------------------------------------------

[logging]
# Directory for log files
log_dir = "/var/log/musicfs"

# Output logs as JSON (for log aggregators)
json_output = false

# Send logs to systemd journal
journald = true

# Log level filter (tracing format)
# Examples: "info", "debug", "musicfs=debug,warn", "musicfs_fuse=trace"
level = "musicfs=info,warn"

# Trace sampling rate for performance tracing (0.0 to 1.0)
trace_sample_rate = 1.0
```

### Cache Layout on Disk

```
~/.cache/musicfs/
├── musicfs.db       # SQLite: file metadata, virtual tree, overlay data
├── musicfs.lock     # Single-instance lock
├── musicfs.pid      # Daemon PID
├── chunks/          # Content-addressable chunk files
│   ├── aa/          # 256 subdirs (first 2 hex chars of hash)
│   │   └── aa1b2c…  # 64 KB average chunk
│   └── ...
├── search.idx/      # Tantivy full-text search index
└── chunks.sled/     # Sled KV: content hash → chunk location
```

---

## CLI Reference

```
musicfs [OPTIONS] <COMMAND>

OPTIONS:
  -l, --log-level <LEVEL>   Log verbosity [default: info]
```

### `mount` — Start the filesystem

```bash
# From CLI flags (quick start)
musicfs mount /mnt/music --origin /path/to/music

# From config file
musicfs mount --config /etc/musicfs/config.toml

# All flags
musicfs mount [MOUNTPOINT] \
  --config <path>         # Config file (overrides flags)
  --origin <path>         # Source music directory
  --cache-dir <path>      # Cache location [default: ~/.cache/musicfs]
  --grpc-port <port>      # gRPC server port [default: 50052]
```

### `status` — Daemon status

```bash
musicfs status
```

### `cache` — Cache management

```bash
musicfs cache stats                    # Hit rate, size, dedup ratio
musicfs cache clear                    # Clear all caches
musicfs cache clear <origin-id>        # Clear cache for one origin
musicfs cache prefetch <path> [path…]  # Pre-warm cache for paths
```

### `search` — Full-text search

```bash
musicfs search "metallica"             # Search across all metadata
musicfs search "dark side" --limit 20  # Limit results [default: 100]
```

Search results are also browsable as a virtual directory (see [Search](#search)).

### `origin` — Origin management

```bash
musicfs origin list                    # List all configured origins
musicfs origin health <id>             # Check health of one origin
musicfs origin rescan <id>             # Force re-scan and re-index
```

### `metadata` — Metadata overlay

```bash
# Requires running daemon
musicfs metadata get "/Artist/Album/01 - Track.flac"
musicfs metadata get "/Artist/Album/01 - Track.flac" --field artist

musicfs metadata set "/Artist/Album/01 - Track.flac" \
  --title "New Title" \
  --artist "New Artist" \
  --album "New Album" \
  --track 1 \
  --genre "Rock" \
  --date "2023"

# Set from JSON
musicfs metadata set "/path/to/file.flac" --json '{"title":"foo","year":2023}'

# Show current (overlaid) metadata
musicfs metadata diff "/path/to/file.flac"

# Revert overlay — restore original metadata
musicfs metadata clear "/path/to/file.flac"

# Bulk import/export
musicfs metadata import library.csv
musicfs metadata import library.json
musicfs metadata export --output library.json
musicfs metadata export --output library.csv --query "artist:Metallica"
```

> **Note:** `--endpoint` flag (default `http://[::1]:50051`) selects the gRPC server.

### `trash` — Deleted file recovery

When files disappear from the origin, MusicFS moves them to a virtual trash rather than removing them immediately.

```bash
musicfs trash list --config /etc/musicfs/config.toml
musicfs trash list --since 7d              # Deleted in last 7 days
musicfs trash list --origin local          # Filter by origin
musicfs trash list --path "/Metallica"     # Filter by path prefix

musicfs trash restore "/Metallica/72 Seasons"  # Restore folder
musicfs trash restore --all                    # Restore everything

musicfs trash empty --older-than 30d       # Permanently delete old entries
musicfs trash empty --pattern "/Unknown*"  # Delete by pattern
```

### `events` — Live event stream

```bash
musicfs events                         # All events
musicfs events --type file_added       # Filter by type
# Event types: file_added, file_removed, file_modified,
#              origin_connected, origin_disconnected,
#              sync_started, sync_completed, cache_eviction
```

### `shutdown` — Stop the daemon

```bash
musicfs shutdown                       # Graceful (drain in-flight ops)
musicfs shutdown --graceful false      # Immediate
musicfs shutdown --timeout 60          # Max drain timeout seconds
```

---

## Storage Origins

### Local Filesystem

```toml
[[origins]]
id          = "local"
origin_type = "local"
priority    = 1
path        = "/mnt/nas/music"
```

Changes detected via `inotify`. Zero-latency access.

### NFS

```toml
[[origins]]
id          = "nfs"
origin_type = "nfs"
priority    = 2
host        = "nas.local"
export      = "/exports/music"
```

### SMB / CIFS

```toml
[[origins]]
id          = "smb"
origin_type = "smb"
priority    = 3
host        = "nas.local"
share       = "music"
```

### S3 (stub — not yet functional)

```toml
[[origins]]
id          = "s3"
origin_type = "s3"
priority    = 4
bucket      = "my-music"
region      = "us-east-1"
# Credentials via AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY env vars
```

### SFTP (stub — not yet functional)

```toml
[[origins]]
id          = "sftp"
origin_type = "sftp"
priority    = 4
host        = "server.example.com"
port        = 22
username    = "alice"
# Auth via SSH agent or key file — never store passwords in config
```

### Multi-Origin Failover

Multiple origins are federates into a single virtual tree. MusicFS selects origins by priority, falling back automatically when one becomes unhealthy. Health is polled every `check_interval_secs` (default: 30s). When all origins for a file are unavailable, cached data is served seamlessly.

---

## Virtual Filesystem Layout

### Path Templates

The virtual path for each file is built from its audio metadata using a configurable template. Variables are sanitized (no `/`, `\`, `:`).

**Default template:**
```
$artist/$album ($year) [$format_upper]/$track - $title.$format
```

**Template variables:**

| Variable | Description | Example |
|----------|-------------|---------|
| `$artist` | Track artist | `Metallica` |
| `$album` | Album name | `72 Seasons` |
| `$title` | Track title | `Lux Æterna` |
| `$track` | Track number (zero-padded) | `03` |
| `$disc` | Disc number | `1` |
| `$year` | Release year | `2023` |
| `$genre` | Genre | `Metal` |
| `$format` | File extension (lowercase) | `flac` |
| `$format_upper` | File extension (uppercase) | `FLAC` |

Files with missing metadata fall back to `Unknown Artist/Unknown Album/filename`.

### Album Art

Each album directory includes a virtual `cover.jpg` extracted from the embedded tags of the first track. No files are written to disk by MusicFS — the image is synthesized on read.

### Search

The `/.search/` virtual directory exposes full-text search as filesystem paths:

```bash
# Search via filesystem — use the query as a directory name
ls "/mnt/music/.search/dark side of the moon/"
# → Returns matching tracks as symlinks to their virtual paths

# Or use the CLI
musicfs search "dark side of the moon"
musicfs search "artist:Metallica" --limit 50
```

**Query syntax** (powered by [tantivy](https://github.com/quickwit-oss/tantivy)):

| Syntax | Example | Matches |
|--------|---------|---------|
| Simple terms | `metallica sandman` | All fields contain both words |
| Field-specific | `artist:Metallica` | Artist field only |
| Phrase | `album:"Master of Puppets"` | Exact phrase in album |
| Fuzzy | `metalica~1` | Within Levenshtein distance 1 |
| Range | `year:[1980 TO 1989]` | Numeric range |
| Boolean | `genre:Metal AND year:[1980 TO 1989]` | Combined conditions |

Indexed fields: `title`, `artist`, `album`, `album_artist`, `genre`, `composer`, `year`.
Results cached for 5 minutes. Max 1000 results per query. Queries capped at 256 characters.

### Smart Collections

Built-in and custom query-based virtual folders appear alongside regular directories:

- **Recently Added** — tracks added in the last 30 days
- **80s Music** — year 1980–1989
- **90s Music** — year 1990–1999

Custom collections can be defined via the gRPC API with compound boolean queries over any indexed field.

---

## Metadata Overlay

MusicFS lets you override metadata in the virtual layer **without touching origin files**. Overlaid metadata is synthesized into the audio file header on read — players see your corrected tags, the origin file is unchanged.

```bash
# Fix a misnamed artist
musicfs metadata set "/Unknown/Best Of/01 - Track.flac" \
  --artist "The Beatles" \
  --album "Past Masters"

# Verify
musicfs metadata get "/The Beatles/Past Masters/01 - Track.flac"

# See what's been overlaid vs. original
musicfs metadata diff "/The Beatles/Past Masters/01 - Track.flac"

# Revert
musicfs metadata clear "/The Beatles/Past Masters/01 - Track.flac"
```

Supported fields: `title`, `artist`, `album`, `album-artist`, `track`, `disc`, `genre`, `date`, `composer`, `comment`, `lyrics`, `copyright`, `compilation`, sort fields (`artist-sort`, etc.), MusicBrainz IDs, ReplayGain values, and arbitrary custom tags.

---

## Plugin Development

Plugins extend MusicFS without modifying core code. Three plugin types:

| Type | Purpose | Examples |
|------|---------|---------|
| **Origin** | Custom storage backends | Google Drive, Dropbox, custom NAS protocol |
| **Metadata** | External tag enrichment | MusicBrainz, Discogs, Last.fm |
| **Format** | Custom audio formats | Game audio, proprietary codecs |

### Native Plugin (`.so`)

```rust
// Cargo.toml
[lib]
crate-type = ["cdylib"]

[dependencies]
musicfs-plugins = { path = "..." }
semver = "1"
serde_json = "1"
```

```rust
use musicfs_plugins::{declare_plugin, Plugin, PluginType, FormatPlugin};
use musicfs_core::AudioMeta;
use semver::Version;
use serde_json::Value;

struct MyFormatPlugin;

impl Plugin for MyFormatPlugin {
    fn name(&self) -> &str { "my-format" }
    fn version(&self) -> Version { Version::new(1, 0, 0) }
    fn plugin_type(&self) -> PluginType { PluginType::Format }
    fn init(&mut self, _config: Value) -> musicfs_plugins::Result<()> { Ok(()) }
    fn shutdown(&mut self) -> musicfs_plugins::Result<()> { Ok(()) }
}

impl FormatPlugin for MyFormatPlugin {
    fn extensions(&self) -> &[&str] { &["xyz"] }

    fn parse(&self, reader: &mut dyn std::io::Read) -> musicfs_plugins::Result<AudioMeta> {
        // Parse your format and return metadata
        todo!()
    }

    fn synthesize_header(&self, metadata: &AudioMeta) -> musicfs_plugins::Result<Vec<u8>> {
        // Build a new file header with updated metadata
        todo!()
    }
}

// Required export — MusicFS calls this to instantiate the plugin
declare_plugin!(MyFormatPlugin, MyFormatPlugin);
```

```bash
cargo build --release
# produces target/release/libmy_format_plugin.so
```

### Loading Plugins

```toml
[plugins]
enabled = true
search_paths = ["/usr/lib/musicfs/plugins"]  # Auto-discover .so files here

[plugins.plugins.my-format]
path    = "/path/to/libmy_format_plugin.so"
enabled = true
config  = { key = "value" }  # Passed to Plugin::init()
```

### WASM Plugins (experimental)

```toml
[plugins.wasm]
enabled       = true
max_memory_mb = 64
max_cpu_time_ms = 5000
```

Load a `.wasm` binary at runtime via the gRPC API or by placing it in a search path. WASM plugins run sandboxed inside [wasmtime](https://wasmtime.dev/).

### Plugin API Version

Current: `0.1.0`. Breaking changes will increment the major version. MusicFS checks `musicfs_plugin_api_version()` before loading any native plugin.

---

## Control API (gRPC)

MusicFS exposes a gRPC API for programmatic control. The server starts automatically with the daemon.

**Default port:** `50052` (override with `--grpc-port`)
**Proto definition:** `crates/musicfs-grpc/proto/musicfs.proto`

### Available RPCs

```
MusicFS service:
  GetStatus          → daemon version, uptime, mount state, open handles
  Shutdown           → graceful or forced stop
  GetCacheStats      → hit rate, chunk count, dedup ratio, per-tier breakdown
  ClearCache         → clear all or per-origin, per-tier, dry-run supported
  Prefetch           → pre-warm cache for paths or search queries
  ListOrigins        → all configured origins with file count and health
  GetOriginHealth    → health status and latency for one origin
  RescanOrigin       → force re-scan with streaming progress
  Search             → full-text search (paginated or streaming)
  SubscribeEvents    → server-streaming live event feed

MetadataService:
  GetMetadata        → all tags for a virtual path
  UpdateMetadata     → set overlay tags for a file
  ClearOverlay       → revert to original metadata
  ImportMetadata     → bulk import from CSV/JSON (streaming progress)
```

### Query with `grpcurl`

```bash
# Daemon status
grpcurl -plaintext localhost:50052 musicfs.v1.MusicFS/GetStatus

# Search
grpcurl -plaintext -d '{"query": "metallica", "limit": 10}' \
  localhost:50052 musicfs.v1.MusicFS/Search

# Cache stats
grpcurl -plaintext localhost:50052 musicfs.v1.MusicFS/GetCacheStats

# List origins
grpcurl -plaintext localhost:50052 musicfs.v1.MusicFS/ListOrigins

# Trigger rescan with live progress
grpcurl -plaintext -d '{"origin_id": "local"}' \
  localhost:50052 musicfs.v1.MusicFS/RescanOrigin

# Live event stream
grpcurl -plaintext localhost:50052 musicfs.v1.MusicFS/SubscribeEvents
```

---

## Production Deployment

### systemd

```bash
sudo cp dist/musicfs.service /etc/systemd/system/

# Edit the service to match your paths:
# ExecStart=/usr/bin/musicfs mount --config /etc/musicfs/config.toml

sudo systemctl enable --now musicfs
sudo systemctl status musicfs
```

<!-- embedme dist/musicfs.service -->
```ini
[Unit]
Description=MusicFS - Virtual FUSE Filesystem for Music
After=network.target

[Service]
ExecStart=/usr/bin/musicfs mount /mnt/music --origin /path/to/music
ExecStopPost=/usr/bin/fusermount -u /mnt/music
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

MusicFS sends `sd_notify(READY)` when the mount is live and `sd_notify(STOPPING)` during shutdown. Use `Type=notify` for precise readiness tracking.

### Signals

| Signal | Behavior |
|--------|---------|
| `SIGTERM` | Graceful shutdown — drains in-flight ops, unmounts |
| `SIGINT` | Graceful shutdown (same) |
| `SIGHUP` | Process pending file restores from trash |

### Security Notes

- Run as an **unprivileged user** — no root required.
- Store remote credentials in the **system keyring** or environment variables. Never put them in the config file.
- Credentials are redacted from logs and `RUST_LOG` output.
- WASM plugins run sandboxed. Native `.so` plugins have full process access — only load plugins you trust.

---

## Observability

### Logs

```bash
# Set level at startup
musicfs mount ... --log-level debug
# or via env
RUST_LOG=musicfs=debug,warn musicfs mount ...
```

| Level | Content |
|-------|---------|
| `error` | Unrecoverable failures, data corruption |
| `warn` | Recoverable failures, origin timeouts, skipped files |
| `info` | Mount/unmount, sync completion, config reload |
| `debug` | Cache hits/misses, origin selection, file scans |
| `trace` | Individual FUSE operations, chunk I/O |

Log files rotate daily in `log_dir` (default: `/var/log/musicfs/`). Structured JSON available with `json_output = true`. On Linux, logs forward to journald by default (`journald = true`).

### Prometheus Metrics

Metrics are exposed in Prometheus format via the gRPC API:

```
musicfs_fuse_ops_total{op="read"}           152341
musicfs_fuse_ops_total{op="readdir"}        8234
musicfs_fuse_latency_seconds{op="read",quantile="0.99"}  0.004
musicfs_cache_hits_total                    142107
musicfs_cache_misses_total                  10234
musicfs_cache_size_bytes                    5368709120
musicfs_origin_health{origin="local"}       1
musicfs_origin_health{origin="s3"}          0
musicfs_sync_files_changed{origin="local"}  15
```

---

## Performance

| Operation | Target | Maximum |
|-----------|--------|---------|
| Mount (any library size) | <100ms | 500ms |
| `stat()` cached | <1ms | 5ms |
| `readdir()` cached | <10ms | 50ms |
| `open()` cached | <5ms | 20ms |
| `read()` cached | <1ms | 5ms |
| `read()` cache miss, local | <50ms | 200ms |
| `read()` cache miss, remote | <200ms | 1000ms |
| Search (1M tracks) | <500ms | 1000ms |
| Sequential read (cached) | >500 MB/s | — |
| Metadata ops | >1000 ops/s | — |

Memory: <50 MB idle, <200 MB with 1K files active, <500 MB peak.
Scales to 10M+ files with O(1) mount and O(log n) lookups.

---

## Known Limitations

These are tracked issues — see `docs/v2/plans/` for details.

| Issue | Impact | Workaround |
|-------|--------|-----------|
| **No persistent state on mount** | Every restart does a full origin scan (O(N)). SQLite/search index persist but are not loaded on startup. | — |
| **S3 and SFTP origins are stubs** | Only `local`, `nfs`, and `smb` have real implementations. | Use NFS/SMB mount as proxy for remote storage. |
| **No write-through for metadata** | Overlaid metadata exists only in MusicFS's database, not in the actual audio files. | Use a tagger (beets, mp3tag) to write back if needed. |
| **FUSE↔tokio deadlock risk** | `block_on()` in sync FUSE callbacks can stall under heavy concurrent load. | Keep concurrent open handles below ~500. |
| **No background task supervision** | Health monitor, watcher, and indexer are fire-and-forget. A crash silently stops background work. | Restart the daemon periodically in critical deployments. |

---

## Architecture

MusicFS is a workspace of 11 Rust crates:

```
musicfs-cli       → binary, CLI parsing, startup wiring
musicfs-fuse      → FUSE operations (fuser), virtual tree serving
musicfs-core      → shared types, config, events, errors
musicfs-cache     → SQLite metadata DB, virtual tree, format handlers
musicfs-cas       → content-addressable chunk store (sled + xxHash64)
musicfs-origins   → origin backends (local, NFS, SMB, S3 stub, SFTP stub)
musicfs-metadata  → audio tag extraction (symphonia)
musicfs-sync      → delta sync, CDC chunking (FastCDC), inotify watcher
musicfs-search    → full-text index (tantivy), .search/ virtual dir
musicfs-grpc      → gRPC server (tonic + prost), proto codegen
musicfs-plugins   → plugin host, native .so loader, WASM sandbox
```

Data flow on a cache miss: `FUSE read()` → `VirtualPathResolver` → `CAS` (chunk lookup) → `OriginFederation` (fetch missing range) → CDC chunk → store → return.

Full design: [`docs/v2/architecture.md`](docs/v2/architecture.md)
Requirements: [`docs/v2/requirements.md`](docs/v2/requirements.md)
Roadmap: [`docs/v2/development-plan.md`](docs/v2/development-plan.md)

---

## Development

```bash
nix develop                    # Enter dev shell

cargo check                    # Fast compile check
cargo test                     # All 162 tests
cargo test -p musicfs-core     # Single crate
cargo clippy                   # Lint
cargo fmt                      # Format
cargo nextest run               # Parallel test runner (faster)
cargo watch -x check -x test   # Watch mode

# Cargo aliases
cargo t   # test
cargo c   # check
cargo b   # build

# gRPC codegen (runs via build.rs automatically)
cargo build -p musicfs-grpc
```

Pre-commit hooks (rustfmt + clippy) are installed automatically in the Nix dev shell.

---

## License

MIT OR Apache-2.0 — see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
