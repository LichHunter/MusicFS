**Date**: 2026-05-17
**Status**: Shipped

# Feature: Remove (rm)

## Overview

MusicFS supports removing files and directories. Deleted files are moved to a virtual `/.trash/` directory and can be restored. The trash is browsable — users can manually move files out.

## Behavior

### Remove File

```bash
rm "/mnt/music/Artist/Album/track.flac"
```

- File moves to `/.trash/Artist/Album/track.flac`
- Original directory structure preserved in trash
- File still accessible via `/.trash/` path
- Database marks file as `trashed=1` with original path stored

### Remove Empty Directory

```bash
rmdir "/mnt/music/Empty Folder"
```

- Removes empty directory from tree
- Removes from `directories` table if user-created
- Fails with `ENOTEMPTY` if directory has children

### Remove Directory Recursively

```bash
rm -rf "/mnt/music/Artist"
```

- Shell handles recursion (depth-first unlink + rmdir)
- All files moved to `/.trash/Artist/...`
- Empty directories removed after files are trashed

## The `.trash/` Directory

Deleted files live in `/.trash/` with their original path structure:

```
/.trash/
├── Artist/
│   └── Album/
│       ├── track1.flac
│       └── track2.flac
└── Other Artist/
    └── song.flac
```

### Browse Trash

```bash
ls "/.trash/"
ls "/.trash/Artist/Album/"
```

### Manual Restore

```bash
# Move file back manually - trashed flag is automatically cleared
mv "/.trash/Artist/Album/track.flac" "/Artist/Album/"
```

When moving a file out of `/.trash/`, the database `trashed` flag is automatically cleared.

## CLI Commands

All trash commands require either `--config` or `--cache-dir`:

```bash
musicfs trash -c config.toml <command>
musicfs trash --cache-dir ./dev/cache/musicfs <command>
```

### List Deleted Files

```bash
musicfs trash -c config.toml list
musicfs trash -c config.toml list --origin local-storage
musicfs trash -c config.toml list --since 7d
musicfs trash -c config.toml list --path "/Artist"
```

Output shows index, deletion time, and original path.

### Restore Files

```bash
# Restore single file or folder
musicfs trash -c config.toml restore "/Artist/Album/track.flac"

# Restore entire folder recursively
musicfs trash -c config.toml restore "/Artist"

# Restore everything
musicfs trash -c config.toml restore --all
```

CLI restore writes paths to a pending restore file and sends SIGHUP to the daemon.
The daemon processes pending restores and moves files back from `/.trash/`.

### Empty Trash

```bash
# Permanently delete all trashed files
musicfs trash -c config.toml empty

# Delete old items only
musicfs trash -c config.toml empty --older-than 30d

# Delete by path pattern
musicfs trash -c config.toml empty --pattern "/Artist"
```

**Warning:** Empty permanently removes files from MusicFS database. Origin files are unaffected.

## Error Codes

| Condition | Error |
|-----------|-------|
| Path doesn't exist | `ENOENT` |
| `rm` on directory (without `-r`) | `EISDIR` |
| `rmdir` on file | `ENOTDIR` |
| `rmdir` on non-empty directory | `ENOTEMPTY` |
| `rmdir` on `/.trash/` | `EPERM` |

## Database Schema

Files table extended with trash columns:

```sql
trashed         INTEGER NOT NULL DEFAULT 0,
original_path   TEXT,
trashed_at      INTEGER
```

Partial index for efficient trash queries:
```sql
CREATE INDEX idx_files_trashed ON files(trashed) WHERE trashed = 1;
```

## How It Works

1. **Delete (`rm`)**: FUSE `unlink` moves file to `/.trash/`, marks `trashed=1` in DB
2. **Manual restore (`mv`)**: Moving out of `/.trash/` automatically clears `trashed` flag  
3. **CLI restore**: Writes pending paths, sends SIGHUP to daemon, daemon processes restores
4. **Empty**: Deletes matching records from database

## Persistence

- Trashed files persist across remounts (stored in `/.trash/` subtree)
- Files marked with `trashed=1`, `original_path`, `trashed_at` in database
- PID file at `{cache_dir}/musicfs.pid` for CLI→daemon communication

## Limitations

- **No hard delete of remote files**: Origin content is never modified
- **Trash uses virtual space**: Files still in tree under `/.trash/` until emptied
- **CLI restore requires running daemon**: Manual `mv` works without daemon
