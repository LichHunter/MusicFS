# Week 12: External Metadata Integration

**Phase**: 5 - P1 Feature Completion  
**Goal**: Integrate external metadata sources for automatic tagging and artwork  
**Requirements**: FR-21.1-21.4, FR-16.5

---

## Deliverables

| Task | Crate | Files | Requirements |
|------|-------|-------|--------------|
| MusicBrainz client | musicfs-external | `musicbrainz.rs` | FR-21.1 |
| Discogs client | musicfs-external | `discogs.rs` | FR-21.2 |
| Last.fm client | musicfs-external | `lastfm.rs` | FR-21.3 |
| AcoustID/Chromaprint | musicfs-external | `acoustid.rs` | FR-21.4 |
| Online artwork fetch | musicfs-external | `artwork_fetch.rs` | FR-16.5 |
| Metadata enrichment | musicfs-external | `enrichment.rs` | All |
| Plugin integration | musicfs-plugins | `metadata_plugin.rs` | FR-21.5 |

---

## Task 1: Create `musicfs-external` Crate

### 1.1 `Cargo.toml`

```toml
[package]
name = "musicfs-external"
version.workspace = true
edition.workspace = true

[dependencies]
musicfs-core = { path = "../musicfs-core" }
reqwest = { version = "0.11", features = ["json"] }
serde = { workspace = true, features = ["derive"] }
serde_json.workspace = true
tokio.workspace = true
tracing.workspace = true
thiserror.workspace = true
chromaprint = "0.6"  # Audio fingerprinting
base64 = "0.21"

[dev-dependencies]
wiremock = "0.5"  # Mock HTTP responses
tokio-test = "0.4"
```

### 1.2 `src/lib.rs`

```rust
pub mod musicbrainz;
pub mod discogs;
pub mod lastfm;
pub mod acoustid;
pub mod artwork_fetch;
pub mod enrichment;

pub use enrichment::MetadataEnricher;
```

---

## Task 2: MusicBrainz Client (`musicfs-external/src/musicbrainz.rs`)

```rust
use serde::Deserialize;

const MB_API: &str = "https://musicbrainz.org/ws/2";
const USER_AGENT: &str = "MusicFS/0.1.0 (https://github.com/user/musicfs)";

#[derive(Debug, Deserialize)]
pub struct MbRecording {
    pub id: String,
    pub title: String,
    pub length: Option<u64>,
    #[serde(rename = "artist-credit")]
    pub artist_credit: Vec<ArtistCredit>,
    pub releases: Option<Vec<MbRelease>>,
}

#[derive(Debug, Deserialize)]
pub struct MbRelease {
    pub id: String,
    pub title: String,
    pub date: Option<String>,
    #[serde(rename = "release-group")]
    pub release_group: Option<MbReleaseGroup>,
}

#[derive(Debug, Deserialize)]
pub struct MbReleaseGroup {
    pub id: String,
    #[serde(rename = "primary-type")]
    pub primary_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ArtistCredit {
    pub artist: MbArtist,
}

#[derive(Debug, Deserialize)]
pub struct MbArtist {
    pub id: String,
    pub name: String,
    #[serde(rename = "sort-name")]
    pub sort_name: String,
}

pub struct MusicBrainzClient {
    client: reqwest::Client,
    rate_limiter: RateLimiter,  // 1 req/sec per MB guidelines
}

impl MusicBrainzClient {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .expect("client build");
        
        Self {
            client,
            rate_limiter: RateLimiter::new(Duration::from_secs(1)),
        }
    }
    
    /// Search by recording title + artist (FR-21.1)
    pub async fn search_recording(
        &self,
        title: &str,
        artist: Option<&str>,
    ) -> Result<Vec<MbRecording>, ExternalError> {
        self.rate_limiter.wait().await;
        
        let mut query = format!("recording:{}", title);
        if let Some(artist) = artist {
            query.push_str(&format!(" AND artist:{}", artist));
        }
        
        let resp = self.client
            .get(format!("{}/recording", MB_API))
            .query(&[
                ("query", query.as_str()),
                ("fmt", "json"),
                ("limit", "5"),
            ])
            .send()
            .await?;
        
        let body: SearchResponse<MbRecording> = resp.json().await?;
        Ok(body.recordings)
    }
    
    /// Get release artwork from Cover Art Archive
    pub async fn get_cover_art(&self, release_id: &str) -> Result<Option<Vec<u8>>, ExternalError> {
        let url = format!("https://coverartarchive.org/release/{}/front-500", release_id);
        
        let resp = self.client.get(&url).send().await?;
        if resp.status() == 404 {
            return Ok(None);
        }
        
        let bytes = resp.bytes().await?;
        Ok(Some(bytes.to_vec()))
    }
    
    /// Lookup recording by MusicBrainz ID
    pub async fn get_recording(&self, mbid: &str) -> Result<MbRecording, ExternalError> {
        self.rate_limiter.wait().await;
        
        let resp = self.client
            .get(format!("{}/recording/{}", MB_API, mbid))
            .query(&[
                ("inc", "artist-credits+releases+release-groups"),
                ("fmt", "json"),
            ])
            .send()
            .await?;
        
        Ok(resp.json().await?)
    }
}

struct RateLimiter {
    interval: Duration,
    last_request: Mutex<Instant>,
}

impl RateLimiter {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_request: Mutex::new(Instant::now() - interval),
        }
    }
    
    async fn wait(&self) {
        let mut last = self.last_request.lock().await;
        let elapsed = last.elapsed();
        if elapsed < self.interval {
            tokio::time::sleep(self.interval - elapsed).await;
        }
        *last = Instant::now();
    }
}
```

