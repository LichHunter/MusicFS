# AGENTS.md — beetfs/musicfs

> Everything an AI agent needs to get work done on this project.

---

## Quick Start

```bash
cd beetfs/musicfs
nix develop                    # Enter dev shell (ALL tooling provided)
cargo check                    # Verify compilation
cargo test                     # Run all tests (162 tests, ~10s)
cargo build --release          # Build release binary
```

**No `rustup`, no `apt install`, no manual dependency management.** The Nix flake provides everything: Rust stable + rust-analyzer + clippy + rustfmt, FUSE3, SQLite, OpenSSL, protobuf, grpcurl, cargo-nextest, cargo-criterion, lld linker.

---

## Project Overview

MusicFS is a **read-only FUSE filesystem** that presents music libraries organized by metadata (artist/album/track) instead of physical file paths. It supports multiple storage backends (local, NFS, S3, SFTP), content-addressable caching with delta sync, and full-text search.

**Key constraint**: Read-only. Never modifies origin files. Never pushes changes to the origin server.

---

## Repository Layout

```
beetfs/
├── musicfs/                        # Rust implementation (active)
│   ├── Cargo.toml                  # Workspace root
│   ├── flake.nix                   # Nix dev shell
│   ├── .cargo/config.toml          # LLD linker, aliases (t/c/b)
│   ├── crates/                     # 11 workspace crates
│   │   ├── musicfs-cli/            # Binary entry point (clap)
│   │   ├── musicfs-core/           # Types, errors, config, events
│   │   ├── musicfs-fuse/           # FUSE ops (fuser)
│   │   ├── musicfs-metadata/       # Audio parsing (symphonia)
│   │   ├── musicfs-cache/          # Cache: tree, metadata, patterns, eviction
│   │   ├── musicfs-cas/            # Content-addressable store (sled + chunks)
│   │   ├── musicfs-origins/        # Origin backends: local, NFS, SMB, S3, SFTP
│   │   ├── musicfs-sync/           # Delta sync, CDC chunking (fastcdc), watcher
│   │   ├── musicfs-search/         # Full-text search (tantivy)
│   │   ├── musicfs-grpc/           # gRPC control API (tonic + prost)
│   │   └── musicfs-plugins/        # Plugin system (native .so + WASM)
│   ├── tests/
│   │   ├── e2e/e2e_players.rs      # E2E: mpv/VLC playback (manual, #[ignore])
│   │   └── integration/            # (placeholder)
│   └── dist/                       # Deployment
│       ├── musicfs.service          # systemd unit
│       ├── config.example.toml      # Example config
│       ├── logrotate.d/musicfs      # Log rotation
│       ├── PKGBUILD                 # Arch package
│       └── musicfs.spec             # RPM spec
├── docs/
│   ├── templates/
│   │   ├── bluedoc.md              # Full design doc (5-20+ pages)
│   │   └── greendoc.md             # One-pager (1-2 pages)
│   ├── v2/
│   │   ├── architecture.md         # System design (GOLDEN — source of truth)
│   │   ├── requirements.md         # Functional + non-functional requirements
│   │   ├── development-plan.md     # Implementation roadmap (weeks 1-14)
│   │   └── plans/                  # Weekly plans, feature plans, research
│   └── v1/                         # Original Python beetfs docs (reference only)
└── beetsplug/beetFs.py             # Original Python implementation (archived)
```

---

## Build & Test Commands

```bash
# Cargo aliases (.cargo/config.toml)
cargo t                            # cargo test
cargo c                            # cargo check
cargo b                            # cargo build

# Common workflows
cargo check                        # Fast compile check
cargo test                         # All tests
cargo test -p musicfs-core         # Single crate
cargo clippy                       # Lint
cargo fmt                          # Format
cargo nextest run                  # Parallel test runner (faster)

# gRPC
cargo build -p musicfs-grpc        # Triggers proto codegen via build.rs
grpcurl -unix /run/musicfs.sock musicfs.v1.MusicFS/GetStatus

# Watch mode
cargo watch -x 'check' -x 'test'

# Release
cargo build --release
```

**Proto file location**: `crates/musicfs-grpc/proto/musicfs.proto` (codegen via `tonic-build` in `build.rs`)

---

