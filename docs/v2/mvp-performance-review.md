# MusicFS MVP Performance Review

**Date**: 2026-05-12  
**Test Data**: Metallica - 72 Seasons (12 FLAC tracks, 625MB, 16-bit/44.1kHz)  
**Origin**: Local filesystem (Docker volume)  
**System**: Linux, NixOS

---

## Executive Summary

**Phase 1 MVP is functional** - the system mounts, browses, and reads files successfully. Audio playback works with valid FLAC headers served. However, there's a **critical gap** between the architecture specification and current implementation regarding content chunking.

---

## Benchmark Results

### Throughput Comparison

| Metric | Direct FS | MusicFS Cold | MusicFS Warm | Target (Spec) | Status |
|--------|-----------|--------------|--------------|---------------|--------|
| Single file read (64MB) | 0.022s (3 GB/s) | 0.035s (1.8 GB/s) | 0.020s (3.2 GB/s) | >500 MB/s | ✅ |
| Full album read (625MB) | 0.149s (4.2 GB/s) | 0.274s (2.3 GB/s) | 0.211s (3.0 GB/s) | >500 MB/s | ✅ |

### Metadata Operations

| Operation | Result | Target (Spec) | Status |
|-----------|--------|---------------|--------|
| Root listing | 0.006s | <10ms | ✅ |
| Full tree traversal (12 files) | 0.007s | <50ms | ✅ |
| stat() per operation | 0.003s | <1ms | ⚠️ |
| 4KB small reads (per op) | 0.006s | <1ms | ⚠️ |
| Random seek 1MB | 0.008-0.015s | <50ms | ✅ |
| Mount time | ~8ms | <500ms | ✅ |

### Cache Performance

| Metric | Value |
|--------|-------|
| Cache speedup (single file) | 1.75x |
| Cache speedup (full album) | 1.30x |
| Cache size | 25MB |
| Chunk count | 12 |
| Expected cache size | 625MB |

### FUSE Overhead

| Scenario | Overhead vs Direct |
|----------|-------------------|
| Single file cold cache | 59% slower |
| Single file warm cache | 9% faster* |
| Full album cold cache | 84% slower |
| Full album warm cache | 42% slower |

*Warm cache appears faster due to OS page cache effects on both paths.

---

## What's Working Well ✅

### 1. Mount Performance
- Mount completes in ~8ms (spec: <500ms) — **62x better than target**
- O(1) mount time achieved — no file scanning blocks mount
- Lazy loading working as designed per architecture section 4.3.1