---

## Task 3: Discogs Client (`musicfs-external/src/discogs.rs`)

```rust
const DISCOGS_API: &str = "https://api.discogs.com";

pub struct DiscogsClient {
    client: reqwest::Client,
    token: Option<String>,
    rate_limiter: RateLimiter,  // 60 req/min authenticated
}

impl DiscogsClient {
    pub fn new(token: Option<String>) -> Self;
    
    /// Search releases (FR-21.2)
    pub async fn search(
        &self,
        query: &str,
        artist: Option<&str>,
    ) -> Result<Vec<DiscogsRelease>, ExternalError>;
    
    /// Get master release details
    pub async fn get_master(&self, id: u64) -> Result<DiscogsMaster, ExternalError>;
    
    /// Get release images
    pub async fn get_images(&self, release_id: u64) -> Result<Vec<DiscogsImage>, ExternalError>;
}

#[derive(Debug, Deserialize)]
pub struct DiscogsRelease {
    pub id: u64,
    pub title: String,
    pub year: Option<u16>,
    pub thumb: Option<String>,
    pub master_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct DiscogsImage {
    pub uri: String,
    pub width: u32,
    pub height: u32,
    #[serde(rename = "type")]
    pub image_type: String,  // "primary" or "secondary"
}
```

---

## Task 4: Last.fm Client (`musicfs-external/src/lastfm.rs`)

```rust
const LASTFM_API: &str = "https://ws.audioscrobbler.com/2.0";

pub struct LastFmClient {
    client: reqwest::Client,
    api_key: String,
}

impl LastFmClient {
    pub fn new(api_key: String) -> Self;
    
    /// Get track info with play counts, tags (FR-21.3)
    pub async fn get_track_info(
        &self,
        track: &str,
        artist: &str,
    ) -> Result<LastFmTrack, ExternalError>;
    
    /// Get album info with artwork
    pub async fn get_album_info(
        &self,
        album: &str,
        artist: &str,
    ) -> Result<LastFmAlbum, ExternalError>;
    
    /// Get artist info
    pub async fn get_artist_info(&self, artist: &str) -> Result<LastFmArtist, ExternalError>;
}

#[derive(Debug, Deserialize)]
pub struct LastFmTrack {
    pub name: String,
    pub playcount: Option<u64>,
    pub listeners: Option<u64>,
    pub duration: Option<u64>,
    pub toptags: Option<Tags>,
    pub album: Option<LastFmAlbumRef>,
}

#[derive(Debug, Deserialize)]
pub struct LastFmAlbum {
    pub name: String,
    pub artist: String,
    pub image: Vec<LastFmImage>,
    pub tracks: Option<Tracks>,
}

#[derive(Debug, Deserialize)]
pub struct LastFmImage {
    #[serde(rename = "#text")]
    pub url: String,
    pub size: String,  // "small", "medium", "large", "extralarge", "mega"
}
```

---

## Task 5: AcoustID/Chromaprint (`musicfs-external/src/acoustid.rs`)

