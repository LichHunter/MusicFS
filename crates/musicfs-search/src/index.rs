use musicfs_core::{AudioMeta, FileId, FileMeta, VirtualPath};
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, FuzzyTermQuery, Occur, Query, QueryParser};
use tantivy::schema::{Field, Schema, Value, INDEXED, STORED, TEXT};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};
use tracing::{debug, info, warn};

const SCHEMA_VERSION: u32 = 1;

pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    writer: Arc<RwLock<IndexWriter>>,
    schema: SearchSchema,
    pub schema_version: u32,
}

struct SearchSchema {
    schema: Schema,
    file_id: Field,
    virtual_path: Field,
    artist: Field,
    album: Field,
    album_artist: Field,
    title: Field,
    genre: Field,
    composer: Field,
    year: Field,
    duration_ms: Field,
    bitrate: Field,
    sample_rate: Field,
}

impl SearchSchema {
    fn new() -> Self {
        let mut builder = Schema::builder();

        Self {
            file_id: builder.add_u64_field("file_id", INDEXED | STORED),
            virtual_path: builder.add_text_field("virtual_path", STORED),
            artist: builder.add_text_field("artist", TEXT | STORED),
            album: builder.add_text_field("album", TEXT | STORED),
            album_artist: builder.add_text_field("album_artist", TEXT | STORED),
            title: builder.add_text_field("title", TEXT | STORED),
            genre: builder.add_text_field("genre", TEXT | STORED),
            composer: builder.add_text_field("composer", TEXT | STORED),
            year: builder.add_u64_field("year", INDEXED | STORED),
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
    pub fn open(index_path: &Path) -> Result<Self, SearchError> {
        let schema_obj = SearchSchema::new();

        let index = if index_path.exists() && index_path.join("meta.json").exists() {
            Index::open_in_dir(index_path)?
        } else {
            std::fs::create_dir_all(index_path)?;
            Index::create_in_dir(index_path, schema_obj.schema.clone())?
        };

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;

        let writer = index.writer(50_000_000)?;

        info!("Search index opened at {:?}", index_path);

        Ok(Self {
            index,
            reader,
            writer: Arc::new(RwLock::new(writer)),
            schema: schema_obj,
            schema_version: SCHEMA_VERSION,
        })
    }

    pub fn open_with_recovery(index_path: &Path) -> Result<Self, SearchError> {
        match Self::open(index_path) {
            Ok(index) => {
                let docs = index.reader.searcher().num_docs();
                info!(docs, "Search index opened successfully");
                Ok(index)
            }
            Err(e) => {
                warn!(
                    error = %e,
                    path = ?index_path,
                    "Search index corrupted, rebuilding from scratch"
                );
                if index_path.exists() {
                    std::fs::remove_dir_all(index_path).map_err(SearchError::Io)?;
                }
                Self::open(index_path)
            }
        }
    }

    pub fn index_file(&self, file: &FileMeta) -> Result<(), SearchError> {
        let mut doc = TantivyDocument::new();

        doc.add_u64(self.schema.file_id, file.id.0 as u64);
        doc.add_text(self.schema.virtual_path, file.virtual_path.as_str());

        if let Some(ref audio) = file.audio {
            Self::add_audio_fields(&mut doc, &self.schema, audio);
        }

        self.writer.read().add_document(doc)?;
        debug!("Indexed file {:?}", file.id);
        Ok(())
    }

    fn add_audio_fields(doc: &mut TantivyDocument, schema: &SearchSchema, audio: &AudioMeta) {
        if let Some(ref v) = audio.artist {
            doc.add_text(schema.artist, v);
        }
        if let Some(ref v) = audio.album {
            doc.add_text(schema.album, v);
        }
        if let Some(ref v) = audio.album_artist {
            doc.add_text(schema.album_artist, v);
        }
        if let Some(ref v) = audio.title {
            doc.add_text(schema.title, v);
        }
        if let Some(ref v) = audio.genre {
            doc.add_text(schema.genre, v);
        }
        if let Some(ref v) = audio.year {
            doc.add_u64(schema.year, *v as u64);
        }
        if let Some(v) = audio.duration_ms {
            doc.add_u64(schema.duration_ms, v);
        }
        if let Some(v) = audio.bitrate {
            doc.add_u64(schema.bitrate, v as u64);
        }
        if let Some(v) = audio.sample_rate {
            doc.add_u64(schema.sample_rate, v as u64);
        }
    }

    pub fn remove_file(&self, file_id: FileId) -> Result<(), SearchError> {
        let term = tantivy::Term::from_field_u64(self.schema.file_id, file_id.0 as u64);
        self.writer.read().delete_term(term);
        debug!("Removed file {:?} from index", file_id);
        Ok(())
    }

    pub fn remove_by_path(&self, path: &VirtualPath) -> Result<bool, SearchError> {
        let searcher = self.reader.searcher();
        let query_parser = QueryParser::for_index(&self.index, vec![self.schema.virtual_path]);
        let query = query_parser.parse_query(&format!("\"{}\"", path.as_str()))?;
        let top_docs = searcher.search(&query, &TopDocs::with_limit(1))?;

        if let Some((_, doc_address)) = top_docs.first() {
            let doc: TantivyDocument = searcher.doc(*doc_address)?;
            if let Some(file_id) = doc.get_first(self.schema.file_id).and_then(|v| v.as_u64()) {
                self.remove_file(FileId(file_id as i64))?;
                debug!("Removed file by path {:?}", path);
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn commit(&self) -> Result<(), SearchError> {
        self.writer.write().commit()?;
        self.reader.reload()?;
        info!("Search index committed");
        Ok(())
    }

    pub fn search(&self, query_str: &str, limit: usize) -> Result<Vec<SearchHit>, SearchError> {
        let searcher = self.reader.searcher();

        let default_fields = vec![
            self.schema.artist,
            self.schema.album,
            self.schema.album_artist,
            self.schema.title,
            self.schema.genre,
            self.schema.composer,
        ];

        let query: Box<dyn Query> =
            if let Some((term, distance)) = Self::parse_fuzzy_query(query_str) {
                let subqueries: Vec<(Occur, Box<dyn Query>)> = default_fields
                    .iter()
                    .map(|&field| {
                        let term = Term::from_field_text(field, &term);
                        let fuzzy = FuzzyTermQuery::new(term, distance, true);
                        (Occur::Should, Box::new(fuzzy) as Box<dyn Query>)
                    })
                    .collect();
                Box::new(BooleanQuery::new(subqueries))
            } else {
                let query_parser = QueryParser::for_index(&self.index, default_fields);
                query_parser.parse_query(query_str)?
            };

        let top_docs = searcher.search(&*query, &TopDocs::with_limit(limit))?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address)?;

            let file_id = doc
                .get_first(self.schema.file_id)
                .and_then(|v| v.as_u64())
                .map(|id| FileId(id as i64))
                .ok_or(SearchError::CorruptedIndex)?;

            let virtual_path = doc
                .get_first(self.schema.virtual_path)
                .and_then(|v| v.as_str())
                .map(|s| VirtualPath::new(s))
                .ok_or(SearchError::CorruptedIndex)?;

            results.push(SearchHit {
                file_id,
                virtual_path,
                artist: doc
                    .get_first(self.schema.artist)
                    .and_then(|v| v.as_str())
                    .map(String::from),
                album: doc
                    .get_first(self.schema.album)
                    .and_then(|v| v.as_str())
                    .map(String::from),
                title: doc
                    .get_first(self.schema.title)
                    .and_then(|v| v.as_str())
                    .map(String::from),
                score,
            });
        }

        debug!("Search '{}' returned {} results", query_str, results.len());
        Ok(results)
    }

    pub fn count(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    fn parse_fuzzy_query(query_str: &str) -> Option<(String, u8)> {
        let query_str = query_str.trim();
        if let Some(tilde_pos) = query_str.rfind('~') {
            let term = &query_str[..tilde_pos];
            let distance_str = &query_str[tilde_pos + 1..];
            if !term.is_empty() && !term.contains(':') && !term.contains(' ') {
                if let Ok(distance) = distance_str.parse::<u8>() {
                    if distance <= 2 {
                        return Some((term.to_lowercase(), distance));
                    }
                }
            }
        }
        None
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use musicfs_core::{AudioFormat, OriginId, RealPath};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_file(id: i64, artist: &str, album: &str, title: &str) -> FileMeta {
        FileMeta {
            id: FileId(id),
            virtual_path: VirtualPath::new(format!("/{}/{}/{}.flac", artist, album, title)),
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
                genre: Some("Metal".to_string()),
                format: AudioFormat::Flac,
                ..Default::default()
            }),
        }
    }

    #[test]
    fn test_search_basic() {
        let dir = TempDir::new().unwrap();
        let index = SearchIndex::open(dir.path()).unwrap();

        index
            .index_file(&make_file(1, "Metallica", "Black Album", "Enter Sandman"))
            .unwrap();
        index
            .index_file(&make_file(2, "Metallica", "Master of Puppets", "Battery"))
            .unwrap();
        index
            .index_file(&make_file(3, "Iron Maiden", "Powerslave", "Aces High"))
            .unwrap();
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

        index
            .index_file(&make_file(1, "Metallica", "Black Album", "Enter Sandman"))
            .unwrap();
        index.commit().unwrap();

        let results = index.search("metalica~1", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_genre() {
        let dir = TempDir::new().unwrap();
        let index = SearchIndex::open(dir.path()).unwrap();

        index
            .index_file(&make_file(1, "Metallica", "Black Album", "Enter Sandman"))
            .unwrap();
        index.commit().unwrap();

        let results = index.search("genre:Metal", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_remove_file() {
        let dir = TempDir::new().unwrap();
        let index = SearchIndex::open(dir.path()).unwrap();

        index
            .index_file(&make_file(1, "Test", "Album", "Song"))
            .unwrap();
        index.commit().unwrap();

        assert_eq!(index.search("test", 10).unwrap().len(), 1);

        index.remove_file(FileId(1)).unwrap();
        index.commit().unwrap();

        assert_eq!(index.search("test", 10).unwrap().len(), 0);
    }

    #[test]
    fn test_index_persistence() {
        let dir = TempDir::new().unwrap();

        {
            let index = SearchIndex::open(dir.path()).unwrap();
            index
                .index_file(&make_file(1, "Artist", "Album", "Track"))
                .unwrap();
            index.commit().unwrap();
        }

        {
            let index = SearchIndex::open(dir.path()).unwrap();
            let results = index.search("artist", 10).unwrap();
            assert_eq!(results.len(), 1);
        }
    }
}
