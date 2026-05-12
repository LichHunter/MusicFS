# beetfs Architecture

## System Overview

beetfs implements a **metadata overlay filesystem** using FUSE. The key innovation is separating metadata storage (in beets SQLite database) from audio data storage (original files on disk).

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              USER SPACE                                      │
│  ┌─────────────┐    ┌─────────────────────────────────────────────────────┐ │
│  │ Application │    │                    beetfs                           │ │
│  │  (VLC, etc) │    │  ┌─────────────┐  ┌──────────────┐  ┌────────────┐ │ │
│  │             │◄───┼──┤beetFileSystem│──│ FileHandler  │──│ Interpol.  │ │ │
│  │             │    │  │   (FUSE)    │  │              │  │ FLAC/ID3   │ │ │
│  └─────────────┘    │  └─────────────┘  └──────────────┘  └────────────┘ │ │
│                     │         │                │                │         │ │
│                     │         ▼                ▼                ▼         │ │
│                     │  ┌─────────────┐  ┌──────────────┐  ┌────────────┐ │ │
│                     │  │   FSNode    │  │    Beets     │  │  Original  │ │ │
│                     │  │ (dir tree)  │  │  Database    │  │   Files    │ │ │
│                     │  └─────────────┘  └──────────────┘  └────────────┘ │ │
│                     └─────────────────────────────────────────────────────┘ │
├─────────────────────────────────────────────────────────────────────────────┤
│                              KERNEL SPACE                                    │
│                          ┌───────────────┐                                   │
│                          │   FUSE VFS    │                                   │
│                          └───────────────┘                                   │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Component Architecture

### 1. Plugin Layer

```python
class beetFs(BeetsPlugin):
    """Beets plugin hook - registers the 'mount' subcommand"""
    def commands(self):
        return [beetFs_command]

beetFs_command = Subcommand('mount', help='Mount a beets filesystem')
beetFs_command.func = mount
```

### 2. Initialization Flow

```
beet mount /mountpoint
        │
        ▼
┌───────────────────────────────────────────────────────────────┐
│                        mount() function                        │
│  1. Parse PATH_FORMAT template                                 │
│  2. Create FSNode root (directory_structure)                   │
│  3. Iterate all items in beets library                         │
│  4. For each item:                                             │
│     - Build template substitution map                          │
│     - Add directories to FSNode tree                           │
│     - Add file entry (filename → item.id mapping)              │
│  5. Create beetFileSystem FUSE server                          │
│  6. server.main() - enter FUSE event loop                      │
└───────────────────────────────────────────────────────────────┘
```

### 3. Virtual Directory Structure

The default path template:
```python
PATH_FORMAT = "$artist/$album ($year) [$format_upper]/$track - $artist - $title.$format"
```

Results in structure like:
```
/mountpoint/
├── Pink Floyd/
│   └── The Wall (1979) [FLAC]/
│       ├── 01 - Pink Floyd - In The Flesh?.flac
│       └── 02 - Pink Floyd - The Thin Ice.flac
└── Led Zeppelin/
    └── IV (1971) [FLAC]/
        └── 01 - Led Zeppelin - Black Dog.flac
```

### 4. FSNode Tree Structure

```python
class FSNode:
    dirs: Dict[str, FSNode]   # subdirectories
    files: Dict[str, int]     # filename → beets item ID

# Example tree:
FSNode(
    dirs={
        "Pink Floyd": FSNode(
            dirs={
                "The Wall (1979) [FLAC]": FSNode(
                    dirs={},
                    files={
                        "01 - Pink Floyd - In The Flesh?.flac": 42,
                        "02 - Pink Floyd - The Thin Ice.flac": 43
                    }
                )
            },
            files={}
        )
    },
    files={}
)
```

## Core Data Flow

### Read Operation

```
Application: read("/mount/Artist/Album/track.flac", offset=0, size=4096)
                                    │
                                    ▼
                        ┌───────────────────────┐
                        │ beetFileSystem.read() │
                        │   Lines 1077-1106     │
                        └───────────┬───────────┘
                                    │
                    ┌───────────────┴───────────────┐
                    │ Get/Create FileHandler        │
                    │ for this path                 │
                    └───────────────┬───────────────┘
                                    │
                        ┌───────────┴───────────┐
                        │ FileHandler.read()    │
                        │   Lines 497-517       │
                        └───────────┬───────────┘
                                    │
                    ┌───────────────┴───────────────┐
                    ▼                               ▼
        ┌─────────────────────┐         ┌─────────────────────┐
        │ offset < bound      │         │ offset >= bound     │
        │ (in header area)    │         │ (in audio area)     │
        └──────────┬──────────┘         └──────────┬──────────┘
                   │                               │
                   ▼                               ▼
        ┌─────────────────────┐         ┌─────────────────────┐
        │ Return modified     │         │ Return original     │
        │ header from DB      │         │ audio from file     │
        │                     │         │                     │
        │ self.header[...]    │         │ self.music_data[...]│
        └─────────────────────┘         └─────────────────────┘
```

