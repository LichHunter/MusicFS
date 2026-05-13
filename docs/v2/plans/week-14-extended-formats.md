# Week 14: Extended Formats & Audio Fingerprinting

**Phase**: 5 - P1 Feature Completion  
**Goal**: Audio fingerprint search and audiobook format support  
**Requirements**: FR-14.4, FR-24.2

---

## Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| Fingerprint indexing | musicfs-search | `fingerprint.rs` | FR-14.4 |
| Fingerprint search | musicfs-search | `fingerprint_search.rs` | FR-14.4 |
| M4B audiobook support | musicfs-metadata | `formats/m4b.rs` | FR-24.2 |
| Chapter extraction | musicfs-metadata | `chapters.rs` | FR-24.2 |
| Virtual chapter files | musicfs-fuse | `ops/chapters.rs` | FR-24.2 |

---

## Task 1: Audio Fingerprint Generation

### 1.1 Add Dependencies

```toml
# In musicfs-search/Cargo.toml
[dependencies]
chromaprint = "0.6"
symphonia = { version = "0.5", features = ["all"] }
```

### 1.2 Fingerprint Generation (`musicfs-search/src/fingerprint.rs`)

```rust
use chromaprint::{Configuration, Fingerprinter};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use std::path::Path;

/// Audio fingerprint using Chromaprint algorithm
#[derive(Debug, Clone)]
pub struct AudioFingerprint {
    pub raw: Vec<u32>,
    pub duration_secs: u32,
}

impl AudioFingerprint {
    /// Generate fingerprint from audio file (FR-14.4)
    pub fn from_file(path: &Path) -> Result<Self, FingerprintError> {
        let file = std::fs::File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        
        let probed = symphonia::default::get_probe()
            .format(&Hint::new(), mss, &FormatOptions::default(), &MetadataOptions::default())?;
        
        let mut format = probed.format;
        let track = format.tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or(FingerprintError::NoAudioTrack)?;
        
        let sample_rate = track.codec_params.sample_rate
            .ok_or(FingerprintError::NoSampleRate)?;
        
        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())?;
        
        // Chromaprint configuration
        let config = Configuration::preset_test1();
        let mut fingerprinter = Fingerprinter::new(&config);
        fingerprinter.start(sample_rate, 1)?;  // Mono
        
        let mut samples: Vec<i16> = Vec::new();
        let mut duration_samples = 0u64;
        
        // Decode and collect samples (first 120 seconds max)
        let max_samples = sample_rate as u64 * 120;
        
        loop {
            match format.next_packet() {
                Ok(packet) => {
                    let decoded = decoder.decode(&packet)?;
                    let mut sample_buf = SampleBuffer::<i16>::new(
                        decoded.capacity() as u64,
                        *decoded.spec(),
                    );
                    sample_buf.copy_interleaved_ref(decoded);
                    
                    // Convert to mono if stereo
                    let mono: Vec<i16> = if decoded.spec().channels.count() > 1 {
                        sample_buf.samples()
                            .chunks(decoded.spec().channels.count())
                            .map(|chunk| (chunk.iter().map(|&s| s as i32).sum::<i32>() / chunk.len() as i32) as i16)
                            .collect()
                    } else {
                        sample_buf.samples().to_vec()
                    };
                    
                    samples.extend(&mono);
                    duration_samples += mono.len() as u64;
                    
                    if duration_samples >= max_samples {
                        break;
                    }
                }
                Err(symphonia::core::errors::Error::IoError(e)) 
                    if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
        }
        
        // Feed samples to fingerprinter
        fingerprinter.feed(&samples)?;
        fingerprinter.finish()?;
        
        let raw = fingerprinter.fingerprint().to_vec();
        let duration_secs = (duration_samples / sample_rate as u64) as u32;
        
        Ok(Self { raw, duration_secs })
    }
    
    /// Compress fingerprint for storage
    pub fn to_bytes(&self) -> Vec<u8> {
        // Use chromaprint's compressed format
        chromaprint::encode_fingerprint(&self.raw, chromaprint::Algorithm::Test1)
    }
    
    /// Decompress fingerprint
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FingerprintError> {
        let (raw, _) = chromaprint::decode_fingerprint(bytes)?;
        Ok(Self { raw, duration_secs: 0 })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FingerprintError {
    #[error("No audio track found")]
    NoAudioTrack,
    #[error("No sample rate")]
    NoSampleRate,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Decode error: {0}")]
    Decode(String),
    #[error("Chromaprint error: {0}")]
    Chromaprint(String),
}
```

---

## Task 2: Fingerprint Search (`musicfs-search/src/fingerprint_search.rs`)

