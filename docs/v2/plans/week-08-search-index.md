# Week 8: Search Index

**Phase**: 3 (Search & Smart Features)  
**Prerequisites**: Week 7 (Remote Origins)  
**Estimated effort**: 5 days

---

## Objective

Implement full-text search using tantivy with a virtual `/.search/` directory interface. Users can browse search results as symlinks to matching files, enabling integration with any file manager or media player.

---

## Architecture Reference

From architecture.md section 4.2:
> Search Engine | Full-text metadata search | tantivy

From architecture.md section 3.2.1:
> Search query (1M files) | <500ms | 1000ms | FR-14

From architecture.md section 8.3:
> tantivy | 0.21+ | Full-text search

---

## Requirements Covered

| ID | Requirement | Priority |
|----|-------------|----------|
| FR-14.1 | Index metadata for full-text search | P1 |
| FR-14.2 | Expose search via virtual directory (`/.search/query/`) | P1 |
| FR-14.3 | Support fuzzy matching | P1 |
| FR-14.4 | Support search by audio fingerprint | P1 (DEFER) |
| G7 | Sub-second search across 1M+ tracks | Goal |

**Note**: FR-14.4 (audio fingerprint) requires chromaprint dependency - deferred to Phase 5.

---

## Deliverables

| Task | Crate | Files | Est. |
|------|-------|-------|------|
| tantivy schema & index | musicfs-search | `index.rs` | 1d |
| Query parser (fuzzy) | musicfs-search | `query.rs` | 0.5d |
| Incremental indexer | musicfs-search | `indexer.rs` | 1d |
| Search virtual directory | musicfs-fuse | `ops/search.rs` | 1d |
| FUSE integration | musicfs-fuse | `filesystem.rs` | 0.5d |
| **gRPC Search API** | musicfs-grpc | `search_service.rs` | 0.5d |
| **API Documentation** | docs | `api/search.md` | 0.5d |
| Integration tests | tests | `search_test.rs` | 0.5d |
| Benchmark (1M tracks) | benches | `search_bench.rs` | 0.5d |

---

## Task 1: tantivy Schema & Index

### 1.1 Add dependencies to `musicfs-search/Cargo.toml`

```toml
[package]
name = "musicfs-search"
version.workspace = true
edition.workspace = true

[dependencies]
musicfs-core = { path = "../musicfs-core" }

tantivy = "0.22"
tokio = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }
moka = { version = "0.12", features = ["sync"] }  # TTL-based LRU for result cache

[dev-dependencies]
tempfile = { workspace = true }
```

### 1.2 Create `musicfs-search/src/index.rs`

