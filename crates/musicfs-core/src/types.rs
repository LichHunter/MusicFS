use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OriginId(pub String);

impl From<&str> for OriginId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for OriginId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VirtualPath(pub PathBuf);

impl VirtualPath {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    pub fn as_path(&self) -> &std::path::Path {
        &self.0
    }

    pub fn as_str(&self) -> &str {
        self.0.to_str().unwrap_or("")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealPath {
    pub origin_id: OriginId,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash(pub [u8; 8]);

impl ContentHash {
    pub fn from_bytes(data: &[u8]) -> Self {
        use xxhash_rust::xxh64::xxh64;
        Self(xxh64(data, 0).to_le_bytes())
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkHash(pub [u8; 8]);

impl ChunkHash {
    pub fn from_bytes(data: &[u8]) -> Self {
        use xxhash_rust::xxh64::xxh64;
        Self(xxh64(data, 0).to_le_bytes())
    }

    pub fn as_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn to_hex(&self) -> String {
        self.as_hex()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        let bytes = hex::decode(s).ok()?;
        if bytes.len() != 8 {
            return None;
        }
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&bytes);
        Some(Self(arr))
    }
}

impl std::fmt::Display for ChunkHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_hex())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AudioFormat {
    Flac,
    Mp3,
    Opus,
    Vorbis,
    Aac,
    Alac,
    Wav,
    #[default]
    Unknown,
}

impl AudioFormat {
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "flac" => Self::Flac,
            "mp3" => Self::Mp3,
            "opus" => Self::Opus,
            "ogg" => Self::Vorbis,
            "m4a" | "aac" => Self::Aac,
            "wav" => Self::Wav,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AudioMeta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub duration_ms: Option<u64>,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub format: AudioFormat,
    pub track_total: Option<u32>,
    pub disc_total: Option<u32>,
    pub date: Option<String>,
    pub composer: Option<String>,
    pub comment: Option<String>,
    pub lyrics: Option<String>,
    pub copyright: Option<String>,
    pub compilation: Option<bool>,
    pub artist_sort: Option<String>,
    pub album_artist_sort: Option<String>,
    pub album_sort: Option<String>,
    pub title_sort: Option<String>,
    pub mb_recording_id: Option<String>,
    pub mb_album_id: Option<String>,
    pub mb_artist_id: Option<String>,
    pub mb_album_artist_id: Option<String>,
    pub mb_release_group_id: Option<String>,
    pub replaygain_track_gain: Option<f32>,
    pub replaygain_track_peak: Option<f32>,
    pub replaygain_album_gain: Option<f32>,
    pub replaygain_album_peak: Option<f32>,
    pub channels: Option<u32>,
    pub bits_per_sample: Option<u32>,
    pub encoder: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMeta {
    pub id: FileId,
    pub virtual_path: VirtualPath,
    pub real_path: RealPath,
    pub size: u64,
    pub mtime: SystemTime,
    pub content_hash: Option<ContentHash>,
    pub audio: Option<AudioMeta>,
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: SystemTime,
}

#[derive(Debug, Clone)]
pub struct FileStat {
    pub size: u64,
    pub mtime: SystemTime,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
    #[default]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_hash() {
        let data = b"hello world";
        let hash1 = ContentHash::from_bytes(data);
        let hash2 = ContentHash::from_bytes(data);
        assert_eq!(hash1, hash2);

        let hash3 = ContentHash::from_bytes(b"different");
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_audio_format_from_extension() {
        assert_eq!(AudioFormat::from_extension("flac"), AudioFormat::Flac);
        assert_eq!(AudioFormat::from_extension("MP3"), AudioFormat::Mp3);
        assert_eq!(AudioFormat::from_extension("unknown"), AudioFormat::Unknown);
    }

    #[test]
    fn test_virtual_path() {
        let path = VirtualPath::new("/Artist/Album/Track.flac");
        assert_eq!(path.as_str(), "/Artist/Album/Track.flac");
    }
}