```rust
use crate::fingerprint::AudioFingerprint;

/// Fingerprint similarity search using bit-level comparison
pub struct FingerprintIndex {
    db: Arc<Database>,
}

impl FingerprintIndex {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }
    
    /// Index a file's fingerprint
    pub fn index(&self, file_id: FileId, fingerprint: &AudioFingerprint) -> Result<(), SearchError> {
        let bytes = fingerprint.to_bytes();
        self.db.store_fingerprint(file_id, &bytes, fingerprint.duration_secs)?;
        Ok(())
    }
    
    /// Search by fingerprint similarity (FR-14.4)
    pub fn search(
        &self,
        query: &AudioFingerprint,
        threshold: f32,  // 0.0-1.0, higher = more similar
        limit: usize,
    ) -> Result<Vec<FingerprintMatch>, SearchError> {
        let candidates = self.db.get_fingerprints_by_duration(
            query.duration_secs.saturating_sub(10),
            query.duration_secs + 10,
        )?;
        
        let mut matches: Vec<FingerprintMatch> = candidates
            .into_iter()
            .filter_map(|(file_id, fp_bytes, duration)| {
                let fp = AudioFingerprint::from_bytes(&fp_bytes).ok()?;
                let similarity = self.compare(&query.raw, &fp.raw);
                
                if similarity >= threshold {
                    Some(FingerprintMatch { file_id, similarity, duration })
                } else {
                    None
                }
            })
            .collect();
        
        // Sort by similarity descending
        matches.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap());
        matches.truncate(limit);
        
        Ok(matches)
    }
    
    /// Compare two fingerprints using bit error rate
    fn compare(&self, a: &[u32], b: &[u32]) -> f32 {
        let len = a.len().min(b.len());
        if len == 0 {
            return 0.0;
        }
        
        let mut matching_bits = 0u32;
        let mut total_bits = 0u32;
        
        for i in 0..len {
            let xor = a[i] ^ b[i];
            matching_bits += 32 - xor.count_ones();
            total_bits += 32;
        }
        
        matching_bits as f32 / total_bits as f32
    }
    
    /// Find duplicates by fingerprint
    pub fn find_duplicates(&self, threshold: f32) -> Result<Vec<DuplicateGroup>, SearchError> {
        let all_fps = self.db.get_all_fingerprints()?;
        let mut groups: Vec<DuplicateGroup> = Vec::new();
        let mut processed: HashSet<FileId> = HashSet::new();
        
        for (file_id, fp_bytes, duration) in &all_fps {
            if processed.contains(file_id) {
                continue;
            }
            
            let fp = AudioFingerprint::from_bytes(fp_bytes)?;
            let matches = self.search(&fp, threshold, 100)?;
            
            if matches.len() > 1 {
                let group = DuplicateGroup {
                    files: matches.iter().map(|m| m.file_id).collect(),
                    similarity: matches.iter().map(|m| m.similarity).sum::<f32>() / matches.len() as f32,
                };
                
                for m in &matches {
                    processed.insert(m.file_id);
                }
                
                groups.push(group);
            }
        }
        
        Ok(groups)
    }
}

#[derive(Debug)]
pub struct FingerprintMatch {
    pub file_id: FileId,
    pub similarity: f32,
    pub duration: u32,
}

#[derive(Debug)]
pub struct DuplicateGroup {
    pub files: Vec<FileId>,
    pub similarity: f32,
}
```

---

## Task 3: M4B Audiobook Support (`musicfs-metadata/src/formats/m4b.rs`)

