# beetfs Benchmark Plan

## Executive Summary

Benchmark suite to measure beetfs FUSE filesystem performance across mount time, metadata operations, file I/O, and memory usage. Focus on realistic music library workloads.

## Critical Performance Findings (Pre-Benchmark)

### Architecture Bottlenecks Identified

| Bottleneck | Location | Impact |
|------------|----------|--------|
| **Full file load into RAM** | `FileHandler.__init__` line 481 | 50-100MB per open FLAC |
| **Mount-time bulk load** | `mount()` line 143 | O(N) for N library items |
| **GIL serialization** | Python 2.7 | Single-core limit for metadata ops |
| **Per-file DB lookup** | `getattr()`, `access()` | SQLite query per stat call |

### Expected Performance Characteristics

| Operation | Expected Performance | Bottleneck |
|-----------|---------------------|------------|
| Mount (10K items) | 5-30 seconds | `lib.items()` + FSNode construction |
| readdir | Fast (in-memory dict) | None |
| getattr (file) | Slow (~1ms) | DB lookup + real file stat |
| open (first) | Very slow | Full file read into RAM |
| read | Fast | Memory-to-memory copy |
| Memory (10 open files) | 500MB-1GB | FileHandler caches entire files |

---

## Benchmark Tools

### Primary Tools

| Tool | Purpose | Install |
|------|---------|---------|
| **fio** | I/O throughput, IOPS, latency | `nix-shell -p fio` |
| **mdtest** | Metadata operations (stat, readdir) | `nix-shell -p ior` |
| **hyperfine** | Mount time, command timing | `nix-shell -p hyperfine` |
| **time** | Basic timing | builtin |
| **/usr/bin/time -v** | Memory usage (maxrss) | builtin |

### Measurement Scripts

All benchmarks use synthetic FLAC files (5-10MB) to avoid I/O variance from real storage.

---

## Benchmark Categories

### 1. Mount Time Scaling

**Goal**: Measure how mount time scales with library size.

**Method**:
```bash
# Create libraries with N items: 100, 1K, 10K, 50K, 100K
hyperfine --warmup 1 --runs 5 \
  'beet mount /mnt/beetfs && sleep 1 && fusermount -u /mnt/beetfs'
```

**Metrics**:
- Time to mount (seconds)
- Memory usage at mount completion (RSS)

**Expected scaling**: O(N) - linear with library size

**Test matrix**:
| Library Size | Expected Mount Time | Expected Memory |
|--------------|--------------------:|----------------:|
| 100 items | <1s | ~50MB |
| 1,000 items | 1-3s | ~60MB |
| 10,000 items | 5-15s | ~100MB |
| 50,000 items | 30-60s | ~300MB |
| 100,000 items | 60-120s | ~500MB |

---

### 2. Metadata Operations (stat/readdir)

**Goal**: Measure getattr and readdir performance - critical for music players that scan libraries.

#### 2a. Single stat latency

```bash
# Measure single stat call latency
hyperfine --warmup 10 --runs 100 \
  'stat /mnt/beetfs/Artist/Album/01-Track.flac'
```

**Target**: <5ms average, <20ms p99

#### 2b. Bulk stat (library scan simulation)

```bash
# Stat all files in library
hyperfine --warmup 1 --runs 5 \
  'find /mnt/beetfs -type f -exec stat {} + > /dev/null'
```

**Metrics**:
- Total time for N files
- stat operations per second
- p50, p95, p99 latency

**Target**: >500 stat/s (Python FUSE baseline)

#### 2c. Directory listing

```bash
# List directory with N entries
hyperfine --warmup 3 --runs 10 \
  'ls /mnt/beetfs/Artist/Album/'
```

**Test matrix**:
| Directory entries | Target time |
|------------------:|------------:|
| 10 | <50ms |
| 100 | <100ms |
| 1,000 | <500ms |

---

### 3. File Open Performance

**Goal**: Measure file open latency - the critical bottleneck due to full file load.

#### 3a. First open (cold)

```bash
# Clear any caches, then open file
echo 3 > /proc/sys/vm/drop_caches
hyperfine --warmup 0 --runs 10 \
  'head -c 1 /mnt/beetfs/Artist/Album/01-Track.flac > /dev/null'
```

**Test matrix**:
| File size | Expected open time |
|----------:|-------------------:|
| 5MB | 50-200ms |
| 20MB | 200-500ms |
| 50MB | 500ms-1s |
| 100MB | 1-2s |

#### 3b. Cached open (warm)

```bash
# File already opened once
hyperfine --warmup 5 --runs 50 \
  'head -c 1 /mnt/beetfs/Artist/Album/01-Track.flac > /dev/null'
```

**Target**: <10ms (should hit FileHandler cache)

---

### 4. Read Throughput

**Goal**: Measure sequential and random read performance.

#### 4a. Sequential read

```bash
fio --name=seq_read \
    --filename=/mnt/beetfs/Artist/Album/01-Track.flac \
    --rw=read --bs=1M --direct=0 \
    --ioengine=sync --numjobs=1 \
    --runtime=30 --time_based
```

**Metrics**: MB/s throughput

**Target**: >100 MB/s (memory-backed after first read)

#### 4b. Random read (simulates seeking in audio player)

```bash
fio --name=rand_read \
    --filename=/mnt/beetfs/Artist/Album/01-Track.flac \
    --rw=randread --bs=64k --direct=0 \
    --ioengine=sync --numjobs=1 \
    --runtime=30 --time_based
```

**Metrics**: IOPS, latency histogram

---

### 5. Memory Usage

**Goal**: Measure memory consumption under load.

