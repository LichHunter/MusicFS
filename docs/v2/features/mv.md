**Date**: 2026-05-17
**Status**: Shipped

# Feature: Move/Rename (mv)

## Overview

MusicFS supports moving and renaming files and directories within the virtual filesystem. Moves are persisted to the SQLite database and survive remounts.

## Behavior

### File Rename

```bash
mv "/mnt/music/Artist/Album/old.flac" "/mnt/music/Artist/Album/new.flac"
```

- Renames file within same directory
- Updates `virtual_path` in database
- Original file on origin is unchanged

### File Move

```bash
mv "/mnt/music/Artist/Album/track.flac" "/mnt/music/Other Artist/Other Album/track.flac"
```

- Moves file to different directory
- **Requires target directory to exist** (use `mkdir` first)
- Returns `ENOENT` if target parent doesn't exist

### Directory Rename

```bash
mv "/mnt/music/Old Artist" "/mnt/music/New Artist"
```

- Renames directory and all descendants
- All files under the directory have their `virtual_path` updated in DB
- Single atomic operation

### Directory Move

```bash
mv "/mnt/music/Artist/Album" "/mnt/music/Other Artist/Album"
```

- Moves directory subtree to new parent
- **Requires target parent to exist**
- Returns `ENOENT` if target parent doesn't exist

## Error Codes

| Condition | Error |
|-----------|-------|
| Source doesn't exist | `ENOENT` |
| Target already exists | `EEXIST` |
| Target parent doesn't exist | `ENOENT` |
| Source is file but treated as dir | `EISDIR` |
| Source is dir but treated as file | `ENOTDIR` |

## Persistence

- File moves: `virtual_path` column updated in `files` table
- Directory moves: All matching `virtual_path` entries updated with new prefix
- User directories: Tracked in separate `directories` table
- Changes persist across unmount/remount cycles

On mount, the CLI:
1. Scans origin files
2. For each file, checks DB for stored `virtual_path` (by origin_id + real_path)
3. Uses stored path if found, otherwise generates from metadata
4. Restores user-created directories from `directories` table

## Limitations

- **Read-only content**: File contents cannot be modified, only paths
- **No cross-origin moves**: All files remain on their original origin
- **No overwrite**: Moving to existing path fails (no implicit delete)

## Implementation

| Component | File |
|-----------|------|
| Database | `crates/musicfs-cache/src/db.rs` |
| Tree | `crates/musicfs-cache/src/tree.rs` |
| FUSE | `crates/musicfs-fuse/src/filesystem.rs` |

### Key Functions

- `Database::update_virtual_path()` - Update single file path
- `Database::rename_directory()` - Bulk update paths with prefix
- `VirtualTree::rename_file()` - Move file node in tree
- `VirtualTree::rename_directory()` - Move directory subtree
