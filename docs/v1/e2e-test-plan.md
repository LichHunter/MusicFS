# beetfs E2E Test Plan

> **Reviewed by Oracle** - Critical bug discovered, plan updated accordingly

## Test Results (Latest Run)

```
Tests run:   74
Passed:      12
Failures:    56
Errors:       3
Skipped:      3
Duration:    ~103 seconds
```

### Bugs Detected by Tests

| Bug | Tests Affected | Description |
|-----|----------------|-------------|
| **Nested Methods** | 56 | Lines 758-1144 indented inside `access()` - FUSE operations unreachable |
| **Directory Tree Building** | 3 | `KeyError` in `FSNode.getnode()` when adding files |
| **Unmount** | 1 | Filesystem not unmounting cleanly |

### Passing Tests (12)

- `test_fuse_available` - FUSE/fusermount detected
- `test_library_fixture_created` - SQLite DB and music dir created
- `test_temp_directory_created` - Temp dirs set up correctly
- `test_mount_empty_library` - **Mount works with empty library!**
- `test_list_empty_root` - Empty root returns empty list
- `test_list_root_returns_list` - Returns list type
- `test_access_empty_path` - Handles empty path
- Plus 5 nested bug detection tests (confirming bug exists)

## Executive Summary

E2E tests for beetfs FUSE filesystem using real music files from qBittorrent container. No mocks - actual filesystem operations against mounted beetfs.

### Critical Finding

**BUG DISCOVERED**: Lines 758-1144 in `beetFs.py` are indented inside `access()` method, making these FUSE operations unreachable as class methods:
- `readdir`, `open`, `read`, `write`, `mkdir`, `unlink`, `rmdir`, `symlink`, `link`, `rename`, `chmod`, `chown`, `truncate`, `opendir`, `releasedir`, `fsyncdir`, `create`, `fgetattr`, `release`, `fsync`, `flush`, `ftruncate`

Tests will expose this immediately - write `test_readdir.py` first.

---

## Test Environment

| Component | Status | Details |
|-----------|--------|---------|
| Real Music | Available | Metallica "72 Seasons" (12 FLAC, 650MB) at `/home/fujin/.local/share/docker/volumes/containers_downloads/_data/Metallica - 72 Seasons (2023) [FLAC] 88/` |
| Synthetic Music | Create | 5-10MB FLACs for most tests (avoid RAM explosion) |
| Beets Config | Create | `~/.config/beets/config.yaml` for test isolation |
| Beets Library | Empty | Needs import of test files |
| Python | 2.7.15 | Via Nix flake (nixpkgs-18.09) |
| Test Framework | unittest | stdlib, no external deps for Py2.7 |

---

## Test Architecture

```
beetfs/tests/
├── __init__.py
├── conftest.py              # Test fixtures, beets library setup, synthetic FLAC creation
├── test_smoke.py            # Mount/unmount lifecycle (run FIRST)
├── test_nested_bug.py       # Verify the indentation bug (run SECOND)
├── test_readdir.py          # Directory listing operations
├── test_read.py             # File reading with metadata overlay (CORE FEATURE)
├── test_stat.py             # getattr, fgetattr, statfs
├── test_write.py            # Metadata write operations
├── test_error_handling.py   # ENOENT, EOPNOTSUPP scenarios
├── test_edge_cases.py       # Unicode, concurrent opens, special chars
├── test_integration.py      # Real 650MB files (skip by default)
└── fixtures/
    ├── synthetic/           # Generated 5-10MB test FLACs
    └── real -> /home/fujin/.local/share/docker/volumes/containers_downloads/_data/
```

---

## Test Tiers

### Tier 1: Unit-ish (Synthetic FLACs, ~500KB each)
- Fast execution
- No memory issues (FileHandler loads entire file to RAM)
- Run on every commit

### Tier 2: Integration (Subset of real files, 1-2 tracks)
- Uses real Metallica FLACs
- Tests real-world metadata
- Run before merge

### Tier 3: E2E (All 12 tracks, 650MB)
- Full album processing
- Memory stress testing
- Run via `E2E=1 python -m unittest discover`
- Skip by default

---

## Test Isolation Strategy

| Resource | Strategy | Rationale |
|----------|----------|-----------|
| Audio Files | **Symlinks** for reads | beetfs NEVER writes to source files, only to beets DB |
| Beets DB | **Copy per test** | Writes mutate DB; need isolation |
| Mount Point | **Fresh tempdir** | Each test gets clean mount |
| Global State | **Fresh subprocess** | `library`, `directory_structure` are module globals |

---

## Implementation Order

> Reordered per Oracle recommendation: smoke → nested-bug → read → write → errors → edge

### Phase 1: Infrastructure (Day 1 AM)