#### 5a. Idle memory (mounted, no activity)

```bash
# Mount and measure RSS
beet mount /mnt/beetfs &
sleep 5
ps -o rss= -p $(pgrep -f beetfs)
```

#### 5b. Memory per open file

```bash
# Open N files, measure memory growth
for i in 1 5 10 20; do
  # Open $i files simultaneously
  cat /mnt/beetfs/Artist/Album/0{1..$i}*.flac > /dev/null &
  ps -o rss= -p $(pgrep -f beetfs)
done
```

**Expected**: ~file_size × open_files (FileHandler caches entire file)

#### 5c. Memory leak detection

```bash
# Repeatedly open/close files, check for memory growth
for i in {1..100}; do
  cat /mnt/beetfs/Artist/Album/01-Track.flac > /dev/null
done
# Compare RSS before and after
```

---

### 6. Concurrent Access

**Goal**: Measure performance under parallel access (multiple processes).

```bash
# Parallel stat operations
hyperfine --warmup 1 --runs 5 \
  'seq 1 100 | xargs -P 4 -I {} stat /mnt/beetfs/Artist/Album/0{}-Track.flac'
```

**Metrics**:
- Throughput scaling with parallelism (1, 2, 4, 8 workers)
- Latency degradation

**Expected**: Limited scaling due to Python GIL

---

### 7. Realistic Workloads

#### 7a. Music player library scan

Simulates: Rhythmbox/Clementine scanning library at startup

```bash
# Recursive stat + readdir
time find /mnt/beetfs -type f -name "*.flac" -exec stat {} + | wc -l
```

#### 7b. Album playback

Simulates: Playing 12-track album sequentially

```bash
# Open each file, read 1MB (simulate buffering), close
for f in /mnt/beetfs/Artist/Album/*.flac; do
  dd if="$f" of=/dev/null bs=1M count=1 2>/dev/null
done
```

#### 7c. Metadata edit

Simulates: Editing tags in Picard/Kid3

```bash
# Open file, write to header region, close
# (Requires write support to be functional)
```

---

## Baseline Comparisons

### Reference Filesystems

| Filesystem | Purpose |
|------------|---------|
| **ext4 (local)** | Best-case baseline |
| **fuse-passthrough** | FUSE overhead baseline |
| **sshfs** | Network FUSE comparison |

### Comparison Method

Run identical benchmarks on:
1. Real music files on ext4
2. Same files via FUSE passthrough
3. Same files via beetfs

Calculate overhead: `(beetfs_time - ext4_time) / ext4_time × 100%`

---

## Test Environment

### Hardware Requirements

- CPU: 4+ cores (to test GIL impact)
- RAM: 8+ GB (for large library tests)
- Storage: SSD recommended (reduces I/O variance)

### Software Requirements

```nix
# Add to flake.nix devShell
buildInputs = [
  fio
  hyperfine
  # ior  # includes mdtest
];
```

### Cache Control

```bash
# Clear all caches before cold benchmarks
sync
echo 3 > /proc/sys/vm/drop_caches

# Disable kernel FUSE caching for accurate measurements
mount -o entry_timeout=0,attr_timeout=0,negative_timeout=0
```

---

## Success Criteria

### Minimum Viable Performance

| Metric | Minimum | Target | Excellent |
|--------|--------:|-------:|----------:|
| Mount time (10K items) | <60s | <15s | <5s |
| stat latency (avg) | <20ms | <5ms | <1ms |
| stat throughput | >100/s | >500/s | >2000/s |
| File open (50MB, cold) | <5s | <1s | <200ms |
| Read throughput | >50 MB/s | >200 MB/s | >500 MB/s |
| Memory (idle, 10K items) | <500MB | <100MB | <50MB |
| Memory per open file | <2× file size | <1.5× | <1.1× |

### Regression Detection

Any benchmark result >20% worse than baseline triggers investigation.

---

## Implementation Notes

### Test Data Generation

Use existing test infrastructure from `tests/conftest.py`:
- `create_synthetic_flac()` - generates valid FLAC files
- `BeetFSTestCase` - creates isolated beets library

### Benchmark Script Structure

```
beetfs/
├── benchmarks/
│   ├── run_all.sh          # Master script
│   ├── bench_mount.sh      # Mount time tests
│   ├── bench_metadata.sh   # stat/readdir tests
│   ├── bench_io.sh         # Read/write throughput
│   ├── bench_memory.sh     # Memory profiling
│   └── results/            # Output directory
│       ├── mount_scaling.csv
│       ├── stat_latency.csv
│       └── ...
```

### Output Format

```csv
# Example: mount_scaling.csv
library_size,mount_time_ms,memory_rss_kb,timestamp
100,450,52000,2024-01-15T10:30:00
1000,2100,61000,2024-01-15T10:31:00
10000,12500,98000,2024-01-15T10:33:00
```

---

## Known Limitations

1. **Python 2.7 GIL**: Cannot achieve true parallelism - expect flat scaling beyond 1 core
2. **FileHandler memory**: Each open file = full file in RAM - will OOM with many large files
3. **No lazy loading**: All library items loaded at mount - slow for large libraries
4. **SQLite single-writer**: Concurrent writes will serialize

## Optimization Opportunities (Post-Benchmark)

Based on benchmark results, consider:

1. **Lazy FSNode construction** - Build tree on first access, not mount
2. **Memory-mapped file access** - mmap instead of full read
3. **LRU cache for FileHandler** - Evict old files instead of holding all
4. **Metadata caching** - Cache getattr results, invalidate on DB change
5. **Batch DB queries** - Prefetch metadata for directory listings
