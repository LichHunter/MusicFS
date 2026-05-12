# beetfs Performance Analysis

## Executive Summary

beetfs has significant performance limitations due to its 2010-era design assumptions. The primary issues are **full file loading into RAM** and **blocking I/O on file open**.

---

## 1. Latency Analysis

### Operation Latencies

| Operation | Time Complexity | Typical Latency | Notes |
|-----------|-----------------|-----------------|-------|
| **File Open** | O(file_size) | 50ms - 1s+ | Reads entire file into memory |
| **File Read** | O(1) | <1ms | Pure memory slice |
| **File Write** | O(file_size) | 100ms - 2s+ | Reconstructs + DB write |
| **Directory List** | O(n) | <10ms | In-memory tree traversal |
| **getattr** | O(depth) | <1ms | Tree navigation + stat |

### File Open Breakdown

The file open operation is the critical bottleneck:

```
Time breakdown for opening 50MB FLAC file:
┌────────────────────────────────────────────────────────────┐
│  1. open() syscall                          │     ~1ms    │
│  2. file_object.read() - load entire file   │  ~100-200ms │
│  3. InterpolatedFLAC() - parse FLAC         │   ~20-50ms  │
│  4. Inject DB metadata                      │     ~1ms    │
│  5. get_header() - generate new header      │   ~10-20ms  │
│  6. Seek to audio offset                    │     ~1ms    │
│  7. Read audio into music_data              │  ~100-200ms │
├────────────────────────────────────────────────────────────┤
│  TOTAL                                      │  ~230-470ms │
└────────────────────────────────────────────────────────────┘
```

**Code Evidence** (lines 461-483):
```python
# Step 2-5: Load and parse entire file
self.inf = InterpolatedFLAC(self.file_object.read())  # FULL FILE READ
self.inf["title"] = self.item.title
# ...
self.header = self.inf.get_header(self.real_path)

# Step 6-7: Cache all audio data
self.file_object.seek(self.music_offset)
self.music_data = self.file_object.read()  # ANOTHER FULL READ
```

### Read Operation (Post-Open)

After file is opened, reads are fast:

```python
def read(self, size, offset):
    if offset < self.bound:
        return self.header[offset:offset+size]  # Memory slice: O(1)
    else:
        return self.music_data[offset - len(self.header):...]  # Memory slice: O(1)
```

### Write Operation

Writes to header area trigger expensive reconstruction:

```
Time breakdown for tag write:
┌────────────────────────────────────────────────────────────┐
│  1. Reconstruct filedata in memory          │   ~10-50ms  │
│  2. Parse as InterpolatedFLAC               │   ~20-50ms  │
│  3. Extract tag values                      │     ~1ms    │
│  4. lib.store() + lib.save() (SQLite)       │   ~10-50ms  │
│  5. Regenerate header                       │   ~10-20ms  │
├────────────────────────────────────────────────────────────┤
│  TOTAL                                      │   ~50-170ms │
└────────────────────────────────────────────────────────────┘
```

---

## 2. Memory Footprint

### Per-File Memory Usage

```
┌─────────────────────────────────────────────────────────────────────┐
│                     FileHandler Memory Layout                        │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │  self.music_data (bytes)                                     │   │
│  │  Size: file_size - original_header_size                      │   │
│  │  Typical: 95-99% of file size                                │   │
│  │  Example: 48.5 MB for 50 MB file                             │   │
│  └─────────────────────────────────────────────────────────────┘   │
│                                                                     │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │  self.header (bytes)                                         │   │
│  │  Size: Generated FLAC header with DB metadata                │   │
│  │  Typical: 4 KB - 64 KB (depends on metadata + padding)       │   │
│  └─────────────────────────────────────────────────────────────┘   │
│                                                                     │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │  self.inf (InterpolatedFLAC)                                 │   │
│  │  Size: Parsed metadata blocks + internal state               │   │
│  │  Typical: 10 KB - 100 KB                                     │   │
│  └─────────────────────────────────────────────────────────────┘   │
│                                                                     │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │  Other attributes                                            │   │
│  │  path, real_path, item reference, format, etc.               │   │
│  │  Typical: ~1 KB                                              │   │
│  └─────────────────────────────────────────────────────────────┘   │
│                                                                     │
├─────────────────────────────────────────────────────────────────────┤
│  TOTAL per file: ~1.0x - 1.1x original file size                    │
└─────────────────────────────────────────────────────────────────────┘
```

