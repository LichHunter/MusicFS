# beetfs Modernization Guide

## Current State Analysis

### Technical Debt

| Issue | Severity | Location |
|-------|----------|----------|
| Python 2 syntax | 🔴 Critical | Throughout |
| fuse-python (deprecated) | 🔴 Critical | Lines 25, 51 |
| `basestring` usage | 🔴 Critical | Line 89 |
| `reduce` without import | 🟡 Medium | Line 197 |
| `0755` octal syntax | 🟡 Medium | Lines 654, 700 |
| `print` as statement | 🟡 Medium | N/A (not used) |
| `except Exception, e` | 🔴 Critical | Line 181 |
| Long integers (`0L`) | 🟡 Medium | Line 197 |
| Global state | 🟡 Medium | Lines 125-140 |
| Memory-heavy design | 🟡 Medium | Line 481 |

### Dependencies to Update

| Original | Replacement | Notes |
|----------|-------------|-------|
| `fuse-python` | `pyfuse3` or `llfuse` | Modern FUSE bindings |
| `beets` (old API) | `beets >= 1.6` | Check API compatibility |
| `mutagen` | `mutagen >= 1.45` | Mostly compatible |
| Python 2.7 | Python 3.9+ | Full migration needed |

---

## Migration Steps

### Phase 1: Python 3 Compatibility

#### 1.1 Fix Syntax Issues

```python
# BEFORE (Python 2)
except fuse.FuseError, e:
    log.error(str(e))

# AFTER (Python 3)
except fuse.FuseError as e:
    log.error(str(e))
```

```python
# BEFORE
if isinstance(value, basestring):

# AFTER
if isinstance(value, str):
```

```python
# BEFORE
return reduce(lambda a, b: (a << 8) + ord(b), string, 0L)

# AFTER
from functools import reduce
return reduce(lambda a, b: (a << 8) + b, string, 0)
```

```python
# BEFORE
mode = stat.S_IFDIR | 0755

# AFTER
mode = stat.S_IFDIR | 0o755
```

#### 1.2 Fix String/Bytes Handling

```python
# BEFORE - implicit string/bytes mixing
self.header = self.inf.get_header(self.real_path)
return self.header[offset:offset+size]

# AFTER - explicit bytes handling
self.header: bytes = self.inf.get_header(self.real_path)
return self.header[offset:offset+size]
```

```python
# BEFORE
self.item.title = str(self.inf["title"][0]).encode('utf-8')

# AFTER
self.item.title = self.inf["title"][0]  # Already str in Python 3
```

#### 1.3 Fix Dictionary Methods

```python
# BEFORE
return node.dirs.keys()

# AFTER
return list(node.dirs.keys())  # If list is needed
# or just
return node.dirs.keys()  # If iteration is sufficient
```

---

### Phase 2: FUSE Library Migration

#### Option A: pyfuse3 (Recommended)

Modern, async-capable FUSE bindings.

```python
# BEFORE (fuse-python)
import fuse
fuse.fuse_python_api = (0, 2)

class beetFileSystem(fuse.Fuse):
    def read(self, path, size, offset):
        return data

# AFTER (pyfuse3)
import pyfuse3
import trio

class BeetFS(pyfuse3.Operations):
    async def read(self, fh, offset, size):
        return data

async def main():
    fs = BeetFS()
    fuse_options = set(pyfuse3.default_options)
    fuse_options.add('fsname=beetfs')
    pyfuse3.init(fs, mountpoint, fuse_options)
    try:
        await pyfuse3.main()
    finally:
        pyfuse3.close()

trio.run(main)
```

**Key Differences**:
| fuse-python | pyfuse3 |
|-------------|---------|
| `read(path, size, offset)` | `read(fh, offset, size)` |
| Synchronous | Async (trio) |
| Return data directly | Return bytes |
| Path-based | File handle based |

#### Option B: llfuse (Alternative)

Lower-level, synchronous.

```python
import llfuse

class BeetFS(llfuse.Operations):
    def read(self, fh, offset, size):
        return data

def main():
    fs = BeetFS()
    llfuse.init(fs, mountpoint, options)
    try:
        llfuse.main()
    finally:
        llfuse.close()
```