## Architecture

**Read the full design**: [`docs/v2/architecture.md`](docs/v2/architecture.md) — this is the source of truth for all architectural decisions, component design, data schemas, API definitions, and diagrams.

**Quick orientation** — MusicFS is a workspace of 11 crates. The dependency flow is:

`musicfs-cli` → `musicfs-fuse` / `musicfs-grpc` / `musicfs-search` → `musicfs-core` → `musicfs-cache` / `musicfs-origins` / `musicfs-metadata` → `musicfs-cas` → `musicfs-sync`

Key concepts: Virtual Tree (in-memory directory structure from metadata), CAS (content-addressable chunk storage with sled index), Origin Federation (multi-backend with failover and health monitoring), CDC (content-defined chunking for delta sync), Event Bus (tokio broadcast for cross-component notifications).

For performance targets, scalability requirements, and quantitative constraints, see [`docs/v2/requirements.md`](docs/v2/requirements.md).

---

## Code Conventions

### Rust

- **Edition**: 2021, **MSRV**: 1.75+
- **Linker**: LLD via clang (configured in `.cargo/config.toml`)
- **Error handling**: `thiserror` for library errors, `anyhow` for CLI
- **Async**: `tokio` runtime, `async-trait` for trait objects
- **Concurrency**: `parking_lot` for hot-path locks, `dashmap` for concurrent maps, `std::sync::RwLock` elsewhere
- **Logging**: `tracing` with structured fields (`#[instrument]`, `info!`, `debug!`, etc.)
- **Serialization**: `serde` + `toml` for config, `rmp-serde` (msgpack) for binary data, `prost` for protobuf

### Never Do

- `as any`, `@ts-ignore` equivalents — no `unsafe` without justification
- Empty `catch` / `let _ = result` on operations that can fail meaningfully
- Suppress type errors
- Commit secrets or credentials

### Testing Patterns

- **Fixtures**: `TempDir::new().unwrap()` for isolated storage (used in 29 files)
- **In-memory DB**: `Database::open_memory()` for fast SQLite tests
- **No mocking framework** — tests use real implementations with temp directories
- **Async tests**: `#[tokio::test]`
- **Helper functions**: `make_file_meta()`, `mock_health()` — currently duplicated per module

---

## Golden Documents

These are the authoritative references. All implementations must match them.

| Document | Path | Role |
|----------|------|------|
| **Architecture** | `docs/v2/architecture.md` | System design — THE source of truth |
| **Requirements** | `docs/v2/requirements.md` | What to build (FR-*, NFR-*) |
| **Development Plan** | `docs/v2/development-plan.md` | How to build it (week-by-week) |
| **Proto Definition** | `crates/musicfs-grpc/proto/musicfs.proto` | API contract |

If code contradicts architecture.md, the architecture doc wins (unless explicitly superseded by a newer plan document).

---

## Documentation Rules

### Templates

Two templates exist in `docs/templates/`:

| Template | When to Use | Length | Review Level |
|----------|-------------|--------|-------------|
| **BlueDoc** | New systems, major architecture changes, new services | 5-20+ pages | Cross-functional |
| **GreenDoc** | Bug fixes, small features, optimizations, config changes | 1-2 pages | Peer review |

**Decision rule**: If any GreenDoc section needs more than 3 paragraphs, upgrade to a BlueDoc.

### When Neither Template Fits

If the work doesn't fit BlueDoc or GreenDoc (e.g., research summaries, audit reports, testing strategies, runbooks):
1. **Stop** — do not force-fit content into wrong template
2. **Propose** a new template format to the user with: name, intended use case, suggested structure
3. **Get approval** before writing the document
4. Save approved template to `docs/templates/{name}.md` for future use

### Document Metadata

Every document MUST have at the top:

```markdown
**Date**: YYYY-MM-DD
**Status**: [Draft / In-Review / Approved / Shipped / Obsolete]
**Prerequisites**: [links to dependent docs]
```

BlueDoc additionally requires: Authors, Reviewers, Approvers.

### Writing Conventions