### Memory Scaling

| Scenario | Files Open | Avg File Size | RAM Usage |
|----------|------------|---------------|-----------|
| Single track playback | 1 | 30 MB | ~32 MB |
| Album playback (gapless) | 2-3 | 30 MB | ~65-100 MB |
| Album fully opened | 10 | 30 MB | ~320 MB |
| Jellyfin library scan | 50-100 | 30 MB | **1.6 - 3.2 GB** |
| Full library scan | 1000 | 30 MB | **32 GB** (OOM) |

### Global Memory

```python
# Directory tree structure
directory_structure = FSNode({}, {})
# Memory: O(number_of_items)
# Typical: 1-10 MB for libraries with 10,000-100,000 tracks

# Open file handles
self.files = {}  # Dict[str, FileHandler]
# Memory: Sum of all FileHandler instances
# Unbounded - grows with concurrent opens
```

---

## 3. I/O Patterns

### Current (Inefficient)

```
File Open:
  Disk → [Read ALL] → RAM (music_data)
                    → RAM (inf object)
                    → RAM (header)

File Read:
  RAM (header or music_data) → Application

Total I/O: 1x-2x file size on open, 0 on read
```

### Optimal (Not Implemented)

```
File Open:
  Disk → [Read header only] → RAM (small)
  
File Read:
  If header region:
    RAM (header) → Application
  If audio region:
    Disk → [Seek + Read chunk] → Application

Total I/O: ~64KB on open, on-demand reads
```

---

## 4. Concurrency

### Current Model

```python
server.multithreaded = 0  # Single-threaded
```

**Implications:**
- All FUSE operations serialized
- One slow file open blocks everything
- No benefit from multi-core CPUs

### Impact on Use Cases

| Use Case | Impact |
|----------|--------|
| Single player (VLC) | Acceptable - one file at a time |
| Media server scan | Severe - sequential processing |
| Multiple clients | Severe - requests queue up |
| Concurrent reads | Moderate - reads are fast once open |

---

## 5. Benchmarks (Theoretical)

Based on code analysis, not actual measurements:

### File Open Time vs Size

```
File Size    Open Time (HDD)    Open Time (SSD)
────────────────────────────────────────────────
  10 MB         50-100 ms          20-50 ms
  30 MB        150-300 ms          50-100 ms
  50 MB        250-500 ms         100-200 ms
 100 MB        500-1000 ms        200-400 ms
 200 MB       1000-2000 ms        400-800 ms
```

### Memory vs Concurrent Opens

```
Open Files    RAM Usage (30MB avg)
─────────────────────────────────────
     1             ~32 MB
     5            ~160 MB
    10            ~320 MB
    25            ~800 MB
    50           ~1.6 GB
   100           ~3.2 GB
```

---

## 6. Comparison with Alternatives

| Metric | beetfs | Direct File | NFS | FUSE passthrough |
|--------|--------|-------------|-----|------------------|
| Open latency | 200-500ms | <10ms | 10-50ms | <10ms |
| Read latency | <1ms | <1ms | 1-10ms | <1ms |
| Memory/file | ~1x size | ~0 | ~0 | ~0 |
| Metadata source | Database | File | File | File |
| Modify original | No | Yes | Yes | Yes |

---

## 7. Recommendations

### For Current Usage

1. **Limit concurrent opens** - Don't scan full library
2. **Use SSDs** - Reduces open latency by 2-3x
3. **Increase RAM** - Expect 1x file size per open
4. **Avoid large files** - 24-bit/192kHz FLACs are problematic

### For Modernization

1. **Implement lazy loading** - Read audio on demand
2. **Add file handle caching** - Keep headers, release audio
3. **Enable multi-threading** - Parallelize opens
4. **Add memory limits** - Evict old FileHandlers