```rust
use symphonia::core::meta::StandardTagKey;

/// M4B audiobook metadata (FR-24.2)
#[derive(Debug, Clone, Default)]
pub struct AudiobookMeta {
    pub title: Option<String>,
    pub author: Option<String>,  // Maps to "artist" in audio
    pub narrator: Option<String>,
    pub series: Option<String>,
    pub series_part: Option<u32>,
    pub description: Option<String>,
    pub publisher: Option<String>,
    pub year: Option<u32>,
    pub duration_ms: Option<u64>,
    pub chapters: Vec<Chapter>,
}

#[derive(Debug, Clone)]
pub struct Chapter {
    pub index: u32,
    pub title: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

impl Chapter {
    pub fn duration_ms(&self) -> u64 {
        self.end_ms - self.start_ms
    }
}

pub struct M4bParser;

impl M4bParser {
    /// Parse M4B audiobook with chapters
    pub fn parse(&self, path: &Path) -> Result<AudiobookMeta, MetadataError> {
        let file = std::fs::File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        
        let mut hint = Hint::new();
        hint.with_extension("m4b");
        
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())?;
        
        let mut meta = AudiobookMeta::default();
        let format = probed.format;
        
        // Extract metadata
        if let Some(metadata) = format.metadata().current() {
            for tag in metadata.tags() {
                if let Some(std_key) = tag.std_key {
                    let value = tag.value.to_string();
                    match std_key {
                        StandardTagKey::TrackTitle | StandardTagKey::Album => {
                            meta.title = Some(value);
                        }
                        StandardTagKey::Artist => {
                            meta.author = Some(value);
                        }
                        StandardTagKey::Composer => {
                            meta.narrator = Some(value);
                        }
                        StandardTagKey::Description => {
                            meta.description = Some(value);
                        }
                        StandardTagKey::Label => {
                            meta.publisher = Some(value);
                        }
                        StandardTagKey::Date => {
                            meta.year = value.chars().take(4).collect::<String>().parse().ok();
                        }
                        _ => {}
                    }
                }
            }
        }
        
        // Extract chapters from MP4 chpl atom
        meta.chapters = self.extract_chapters(&format)?;
        
        // Get total duration
        if let Some(track) = format.tracks().first() {
            if let (Some(n_frames), Some(sample_rate)) = 
                (track.codec_params.n_frames, track.codec_params.sample_rate) 
            {
                meta.duration_ms = Some((n_frames as u64 * 1000) / sample_rate as u64);
            }
        }
        
        Ok(meta)
    }
    
    fn extract_chapters(&self, format: &dyn FormatReader) -> Result<Vec<Chapter>, MetadataError> {
        let mut chapters = Vec::new();
        
        // Symphonia exposes chapters via cues
        if let Some(cues) = format.cues() {
            for (idx, cue) in cues.iter().enumerate() {
                let start_ms = (cue.start_ts as f64 / cue.start_offset_ts.unwrap_or(1) as f64 * 1000.0) as u64;
                
                // End time is start of next chapter or track end
                let end_ms = cues.get(idx + 1)
                    .map(|next| (next.start_ts as f64 / next.start_offset_ts.unwrap_or(1) as f64 * 1000.0) as u64)
                    .unwrap_or(u64::MAX);  // Will be clamped to duration
                
                chapters.push(Chapter {
                    index: idx as u32,
                    title: cue.tags.iter()
                        .find(|t| t.std_key == Some(StandardTagKey::TrackTitle))
                        .map(|t| t.value.to_string())
                        .unwrap_or_else(|| format!("Chapter {}", idx + 1)),
                    start_ms,
                    end_ms,
                });
            }
        }
        
        Ok(chapters)
    }
}
```

---

## Task 4: Chapter Extraction (`musicfs-metadata/src/chapters.rs`)

```rust
/// Generic chapter support for various formats
pub trait ChapterSource {
    fn chapters(&self) -> &[Chapter];
    fn chapter_at(&self, position_ms: u64) -> Option<&Chapter>;
}

impl ChapterSource for AudiobookMeta {
    fn chapters(&self) -> &[Chapter] {
        &self.chapters
    }
    
    fn chapter_at(&self, position_ms: u64) -> Option<&Chapter> {
        self.chapters.iter()
            .find(|c| position_ms >= c.start_ms && position_ms < c.end_ms)
    }
}

/// Virtual chapter file generator
pub struct ChapterFileGenerator;

impl ChapterFileGenerator {
    /// Generate virtual files for each chapter
    /// Example: book.m4b -> book/01 - Introduction.m4b.chapter
    pub fn generate_virtual_files(&self, meta: &AudiobookMeta, base_path: &VirtualPath) -> Vec<VirtualChapterFile> {
        meta.chapters.iter()
            .map(|chapter| {
                let filename = format!(
                    "{:02} - {}.chapter",
                    chapter.index + 1,
                    sanitize_filename(&chapter.title)
                );
                
                VirtualChapterFile {
                    path: base_path.join(&filename),
                    chapter_index: chapter.index,
                    start_ms: chapter.start_ms,
                    end_ms: chapter.end_ms,
                    title: chapter.title.clone(),
                }
            })
            .collect()
    }
}

#[derive(Debug)]
pub struct VirtualChapterFile {
    pub path: VirtualPath,
    pub chapter_index: u32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub title: String,
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}
```

---

## Task 5: Virtual Chapter Files (`musicfs-fuse/src/ops/chapters.rs`)