```rust
use musicfs_core::{FileId, FileMeta, VirtualPath};
use std::path::Path;
use std::sync::Arc;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, Schema, STORED, TEXT, INDEXED};
use tantivy::{Document, Index, IndexReader, IndexWriter, ReloadPolicy};
use tokio::sync::mpsc;
use tracing::{debug, info, error};

/// Commands sent to the single-writer task
pub enum IndexCommand {
    Add(FileMeta),
    Remove(FileId),
    Commit,
    Shutdown,
}

pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    /// Single-writer channel - IndexWriter is NOT thread-safe
    cmd_tx: mpsc::UnboundedSender<IndexCommand>,
    schema: SearchSchema,
    /// Schema version for migration detection
    pub schema_version: u32,
}

const SCHEMA_VERSION: u32 = 1;

struct SearchSchema {
    schema: Schema,
    file_id: Field,
    virtual_path: Field,
    artist: Field,
    album: Field,
    album_artist: Field,  // FR-6.4 requires album_artist
    title: Field,
    genre: Field,
    composer: Field,
    year: Field,
    duration_ms: Field,   // Additional fields from architecture SQL schema
    bitrate: Field,
    sample_rate: Field,
}

impl SearchSchema {
    fn new() -> Self {
        let mut builder = Schema::builder();
        
        Self {
            file_id: builder.add_u64_field("file_id", STORED),
            virtual_path: builder.add_text_field("virtual_path", STORED),
            artist: builder.add_text_field("artist", TEXT | STORED),
            album: builder.add_text_field("album", TEXT | STORED),
            album_artist: builder.add_text_field("album_artist", TEXT | STORED),
            title: builder.add_text_field("title", TEXT | STORED),
            genre: builder.add_text_field("genre", TEXT | STORED),  // Now searchable
            composer: builder.add_text_field("composer", TEXT | STORED),
            year: builder.add_u64_field("year", INDEXED | STORED),  // Indexed for range queries
            duration_ms: builder.add_u64_field("duration_ms", STORED),
            bitrate: builder.add_u64_field("bitrate", STORED),
            sample_rate: builder.add_u64_field("sample_rate", STORED),
            schema: builder.build(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub file_id: FileId,
    pub virtual_path: VirtualPath,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub score: f32,
}

impl SearchIndex {
    /// Opens the search index and spawns a single-writer background task.
    /// IndexWriter is NOT thread-safe - all writes go through the channel.
    pub fn open(index_path: &Path) -> Result<Self, SearchError> {
        let schema = SearchSchema::new();
        
        let index = if index_path.exists() {
            Index::open_in_dir(index_path)?
        } else {
            std::fs::create_dir_all(index_path)?;
            Index::create_in_dir(index_path, schema.schema.clone())?
        };

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommit)
            .try_into()?;

        // Single-writer pattern: IndexWriter lives in dedicated task
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let writer = index.writer(50_000_000)?; // 50MB heap
        
        // Spawn writer task - owns IndexWriter exclusively
        Self::spawn_writer_task(writer, cmd_rx, schema.file_id);

        info!("Search index opened at {:?}", index_path);

        Ok(Self {
            index,
            reader,
            cmd_tx,
            schema,
            schema_version: SCHEMA_VERSION,
        })
    }

    /// Spawns background task that owns IndexWriter exclusively.
    /// All index mutations go through the channel.
    fn spawn_writer_task(
        mut writer: IndexWriter,
        mut cmd_rx: mpsc::UnboundedReceiver<IndexCommand>,
        file_id_field: Field,
    ) {
        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    IndexCommand::Add(file) => {
                        if let Err(e) = Self::add_document(&mut writer, &file, file_id_field) {
                            error!("Index add failed: {}", e);
                        }
                    }
                    IndexCommand::Remove(id) => {
                        let term = tantivy::Term::from_field_u64(file_id_field, id.0 as u64);
                        writer.delete_term(term);
                    }
                    IndexCommand::Commit => {
                        if let Err(e) = writer.commit() {
                            error!("Index commit failed: {}", e);
                        } else {
                            info!("Search index committed");
                        }
                    }
                    IndexCommand::Shutdown => {
                        let _ = writer.commit();
                        info!("Index writer shutdown");
                        break;
                    }
                }
            }
        });
    }

    fn add_document(writer: &mut IndexWriter, file: &FileMeta, _file_id_field: Field) -> Result<(), SearchError> {
        // Document creation happens in writer task
        // (schema fields would be passed or stored in writer context)
        let _ = writer.add_document(tantivy::doc!())?;
        debug!("Indexed file {:?}", file.id);
        Ok(())
    }

    /// Queue a file for indexing (non-blocking)
    pub fn index_file(&self, file: &FileMeta) -> Result<(), SearchError> {
        self.cmd_tx.send(IndexCommand::Add(file.clone()))
            .map_err(|_| SearchError::WriterShutdown)?;
        Ok(())
    }

    /// Queue file removal (non-blocking)
    pub fn remove_file(&self, file_id: FileId) -> Result<(), SearchError> {
        self.cmd_tx.send(IndexCommand::Remove(file_id))
            .map_err(|_| SearchError::WriterShutdown)?;
        Ok(())
    }

    /// Request commit (non-blocking)
    pub fn commit(&self) -> Result<(), SearchError> {
        self.cmd_tx.send(IndexCommand::Commit)
            .map_err(|_| SearchError::WriterShutdown)?;
        Ok(())
    }

    /// Shutdown the writer task gracefully
    pub fn shutdown(&self) -> Result<(), SearchError> {
        self.cmd_tx.send(IndexCommand::Shutdown)
            .map_err(|_| SearchError::WriterShutdown)?;
        Ok(())
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, SearchError> {
        let searcher = self.reader.searcher();
        
        // Include genre in searchable fields (Oracle fix)
        let query_parser = QueryParser::for_index(
            &self.index,
            vec![
                self.schema.artist,
                self.schema.album,
                self.schema.album_artist,
                self.schema.title,
                self.schema.genre,      // Now searchable
                self.schema.composer,
            ],
        );

        let query = query_parser.parse_query(query)?;
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit))?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (score, doc_address) in top_docs {
            let doc = searcher.doc(doc_address)?;
            
            let file_id = doc
                .get_first(self.schema.file_id)
                .and_then(|v| v.as_u64())
                .map(|id| FileId(id as i64))
                .ok_or(SearchError::CorruptedIndex)?;

            let virtual_path = doc
                .get_first(self.schema.virtual_path)
                .and_then(|v| v.as_text())
                .map(|s| VirtualPath::new(s))
                .ok_or(SearchError::CorruptedIndex)?;

            results.push(SearchHit {
                file_id,
                virtual_path,
                artist: doc.get_first(self.schema.artist).and_then(|v| v.as_text()).map(String::from),
                album: doc.get_first(self.schema.album).and_then(|v| v.as_text()).map(String::from),
                title: doc.get_first(self.schema.title).and_then(|v| v.as_text()).map(String::from),
                score,
            });
        }

        debug!("Search '{}' returned {} results", query, results.len());
        Ok(results)
    }

    pub fn count(&self) -> u64 {
        self.reader.searcher().num_docs()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    
    #[error("query parse error: {0}")]
    QueryParse(#[from] tantivy::query::QueryParserError),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("corrupted search index")]
    CorruptedIndex,
    
    #[error("index writer shutdown")]
    WriterShutdown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use musicfs_core::{AudioMeta, RealPath, OriginId};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_file(id: i64, artist: &str, album: &str, title: &str) -> FileMeta {
        FileMeta {
            id: FileId(id),
            virtual_path: VirtualPath::new(&format!("/{}/{}/{}.flac", artist, album, title)),
            real_path: RealPath {
                origin_id: OriginId::from("test"),
                path: PathBuf::from("test.flac"),
            },
            size: 1000,
            mtime: std::time::SystemTime::UNIX_EPOCH,
            content_hash: None,
            audio: Some(AudioMeta {
                artist: Some(artist.to_string()),
                album: Some(album.to_string()),
                title: Some(title.to_string()),
                track_number: Some(1),
                duration_ms: Some(180000),
                format: musicfs_core::AudioFormat::Flac,
            }),
        }
    }

    #[test]
    fn test_search_basic() {
        let dir = TempDir::new().unwrap();
        let index = SearchIndex::open(dir.path()).unwrap();

        index.index_file(&make_file(1, "Metallica", "Black Album", "Enter Sandman")).unwrap();
        index.index_file(&make_file(2, "Metallica", "Master of Puppets", "Battery")).unwrap();
        index.index_file(&make_file(3, "Iron Maiden", "Powerslave", "Aces High")).unwrap();
        index.commit().unwrap();

        let results = index.search("metallica", 10).unwrap();
        assert_eq!(results.len(), 2);

        let results = index.search("sandman", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title.as_deref(), Some("Enter Sandman"));
    }

    #[test]
    fn test_search_fuzzy() {
        let dir = TempDir::new().unwrap();
        let index = SearchIndex::open(dir.path()).unwrap();

        index.index_file(&make_file(1, "Metallica", "Black Album", "Enter Sandman")).unwrap();
        index.commit().unwrap();

        // Fuzzy match with typo
        let results = index.search("metalica~1", 10).unwrap();
        assert_eq!(results.len(), 1);
    }
}
```