#### Option C: fusepy (Simple)

Simple wrapper, but less maintained.

```python
from fuse import FUSE, Operations

class BeetFS(Operations):
    def read(self, path, size, offset, fh):
        return data

FUSE(BeetFS(), mountpoint, foreground=True)
```

---

### Phase 3: Architecture Improvements

#### 3.1 Remove Global State

```python
# BEFORE - Global variables
global structure_split
global structure_depth
global library
global directory_structure

# AFTER - Instance variables
class BeetFS:
    def __init__(self, lib: Library, path_format: str):
        self.lib = lib
        self.path_format = path_format
        self.structure_split = path_format.split("/")
        self.structure_depth = len(self.structure_split)
        self.directory_structure = FSNode({}, {})
        self._build_tree()
```

#### 3.2 Reduce Memory Usage

```python
# BEFORE - Load entire audio into memory
self.music_data = self.file_object.read()  # Could be 100MB+

# AFTER - Lazy loading with mmap or seek
class FileHandler:
    def __init__(self, path, lib):
        self.real_path = self._resolve_path(path)
        self.file_object = open(self.real_path, 'rb')
        self._header = None  # Lazy load
        self._music_offset = None
    
    @property
    def header(self) -> bytes:
        if self._header is None:
            self._header = self._generate_header()
        return self._header
    
    def read(self, size: int, offset: int) -> bytes:
        if offset < len(self.header):
            # Header region - return from generated header
            if offset + size <= len(self.header):
                return self.header[offset:offset+size]
            else:
                # Span header and audio
                header_part = self.header[offset:]
                audio_offset = 0
                audio_size = size - len(header_part)
                audio_part = self._read_audio(audio_offset, audio_size)
                return header_part + audio_part
        else:
            # Audio region - read directly from file
            audio_offset = offset - len(self.header)
            return self._read_audio(audio_offset, size)
    
    def _read_audio(self, offset: int, size: int) -> bytes:
        self.file_object.seek(self._music_offset + offset)
        return self.file_object.read(size)
```

#### 3.3 Add Type Hints

```python
from typing import Dict, List, Optional, Tuple
from pathlib import Path

class FSNode:
    def __init__(self, dirs: Dict[str, 'FSNode'], files: Dict[str, int]):
        self.dirs: Dict[str, FSNode] = dirs
        self.files: Dict[str, int] = files
    
    def getnode(self, elements: List[str], root: Optional['FSNode'] = None) -> 'FSNode':
        ...
    
    def addfile(self, elements: List[str], filename: str, item_id: int) -> None:
        ...
```

#### 3.4 Add MP3 Support

```python
class FileHandler:
    def __init__(self, path: str, lib: Library):
        self.format = Path(path).suffix[1:].lower()
        
        if self.format == "flac":
            self._handler = FLACHandler(self.real_path, self.item)
        elif self.format == "mp3":
            self._handler = MP3Handler(self.real_path, self.item)
        elif self.format in ("ogg", "opus"):
            self._handler = OggHandler(self.real_path, self.item)
        else:
            raise UnsupportedFormatError(f"Format {self.format} not supported")

class FLACHandler:
    def generate_header(self, item: Item) -> bytes:
        inf = InterpolatedFLAC(self.file_data)
        inf["title"] = item.title
        inf["album"] = item.album
        inf["artist"] = item.artist
        inf["genre"] = item.genre
        return inf.get_header()

class MP3Handler:
    def generate_header(self, item: Item) -> bytes:
        # Implement ID3v2 header generation
        id3 = InterpolatedID3()
        id3.add(TIT2(encoding=3, text=item.title))
        id3.add(TPE1(encoding=3, text=item.artist))
        id3.add(TALB(encoding=3, text=item.album))
        id3.add(TCON(encoding=3, text=item.genre))
        
        # Calculate padding to match original header size
        ...
        return id3.render()
```

---

### Phase 4: Testing

#### 4.1 Unit Tests

