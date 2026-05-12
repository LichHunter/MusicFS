# beetfs Drawbacks & Limitations

## Overview

This document catalogs all identified issues, limitations, and missing features in beetfs. Issues are categorized by severity and type.

---

## Critical Issues (🔴)

### 1. Full File Loading into Memory

**Location**: Lines 463, 480-481

```python
self.inf = InterpolatedFLAC(self.file_object.read())  # Entire file
# ...
self.music_data = self.file_object.read()  # Audio portion again
```

**Impact**:
- Memory usage = O(file_size) per open file
- 50MB FLAC = ~50MB RAM
- Library scan of 100 files = 5GB+ RAM
- Out-of-memory crashes on large libraries

**Fix Required**: Implement lazy loading with seek-based reads.

---

### 2. MP3 Support Disabled

**Location**: Lines 475-477

```python
elif self.format == "mp3":
    self.bound = 0  # disable interpolation for now
    self.music_offset = 0  # disable interpolation for now
```

**Impact**:
- MP3 files return original metadata, not database metadata
- Breaks the core promise of metadata overlay
- MP3 is still one of the most common formats

**Fix Required**: Implement `InterpolatedID3` header generation.

---

### 3. Python 2 Only

**Location**: Throughout

```python
except fuse.FuseError, e:  # Python 2 syntax
if isinstance(value, basestring):  # Removed in Python 3
return reduce(lambda a, b: (a << 8) + ord(b), string, 0L)  # Long literals
```

**Impact**:
- Python 2 EOL was January 2020
- Security vulnerabilities unfixed
- No modern library support
- Cannot run on Python 3 without migration

**Fix Required**: Full Python 3 migration (see modernization.md).

---

### 4. Deprecated FUSE Library

**Location**: Line 25, 51

```python
import fuse
fuse.fuse_python_api = (0, 2)
```

**Impact**:
- fuse-python is unmaintained
- Missing modern FUSE features (FUSE 3.x)
- Compatibility issues with recent kernels
- No async support

**Fix Required**: Migrate to pyfuse3 or llfuse.

---

### 5. Single-Threaded Execution

**Location**: Line 178

```python
server.multithreaded = 0
```

**Impact**:
- All operations serialized
- One slow open blocks all other operations
- Cannot utilize multiple CPU cores
- Poor performance under concurrent access

**Fix Required**: Enable multithreading with proper locking.

---

## Major Issues (🟡)

### 6. Limited Metadata Fields

**Location**: Lines 466-469, 540-547

```python
# Only these 4 fields are actually used:
self.inf["title"] = self.item.title
self.inf["album"] = self.item.album
self.inf["artist"] = self.item.artist
self.inf["genre"] = self.item.genre
```

**Defined but not implemented** (lines 55-77):
- `composer`, `grouping`
- `year`, `month`, `day`
- `track`, `tracktotal`
- `disc`, `disctotal`
- `lyrics`, `comments`
- `bpm`, `comp`
- `albumartist` (not even defined)

**Impact**:
- Track numbers not from database
- Album artist not supported
- Year/date not interpolated
- Cover art not handled

---

### 7. No File Handle Caching/Eviction

**Location**: Lines 1004-1018

```python
if path in self.files:
    self.files[path].open()
else:
    self.files[path] = FileHandler(path, self.lib)
```

**Missing**:
- No maximum cache size
- No LRU eviction
- No memory pressure handling
- Files stay in memory until explicitly closed

**Impact**:
- Memory grows unbounded
- No protection against OOM
- Applications that open-then-close still leave data cached

---

### 8. Blocking Database Operations

**Location**: Lines 549-550

```python
self.lib.store(self.item)
self.lib.save()
```

**Impact**:
- SQLite operations in FUSE thread
- Write operations block all reads
- No transaction batching
- Potential deadlocks with beets

---

### 9. No Library Hot Reload

**Issue**: Virtual directory tree built once at mount time.

**Location**: Lines 142-172

```python
for item in lib.items():
    # Build tree...
```

**Impact**:
- New files added to beets library not visible
- Deleted files still appear (ENOENT on access)
- Metadata changes in beets not reflected until remount
- Must unmount/remount to see changes

---

### 10. Static Path Format

**Location**: Lines 44-45

```python
PATH_FORMAT = ("$artist/$album ($year) [$format_upper]/"
               "$track - $artist - $title.$format")
```

**Impact**:
- Cannot customize organization
- Hard-coded template
- No configuration option
- Incompatible with different organizational preferences

---

### 11. No Extended Attribute Support

**Location**: Not implemented

**Impact**:
- Cannot store/retrieve xattrs
- Some applications use xattrs for metadata
- macOS Finder metadata lost
- Linux capabilities not supported

---

### 12. No Symlink Support

**Location**: Lines 758-765

```python
def readlink(self, path):
    return -errno.EOPNOTSUPP
```

**Impact**:
- Cannot create symlinks in mount
- Some applications expect symlink support
- Cannot link to external files

---

### 13. Silent Error Swallowing

**Location**: Lines 705-707, 1019-1021, 1103-1104

```python
except Exception as e:
    logging.error(e)
    return -errno.ENOENT  # Always returns same error
```

**Impact**:
- All errors appear as "file not found"
- Hard to debug issues
- No distinction between permission, I/O, parse errors
- Lost stack traces in many cases

---

## Minor Issues (🟢)

### 14. Global State

**Location**: Lines 125-140