### 2. Virtual Tree Organization
- Correct Artist/Album/Track hierarchy derived from metadata
- Example path: `/Metallica/72 Seasons/01. 72 Seasons.flac`
- Special character sanitization working (`/`, `\`, `:`, etc.)

### 3. File Reading
- Valid FLAC headers served (`fLaC` magic bytes verified)
- Sequential reads work correctly
- Random access (seek) functional
- Concurrent reads from multiple processes work

### 4. FUSE Integration
- Read-only enforcement (EROFS returned on write attempts)
- Proper inode assignment and file attributes
- AllowOther mount option working
- Clean unmount via fusermount3

### 5. Throughput
- Exceeds 500 MB/s target significantly (2-3 GB/s achieved)
- Parallel reads scale appropriately (4 files in 0.060s)

---

## Critical Issues 🔴

### Issue 1: Incomplete File Caching

**Symptom**: Cache is 25MB instead of expected 625MB (12 files × ~2MB each instead of full files)

**Root Cause**: In `fetcher.rs:74`:
```rust
let data = origin.read(&meta.real_path.path, 0, meta.size as u32).await?;
```

And in `local.rs:96-98`:
```rust
let mut buffer = vec![0u8; size as usize];
let bytes_read = file.read(&mut buffer).await?;
buffer.truncate(bytes_read);
```

`tokio::fs::File::read()` reads **up to** buffer size but returns when the kernel buffer is exhausted (~2MB typical). Only first ~2MB of each file is being cached.

**Impact**: 
- Subsequent reads beyond 2MB offset hit origin every time
- No cache benefit for majority of file content
- Cache eviction policy not being exercised

**Required Fix**: Use `read_to_end()` or loop until all bytes read:
```rust
let mut buffer = Vec::with_capacity(size as usize);
file.read_to_end(&mut buffer).await?;
```

### Issue 2: No CDC Chunking Implemented

**Architecture Spec** (Section 4.3.2):
> "All file content is stored as content-addressed chunks... Avg chunk: 64KB, Min: 16KB, Max: 256KB"

**Current Implementation**: Each file stored as ONE chunk (no FastCDC integration)

**Impact**:
- No content deduplication possible
- Delta sync impossible (FR-11.2 unmet)
- Cache efficiency severely reduced for similar files

---

## Architecture Gaps 🟡

| Spec Requirement | Current State | Gap |
|------------------|---------------|-----|
| CDC chunking (64KB avg) | No chunking | Missing FastCDC integration |
| Delta sync (>90% bandwidth reduction) | Not implemented | Requires CDC first |
| Deduplication (FR-20) | Not implemented | Requires CDC first |
| Search engine (tantivy) | Not implemented | Phase 3 scope |
| gRPC Control API | Not implemented | Phase 4 scope |
| Multi-origin federation | Single origin only | Phase 2 scope |
| Metadata persistence (SQLite) | In-memory HashMap | Missing persistence |

---

## Performance Analysis

### Why Warm Cache Appears Faster Than Direct FS

The warm cache shows 3.2 GB/s vs direct 3.0 GB/s because:
1. OS page cache is warm for both MusicFS chunks AND origin files
2. Both measurements are essentially hitting RAM, variance expected
3. MusicFS chunks may have slightly better cache locality

### stat() Latency Above Target

Current: 3ms per stat() vs target <1ms

Possible causes:
1. `RwLock<VirtualTree>` contention overhead
2. HashMap lookup plus FUSE context switch
3. Measurement includes full round-trip through FUSE

Mitigation options:
- Consider lock-free concurrent data structures
- Implement finer-grained locking
- Cache hot inodes in separate fast-path structure

---

## Recommendations

### Immediate Fixes (Before Phase 2)

1. **Fix file reading** — Use `read_to_end()` or implement proper streaming read loop
2. **Add CDC chunking** — Integrate FastCDC per architecture spec section 4.3.2
3. **Persist metadata** — Move from in-memory HashMap to SQLite as specified

### Phase 2 Priorities

1. Complete CDC chunking implementation (prerequisite for delta sync)
2. Add SQLite metadata persistence (FR-7.2)
3. Implement multi-origin support (FR-13)

### Testing Gaps to Address

1. No automated E2E tests for real FUSE operations
2. No stress testing with concurrent access patterns
3. No large library testing (target: 1M+ files per NFR-3.1)
4. No offline mode testing (origin unavailable scenarios)

---

## Test Environment Details

```
Origin Path: /home/fujin/.local/share/docker/volumes/containers_downloads/_data/Metallica - 72 Seasons (2023) [FLAC] 88/
Mount Point: /tmp/musicfs-benchmark/mount
Cache Dir: /tmp/musicfs-benchmark/cache
Binary: target/release/musicfs (via nix develop)

Files:
  01. 72 Seasons.flac        64MB
  02. Shadows Follow.flac    50MB
  03. Screaming Suicide.flac 45MB
  04. Sleepwalk My Life Away.flac 54MB
  05. You Must Burn!.flac    57MB
  06. Lux Æterna.flac        27MB
  07. Crown Of Barbed Wire.flac 46MB
  08. Chasing Light.flac     55MB
  09. If Darkness Had A Son.flac 51MB
  10. Too Far Gone_.flac     37MB
  11. Room Of Mirrors.flac   45MB
  12. Inamorata.flac         89MB
  Total: 625MB, 12 tracks
```

---

## Conclusion

**The MVP demonstrates core functionality works** — mounting, browsing, and reading audio files through FUSE. Throughput performance exceeds targets significantly.

However, **the cache implementation is incomplete**:
- Only ~4% of file content is being cached (25MB/625MB)
- No CDC chunking means no deduplication or delta sync capability
- Architecture requirements FR-8.2, FR-11.2, FR-20 are unmet

**Recommendation**: Fix the file reading issue and add CDC chunking before proceeding to Phase 2. The architecture is sound; implementation needs to catch up to specification.

---

## References

- [Architecture Specification](architecture.md) — Section 4.3.2 (CAS), Section 4.3.5 (Read Flow)
- [Requirements Specification](requirements.md) — FR-8 (Content Cache), FR-11 (Delta Sync), FR-20 (CAS)
- [Week 4b Plan](plans/week-04b-origin-connector.md) — ContentFetcher implementation