### Write Operation

```
Application: write("/mount/Artist/Album/track.flac", data, offset=100)
                                    │
                                    ▼
                        ┌───────────────────────┐
                        │ beetFileSystem.write()│
                        │   Lines 1108-1135     │
                        └───────────┬───────────┘
                                    │
                        ┌───────────┴───────────┐
                        │ FileHandler.write()   │
                        │   Lines 519-565       │
                        └───────────┬───────────┘
                                    │
                    ┌───────────────┴───────────────┐
                    ▼                               ▼
        ┌─────────────────────┐         ┌─────────────────────┐
        │ offset < bound      │         │ offset >= bound     │
        │ (in header area)    │         │ (in audio area)     │
        └──────────┬──────────┘         └──────────┬──────────┘
                   │                               │
                   ▼                               ▼
        ┌─────────────────────┐         ┌─────────────────────┐
        │ 1. Patch header     │         │ DISCARD             │
        │ 2. Parse new tags   │         │ (audio writes       │
        │ 3. Extract values   │         │  not allowed)       │
        │ 4. Update beets DB  │         │                     │
        │ 5. Regenerate header│         │                     │
        └─────────────────────┘         └─────────────────────┘
```

## Memory Model

### FileHandler State

```python
class FileHandler:
    # Paths
    path: str           # Virtual path in FUSE mount
    real_path: str      # Actual file on disk
    
    # Beets integration
    item: Item          # Beets library item
    lib: Library        # Beets library reference
    
    # File data
    file_object: File   # File handle (closed after init)
    music_data: bytes   # Audio data cached in memory
    
    # Metadata
    format: str         # "flac" or "mp3"
    inf: FLAC/ID3       # Interpolated metadata object
    header: bytes       # Generated header with DB metadata
    bound: int          # Byte offset where header ends
    music_offset: int   # Byte offset where audio starts in original
    
    # Reference counting
    instance_count: int # Number of open handles
```

### Memory Layout

```
Virtual File (as seen by application):
┌────────────────────────────────────────────────────────────────┐
│          HEADER (from DB)           │    AUDIO (from file)    │
│  [0 ... bound)                      │  [bound ... EOF)        │
│                                     │                         │
│  Generated by InterpolatedFLAC      │  Cached in music_data   │
│  Contains: title, artist, album,    │  Original audio frames  │
│           genre from beets DB       │  Unchanged              │
└────────────────────────────────────────────────────────────────┘
                    ▲                              ▲
                    │                              │
           self.header                    self.music_data


Original File (on disk):
┌────────────────────────────────────────────────────────────────┐
│     ORIGINAL HEADER      │           AUDIO DATA               │
│  [0 ... music_offset)    │   [music_offset ... EOF)           │
│                          │                                    │
│  May have different      │  Same as virtual file              │
│  tag values              │                                    │
└────────────────────────────────────────────────────────────────┘
```

## Threading Model

```python
server.multithreaded = 0  # Single-threaded mode
```

beetfs runs in **single-threaded mode** to avoid concurrency issues with:
- Shared `files` dictionary
- Beets library access
- File handle reference counting

## Global State

```python
# Module-level globals (set during mount)
structure_split: List[str]      # PATH_FORMAT split by "/"
structure_depth: int            # Number of path components
library: Library                # Beets library instance
directory_structure: FSNode     # Root of virtual directory tree
```

## Error Handling

| Situation | Response |
|-----------|----------|
| File not found | Return `-errno.ENOENT` |
| Permission denied | Return `-errno.EACCES` |
| Operation not supported | Return `-errno.EOPNOTSUPP` |
| Parse error | Log and return `-errno.ENOENT` |

## Limitations

1. **Format Support**: Only FLAC fully implemented; MP3 support is incomplete
2. **Memory Usage**: Entire audio portion cached in memory per open file
3. **Single-threaded**: No concurrent access optimization
4. **No Streaming**: Full file must be read into memory
5. **Python 2**: Uses deprecated language features
6. **fuse-python**: Old FUSE bindings, not maintained