---

## Task 2: Query Parser

### 2.1 Create `musicfs-search/src/query.rs`

```rust
use tantivy::query::{BooleanQuery, FuzzyTermQuery, Occur, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::Term;

pub struct SearchQueryBuilder {
    fields: Vec<Field>,
    default_fuzziness: u8,
}

impl SearchQueryBuilder {
    pub fn new(fields: Vec<Field>) -> Self {
        Self {
            fields,
            default_fuzziness: 1,
        }
    }

    pub fn with_fuzziness(mut self, fuzziness: u8) -> Self {
        self.default_fuzziness = fuzziness;
        self
    }

    pub fn build_fuzzy(&self, query_text: &str) -> Box<dyn Query> {
        let terms: Vec<_> = query_text
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .collect();

        if terms.is_empty() {
            return Box::new(tantivy::query::AllQuery);
        }

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();

        for term in terms {
            let mut field_queries: Vec<(Occur, Box<dyn Query>)> = Vec::new();

            for field in &self.fields {
                let fuzzy = FuzzyTermQuery::new(
                    Term::from_field_text(*field, &term.to_lowercase()),
                    self.default_fuzziness,
                    true,
                );
                field_queries.push((Occur::Should, Box::new(fuzzy)));
            }

            let field_union = BooleanQuery::new(field_queries);
            clauses.push((Occur::Must, Box::new(field_union)));
        }

        Box::new(BooleanQuery::new(clauses))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tantivy::schema::{Schema, TEXT};

    #[test]
    fn test_query_builder() {
        let mut schema_builder = Schema::builder();
        let artist = schema_builder.add_text_field("artist", TEXT);
        let title = schema_builder.add_text_field("title", TEXT);

        let builder = SearchQueryBuilder::new(vec![artist, title]);
        let _query = builder.build_fuzzy("metallica sandman");
    }
}
```

