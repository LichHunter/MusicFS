# beetfs Feature Set

## Overview

beetfs is a FUSE filesystem plugin for [beets](https://beets.io/) that presents your music library as a virtual filesystem organized by metadata. Files appear with paths derived from their database metadata, and reading file headers returns metadata from the beets database rather than the actual file tags.

**Author**: Martin Eve (2010)  
**License**: GPLv3  
**Python**: 2.7 (uses fuse-python)

## Core Features

### 1. Virtual Metadata-Based Directory Structure

Files are presented in a configurable path format based on beets database fields:

```
$artist/$album ($year) [$format_upper]/$track - $artist - $title.$format
```

**Example**:
```
/mnt/beetfs/
├── Metallica/
│   └── 72 Seasons (2023) [FLAC]/
│       ├── 01 - Metallica - 72 Seasons.flac
│       ├── 02 - Metallica - Shadows Follow.flac
│       └── ...
├── Pink Floyd/
│   └── The Dark Side of the Moon (1973) [FLAC]/
│       └── ...
```

**Available template variables**:
- `$artist`, `$album`, `$title`, `$genre`, `$composer`, `$grouping`
- `$year`, `$month`, `$day`
- `$track`, `$tracktotal`, `$disc`, `$disctotal`
- `$format`, `$format_upper` (file extension)
- `$lyrics`, `$comments`, `$bpm`, `$comp`

### 2. Metadata Overlay (Read)

When you read a file through beetfs, the **metadata header is synthesized from the beets database**, not read from the actual file on disk.

**How it works**:
1. Open file → beetfs reads the real file from disk
2. Parse the audio format header (FLAC/MP3)
3. Replace metadata fields with values from beets database
4. Return synthesized header + original audio data

**Supported fields for overlay**:
- `title`, `artist`, `album`, `genre` (FLAC only currently)

**Use case**: Your files may have inconsistent or wrong tags, but beetfs presents them with the corrected metadata from your beets library.

### 3. Metadata Passthrough (Write)

When you write to file headers through beetfs, the **changes are saved to the beets database**, not to the actual file.

**How it works**:
1. Application writes new metadata to file header region
2. beetfs intercepts the write
3. Parses the new metadata values
4. Updates the beets database (`lib.store()`, `lib.save()`)
5. Regenerates the synthesized header

**Result**: Tag editors (Picard, Kid3, etc.) can edit metadata through beetfs, and changes persist in the beets database without modifying the original files.

### 4. Format Support

| Format | Read | Metadata Overlay | Write to DB |
|--------|------|------------------|-------------|
| FLAC | ✅ | ✅ Full | ✅ |
| MP3 | ✅ | ❌ Disabled | ❌ |
| Other | ❌ | ❌ | ❌ |

**FLAC Implementation**:
- Uses `InterpolatedFLAC` class extending mutagen
- Reconstructs Vorbis comment block with DB values
- Preserves audio data and other metadata blocks

**MP3 Implementation**:
- Passthrough only (no interpolation)
- `self.bound = 0` disables header replacement

### 5. File Caching

Open files are cached in `FileHandler` objects:

- First open: Load entire file into memory, parse headers
- Subsequent opens: Reuse cached `FileHandler`
- Reference counting for multiple opens
- Release when reference count reaches zero

**Memory impact**: Each open file consumes ~filesize RAM.

## FUSE Operations

### Implemented (Functional)

| Operation | Description |
|-----------|-------------|
| `getattr` | File/directory stat (size, mode, timestamps) |
| `access` | Permission checking |
| `opendir` | Open directory for listing |
| `readdir` | List directory contents |
| `releasedir` | Close directory |
| `open` | Open file for reading/writing |
| `read` | Read file contents |
| `write` | Write to file (header region only) |
| `release` | Close file |
| `fgetattr` | Stat with file handle |
| `statfs` | Filesystem statistics |

### Not Implemented (Return EOPNOTSUPP)

| Operation | Reason |
|-----------|--------|
| `create` | Read-only structure |
| `mknod` | Read-only structure |
| `mkdir` | Read-only structure |
| `unlink` | Read-only structure |
| `rmdir` | Read-only structure |
| `symlink` | Not needed |
| `link` | Not needed |
| `rename` | Would break DB consistency |
| `chmod` | Metadata-only FS |
| `chown` | Metadata-only FS |
| `truncate` | Would corrupt audio |
| `utime` | Metadata-only FS |

## Usage

### Mount

```bash
beet mount /mnt/beetfs
```

### Unmount

```bash
fusermount -u /mnt/beetfs
```

### Example Session

```bash
# Mount the filesystem
beet mount /mnt/music

# Browse by artist
ls /mnt/music/
# Metallica/  Pink Floyd/  The Beatles/  ...

# List an album
ls "/mnt/music/Metallica/72 Seasons (2023) [FLAC]/"
# 01 - Metallica - 72 Seasons.flac
# 02 - Metallica - Shadows Follow.flac
# ...

# Play through any music player
mpv "/mnt/music/Metallica/72 Seasons (2023) [FLAC]/01 - Metallica - 72 Seasons.flac"

# Edit tags (changes go to beets DB)
kid3 "/mnt/music/Metallica/72 Seasons (2023) [FLAC]/"

# Unmount
fusermount -u /mnt/music
```

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     User Applications                        │
│              (mpv, Rhythmbox, Kid3, etc.)                   │
└─────────────────────────┬───────────────────────────────────┘
                          │ POSIX calls (open, read, write)
                          ▼
┌─────────────────────────────────────────────────────────────┐
│                      Linux Kernel                            │
│                       FUSE module                            │
└─────────────────────────┬───────────────────────────────────┘
                          │ /dev/fuse
                          ▼
┌─────────────────────────────────────────────────────────────┐
│                        beetfs                                │
│  ┌─────────────┐  ┌──────────────┐  ┌───────────────────┐   │
│  │ FSNode Tree │  │ FileHandler  │  │ InterpolatedFLAC  │   │
│  │ (in-memory) │  │   (cache)    │  │  (header synth)   │   │
│  └─────────────┘  └──────────────┘  └───────────────────┘   │
└────────┬────────────────┬───────────────────┬───────────────┘
         │                │                   │
         ▼                ▼                   ▼
┌─────────────┐  ┌─────────────────┐  ┌─────────────────┐
│ Beets DB    │  │  Real Files    │  │   Mutagen       │
│ (SQLite)    │  │  (on disk)     │  │   (parsing)     │
└─────────────┘  └─────────────────┘  └─────────────────┘
```

## Limitations

### Current Bugs (Non-Functional)

1. **Nested Methods Bug**: Lines 758-1144 are indented inside `access()`, making FUSE operations unreachable
2. **Directory Tree Bug**: `FSNode.adddir()` crashes when building tree for non-empty library

### Design Limitations

1. **Memory Usage**: Entire file loaded into RAM on open
2. **Mount Time**: O(N) - loads all library items at mount
3. **No Lazy Loading**: Full directory tree built upfront
4. **Single Format**: Only FLAC has full metadata overlay
5. **No Real File Modification**: Writes only update DB, not actual files
6. **Python 2.7 GIL**: Single-threaded performance

### Not Supported

- Creating/deleting files or directories
- Moving/renaming files
- Modifying audio content
- Album art / embedded images
- Multi-value tags
- Non-ASCII in some edge cases

## Configuration

Currently hardcoded. Potential configuration points:

| Setting | Current Value | Description |
|---------|---------------|-------------|
| `PATH_FORMAT` | `$artist/$album ($year)...` | Directory structure template |
| `METADATA_RW_FIELDS` | 17 fields | Fields available for read/write |
| Caching | Always on | FileHandler caching behavior |
| Threading | Disabled | `multithreaded = 0` |

## Dependencies

- Python 2.7
- fuse-python
- beets 1.4.x
- mutagen (FLAC/MP3 parsing)

## See Also

- [e2e-test-plan.md](e2e-test-plan.md) - Test strategy and bug documentation
- [benchmark-plan.md](benchmark-plan.md) - Performance measurement methodology
- [benchmark-results.md](benchmark-results.md) - Current benchmark status