```rust
use chromaprint::{Fingerprinter, Configuration};

const ACOUSTID_API: &str = "https://api.acoustid.org/v2/lookup";

pub struct AcoustIdClient {
    client: reqwest::Client,
    api_key: String,
}

impl AcoustIdClient {
    pub fn new(api_key: String) -> Self;
    
    /// Generate fingerprint from audio data (FR-21.4)
    pub fn fingerprint(&self, samples: &[i16], sample_rate: u32) -> Result<String, ExternalError> {
        let config = Configuration::preset_test1();
        let mut fp = Fingerprinter::new(&config);
        
        fp.start(sample_rate, 1)?;  // mono
        fp.feed(samples)?;
        fp.finish()?;
        
        Ok(fp.fingerprint().to_string())
    }
    
    /// Lookup fingerprint on AcoustID database
    pub async fn lookup(
        &self,
        fingerprint: &str,
        duration: u32,
    ) -> Result<Vec<AcoustIdResult>, ExternalError> {
        let resp = self.client
            .get(ACOUSTID_API)
            .query(&[
                ("client", self.api_key.as_str()),
                ("fingerprint", fingerprint),
                ("duration", &duration.to_string()),
                ("meta", "recordings+releasegroups"),
            ])
            .send()
            .await?;
        
        let body: AcoustIdResponse = resp.json().await?;
        Ok(body.results)
    }
}

#[derive(Debug, Deserialize)]
pub struct AcoustIdResult {
    pub id: String,
    pub score: f32,
    pub recordings: Option<Vec<AcoustIdRecording>>,
}

#[derive(Debug, Deserialize)]
pub struct AcoustIdRecording {
    pub id: String,  // MusicBrainz recording ID
    pub title: Option<String>,
    pub artists: Option<Vec<AcoustIdArtist>>,
}
```

---

## Task 6: Online Artwork Fetch (`musicfs-external/src/artwork_fetch.rs`)

```rust
pub struct ArtworkFetcher {
    musicbrainz: MusicBrainzClient,
    discogs: Option<DiscogsClient>,
    lastfm: Option<LastFmClient>,
}

impl ArtworkFetcher {
    /// Fetch missing artwork from online sources (FR-16.5)
    /// Tries sources in order: MusicBrainz Cover Art Archive → Discogs → Last.fm
    pub async fn fetch_artwork(
        &self,
        artist: &str,
        album: &str,
        size: ArtworkSize,
    ) -> Result<Option<ArtworkData>, ExternalError> {
        // 1. Try MusicBrainz release search → Cover Art Archive
        if let Some(art) = self.try_musicbrainz(artist, album, size).await? {
            return Ok(Some(art));
        }
        
        // 2. Try Discogs
        if let Some(discogs) = &self.discogs {
            if let Some(art) = self.try_discogs(discogs, artist, album, size).await? {
                return Ok(Some(art));
            }
        }
        
        // 3. Try Last.fm
        if let Some(lastfm) = &self.lastfm {
            if let Some(art) = self.try_lastfm(lastfm, artist, album, size).await? {
                return Ok(Some(art));
            }
        }
        
        Ok(None)
    }
    
    async fn try_musicbrainz(
        &self,
        artist: &str,
        album: &str,
        size: ArtworkSize,
    ) -> Result<Option<ArtworkData>, ExternalError> {
        // Search for release, get cover art from Cover Art Archive
        let releases = self.musicbrainz.search_release(album, Some(artist)).await?;
        
        for release in releases.iter().take(3) {
            if let Some(art) = self.musicbrainz.get_cover_art(&release.id).await? {
                return Ok(Some(ArtworkData {
                    data: art,
                    source: ArtworkSource::MusicBrainz,
                    mime_type: "image/jpeg".to_string(),
                }));
            }
        }
        
        Ok(None)
    }
}

#[derive(Debug)]
pub struct ArtworkData {
    pub data: Vec<u8>,
    pub source: ArtworkSource,
    pub mime_type: String,
}

#[derive(Debug)]
pub enum ArtworkSource {
    MusicBrainz,
    Discogs,
    LastFm,
    Embedded,
}

pub enum ArtworkSize {
    Small,   // 150px
    Medium,  // 300px
    Large,   // 500px
    Original,
}
```

---

## Task 7: Metadata Enrichment (`musicfs-external/src/enrichment.rs`)

