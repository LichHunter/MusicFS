# beetfs - Reverse Engineered Documentation

> **Status**: Archived project (2010-2013), Python 2, fuse-python API
> **Fork**: git@github.com:LichHunter/beetfs.git
> **Original**: https://github.com/jbaiter/beetfs

## Overview

beetfs is a FUSE filesystem that presents audio files with **metadata from a database** while **passing through audio data unchanged** from original files. This enables transparent metadata modification without touching the underlying files.

### The Core Concept

```
┌─────────────────────────────────────────────────────────────────────┐
│                        APPLICATION (VLC, Jellyfin, etc.)            │
│                                                                     │
│                     read("/mount/Artist/Album/track.flac")          │
└─────────────────────────────────┬───────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────────┐
│                           beetfs (FUSE Layer)                        │
│  ┌────────────────────────────────────────────────────────────────┐ │
│  │                        FileHandler                              │ │
│  │  ┌──────────────────────────────────────────────────────────┐  │ │
│  │  │  if offset < header_boundary:                             │  │ │
│  │  │      return MODIFIED_HEADER (from beets database)         │  │ │
│  │  │  else:                                                    │  │ │
│  │  │      return ORIGINAL_AUDIO (from real file on disk)       │  │ │
│  │  └──────────────────────────────────────────────────────────┘  │ │
│  └────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────┘
                    │                               │
        ┌───────────┘                               └───────────┐
        ▼                                                       ▼
┌───────────────────┐                               ┌───────────────────┐
│   Beets Database  │                               │   Original File   │
│  (SQLite - tags)  │                               │   (untouched)     │
│                   │                               │                   │
│  title: "Fixed"   │                               │  [FLAC header]    │
│  artist: "Corr"   │                               │  [Audio frames]   │
│  album: "Right"   │                               │                   │
└───────────────────┘                               └───────────────────┘
```

## Key Features

| Feature | Description |
|---------|-------------|
| **Metadata Overlay** | Returns tags from database, not from file |
| **Audio Passthrough** | Original audio data served unchanged |
| **Write Interception** | Tag edits saved to database, not to file |
| **Virtual Organization** | Presents files in template-based directory structure |
| **Format Support** | FLAC (full), MP3 (partial - read-only) |

## File Structure

```
beetfs/
├── beetsplug/
│   ├── __init__.py      # Package initialization
│   └── beetFs.py        # ALL code (~1144 lines)
├── README.rst           # Original readme
└── COPYING              # GPLv3 license
```

## Quick Architecture Summary

| Component | Lines | Purpose |
|-----------|-------|---------|
| `beetFs` (plugin) | 188-191 | Beets plugin hook |
| `mount()` | 119-183 | CLI entry point, builds virtual tree |
| `FSNode` | 390-436 | Virtual directory tree node |
| `FileHandler` | 439-565 | **CORE**: Metadata interpolation |
| `InterpolatedFLAC` | 274-388 | FLAC header generation |
| `InterpolatedID3` | 200-271 | ID3 tag generation (incomplete) |
| `beetFileSystem` | 622-1144 | FUSE operations implementation |
| `Stat` | 568-619 | File stat structure |

## Documentation Index

1. **[Architecture Overview](./architecture.md)** - System design and component interaction
2. **[Components Deep Dive](./components.md)** - Detailed component analysis
3. **[Data Flow](./data-flow.md)** - Read/write operation flows
4. **[Performance Analysis](./analysis.md)** - Latency, memory footprint, I/O patterns
5. **[Drawbacks & Limitations](./drawbacks.md)** - Known issues and missing features
6. **[Modernization Guide](./modernization.md)** - Notes for updating to Python 3

## Critical Issues Summary

| Issue | Severity | Impact |
|-------|----------|--------|
| Full file loaded into RAM | 🔴 Critical | OOM on large libraries |
| MP3 support disabled | 🔴 Critical | Only FLAC works |
| Python 2 only | 🔴 Critical | EOL, security risk |
| Single-threaded | 🟡 Major | Poor concurrency |
| 4 of 17 metadata fields | 🟡 Major | Limited functionality |

See [drawbacks.md](./drawbacks.md) for complete list (27 identified issues).

## Dependencies (Original)

```
beets >= 1.0
fuse-python (Python 2 FUSE bindings)
mutagen (audio metadata library)
```

## Usage (Original)

```bash
# As beets plugin
beet mount /path/to/mountpoint
```

## License

GPLv3 - See COPYING file