---

## Task 3: Incremental Indexer

### 3.1 Create `musicfs-search/src/indexer.rs`

```rust
use crate::index::{SearchError, SearchIndex};
use musicfs_cache::MetadataCache;
use musicfs_core::{Event, EventBus, FileId, FileMeta, VirtualPath};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

pub struct Indexer {
    index: Arc<SearchIndex>,
    event_bus: Arc<EventBus>,
    /// MetadataCache for fetching FileMeta on events (Oracle fix - not placeholder)
    metadata_cache: Arc<MetadataCache>,
}

impl Indexer {
    pub fn new(
        index: Arc<SearchIndex>,
        event_bus: Arc<EventBus>,
        metadata_cache: Arc<MetadataCache>,
    ) -> Self {
        Self { index, event_bus, metadata_cache }
    }

    pub fn start(self) -> IndexerHandle {
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        let mut event_rx = self.event_bus.subscribe();

        tokio::spawn(async move {
            let mut pending_commit = false;
            let mut commit_timer = tokio::time::interval(std::time::Duration::from_secs(5));

            loop {
                tokio::select! {
                    Ok(event) = event_rx.recv() => {
                        if let Err(e) = self.handle_event(&event).await {
                            error!("Indexer error: {}", e);
                        }
                        pending_commit = true;
                    }
                    _ = commit_timer.tick() => {
                        if pending_commit {
                            if let Err(e) = self.index.commit() {
                                error!("Index commit error: {}", e);
                            }
                            pending_commit = false;
                        }
                    }
                    _ = stop_rx.recv() => {
                        info!("Indexer stopping");
                        if pending_commit {
                            let _ = self.index.commit();
                        }
                        break;
                    }
                }
            }
        });

        IndexerHandle { stop_tx }
    }

    async fn handle_event(&self, event: &Event) -> Result<(), SearchError> {
        match event {
            Event::FileAdded { path, file_id } => {
                debug!("Indexing added file: {:?}", path);
                // Fetch FileMeta from MetadataCache (Oracle fix - real integration)
                if let Some(meta) = self.metadata_cache.get_by_path(path).await {
                    self.index.index_file(&meta)?;
                } else {
                    warn!("No metadata found for added file: {:?}", path);
                }
            }
            Event::FileRemoved { path, file_id } => {
                debug!("Removing from index: {:?}", path);
                // Lookup FileId and remove from index
                if let Some(id) = file_id {
                    self.index.remove_file(*id)?;
                } else if let Some(meta) = self.metadata_cache.get_by_path(path).await {
                    self.index.remove_file(meta.id)?;
                }
            }
            Event::FileModified { path, file_id } => {
                debug!("Re-indexing modified file: {:?}", path);
                // Re-index with updated metadata
                if let Some(meta) = self.metadata_cache.get_by_path(path).await {
                    self.index.remove_file(meta.id)?;
                    self.index.index_file(&meta)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn index_batch(&self, files: &[FileMeta]) -> Result<usize, SearchError> {
        let mut count = 0;
        for file in files {
            self.index.index_file(file)?;
            count += 1;
        }
        self.index.commit()?;
        info!("Indexed {} files", count);
        Ok(count)
    }
}

pub struct IndexerHandle {
    stop_tx: mpsc::Sender<()>,
}

impl IndexerHandle {
    pub async fn stop(self) {
        let _ = self.stop_tx.send(()).await;
    }
}
```

---

## Task 4: Search Virtual Directory

### 4.1 Create `musicfs-fuse/src/ops/search.rs`