- **Tables**: Use for requirements mapping, deliverables tracking, comparisons
- **Code blocks**: Include for implementation examples, config samples, commands
- **Checklists**: `[ ]` for exit criteria and success metrics
- **Section numbering**: Hierarchical (1., 1.1, 1.2)
- **Cross-references**: Relative markdown links (`[architecture](../architecture.md)`)
- **Requirement tracing**: Reference FR-X.Y / NFR-X.Y from requirements.md
- **Diagrams**: PlantUML or Mermaid (architecture.md uses PlantUML)

### Post-Task Documentation Check (MANDATORY)

After completing any non-trivial task, check whether documentation needs updating:

1. **Did the architecture change?** (new component, changed data flow, new dependency) → Update `docs/v2/architecture.md`
2. **Did requirements change?** (new constraint, relaxed target, dropped feature) → Update `docs/v2/requirements.md`
3. **Did the API change?** (new RPC, changed message, removed endpoint) → Update `proto/musicfs.proto` + `docs/api/`
4. **Did build/tooling change?** (new dependency, changed Nix flake, new cargo feature) → Update this `AGENTS.md`
5. **Did a plan get completed or invalidated?** → Update the plan's **Status** field (Draft → Shipped / Obsolete)
6. **Did the config format change?** → Update `dist/config.example.toml`

If documentation is out of date, **fix it in the same commit** as the code change. Do not leave it for later.

### File Naming

```
docs/v2/plans/week-NN-{feature}.md        # Weekly implementation plans
docs/v2/plans/{feature}-{type}.md          # Feature plans, research, proposals
docs/v2/{topic}.md                         # Top-level docs (architecture, requirements)
docs/templates/{name}.md                   # Document templates
```

---

## Current State & Known Issues

### What's Implemented (Weeks 1-11)

- FUSE filesystem with local origin
- Metadata extraction (symphonia: FLAC, MP3, AAC, OGG, Opus)
- Virtual tree with configurable path templates
- CAS with CDC chunking and deduplication
- Multi-origin federation with failover and health monitoring
- NFS/SMB origin wrappers with retry logic
- Full-text search (tantivy) with `.search/` virtual directory
- Smart collections, artwork caching, predictive prefetch
- Plugin system (native + WASM)
- gRPC control API with event streaming
- Comprehensive tracing/logging with journald integration

### Critical Open Issues

Detailed in `docs/v2/plans/resilience-fault-tolerance.md` and `docs/v2/plans/persistent-state.md`:

1. **No persistent state on mount** — every restart does full origin scan (O(N) instead of O(1)). SQLite, tantivy, and manifests persist on disk but are never loaded.
2. **No signal handling** — SIGTERM kills the daemon instantly, no graceful shutdown
3. **No crash recovery** — corrupted cache = crash on startup, no repair
4. **FUSE↔tokio deadlock risk** — `block_on()` in sync FUSE callback can hang under load
5. **Fire-and-forget tasks** — background tasks (health monitor, watcher, indexer) not supervised
6. **RwLock poison** — single panic in a writer kills all FUSE operations

### S3/SFTP Origins

`s3.rs` and `sftp.rs` are **feature-gated stubs** (not implemented). The `Origin` trait and failover infrastructure work, but only `local`, `nfs`, and `smb` origins have real implementations.

---

## Running the Filesystem

```bash
# Development
nix develop
cargo build
./target/debug/musicfs mount /mnt/music --origin /path/to/music

# Production (systemd)
sudo cp dist/musicfs.service /etc/systemd/system/
sudo systemctl enable --now musicfs

# E2E tests (requires mounted filesystem)
MUSICFS_TEST_MOUNT=/mnt/music cargo test --test e2e_players -- --ignored
```

---

## Key Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `fuser` | 0.14 | FUSE interface |
| `tokio` | 1.x | Async runtime (full features) |
| `rusqlite` | 0.31 | SQLite (bundled) |
| `sled` | 0.34 | Embedded KV (CAS chunk index) |
| `tantivy` | 0.22 | Full-text search |
| `symphonia` | 0.5 | Audio metadata extraction |
| `fastcdc` | 3.x | Content-defined chunking |
| `tonic` | 0.11 | gRPC server |
| `tracing` | 0.1 | Structured logging |
| `clap` | 4.x | CLI argument parsing |
| `parking_lot` | 0.12 | Fast locks |
| `dashmap` | 5.x | Concurrent HashMap |
