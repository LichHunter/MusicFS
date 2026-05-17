**Date**: 2026-05-17
**Status**: Shipped

# Feature: Create Directory (mkdir)

## Overview

MusicFS supports creating directories in the virtual filesystem. This enables organizing files into custom folder structures beyond the auto-generated metadata-based layout.

## Behavior

### Basic Usage

```bash
mkdir "/mnt/music/New Artist"
mkdir "/mnt/music/New Artist/New Album"
```

- Creates empty directory at specified path
- Parent directory must exist
- Standard POSIX semantics

### Nested Directories

```bash
# This works (shell handles -p)
mkdir -p "/mnt/music/A/B/C"

# Equivalent to:
mkdir "/mnt/music/A"
mkdir "/mnt/music/A/B"
mkdir "/mnt/music/A/B/C"
```

The `-p` flag is handled by the shell, which makes multiple `mkdir` syscalls.

### Brace Expansion

```bash
# Shell expands this to multiple mkdir calls
mkdir "/mnt/music/Artist/{Album1,Album2,Album3}"

# Equivalent to:
mkdir "/mnt/music/Artist/Album1"
mkdir "/mnt/music/Artist/Album2"
mkdir "/mnt/music/Artist/Album3"
```

Brace expansion is shell functionality, not filesystem.

## Error Codes

| Condition | Error |
|-----------|-------|
| Parent doesn't exist | `ENOENT` |
| Path already exists | `EEXIST` |

## Persistence

**Empty directories persist across remounts.**

- User-created directories are stored in the `directories` table
- On mount, directories are restored from database
- Directories survive even when empty

## Use Cases

### Organizing Downloads

```bash
# Create structure
mkdir "/mnt/music/Unsorted"
mkdir "/mnt/music/Unsorted/2026"

# Move untagged files
mv "/mnt/music/Unknown Artist/Unknown Album/"*.flac "/mnt/music/Unsorted/2026/"
```

### Custom Collections

```bash
# Create playlist-like structure
mkdir "/mnt/music/_Playlists"
mkdir "/mnt/music/_Playlists/Road Trip"

# Move tracks (they'll still be in original location too - wait, no they won't)
# Note: mv moves, doesn't copy
```

## Implementation

| Component | File |
|-----------|------|
| Tree | `crates/musicfs-cache/src/tree.rs` |
| FUSE | `crates/musicfs-fuse/src/filesystem.rs` |

### Key Functions

- `VirtualTree::mkdir()` - Create directory node in tree
- `Filesystem::mkdir()` - FUSE operation handler

## Limitations

- **No permissions**: Mode/umask parameters are ignored (always 0755)
- **No ownership**: UID/GID set to mounting user