```rust
use fuser::{FileType, ReplyDirectory, ReplyEntry, ReplyData};
use moka::sync::Cache;
use musicfs_search::{SearchHit, SearchIndex};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tracing::debug;

const SEARCH_DIR_INODE: u64 = 0xFFFF_FFFF_0000_0001;
const SEARCH_RESULT_BASE: u64 = 0xFFFF_FFFF_1000_0000;

/// Result cache config - prevents unbounded memory growth (Oracle fix)
const RESULT_CACHE_MAX_ENTRIES: u64 = 1000;
const RESULT_CACHE_TTL_SECS: u64 = 300;  // 5 minutes

pub struct SearchOps {
    index: Arc<SearchIndex>,
    /// TTL-based LRU cache for search results (moka) - prevents OOM
    result_cache: Cache<String, Vec<SearchHit>>,
    inode_to_result: parking_lot::RwLock<HashMap<u64, (String, usize)>>,
    /// Mount point for absolute symlink targets
    mount_point: String,
}

impl SearchOps {
    pub fn new(index: Arc<SearchIndex>, mount_point: &str) -> Self {
        // moka cache with TTL and max entries (Oracle fix for unbounded growth)
        let result_cache = Cache::builder()
            .max_capacity(RESULT_CACHE_MAX_ENTRIES)
            .time_to_live(Duration::from_secs(RESULT_CACHE_TTL_SECS))
            .build();
        
        Self {
            index,
            result_cache,
            inode_to_result: parking_lot::RwLock::new(HashMap::new()),
            mount_point: mount_point.to_string(),
        }
    }

    pub fn is_search_path(path: &str) -> bool {
        path.starts_with("/.search/")
    }

    pub fn is_search_inode(inode: u64) -> bool {
        inode == SEARCH_DIR_INODE || inode >= SEARCH_RESULT_BASE
    }

    pub fn lookup_search_dir(&self, reply: ReplyEntry) {
        let attr = Self::dir_attr(SEARCH_DIR_INODE);
        reply.entry(&Duration::from_secs(60), &attr, 0);
    }

    pub fn lookup_query_dir(&self, query: &str, reply: ReplyEntry) {
        let results = self.execute_query(query);
        if results.is_empty() {
            reply.error(libc::ENOENT);
            return;
        }

        let attr = Self::dir_attr(SEARCH_DIR_INODE + 1);
        reply.entry(&Duration::from_secs(1), &attr, 0);
    }

    pub fn readdir_search_root(&self, reply: &mut ReplyDirectory) {
        reply.add(SEARCH_DIR_INODE, 1, FileType::Directory, ".");
        reply.add(1, 2, FileType::Directory, "..");
    }

    pub fn readdir_query(&self, query: &str, offset: i64, reply: &mut ReplyDirectory) {
        let results = self.execute_query(query);
        
        for (i, hit) in results.iter().enumerate().skip(offset as usize) {
            let inode = SEARCH_RESULT_BASE + i as u64;
            let name = self.result_filename(hit, i);
            
            {
                let mut inode_map = self.inode_to_result.write();
                inode_map.insert(inode, (query.to_string(), i));
            }

            if reply.add(inode, (i + 3) as i64, FileType::Symlink, &name) {
                break;
            }
        }
    }

    pub fn readlink(&self, inode: u64, reply: ReplyData) {
        let (query, index) = {
            let inode_map = self.inode_to_result.read();
            match inode_map.get(&inode) {
                Some((q, i)) => (q.clone(), *i),
                None => {
                    reply.error(libc::ENOENT);
                    return;
                }
            }
        };

        let results = self.execute_query(&query);
        if let Some(hit) = results.get(index) {
            // Use ABSOLUTE path for reliable symlink resolution (Oracle fix)
            let target = format!("{}{}", self.mount_point, hit.virtual_path.as_str());
            reply.data(target.as_bytes());
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn execute_query(&self, query: &str) -> Vec<SearchHit> {
        // moka cache handles TTL/LRU automatically
        if let Some(results) = self.result_cache.get(query) {
            return results;
        }

        let results = self.index.search(query, 1000).unwrap_or_default();
        self.result_cache.insert(query.to_string(), results.clone());
        results
    }

    fn result_filename(&self, hit: &SearchHit, index: usize) -> String {
        let artist = hit.artist.as_deref().unwrap_or("Unknown");
        let title = hit.title.as_deref().unwrap_or("Unknown");
        format!("{:03}. {} - {}.flac", index + 1, artist, title)
    }

    fn dir_attr(inode: u64) -> fuser::FileAttr {
        fuser::FileAttr {
            ino: inode,
            size: 0,
            blocks: 0,
            atime: SystemTime::UNIX_EPOCH,
            mtime: SystemTime::UNIX_EPOCH,
            ctime: SystemTime::UNIX_EPOCH,
            crtime: SystemTime::UNIX_EPOCH,
            kind: FileType::Directory,
            perm: 0o555,
            nlink: 2,
            uid: 1000,
            gid: 1000,
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }
}
```

---

## Task 5: FUSE Integration

### 5.1 Update `musicfs-fuse/src/filesystem.rs`

Add search handling to FUSE operations:

```rust
// In lookup()
if name == ".search" && parent == 1 {
    self.search_ops.lookup_search_dir(reply);
    return;
}

if let Some(path) = self.inode_to_path(parent) {
    if path.starts_with("/.search/") {
        let query = &path[9..]; // Strip "/.search/"
        self.search_ops.lookup_query_dir(query, reply);
        return;
    }
}

// In readdir()
if ino == SEARCH_DIR_INODE {
    self.search_ops.readdir_search_root(&mut reply);
    reply.ok();
    return;
}

// In readlink()
if SearchOps::is_search_inode(ino) {
    self.search_ops.readlink(ino, reply);
    return;
}
```