1. Create `tests/` directory structure
2. Implement `BeetFSTestCase` base class with:
   - Subprocess timeout via `threading.Timer` (Py2.7 compatible)
   - Mount wait polling (`os.path.ismount()`)
   - Proper cleanup (`fusermount -u`)
3. Create synthetic FLAC generator using ffmpeg + flac CLI
4. Setup isolated beets config and library

### Phase 2: Bug Detection (Day 1 PM)

5. `test_smoke.py` - Mount/unmount lifecycle
6. `test_nested_bug.py` - Verify `readdir`, `open` are callable (will fail, exposing bug)

### Phase 3: Core Tests (Day 2)

7. `test_readdir.py` - Directory listing
8. `test_read.py` - **Metadata overlay verification** (critical)
9. `test_stat.py` - File/directory attributes

### Phase 4: Write & Errors (Day 3)

10. `test_write.py` - Metadata modification, DB persistence
11. `test_error_handling.py` - ENOENT, EOPNOTSUPP

### Phase 5: Edge Cases (Day 3-4)

12. `test_edge_cases.py` - Unicode, concurrent opens, special chars
13. `test_integration.py` - Real 650MB files (optional tier)

---

## Test Categories

### 1. Smoke Tests (`test_smoke.py`)

| Test | Operation | Expected |
|------|-----------|----------|
| `test_mount_success` | Mount beetfs | `os.path.ismount()` returns True |
| `test_unmount_clean` | Unmount | Process exits 0, dir accessible |
| `test_mount_empty_library` | Mount with 0 items | Mounts successfully, root empty |
| `test_mount_invalid_path` | Mount to non-existent | Fails gracefully |
| `test_fsinit_called` | Check initialization | No crash on mount |

### 2. Nested Methods Bug (`test_nested_bug.py`)

| Test | Operation | Expected |
|------|-----------|----------|
| `test_readdir_exists` | `hasattr(beetFileSystem, 'readdir')` | True (currently False!) |
| `test_open_exists` | `hasattr(beetFileSystem, 'open')` | True (currently False!) |
| `test_read_exists` | `hasattr(beetFileSystem, 'read')` | True (currently False!) |
| `test_readdir_callable` | `os.listdir(mount)` | Returns list (currently fails!) |

### 3. Directory Operations (`test_readdir.py`)

| Test | Operation | Expected |
|------|-----------|----------|
| `test_list_root` | `os.listdir(mount)` | Returns artist directories |
| `test_list_artist` | `os.listdir(mount/artist)` | Returns album directories |
| `test_list_album` | `os.listdir(mount/artist/album)` | Returns track files |
| `test_path_format` | Check structure | Matches `$artist/$album ($year) [$format_upper]/$track - $artist - $title.$format` |
| `test_unicode_paths` | Non-ASCII chars | Handles "Lux Aeterna" correctly |

### 4. Read Operations (`test_read.py`) - CORE FEATURE

| Test | Operation | Expected |
|------|-----------|----------|
| `test_read_header_overlay` | Read + parse with mutagen | Tags match DB, not file |
| `test_read_audio_passthrough` | Compare audio bytes | Identical to original after header |
| `test_read_full_file` | Read entire file | Header from DB + audio from file |
| `test_metadata_artist` | Check artist tag | DB value, not file value |
| `test_metadata_title` | Check title tag | DB value, not file value |
| `test_metadata_album` | Check album tag | DB value, not file value |
| `test_metadata_genre` | Check genre tag | DB value, not file value |
| `test_original_unchanged` | Read original file | Original metadata intact |

#### Metadata Overlay Verification Pattern

```python
import mutagen.flac
from io import BytesIO

def test_read_header_overlay(self):
    # Setup: Import file, modify DB metadata
    # beet import /path/to/file
    # beet modify artist="DB Artist"  # File has "Original Artist"
    
    # Read mounted file as bytes
    with open(os.path.join(self.mount_dir, 'DB Artist/...'), 'rb') as f:
        mounted_data = f.read()
    
    # Parse with mutagen
    flac = mutagen.flac.FLAC(BytesIO(mounted_data))
    
    # Verify overlay worked
    self.assertEqual(flac['artist'][0], 'DB Artist')  # From DB
    self.assertNotEqual(flac['artist'][0], 'Original Artist')  # Not from file
```

### 5. Stat Operations (`test_stat.py`)

| Test | Operation | Expected |
|------|-----------|----------|
| `test_stat_file` | `os.stat(file)` | Valid stat with size, mtime |
| `test_stat_directory` | `os.stat(dir)` | Directory mode (S_IFDIR) |
| `test_statfs` | `os.statvfs(mount)` | Valid filesystem stats |
| `test_access_read` | `os.access(file, R_OK)` | True |
| `test_access_write` | `os.access(file, W_OK)` | True (header writable) |