```rust
pub struct MetadataEnricher {
    musicbrainz: MusicBrainzClient,
    acoustid: Option<AcoustIdClient>,
    artwork_fetcher: ArtworkFetcher,
}

impl MetadataEnricher {
    /// Enrich metadata from external sources
    pub async fn enrich(&self, meta: &AudioMeta) -> Result<EnrichedMetadata, ExternalError> {
        let mut enriched = EnrichedMetadata::from(meta);
        
        // If we have title + artist, search MusicBrainz
        if let (Some(title), Some(artist)) = (&meta.title, &meta.artist) {
            let recordings = self.musicbrainz.search_recording(title, Some(artist)).await?;
            
            if let Some(best) = recordings.first() {
                enriched.musicbrainz_recording_id = Some(best.id.clone());
                
                // Enrich with release info
                if let Some(releases) = &best.releases {
                    if let Some(release) = releases.first() {
                        enriched.musicbrainz_release_id = Some(release.id.clone());
                    }
                }
            }
        }
        
        Ok(enriched)
    }
    
    /// Identify unknown track by audio fingerprint
    pub async fn identify_by_fingerprint(
        &self,
        samples: &[i16],
        sample_rate: u32,
        duration: u32,
    ) -> Result<Option<IdentifiedTrack>, ExternalError> {
        let acoustid = self.acoustid.as_ref()
            .ok_or(ExternalError::ServiceNotConfigured("AcoustID"))?;
        
        let fingerprint = acoustid.fingerprint(samples, sample_rate)?;
        let results = acoustid.lookup(&fingerprint, duration).await?;
        
        // Return best match above threshold
        results.into_iter()
            .filter(|r| r.score > 0.8)
            .flat_map(|r| r.recordings)
            .flatten()
            .next()
            .map(|rec| IdentifiedTrack {
                title: rec.title,
                musicbrainz_id: Some(rec.id),
                artists: rec.artists.map(|a| a.into_iter().map(|x| x.name).collect()),
            })
            .pipe(Ok)
    }
}

#[derive(Debug)]
pub struct EnrichedMetadata {
    pub original: AudioMeta,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_id: Option<String>,
    pub musicbrainz_artist_id: Option<String>,
    pub genres: Vec<String>,
    pub play_count: Option<u64>,
}

#[derive(Debug)]
pub struct IdentifiedTrack {
    pub title: Option<String>,
    pub musicbrainz_id: Option<String>,
    pub artists: Option<Vec<String>>,
}
```

---

## Configuration

```toml
[external]
# MusicBrainz (no auth required, rate limited to 1 req/sec)
musicbrainz.enabled = true

# Discogs (optional, requires token for higher rate limits)
discogs.enabled = true
discogs.token = "your_discogs_token"

# Last.fm (requires API key)
lastfm.enabled = true
lastfm.api_key = "your_lastfm_api_key"

# AcoustID (requires API key)
acoustid.enabled = true
acoustid.api_key = "your_acoustid_api_key"

# Artwork fetching behavior
artwork.fetch_missing = true
artwork.cache_fetched = true
artwork.preferred_size = "large"  # small, medium, large, original
```

---

## Tests

| Test | Type | Validates |
|------|------|-----------|
| `test_musicbrainz_search` | Integration | Recording search (FR-21.1) |
| `test_musicbrainz_cover_art` | Integration | Cover Art Archive |
| `test_discogs_search` | Integration | Release search (FR-21.2) |
| `test_lastfm_track_info` | Integration | Track metadata (FR-21.3) |
| `test_acoustid_fingerprint` | Unit | Chromaprint generation |
| `test_acoustid_lookup` | Integration | Fingerprint lookup (FR-21.4) |
| `test_artwork_fetch_cascade` | Integration | Multi-source artwork (FR-16.5) |
| `test_metadata_enrichment` | Integration | Full enrichment flow |
| `test_rate_limiting` | Unit | Rate limiter works |
| `test_mock_responses` | Unit | Offline testing with mocks |

---

## Exit Criteria

- [ ] MusicBrainz search returns relevant recordings
- [ ] Cover Art Archive artwork downloads work
- [ ] Discogs integration retrieves release info
- [ ] Last.fm integration retrieves track/artist info
- [ ] AcoustID fingerprinting identifies tracks
- [ ] Artwork fetcher tries all sources in cascade
- [ ] Metadata enricher adds external IDs
- [ ] Rate limiting prevents API abuse
- [ ] All tests pass with mock HTTP responses

---

## Architecture Alignment

Per requirements.md:
- FR-21.1: MusicBrainz for canonical metadata ✓
- FR-21.2: Discogs for release info, artwork ✓
- FR-21.3: Last.fm for play counts, tags ✓
- FR-21.4: AcoustID for audio fingerprinting ✓
- FR-16.5: Fetch missing artwork from online ✓

Per architecture.md section 4.3.4:
- External metadata via `MetadataPlugin` trait ✓
- Plugin architecture allows adding more sources ✓