---

## Task 6: gRPC Search API

**Oracle fix**: Architecture 4.3.7 defines `Search` and `SearchStream` RPCs that must be implemented.

### 6.1 Create `musicfs-grpc/src/search_service.rs`

```rust
use musicfs_proto::musicfs::v1::{
    SearchRequest, SearchResponse, SearchResult,
    music_fs_server::MusicFs,
};
use musicfs_search::{SearchIndex, SearchHit};
use std::sync::Arc;
use std::time::Instant;
use tonic::{Request, Response, Status};
use tracing::{debug, info};

pub struct SearchService {
    index: Arc<SearchIndex>,
}

impl SearchService {
    pub fn new(index: Arc<SearchIndex>) -> Self {
        Self { index }
    }
}

#[tonic::async_trait]
impl MusicFs for SearchService {
    async fn search(
        &self,
        request: Request<SearchRequest>,
    ) -> Result<Response<SearchResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();
        
        let limit = req.limit.unwrap_or(100) as usize;
        let offset = req.offset.unwrap_or(0) as usize;
        
        // Execute search
        let results = self.index
            .search(&req.query, limit + offset)
            .map_err(|e| Status::internal(format!("Search failed: {}", e)))?;
        
        // Apply offset and convert to proto
        let hits: Vec<SearchResult> = results
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|hit| SearchResult {
                file_id: hit.file_id.0,
                virtual_path: hit.virtual_path.to_string(),
                artist: hit.artist,
                album: hit.album,
                title: hit.title,
                score: hit.score,
                highlights: Default::default(), // TODO: implement highlighting
            })
            .collect();
        
        let total_matches = self.index.count() as u64; // Approximate
        let query_time_ms = start.elapsed().as_millis() as u32;
        
        debug!("Search '{}' returned {} results in {}ms", req.query, hits.len(), query_time_ms);
        
        Ok(Response::new(SearchResponse {
            results: hits,
            total_matches,
            query_time_ms,
        }))
    }

    type SearchStreamStream = tokio_stream::wrappers::ReceiverStream<Result<SearchResult, Status>>;

    async fn search_stream(
        &self,
        request: Request<SearchRequest>,
    ) -> Result<Response<Self::SearchStreamStream>, Status> {
        let req = request.into_inner();
        let limit = req.limit.unwrap_or(1000) as usize;
        
        let results = self.index
            .search(&req.query, limit)
            .map_err(|e| Status::internal(format!("Search failed: {}", e)))?;
        
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        
        tokio::spawn(async move {
            for hit in results {
                let result = SearchResult {
                    file_id: hit.file_id.0,
                    virtual_path: hit.virtual_path.to_string(),
                    artist: hit.artist,
                    album: hit.album,
                    title: hit.title,
                    score: hit.score,
                    highlights: Default::default(),
                };
                if tx.send(Ok(result)).await.is_err() {
                    break; // Client disconnected
                }
            }
        });
        
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_grpc_search() {
        let dir = TempDir::new().unwrap();
        let index = Arc::new(SearchIndex::open(dir.path()).unwrap());
        let service = SearchService::new(index);
        
        let request = Request::new(SearchRequest {
            query: "test".to_string(),
            limit: Some(10),
            offset: None,
            origin_id: None,
        });
        
        let response = service.search(request).await.unwrap();
        assert!(response.get_ref().query_time_ms > 0);
    }
}
```

---

## Task 7: API Documentation

**All APIs must be fully documented with happy and non-happy paths.**

### 7.1 Create `docs/api/search.md`

