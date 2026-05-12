# Music Library FUSE Filesystem - Requirements Specification

**Version**: 1.0  
**Date**: 2026-05-12  
**Status**: Draft

## 1. Introduction

### 1.1 Purpose

This document specifies the requirements for a FUSE-based virtual filesystem that presents a music library organized by metadata. The system overlays metadata onto audio files without modifying originals and operates as a read-only client against the origin storage.

### 1.2 Scope

The system provides:
- Virtual filesystem accessible via standard POSIX operations
- Metadata-based directory structure (artist/album/track)
- Local caching with delta synchronization
- Support for local and remote origin storage

### 1.3 Definitions

| Term | Definition |
|------|------------|
| **Origin** | The source storage containing original audio files (local FS, NFS, S3, etc.) |
| **Virtual path** | The metadata-derived path shown to users (e.g., `/Artist/Album/Track.flac`) |
| **Real path** | The actual path on origin storage |
| **Metadata overlay** | Serving synthesized file headers from cached metadata |
| **CDC** | Content-Defined Chunking - algorithm for stable file segmentation |

---

## 2. System Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                      User Applications                           │
│                (mpv, Rhythmbox, Plex, etc.)                     │
└─────────────────────────────┬───────────────────────────────────┘
                              │ POSIX (read-only)
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                        FUSE Interface                            │
├─────────────────────────────────────────────────────────────────┤
│                         Plugin Host                              │
│    ┌─────────────┐  ┌─────────────┐  ┌─────────────┐           │
│    │   Origin    │  │  Metadata   │  │   Format    │           │
│    │   Plugins   │  │   Plugins   │  │   Plugins   │           │
│    └─────────────┘  └─────────────┘  └─────────────┘           │
├─────────────────────────────────────────────────────────────────┤
│                        Core Services                             │
│  ┌───────────┐ ┌───────────┐ ┌───────────┐ ┌───────────┐       │
│  │  Virtual  │ │   Event   │ │  Search   │ │  Control  │       │
│  │   Path    │ │    Bus    │ │   Index   │ │    API    │       │
│  │  Resolver │ │           │ │           │ │           │       │
│  └───────────┘ └───────────┘ └───────────┘ └───────────┘       │
├─────────────────────────────────────────────────────────────────┤
│                        Storage Layer                             │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │           Content-Addressable Chunk Store                │   │
│  │    ┌──────────┐  ┌──────────┐  ┌──────────┐             │   │
│  │    │ Metadata │  │  Content │  │   Tree   │             │   │
│  │    │  Cache   │  │  Chunks  │  │  Cache   │             │   │
│  │    │ (SQLite) │  │  (CAS)   │  │          │             │   │
│  │    └──────────┘  └──────────┘  └──────────┘             │   │
│  └─────────────────────────────────────────────────────────┘   │
├─────────────────────────────────────────────────────────────────┤
│                     Origin Federation                            │
│       ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐          │
│       │ Local   │ │   NFS   │ │   S3    │ │  SFTP   │          │
│       │   FS    │ │         │ │         │ │         │          │
│       └─────────┘ └─────────┘ └─────────┘ └─────────┘          │
└─────────────────────────────────────────────────────────────────┘
                              │ read-only
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                       Origin Storage(s)                          │
│                    (original audio files)                        │
└─────────────────────────────────────────────────────────────────┘
```

---

## 3. Functional Requirements

### 3.1 Filesystem Operations

#### FR-1: Mount/Unmount

| ID | Requirement |
|----|-------------|
| FR-1.1 | The system SHALL mount as a FUSE filesystem at a user-specified mountpoint |
| FR-1.2 | The system SHALL return control to the caller within 500ms of mount initiation |
| FR-1.3 | The system SHALL unmount cleanly via `fusermount -u` |
| FR-1.4 | The system SHALL release all resources (file handles, connections) on unmount |

#### FR-2: Directory Operations

| ID | Requirement |
|----|-------------|
| FR-2.1 | The system SHALL present files organized by metadata path format |
| FR-2.2 | The system SHALL support configurable path templates (e.g., `$artist/$album/$track - $title.$format`) |
| FR-2.3 | The system SHALL return directory listings via `readdir()` |
| FR-2.4 | The system SHALL support nested directory traversal to arbitrary depth |
| FR-2.5 | The system SHALL handle directories with 100,000+ entries |

#### FR-3: File Operations (Read)

| ID | Requirement |
|----|-------------|
| FR-3.1 | The system SHALL support `open()` for reading |
| FR-3.2 | The system SHALL support `read()` with arbitrary offset and size |
| FR-3.3 | The system SHALL support `seek()` operations for random access |
| FR-3.4 | The system SHALL return file attributes via `stat()` / `fstat()` |
| FR-3.5 | The system SHALL support concurrent reads from multiple processes |

#### FR-4: Read-Only Constraint

| ID | Requirement |
|----|-------------|
| FR-4.1 | The system SHALL NOT modify original files on the origin storage |
| FR-4.2 | The system SHALL NOT push any changes to the origin server |
| FR-4.3 | The system SHALL return `EROFS` (Read-only filesystem) for write operations |
| FR-4.4 | The system SHALL return `EROFS` for `create()`, `mkdir()`, `unlink()`, `rmdir()` |
| FR-4.5 | The system SHALL return `EROFS` for `rename()`, `chmod()`, `chown()`, `truncate()` |

### 3.2 Metadata Handling

#### FR-5: Metadata Overlay

| ID | Requirement |
|----|-------------|
| FR-5.1 | The system SHALL extract metadata from audio files on first access |
| FR-5.2 | The system SHALL cache extracted metadata in a local database |
| FR-5.3 | The system SHALL serve file headers with metadata from cache |
| FR-5.4 | The system SHALL support FLAC Vorbis comments |
| FR-5.5 | The system SHALL support MP3 ID3v2 tags |
| FR-5.6 | The system SHOULD support additional formats (OGG, M4A, OPUS) |

#### FR-6: Metadata Fields

| ID | Requirement |
|----|-------------|
| FR-6.1 | The system SHALL extract and cache: title, artist, album, genre |
| FR-6.2 | The system SHALL extract and cache: year, track number, disc number |
| FR-6.3 | The system SHALL extract and cache: duration, bitrate, sample rate |
| FR-6.4 | The system SHOULD extract: composer, album artist, lyrics |
| FR-6.5 | The system SHALL handle missing metadata gracefully with defaults |

### 3.3 Caching

#### FR-7: Metadata Cache

| ID | Requirement |
|----|-------------|
| FR-7.1 | The system SHALL persist metadata cache across restarts |
| FR-7.2 | The system SHALL store metadata in SQLite database |
| FR-7.3 | The system SHALL index by both virtual path and real path |
| FR-7.4 | The system SHALL invalidate cache entries when origin file changes |

#### FR-8: Content Cache

| ID | Requirement |
|----|-------------|
| FR-8.1 | The system SHALL cache file content in fixed-size chunks |
| FR-8.2 | The system SHALL use content-defined chunking for cache efficiency |
| FR-8.3 | The system SHALL store chunk hashes for delta detection |
| FR-8.4 | The system SHALL evict chunks under memory/disk pressure |

#### FR-9: Directory Tree Cache

| ID | Requirement |
|----|-------------|
| FR-9.1 | The system SHALL cache directory listings locally |
| FR-9.2 | The system SHALL serve `readdir()` from cache without origin access |
| FR-9.3 | The system SHALL refresh tree cache based on configurable policy |
| FR-9.4 | The system SHALL support forced refresh via signal or special file |

### 3.4 Synchronization

#### FR-10: Change Detection

| ID | Requirement |
|----|-------------|
| FR-10.1 | The system SHALL detect changes to origin files |
| FR-10.2 | The system SHALL use inotify for local filesystem origins |
| FR-10.3 | The system SHALL use polling for remote origins without push support |
| FR-10.4 | The system SHALL compare mtime and size for change detection |
| FR-10.5 | The system SHALL support content-hash verification on demand |

#### FR-11: Delta Sync

| ID | Requirement |
|----|-------------|
| FR-11.1 | The system SHALL download only changed portions of files |
| FR-11.2 | The system SHALL use CDC to identify changed chunks |
| FR-11.3 | The system SHALL preserve unchanged chunks in cache |
| FR-11.4 | The system SHALL handle file additions and deletions |

### 3.5 Origin Support

#### FR-12: Origin Types

| ID | Requirement |
|----|-------------|
| FR-12.1 | The system SHALL support local filesystem as origin |
| FR-12.2 | The system SHOULD support NFS mounted filesystems |
| FR-12.3 | The system SHOULD support SMB/CIFS shares |
| FR-12.4 | The system SHOULD support S3-compatible object storage |
| FR-12.5 | The system SHOULD support SFTP servers |
| FR-12.6 | The system SHALL provide pluggable origin interface |

#### FR-13: Multiple Origins [P0]

| ID | Requirement |
|----|-------------|
| FR-13.1 | The system SHALL support multiple simultaneous origins |
| FR-13.2 | The system SHALL present unified virtual tree across origins |
| FR-13.3 | The system SHALL support origin priority/preference ordering |
| FR-13.4 | The system SHALL handle duplicate files across origins |
| FR-13.5 | The system SHALL support per-origin configuration |

### 3.6 Search & Discovery

#### FR-14: Full-Text Search [P1]

| ID | Requirement |
|----|-------------|
| FR-14.1 | The system SHALL index metadata for full-text search |
| FR-14.2 | The system SHALL expose search via virtual directory (`/.search/query/`) |
| FR-14.3 | The system SHALL support fuzzy matching |
| FR-14.4 | The system SHOULD support search by audio fingerprint |

#### FR-15: Smart Collections [P1]

| ID | Requirement |
|----|-------------|
| FR-15.1 | The system SHALL support query-based virtual folders |
| FR-15.2 | The system SHALL support saved searches as directories |
| FR-15.3 | The system SHALL support dynamic playlists (recently played, most played) |
| FR-15.4 | The system SHOULD support user-defined metadata fields |

### 3.7 Album Art

#### FR-16: Cover Art Handling [P1]

| ID | Requirement |
|----|-------------|
| FR-16.1 | The system SHALL extract embedded album art |
| FR-16.2 | The system SHALL expose art as virtual files (`/Artist/Album/cover.jpg`) |
| FR-16.3 | The system SHALL cache artwork separately from audio |
| FR-16.4 | The system SHALL support multiple art sizes (thumbnail, medium, full) |
| FR-16.5 | The system SHOULD fetch missing art from online sources |

### 3.8 Control & API

#### FR-17: Control Interface [P0]

| ID | Requirement |
|----|-------------|
| FR-17.1 | The system SHALL expose control via Unix socket (gRPC) |
| FR-17.2 | The system SHALL use gRPC with Protocol Buffers for all control APIs |
| FR-17.3 | The system SHALL support cache management commands (clear, refresh, stats) |
| FR-17.4 | The system SHALL support runtime configuration changes |
| FR-17.5 | The system SHALL support graceful shutdown with drain |

#### FR-18: Event System [P0]

| ID | Requirement |
|----|-------------|
| FR-18.1 | The system SHALL emit events for file access |
| FR-18.2 | The system SHALL support webhook notifications |
| FR-18.3 | The system SHOULD support event streaming (SSE/WebSocket) |
| FR-18.4 | The system SHALL log access patterns for analysis |

### 3.9 Caching Enhancements

#### FR-19: Intelligent Prefetching [P1]

| ID | Requirement |
|----|-------------|
| FR-19.1 | The system SHALL learn access patterns |
| FR-19.2 | The system SHALL support playlist-aware prefetching |
| FR-19.3 | The system SHOULD support time-based prefetching |
| FR-19.4 | The system SHALL support manual prefetch hints (`/.prefetch/path/`) |

#### FR-20: Content-Addressable Storage [P0]

| ID | Requirement |
|----|-------------|
| FR-20.1 | The system SHALL store chunks by content hash |
| FR-20.2 | The system SHALL detect identical files across library |
| FR-20.3 | The system SHALL report deduplication statistics |
| FR-20.4 | The system SHALL enable cache sharing via content addressing |

### 3.10 Integration

#### FR-21: Metadata Sources [P1]

| ID | Requirement |
|----|-------------|
| FR-21.1 | The system SHOULD integrate with MusicBrainz |
| FR-21.2 | The system SHOULD integrate with Discogs |
| FR-21.3 | The system SHOULD integrate with Last.fm |
| FR-21.4 | The system SHOULD support AcoustID fingerprinting |
| FR-21.5 | The system SHALL support custom metadata plugins |

#### FR-22: Import & Migration [P1]

| ID | Requirement |
|----|-------------|
| FR-22.1 | The system SHALL import from beets database |
| FR-22.2 | The system SHOULD import from iTunes/Apple Music library |
| FR-22.3 | The system SHALL export library metadata |

### 3.11 Extensibility

#### FR-23: Plugin System [P0]

| ID | Requirement |
|----|-------------|
| FR-23.1 | The system SHALL support loadable plugins |
| FR-23.2 | The system SHALL define stable plugin API |
| FR-23.3 | The system SHALL support plugins for: origins, metadata extractors, formats |
| FR-23.4 | The system SHOULD support WASM plugins for sandboxed execution |
| FR-23.5 | The system SHALL provide plugin lifecycle management (load, unload, reload) |

#### FR-24: Format Extensibility [P1]

| ID | Requirement |
|----|-------------|
| FR-24.1 | The system SHALL support pluggable codec modules |
| FR-24.2 | The system SHOULD support audiobook formats (M4B, chapters) |
| FR-24.3 | The system SHALL allow format plugins to register file extensions |

### 3.12 High Availability [P3]

#### FR-25: Resilience

| ID | Requirement |
|----|-------------|
| FR-25.1 | The system SHOULD support active-passive failover |
| FR-25.2 | The system SHOULD support read replicas |
| FR-25.3 | The system SHALL support zero-downtime upgrades |
| FR-25.4 | The system SHALL support cache backup/restore |
| FR-25.5 | The system SHALL validate cache integrity on startup |

---

## 4. Non-Functional Requirements

### 4.1 Performance

#### NFR-1: Latency

| ID | Requirement | Target | Maximum |
|----|-------------|--------|---------|
| NFR-1.1 | `stat()` on cached file | <1ms | 5ms |
| NFR-1.2 | `readdir()` on cached directory | <10ms | 50ms |
| NFR-1.3 | `open()` on cached file | <5ms | 20ms |
| NFR-1.4 | `read()` from cache | <1ms | 5ms |
| NFR-1.5 | `read()` cache miss (local origin) | <50ms | 200ms |
| NFR-1.6 | `read()` cache miss (remote origin) | <200ms | 1000ms |
| NFR-1.7 | Mount completion | <100ms | 500ms |

#### NFR-2: Throughput

| ID | Requirement | Target |
|----|-------------|--------|
| NFR-2.1 | Sequential read throughput (cached) | >500 MB/s |
| NFR-2.2 | Sequential read throughput (local origin) | >200 MB/s |
| NFR-2.3 | Metadata operations per second | >1000 ops/s |
| NFR-2.4 | Concurrent file handles | >1000 |

#### NFR-3: Scalability

| ID | Requirement |
|----|-------------|
| NFR-3.1 | The system SHALL handle libraries with 1,000,000+ files |
| NFR-3.2 | The system SHALL handle directories with 100,000+ entries |
| NFR-3.3 | The system SHALL maintain O(1) mount time regardless of library size |
| NFR-3.4 | The system SHALL maintain O(log n) lookup time for paths |
| NFR-3.5 | The system SHOULD handle libraries with 10,000,000+ files [P3] |
| NFR-3.6 | The system SHOULD support 100+ concurrent clients [P3] |
| NFR-3.7 | The system SHOULD achieve <100μs cached stat for high-performance use [P3] |

### 4.2 Resource Usage

#### NFR-4: Memory

| ID | Requirement | Limit |
|----|-------------|-------|
| NFR-4.1 | Idle memory usage | <50 MB |
| NFR-4.2 | Active usage (1000 files accessed) | <200 MB |
| NFR-4.3 | Peak usage under load | <500 MB |
| NFR-4.4 | Per-file metadata overhead | <1 KB |
| NFR-4.5 | The system SHALL NOT load entire files into memory |

#### NFR-5: Disk

| ID | Requirement |
|----|-------------|
| NFR-5.1 | Metadata cache size SHALL be configurable (default: 100 MB) |
| NFR-5.2 | Content cache size SHALL be configurable (default: 10 GB) |
| NFR-5.3 | The system SHALL evict cache entries under disk pressure |
| NFR-5.4 | The system SHALL function with cache disabled (passthrough mode) |

#### NFR-6: Network

| ID | Requirement |
|----|-------------|
| NFR-6.1 | The system SHALL minimize network round-trips via batching |
| NFR-6.2 | The system SHALL use connection pooling for remote origins |
| NFR-6.3 | The system SHALL support bandwidth limiting (configurable) |
| NFR-6.4 | Delta sync SHALL achieve >90% bandwidth reduction vs full copy |

### 4.3 Reliability

#### NFR-7: Availability

| ID | Requirement |
|----|-------------|
| NFR-7.1 | The system SHALL serve cached data when origin is unavailable |
| NFR-7.2 | The system SHALL gracefully degrade with network failures |
| NFR-7.3 | The system SHALL retry failed operations with exponential backoff |
| NFR-7.4 | The system SHALL not crash on malformed audio files |

#### NFR-8: Data Integrity

| ID | Requirement |
|----|-------------|
| NFR-8.1 | The system SHALL verify chunk integrity via checksums |
| NFR-8.2 | The system SHALL use ACID transactions for cache database |
| NFR-8.3 | The system SHALL recover from interrupted synchronization |
| NFR-8.4 | The system SHALL detect and report cache corruption |

### 4.4 Usability

#### NFR-9: Configuration

| ID | Requirement |
|----|-------------|
| NFR-9.1 | The system SHALL support configuration via file (TOML/YAML) |
| NFR-9.2 | The system SHALL support configuration via command-line arguments |
| NFR-9.3 | The system SHALL support configuration via environment variables |
| NFR-9.4 | The system SHALL provide sensible defaults for all options |

#### NFR-10: Observability

| ID | Requirement |
|----|-------------|
| NFR-10.1 | The system SHALL log operations at configurable verbosity |
| NFR-10.2 | The system SHALL expose metrics (cache hit rate, latency, etc.) |
| NFR-10.3 | The system SHALL support health check endpoint/signal |
| NFR-10.4 | The system SHOULD support integration with Prometheus/StatsD |

### 4.5 Compatibility

#### NFR-11: Platform Support

| ID | Requirement |
|----|-------------|
| NFR-11.1 | The system SHALL run on Linux (kernel 4.x+) |
| NFR-11.2 | The system SHOULD run on macOS (via macFUSE) |
| NFR-11.3 | The system SHALL require FUSE kernel module |
| NFR-11.4 | The system SHALL run without root privileges (user-space FUSE) |

#### NFR-12: Application Compatibility

| ID | Requirement |
|----|-------------|
| NFR-12.1 | The system SHALL work with standard media players (mpv, VLC, etc.) |
| NFR-12.2 | The system SHALL work with media servers (Plex, Jellyfin) |
| NFR-12.3 | The system SHALL work with file managers (Nautilus, Dolphin) |
| NFR-12.4 | The system SHALL correctly report file sizes and timestamps |

### 4.6 Security

#### NFR-13: Access Control

| ID | Requirement |
|----|-------------|
| NFR-13.1 | The system SHALL respect origin file permissions |
| NFR-13.2 | The system SHALL run as unprivileged user |
| NFR-13.3 | The system SHALL support credential storage for remote origins |
| NFR-13.4 | The system SHALL NOT expose credentials in logs or process list |

### 4.7 Maintainability

#### NFR-14: Code Quality

| ID | Requirement |
|----|-------------|
| NFR-14.1 | The system SHALL be implemented in a memory-safe language |
| NFR-14.2 | The system SHALL have no global interpreter lock (no Python/Ruby) |
| NFR-14.3 | The system SHALL use async I/O for concurrent operations |
| NFR-14.4 | The system SHALL have modular architecture with pluggable components |

---

## 5. Constraints

### 5.1 Technical Constraints

| ID | Constraint |
|----|------------|
| C-1 | Must use FUSE for filesystem interface |
| C-2 | Must not require kernel module development |
| C-3 | Must work with existing audio file formats (no transcoding) |
| C-4 | Cache database must be portable (no external database server) |

### 5.2 Operational Constraints

| ID | Constraint |
|----|------------|
| C-5 | Client is read-only; no writes propagate to origin |
| C-6 | Must function offline with cached data |
| C-7 | Must not corrupt origin files under any circumstances |

---

## 6. Assumptions

| ID | Assumption |
|----|------------|
| A-1 | Origin storage is accessible via supported protocol |
| A-2 | Audio files contain valid metadata headers |
| A-3 | Sufficient local disk space for caching is available |
| A-4 | FUSE kernel module is installed and accessible |
| A-5 | Network connectivity is intermittent but generally available |

---

## 7. Dependencies

| ID | Dependency | Purpose |
|----|------------|---------|
| D-1 | FUSE library (fuser/libfuse) | Filesystem interface |
| D-2 | SQLite | Metadata and tree cache |
| D-3 | Audio parsing library (symphonia) | Metadata extraction |
| D-4 | Async runtime (tokio) | Concurrent I/O |
| D-5 | CDC library (fastcdc) | Content chunking |
| D-6 | Full-text search (tantivy) | Search index [P1] |
| D-7 | Image processing (image) | Album art thumbnails [P1] |
| D-8 | HTTP client (reqwest) | Remote origins, metadata APIs |
| D-9 | WASM runtime (wasmtime) | Plugin sandboxing [P0] |
| D-10 | Hash library (xxhash/blake3) | Content addressing [P0] |

---

## 8. Acceptance Criteria

### 8.1 Functional Acceptance

| ID | Criterion |
|----|-----------|
| AC-1 | Mount filesystem and browse directories via `ls` |
| AC-2 | Play audio file through mounted filesystem with media player |
| AC-3 | Seek within audio file without full download |
| AC-4 | Directory listing completes without network access (when cached) |
| AC-5 | Confirm write operations return EROFS |
| AC-6 | Detect and sync changes from origin within configured interval |

### 8.2 Performance Acceptance

| ID | Criterion |
|----|-----------|
| AC-7 | Mount completes in <500ms for library of any size |
| AC-8 | Cached stat() completes in <5ms (p99) |
| AC-9 | Memory stays under 500MB with 10,000 files accessed |
| AC-10 | Tag-only change syncs <10KB of data |

### 8.3 Reliability Acceptance

| ID | Criterion |
|----|-----------|
| AC-11 | Filesystem remains accessible when origin is offline |
| AC-12 | No data corruption after unclean unmount |
| AC-13 | Recovers automatically when origin comes back online |

### 8.4 Multi-Origin Acceptance [P0]

| ID | Criterion |
|----|-----------|
| AC-14 | Configure and mount multiple origins simultaneously |
| AC-15 | Browse unified tree showing content from all origins |
| AC-16 | Access same file from preferred origin when duplicated |

### 8.5 Search & Discovery Acceptance [P1]

| ID | Criterion |
|----|-----------|
| AC-17 | Search for tracks by partial artist/album/title match |
| AC-18 | Browse smart collection (e.g., "Jazz from 1960s") |
| AC-19 | View album art via virtual cover.jpg file |

### 8.6 Plugin Acceptance [P0]

| ID | Criterion |
|----|-----------|
| AC-20 | Load custom origin plugin at runtime |
| AC-21 | Control daemon via Unix socket (cache stats, refresh) |
| AC-22 | Receive webhook on file access event |

### 8.7 Deduplication Acceptance [P0]

| ID | Criterion |
|----|-----------|
| AC-23 | Identical chunks stored once regardless of file count |
| AC-24 | Deduplication stats visible via control API |

---

## 9. Appendix

### 9.1 Comparison with beetfs

| Requirement Area | beetfs | This Specification |
|------------------|--------|-------------------|
| Mount time | O(N), 5-120s | O(1), <500ms (NFR-1.7) |
| Memory per file | Full file size | <1KB (NFR-4.4) |
| Write to origin | Yes (DB updates) | No (FR-4.1, FR-4.2) |
| Delta sync | None | Required (FR-11) |
| Remote origins | None | Required (FR-12) |
| Offline access | No | Required (NFR-7.1) |
| Cache persistence | No | Required (FR-7.1) |

### 9.2 Path Template Variables

| Variable | Description | Example |
|----------|-------------|---------|
| `$artist` | Track artist | "Metallica" |
| `$album` | Album name | "72 Seasons" |
| `$title` | Track title | "Lux Æterna" |
| `$track` | Track number (zero-padded) | "03" |
| `$disc` | Disc number | "1" |
| `$year` | Release year | "2023" |
| `$genre` | Genre | "Metal" |
| `$format` | File extension | "flac" |
| `$format_upper` | File extension (uppercase) | "FLAC" |

### 9.3 Error Codes

| Operation | Error | Code |
|-----------|-------|------|
| Any write operation | Read-only filesystem | EROFS (30) |
| File not found | No such file | ENOENT (2) |
| Origin unavailable | I/O error | EIO (5) |
| Permission denied | Access denied | EACCES (13) |