```python
import pytest
from beetfs import FSNode, FileHandler

class TestFSNode:
    def test_adddir(self):
        root = FSNode({}, {})
        root.adddir([], "Artist")
        assert "Artist" in root.dirs
    
    def test_addfile(self):
        root = FSNode({}, {})
        root.adddir([], "Artist")
        root.addfile(["Artist"], "track.flac", 42)
        assert root.dirs["Artist"].files["track.flac"] == 42
    
    def test_getnode(self):
        root = FSNode({}, {})
        root.adddir([], "Artist")
        root.adddir(["Artist"], "Album")
        node = root.getnode(["Artist", "Album"])
        assert node is not None

class TestFileHandler:
    def test_read_header(self, mock_flac_file, mock_beets_item):
        handler = FileHandler("/Artist/Album/track.flac", mock_lib)
        data = handler.read(100, 0)
        assert data.startswith(b"fLaC")
    
    def test_read_audio(self, mock_flac_file, mock_beets_item):
        handler = FileHandler("/Artist/Album/track.flac", mock_lib)
        data = handler.read(100, handler.bound + 100)
        # Should be audio data from original file
        assert data == mock_flac_file.audio_data[100:200]
```

#### 4.2 Integration Tests

```python
import subprocess
import tempfile
import os

class TestFUSEMount:
    def test_mount_unmount(self, beets_library):
        with tempfile.TemporaryDirectory() as mountpoint:
            # Mount
            proc = subprocess.Popen(
                ["beet", "mount", mountpoint],
                stdout=subprocess.PIPE
            )
            time.sleep(1)
            
            # Verify mount
            assert os.path.ismount(mountpoint)
            
            # List files
            files = os.listdir(mountpoint)
            assert len(files) > 0
            
            # Unmount
            subprocess.run(["fusermount", "-u", mountpoint])
            proc.wait()
```

---

### Phase 5: Standalone Mode (Optional)

Remove beets dependency for use as standalone metadata overlay.

```python
class StandaloneFS:
    """Metadata overlay without beets dependency."""
    
    def __init__(self, 
                 source_dir: Path,
                 metadata_db: Path,
                 path_format: str):
        self.source_dir = source_dir
        self.db = sqlite3.connect(metadata_db)
        self.path_format = path_format
        self._build_tree()
    
    def _build_tree(self):
        """Build virtual tree from source directory and metadata DB."""
        for audio_file in self.source_dir.rglob("*.flac"):
            # Get metadata from DB or scan file
            metadata = self._get_metadata(audio_file)
            # Build virtual path from template
            virtual_path = self._format_path(metadata)
            # Add to tree
            self.directory_structure.addfile(
                virtual_path.parent.parts,
                virtual_path.name,
                str(audio_file)  # Store actual path instead of ID
            )
```

---

## Recommended Migration Order

```
1. [ ] Fork and set up development environment
2. [ ] Add type hints throughout (helps catch issues)
3. [ ] Fix Python 3 syntax issues
4. [ ] Replace fuse-python with pyfuse3/llfuse
5. [ ] Add unit tests for FSNode and FileHandler
6. [ ] Refactor global state to instance variables
7. [ ] Implement lazy loading for audio data
8. [ ] Add MP3 support
9. [ ] Add integration tests
10. [ ] Optional: Create standalone mode
```

---

## Estimated Effort

| Phase | Effort | Risk |
|-------|--------|------|
| Phase 1 (Python 3) | 2-3 days | Low |
| Phase 2 (FUSE migration) | 3-5 days | Medium |
| Phase 3 (Architecture) | 3-5 days | Medium |
| Phase 4 (Testing) | 2-3 days | Low |
| Phase 5 (Standalone) | 3-5 days | Medium |
| **Total** | **13-21 days** | |

---

## Alternative: Rewrite from Scratch

Given the age of the codebase, a rewrite might be more efficient:

**Pros of Rewrite**:
- Clean architecture from start
- Modern async design
- Better memory management
- Easier to test

**Cons of Rewrite**:
- More initial effort
- Risk of missing edge cases
- Need to re-discover FLAC/ID3 intricacies

**Recommended Approach**: Start with Phase 1-2 to understand the code deeply, then decide whether to continue refactoring or rewrite.