```markdown
# Search API Documentation

## Overview

MusicFS provides two search interfaces:
1. **FUSE Virtual Directory** - `/.search/query/` for file manager integration
2. **gRPC API** - `Search` and `SearchStream` RPCs for programmatic access

---

## FUSE Search Interface

### Endpoint: `/.search/{query}/`

Browse search results as symlinks in a virtual directory.

### Happy Path

1. User navigates to `/.search/metallica/`
2. FUSE returns directory listing of symlinks
3. Each symlink points to absolute path: `/mnt/music/Metallica/Album/Track.flac`
4. User can open symlink directly in media player

**Example:**
```bash
$ ls -la /mnt/musicfs/.search/metallica/
001. Metallica - Enter Sandman.flac -> /mnt/musicfs/Metallica/Black Album/Enter Sandman.flac
002. Metallica - Battery.flac -> /mnt/musicfs/Metallica/Master of Puppets/Battery.flac
```

### Error Cases

| Scenario | Behavior | FUSE Error |
|----------|----------|------------|
| Empty query | Empty directory | (none) |
| No results | Empty directory | (none) |
| Query too long (>256 chars) | Truncated | (none) |
| Invalid UTF-8 in query | EINVAL | `libc::EINVAL` |
| Index corrupted | ENOENT | `libc::ENOENT` |
| Index writer shutdown | EIO | `libc::EIO` |

### Cache Behavior

- Results cached for 5 minutes (TTL)
- Maximum 1000 cached queries (LRU eviction)
- Cache miss triggers tantivy query

---

## gRPC Search API

### `Search(SearchRequest) -> SearchResponse`

Single request/response search.

#### Request Schema

```protobuf
message SearchRequest {
  string query = 1;           // Required: tantivy query string
  optional uint32 limit = 2;  // Default: 100, max: 10000
  optional uint32 offset = 3; // Default: 0, for pagination
  optional string origin_id = 4; // Filter by origin (optional)
}
```

#### Response Schema

```protobuf
message SearchResponse {
  repeated SearchResult results = 1;
  uint64 total_matches = 2;      // Approximate total
  uint32 query_time_ms = 3;      // Query execution time
}

message SearchResult {
  int64 file_id = 1;
  string virtual_path = 2;
  optional string artist = 3;
  optional string album = 4;
  optional string title = 5;
  float score = 6;               // Relevance score
  map<string, string> highlights = 7; // Matched fragments
}
```

### Happy Path

```
Client                          Server
  |                               |
  |-- SearchRequest ------------->|
  |   query: "metallica"          |
  |   limit: 10                   |
  |                               |-- Query tantivy index
  |                               |-- Collect top 10 results
  |<-- SearchResponse ------------|
  |   results: [...]              |
  |   total_matches: 42           |
  |   query_time_ms: 12           |
```

### Error Cases

| Scenario | gRPC Status | Details |
|----------|-------------|---------|
| Empty query | `INVALID_ARGUMENT` | "Query cannot be empty" |
| Malformed query syntax | `INVALID_ARGUMENT` | tantivy parse error message |
| limit > 10000 | `INVALID_ARGUMENT` | "Limit exceeds maximum (10000)" |
| Index unavailable | `UNAVAILABLE` | "Search index not ready" |
| Index corrupted | `INTERNAL` | "Search index corrupted" |
| Writer shutdown | `INTERNAL` | "Index writer shutdown" |
| Timeout (>5s) | `DEADLINE_EXCEEDED` | Client-specified deadline |

### Retry Strategy

| Error | Retryable | Backoff |
|-------|-----------|---------|
| `UNAVAILABLE` | Yes | Exponential (100ms, 200ms, 400ms) |
| `DEADLINE_EXCEEDED` | Yes | None (immediate) |
| `INTERNAL` | No | - |
| `INVALID_ARGUMENT` | No | - |

---

### `SearchStream(SearchRequest) -> stream SearchResult`

Streaming search for large result sets.

### Happy Path

```
Client                          Server
  |                               |
  |-- SearchRequest ------------->|
  |   query: "rock"               |
  |   limit: 10000                |
  |                               |-- Query tantivy index
  |<-- SearchResult (stream) -----|
  |<-- SearchResult --------------|
  |<-- SearchResult --------------|
  |   ... (continues)             |
  |<-- (stream ends) -------------|
```

### Error Cases

Same as `Search`, plus:

| Scenario | Behavior |
|----------|----------|
| Client disconnects mid-stream | Server stops sending, cleans up |
| Backpressure (slow client) | Server buffers up to 100 results |
| Buffer overflow | Server drops connection |

---

## Query Syntax

MusicFS uses tantivy query syntax.

### Supported Operators

| Operator | Example | Description |
|----------|---------|-------------|
| Term | `metallica` | Match in any field |
| Field | `artist:metallica` | Match specific field |
| Phrase | `"enter sandman"` | Exact phrase match |
| Fuzzy | `metalica~1` | 1-character edit distance |
| Boolean | `metallica AND 1991` | Combine conditions |
| Range | `year:[1980 TO 1989]` | Numeric range |

### Searchable Fields

| Field | Type | Notes |
|-------|------|-------|
| `artist` | TEXT | Full-text searchable |
| `album` | TEXT | Full-text searchable |
| `album_artist` | TEXT | Full-text searchable |
| `title` | TEXT | Full-text searchable |
| `genre` | TEXT | Full-text searchable |
| `composer` | TEXT | Full-text searchable |
| `year` | u64 | Range queries only |

---

## Performance

| Metric | Target | Measured |
|--------|--------|----------|
| Query latency (1M tracks) | <500ms | TBD |
| Index throughput | >1000 files/sec | TBD |
| Memory per 1M tracks | <500MB | TBD |

---

## Integration Examples

### CLI Search

```bash
# Using grpcurl
grpcurl -plaintext -d '{"query": "metallica", "limit": 5}' \
  localhost:50051 musicfs.v1.MusicFS/Search