### 6. Write Operations (`test_write.py`)

| Test | Operation | Expected |
|------|-----------|----------|
| `test_write_title` | Modify title in header | DB updated, file unchanged |
| `test_write_artist` | Modify artist | DB updated |
| `test_write_album` | Modify album | DB updated |
| `test_write_genre` | Modify genre | DB updated |
| `test_write_audio_discarded` | Write at offset > bound | Silently discarded |
| `test_write_persistence` | Write -> unmount -> remount | Changes persisted in DB |
| `test_write_mp3_noop` | Write to MP3 header | No error, but no effect (bound=0) |

### 7. Error Handling (`test_error_handling.py`)

| Test | Operation | Expected |
|------|-----------|----------|
| `test_enoent_file` | Read non-existent | `OSError(ENOENT)` |
| `test_enoent_dir` | List non-existent | `OSError(ENOENT)` |
| `test_eopnotsupp_mkdir` | `os.mkdir()` | `OSError(EOPNOTSUPP)` |
| `test_eopnotsupp_unlink` | `os.unlink()` | `OSError(EOPNOTSUPP)` |
| `test_eopnotsupp_rename` | `os.rename()` | `OSError(EOPNOTSUPP)` |
| `test_eopnotsupp_symlink` | `os.symlink()` | `OSError(EOPNOTSUPP)` |

### 8. Edge Cases (`test_edge_cases.py`)

| Test | Operation | Expected |
|------|-----------|----------|
| `test_special_chars_sanitized` | Path with `?/` | Sanitized via `sanitize()` |
| `test_concurrent_opens` | Open same file twice | `instance_count` increments |
| `test_concurrent_release` | Release after double open | File stays cached until count=0 |
| `test_unicode_metadata` | Non-ASCII in artist/title | Handled correctly |
| `test_empty_metadata` | None/empty fields | Doesn't crash |
| `test_mp3_no_interpolation` | Read MP3 | Returns original file (no overlay) |

### 9. Integration (`test_integration.py`)

| Test | Env Var | Expected |
|------|---------|----------|
| `test_real_album_listing` | `E2E=1` | Lists all 12 Metallica tracks |
| `test_real_file_read` | `E2E=1` | Reads 67MB file successfully |
| `test_memory_usage` | `E2E=1` | Documents but doesn't fail on high RAM |

---

## Test Infrastructure Code

### Base Test Class (Python 2.7 Compatible)

```python
# tests/conftest.py
import unittest
import subprocess
import tempfile
import shutil
import os
import time
import threading

class BeetFSTestCase(unittest.TestCase):
    """Base class for beetfs e2e tests - Python 2.7 compatible"""
    
    MOUNT_TIMEOUT = 30  # seconds
    
    @classmethod
    def setUpClass(cls):
        """Check FUSE availability"""
        try:
            with open(os.devnull, 'w') as devnull:
                subprocess.check_call(['which', 'fusermount'], 
                                     stdout=devnull, stderr=devnull)
        except subprocess.CalledProcessError:
            raise unittest.SkipTest("fusermount not available")
    
    def setUp(self):
        self.mount_dir = tempfile.mkdtemp(prefix='beetfs_test_')
        self.fs_process = None
    
    def mount_beetfs(self, library_path=None):
        """Mount beetfs in background with timeout"""
        cmd = ['python', '-c', 
               'from beetsplug.beetFs import mount; mount()']
        # Add mount point and other args as needed
        
        self.fs_process = subprocess.Popen(
            cmd,
            stdout=open(os.devnull, 'w'),
            stderr=subprocess.STDOUT
        )
        
        # Python 2.7 timeout workaround
        timer = threading.Timer(self.MOUNT_TIMEOUT, self._timeout_kill)
        timer.start()
        
        try:
            self._wait_for_mount()
        finally:
            timer.cancel()
    
    def _timeout_kill(self):
        if self.fs_process and self.fs_process.poll() is None:
            self.fs_process.kill()
    
    def _wait_for_mount(self):
        """Wait for filesystem to be mounted"""
        start = time.time()
        while time.time() - start < self.MOUNT_TIMEOUT:
            if os.path.ismount(self.mount_dir):
                return
            if self.fs_process.poll() is not None:
                self.fail("Filesystem process terminated prematurely")
            time.sleep(0.1)
        self.fail("Mount timeout after {} seconds".format(self.MOUNT_TIMEOUT))
    
    def tearDown(self):
        """Cleanup: unmount and kill process"""
        if self.fs_process:
            with open(os.devnull, 'w') as devnull:
                subprocess.call(['fusermount', '-z', '-u', self.mount_dir],
                              stdout=devnull, stderr=devnull)
            
            self.fs_process.terminate()
            
            # Wait for termination (Py2.7 compatible)
            start = time.time()
            while time.time() - start < 5:
                if self.fs_process.poll() is not None:
                    break
                time.sleep(0.1)
            else:
                self.fs_process.kill()
        
        shutil.rmtree(self.mount_dir, ignore_errors=True)
```