```python
global structure_split
global structure_depth
global library
global directory_structure
```

**Impact**:
- Cannot mount multiple instances
- Difficult to unit test
- Tight coupling between components
- No dependency injection

---

### 15. Hard-coded Log File

**Location**: Lines 624-625

```python
LOG_FILENAME = "LOG"
logging.basicConfig(filename=LOG_FILENAME, level=logging.INFO,)
```

**Impact**:
- Log file created in current directory
- No log rotation
- No configurable log level
- Fills disk on busy systems

---

### 16. Reference Count Manual Management

**Location**: Lines 485-495

```python
def open(self):
    self.instance_count = self.instance_count + 1

def release(self):
    if self.instance_count > 0:
        self.instance_count = self.instance_count - 1
```

**Issues**:
- Race conditions possible if multithreaded
- No context manager support
- Manual counting error-prone
- Off-by-one potential

---

### 17. Inefficient Directory Building

**Location**: Lines 153-172

```python
for level in range(0, structure_depth - 1):
    if level-1 in level_subbed:
        sub_elements.append(level_subbed[level-1])
    directory_structure.adddir(sub_elements, level_subbed[level])
```

**Issues**:
- Rebuilds path for every item
- O(items × depth) complexity
- String allocations in inner loop
- Could use trie-based insertion

---

### 18. No Cover Art Handling

**Issue**: Cover art embedded in FLAC not addressed.

**Impact**:
- Cover art from original file used, not database
- Cannot replace/add cover art through overlay
- PICTURE metadata blocks passed through unchanged

---

### 19. No Cue Sheet Support

**Issue**: Cue sheets not handled specially.

**Impact**:
- `.cue` files point to original file paths
- Cannot play cue-referenced tracks correctly
- Split-by-cue not supported

---

### 20. File Size Mismatch Potential

**Issue**: Virtual file size differs from physical if header size changes.

**Location**: Lines 675-688

```python
statinfo = os.stat(item)
st = Stat(st_mode=statinfo.st_mode,
          st_size=statinfo.st_size,  # Original size, not virtual!
          ...)
```

**Impact**:
- `stat()` returns original file size
- If generated header is larger/smaller, size is wrong
- Some applications may fail on size mismatch
- Range requests could break

---

## Missing Features

### Essential

| Feature | Status | Notes |
|---------|--------|-------|
| MP3 metadata interpolation | ❌ Disabled | Code exists but disabled |
| OGG/Opus support | ❌ Missing | No implementation |
| AAC/M4A support | ❌ Missing | No implementation |
| Lazy file loading | ❌ Missing | Full file loaded |
| Memory management | ❌ Missing | No limits or eviction |
| Configuration file | ❌ Missing | Hard-coded values |

### Nice to Have

| Feature | Status | Notes |
|---------|--------|-------|
| Cover art interpolation | ❌ Missing | Would need PICTURE block handling |
| ReplayGain from database | ❌ Missing | Tags not interpolated |
| Lyrics from database | ❌ Missing | Listed in fields, not implemented |
| Watch mode (hot reload) | ❌ Missing | No inotify integration |
| Multiple mount points | ❌ Missing | Global state prevents |
| Remote database | ❌ Missing | Local beets only |
| Read-only mode | ❌ Missing | Always allows writes |
| Custom path templates | ❌ Missing | Hard-coded PATH_FORMAT |

---

## Security Considerations

### 1. No Input Validation

**Location**: Throughout

```python
pathsplit = path[1:].split('/')
item_id = node.files[pathsplit[structure_depth-1]]  # No bounds check
```

**Risk**: Path traversal, injection attacks unlikely but possible.

### 2. Database Credentials Exposed

**Issue**: Uses beets library directly with stored credentials.

**Risk**: Low - local access only.

### 3. No Permission Enforcement

**Location**: Lines 749-756

```python
if flags | os.R_OK:
    pass  # TODO: actually check the file permissions
if flags | os.W_OK:
    pass
```

**Risk**: All users can read/write through mount.

---

## Compatibility Issues

| Component | Issue |
|-----------|-------|
| **Jellyfin** | May scan entire library, causing OOM |
| **Plex** | Same library scan issue |
| **Navidrome** | Expects certain tag fields not implemented |
| **mpd** | Works for playback, database features limited |
| **macOS** | fuse-python macOS support questionable |
| **Docker** | FUSE in containers requires privileged mode |

---

## Summary Table

| Category | Critical | Major | Minor |
|----------|----------|-------|-------|
| Performance | 2 | 4 | 2 |
| Functionality | 2 | 5 | 4 |
| Code Quality | 2 | 2 | 4 |
| **Total** | **6** | **11** | **10** |

---

## Prioritized Fix List

1. 🔴 **Memory**: Implement lazy loading (Critical for usability)
2. 🔴 **Python 3**: Migrate to Python 3 (Required for any changes)
3. 🔴 **FUSE lib**: Switch to pyfuse3/llfuse (Required for Python 3)
4. 🔴 **MP3**: Enable MP3 interpolation (Core functionality)
5. 🟡 **Metadata**: Implement all fields (Feature completeness)
6. 🟡 **Threading**: Enable multithreading (Performance)
7. 🟡 **Config**: Add configuration file (Usability)
8. 🟡 **Hot reload**: Watch for library changes (Usability)
9. 🟢 **Globals**: Remove global state (Code quality)
10. 🟢 **Logging**: Configurable logging (Operations)