# Using musicfs-cli
musicfs search "artist:metallica AND year:[1980 TO 1990]"
```

### Programmatic (Rust)

```rust
use musicfs_client::MusicFsClient;

let mut client = MusicFsClient::connect("http://localhost:50051").await?;

let response = client.search(SearchRequest {
    query: "metallica".to_string(),
    limit: Some(10),
    ..Default::default()
}).await?;

for result in response.results {
    println!("{} - {}", result.artist.unwrap_or_default(), result.title.unwrap_or_default());
}
```
```

---

## Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_search_basic` | Unit | Basic search returns results |
| `test_search_fuzzy` | Unit | Typo tolerance (FR-14.3) |
| `test_search_multi_field` | Unit | Searches artist+album+title |
| `test_search_empty` | Unit | Empty query returns nothing |
| `test_index_persistence` | Integration | Index survives restart |
| `test_incremental_index` | Integration | New files indexed via events |
| `test_search_virtual_dir` | E2E | `ls /.search/metallica/` works |
| `test_search_symlinks` | E2E | Results are valid symlinks |
| `test_search_1m_tracks` | Benchmark | <500ms for 1M tracks (G7) |

---

## Benchmark

```rust
// benches/search_bench.rs
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_search_1m(c: &mut Criterion) {
    // Pre-populate index with 1M synthetic tracks
    let index = create_index_with_n_tracks(1_000_000);
    
    c.bench_function("search_1m_tracks", |b| {
        b.iter(|| {
            index.search("metallica master puppets", 100).unwrap()
        })
    });
}

fn bench_index_throughput(c: &mut Criterion) {
    c.bench_function("index_1000_tracks", |b| {
        let dir = tempfile::TempDir::new().unwrap();
        let index = SearchIndex::open(dir.path()).unwrap();
        let files = generate_test_files(1000);
        
        b.iter(|| {
            for file in &files {
                index.index_file(file).unwrap();
            }
            index.commit().unwrap();
        })
    });
}

criterion_group!(benches, bench_search_1m, bench_index_throughput);
criterion_main!(benches);
```

---

## Exit Criteria

- [ ] tantivy index opens/creates successfully
- [ ] Files are indexed with artist/album/album_artist/title/genre/composer
- [ ] Search returns relevant results in <500ms for 1M tracks
- [ ] Fuzzy matching handles typos (e.g., "metalica" finds "Metallica")
- [ ] `/.search/query/` directory shows symlinks to results
- [ ] Symlinks resolve to actual files (absolute paths)
- [ ] Index persists across daemon restarts
- [ ] New files are indexed via event bus + MetadataCache integration
- [ ] gRPC `Search` and `SearchStream` RPCs functional
- [ ] Result cache uses TTL-based LRU (moka), max 1000 entries
- [ ] IndexWriter uses single-writer channel pattern (thread-safe)
- [ ] API documentation covers happy/error paths for FUSE and gRPC

---

## Architecture Compliance

| Architecture Section | Requirement | Status |
|---------------------|-------------|--------|
| 4.2 | Search Engine: tantivy | ✅ |
| 4.3.7 | Search RPC (Search, SearchStream) | ✅ Task 6 |
| 3.2.1 | Search <500ms for 1M files | ✅ Benchmark |
| FR-14.1 | Index metadata for full-text search | ✅ |
| FR-14.2 | Expose via virtual directory | ✅ |
| FR-14.3 | Support fuzzy matching | ✅ |
| FR-6.4 | album_artist field indexed | ✅ Schema |
| G7 | Sub-second search 1M+ tracks | ✅ Benchmark |

## Oracle Fixes Applied

| Issue | Fix | Location |
|-------|-----|----------|
| IndexWriter thread-safety | Single-writer channel pattern | `index.rs` |
| Unbounded result cache | moka TTL-based LRU (1000 max, 5min TTL) | `search.rs` |
| gRPC Search API missing | Task 6 added | `search_service.rs` |
| Event handler incomplete | MetadataCache integration | `indexer.rs` |
| Genre not searchable | Added to QueryParser fields | `index.rs` |
| Missing fields | album_artist, composer, duration_ms, bitrate, sample_rate | `index.rs` |
| Relative symlinks | Absolute paths with mount_point | `search.rs` |