```rust
use crate::VirtualFs;

impl VirtualFs {
    /// Handle reads from virtual chapter files
    /// These return a byte-range reference to the parent M4B file
    pub async fn read_chapter(
        &self,
        chapter_file: &VirtualChapterFile,
        offset: u64,
        size: usize,
    ) -> Result<Vec<u8>, FuseError> {
        // Get the parent audiobook file
        let parent = self.get_parent_audiobook(&chapter_file.path)?;
        
        // Calculate byte range for this chapter
        // This requires knowing the audio bitrate to convert ms -> bytes
        let meta = self.get_audiobook_meta(&parent)?;
        let bitrate_bps = meta.bitrate.unwrap_or(128_000);  // Default 128kbps
        let bytes_per_ms = bitrate_bps / 8 / 1000;
        
        let chapter_start_bytes = chapter_file.start_ms * bytes_per_ms;
        let chapter_end_bytes = chapter_file.end_ms * bytes_per_ms;
        
        // Adjust offset to be within chapter
        let actual_offset = chapter_start_bytes + offset;
        let max_size = (chapter_end_bytes - actual_offset) as usize;
        let read_size = size.min(max_size);
        
        // Read from the actual file
        self.read_file(&parent, actual_offset, read_size).await
    }
    
    /// List chapter files for an audiobook
    pub fn list_chapters(&self, audiobook_path: &VirtualPath) -> Result<Vec<DirEntry>, FuseError> {
        let meta = self.get_audiobook_meta(audiobook_path)?;
        let generator = ChapterFileGenerator;
        
        let chapters = generator.generate_virtual_files(&meta, audiobook_path);
        
        Ok(chapters.into_iter()
            .map(|c| DirEntry {
                name: c.path.filename().to_string(),
                kind: FileType::RegularFile,
                size: self.estimate_chapter_size(&c),
            })
            .collect())
    }
    
    fn estimate_chapter_size(&self, chapter: &VirtualChapterFile) -> u64 {
        // Estimate based on duration and typical bitrate
        let duration_secs = (chapter.end_ms - chapter.start_ms) / 1000;
        duration_secs * 128_000 / 8  // 128kbps assumption
    }
}
```

---

## Task 6: Fingerprint Search Virtual Directory

```rust
/// Virtual directory for fingerprint search
/// /.search/fingerprint/{base64_fingerprint} -> matching files

impl SearchOps {
    pub async fn search_by_fingerprint(
        &self,
        fingerprint_path: &str,
    ) -> Result<Vec<SearchResult>, SearchError> {
        // Path format: /.search/fingerprint/{base64_encoded_fingerprint}
        let fp_bytes = base64::decode(fingerprint_path)
            .map_err(|_| SearchError::InvalidQuery)?;
        
        let fingerprint = AudioFingerprint::from_bytes(&fp_bytes)?;
        let matches = self.fingerprint_index.search(&fingerprint, 0.8, 20)?;
        
        let mut results = Vec::new();
        for m in matches {
            if let Some(file) = self.db.get_file_by_id(m.file_id)? {
                results.push(SearchResult {
                    path: file.virtual_path,
                    score: m.similarity,
                    snippet: format!("Similarity: {:.1}%", m.similarity * 100.0),
                });
            }
        }
        
        Ok(results)
    }
}
```

---

## Database Schema Additions

```sql
-- Fingerprint storage
CREATE TABLE IF NOT EXISTS fingerprints (
    file_id     INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    fingerprint BLOB NOT NULL,      -- Compressed chromaprint
    duration    INTEGER NOT NULL,   -- Duration in seconds
    indexed_at  INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_fingerprints_duration ON fingerprints(duration);

-- Audiobook chapters
CREATE TABLE IF NOT EXISTS chapters (
    id          INTEGER PRIMARY KEY,
    file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    chapter_idx INTEGER NOT NULL,
    title       TEXT NOT NULL,
    start_ms    INTEGER NOT NULL,
    end_ms      INTEGER NOT NULL,
    UNIQUE(file_id, chapter_idx)
);

CREATE INDEX IF NOT EXISTS idx_chapters_file ON chapters(file_id);
```

---

## Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_fingerprint_generation` | Unit | Chromaprint from audio (FR-14.4) |
| `test_fingerprint_similarity` | Unit | Bit comparison algorithm |
| `test_fingerprint_search` | Integration | Find similar tracks |
| `test_fingerprint_duplicates` | Integration | Detect duplicate audio |
| `test_m4b_parsing` | Unit | M4B metadata extraction (FR-24.2) |
| `test_chapter_extraction` | Unit | Chapter list from M4B |
| `test_virtual_chapter_files` | Integration | Chapter files appear in listing |
| `test_chapter_read` | Integration | Read chapter content |
| `test_audiobook_navigation` | E2E | Browse audiobook chapters |

---

## Exit Criteria

- [ ] Audio fingerprints generated from audio files
- [ ] Fingerprint similarity search finds matching tracks
- [ ] Duplicate detection works across library
- [ ] M4B files parsed with full metadata
- [ ] Chapters extracted and stored
- [ ] Virtual chapter files appear in directory listing
- [ ] Chapter files are readable (return correct byte range)
- [ ] All tests pass

---

## Architecture Alignment

Per requirements.md:
- FR-14.4: Audio fingerprint search ✓
- FR-24.2: Audiobook formats with chapters ✓

Per architecture.md section 4.3.4:
- FormatPlugin trait for M4B support ✓
- Chapter extraction via symphonia ✓
