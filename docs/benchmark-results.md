# beetfs Benchmark Results

**Date**: 2026-05-12  
**Status**: ❌ ALL BENCHMARKS BLOCKED BY BUGS

## Executive Summary

Benchmarks cannot complete due to critical bugs in beetfs. The implementation is non-functional for any library with content.

## Results

| Benchmark | Status | Mean | Error |
|-----------|--------|------|-------|
| mount_time | ❌ FAIL | N/A | Directory tree building bug |
| readdir | ❌ FAIL | N/A | Directory tree building bug |
| stat_latency | ❌ FAIL | N/A | Directory tree building bug |
| enoent_lookup | ❌ FAIL | N/A | Directory tree building bug |
| file_open | ❌ FAIL | N/A | Directory tree building bug |
| read_throughput | ❌ FAIL | N/A | Directory tree building bug |
| memory_usage | ❌ FAIL | N/A | Directory tree building bug |

## Blocking Bugs

### Bug #1: Nested Methods (Lines 758-1144)

All FUSE operations (`readdir`, `open`, `read`, `write`, etc.) are indented inside the `access()` method, making them local functions instead of class methods.

**Impact**: Even if mount succeeds, all file operations return `ENOSYS (Function not implemented)`.

**Fix Required**: Dedent lines 758-1144 by 8 spaces.

### Bug #2: Directory Tree Building (Lines 403-414)

`FSNode.adddir()` calls `getnode()` which assumes parent directories already exist. When building the tree for a new library, parent directories haven't been created yet.

**Error**:
```
KeyError: u'Bench Artist'
  File "beetFs.py", line 403, in getnode
    return self.getnode(elements, root=root.dirs[topdir])
```

**Impact**: Mount crashes when library contains any tracks.

**Fix Required**: `adddir()` must create parent directories recursively before adding child.

### Bug #3: Empty Library Only

The only working configuration is mounting with an empty beets library:
- `test_mount_empty_library`: ✅ PASS
- Any library with tracks: ❌ CRASH

## Test Environment

- **Python**: 2.7.15
- **OS**: Linux (NixOS)
- **Test data**: 10 synthetic FLAC files (5 MB each)
- **Beets**: 1.4.9

## Benchmark Configuration

```python
num_tracks = 10
track_size_mb = 5
mount_runs = 3
stat_runs = 20
readdir_runs = 10
```

## Raw Results

See `benchmarks/results/benchmark_results.json` for full JSON output.

## Next Steps

1. **Fix Bug #2** (directory tree building) - allows mount with content
2. **Fix Bug #1** (nested methods) - allows FUSE operations to work
3. **Re-run benchmarks** - get actual performance numbers

## Conclusion

**beetfs is currently non-functional** for real-world use. Both bugs must be fixed before performance can be measured. The test infrastructure and benchmark suite are ready; only the implementation needs repair.

---

## Appendix: E2E Test Results (For Reference)

From the e2e test suite (74 tests):

| Category | Passed | Failed | Errors |
|----------|--------|--------|--------|
| Smoke tests | 4 | 3 | 0 |
| Nested bug detection | 3 (confirmed bug) | 10 | 0 |
| Readdir | 0 | 10 | 0 |
| Stat | 0 | 8 | 0 |
| Read | 0 | 11 | 0 |
| Write | 0 | 7 | 0 |
| Error handling | 0 | 7 | 3 |
| **Total** | **12** | **56** | **3** |

The 12 passing tests are infrastructure tests and tests that verify the bugs exist.