### Synthetic FLAC Generator

```python
# tests/conftest.py (continued)
import subprocess
import tempfile
import os

def create_synthetic_flac(duration_sec=5, artist="Test Artist", 
                          title="Test Track", album="Test Album"):
    """Create minimal FLAC with known metadata (~500KB for 5s silence)"""
    wav_fd, wav_path = tempfile.mkstemp(suffix='.wav')
    os.close(wav_fd)
    flac_path = wav_path.replace('.wav', '.flac')
    
    try:
        # Generate silence WAV
        subprocess.check_call([
            'ffmpeg', '-f', 'lavfi', '-i', 
            'anullsrc=r=44100:cl=stereo', '-t', str(duration_sec),
            '-y', wav_path
        ], stdout=open(os.devnull, 'w'), stderr=subprocess.STDOUT)
        
        # Convert to FLAC with metadata
        subprocess.check_call([
            'flac', '--best', 
            '-T', 'ARTIST={}'.format(artist),
            '-T', 'TITLE={}'.format(title),
            '-T', 'ALBUM={}'.format(album),
            '-o', flac_path, wav_path
        ], stdout=open(os.devnull, 'w'), stderr=subprocess.STDOUT)
        
        return flac_path
    finally:
        if os.path.exists(wav_path):
            os.unlink(wav_path)
```

---

## Dependencies to Add to flake.nix

```nix
# In devShell buildInputs, add:
pkgs.ffmpeg    # For synthetic FLAC generation
pkgs.flac      # For FLAC encoding

# pythonEnv already has mutagen for verification
```

---

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Memory explosion | High | Use 5-10MB synthetic FLACs, skip 650MB tests by default |
| Nested methods bug | Critical | Tests will expose; fix required before other tests pass |
| Python 2.7 EOL | Medium | Nix provides isolated environment |
| Global state pollution | Medium | Fresh subprocess per test |
| FUSE permissions | Low | Run as regular user, skip privileged tests |
| Concurrent access | Low | Single-threaded mode, sequential tests |

---

## Success Criteria

1. **All smoke tests pass** - beetfs mounts and unmounts cleanly
2. **Nested bug exposed and fixed** - All FUSE methods callable
3. **Metadata overlay verified** - Reads return DB metadata, not file metadata
4. **Writes update DB** - Metadata changes persist
5. **Errors handled gracefully** - Correct errno for unsupported ops
6. **No crashes on edge cases** - Unicode, special chars, concurrent access

---

## Findings from Test Execution

### Bug #1: Nested Methods (CRITICAL)

**Location**: `beetFs.py` lines 758-1144

**Problem**: All FUSE operation methods are indented inside the `access()` method, making them local functions instead of class methods.

**Evidence**:
```python
def access(self, path, flags):   # Line 723 - correct class method
    ...
    return 0

    def readdir(self, path, ...):  # Line 931 - WRONG! Nested inside access()
        ...
    def open(self, path, flags):   # Line 988 - Also nested
        ...
    def read(self, path, ...):     # Line 1077 - Also nested
        ...
```

**Symptom**: `os.listdir()` returns `OSError: [Errno 38] Function not implemented`

**Fix Required**: Dedent lines 758-1144 by 8 spaces to make them class methods.

### Bug #2: Directory Tree Building

**Location**: `beetFs.py` lines 403-414 (`FSNode.getnode()` and `FSNode.adddir()`)

**Problem**: When adding files to the directory structure, the code assumes parent directories already exist.

**Evidence**:
```
KeyError: u'Test Artist'
File "beetFs.py", line 403, in getnode
    return self.getnode(elements, root=root.dirs[topdir])
```

**Symptom**: Mount fails when library contains tracks.

### Bug #3: Unmount Not Clean

**Problem**: After unmounting, `os.path.ismount()` still returns `True`.

**Likely Cause**: FUSE process not terminating properly, or lazy unmount not completing.

---

## Notes from Oracle Review

1. **MP3 is not "readonly"** - metadata overlay is disabled (`bound=0`), but reads still work
2. **Write returns None for MP3** - no explicit return in MP3 path (falls through)
3. **Path format is hardcoded** - tests must match `$artist/$album ($year) [$format_upper]/$track - $artist - $title.$format`
4. **basestring vs str** - use `isinstance(x, basestring)` for Py2.7 string checks
5. **Global variables** - `library`, `directory_structure` must be reset between tests (use subprocesses)
