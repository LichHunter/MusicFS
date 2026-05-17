//! FLAC format handler for metadata synthesis.
//!
//! FLAC files use Vorbis comments for metadata. The file structure is:
//! - "fLaC" marker (4 bytes)
//! - STREAMINFO block (mandatory, 38 bytes total: 4 header + 34 data)
//! - Optional metadata blocks (VORBIS_COMMENT, PICTURE, PADDING, etc.)
//! - Audio frames
//!
//! CRITICAL: STREAMINFO must be preserved from the original file as it contains
//! MD5 checksum, sample count, and audio properties that must match the audio data.

use crate::{FormatError, FormatHandler, FormatLayout};
use lofty::config::ParseOptions;
use lofty::file::AudioFile;
use lofty::flac::FlacFile;
use lofty::ogg::VorbisComments;
use lofty::tag::Accessor;
use musicfs_core::{AudioFormat, AudioMeta};
use std::borrow::Cow;
use std::io::Cursor;

/// FLAC stream marker: "fLaC" in ASCII
const FLAC_MARKER: &[u8; 4] = b"fLaC";

/// FLAC metadata block types
const BLOCK_TYPE_STREAMINFO: u8 = 0;
const BLOCK_TYPE_VORBIS_COMMENT: u8 = 4;

/// STREAMINFO block data size (always 34 bytes)
const STREAMINFO_DATA_SIZE: usize = 34;

/// Metadata block header size (1 byte type/flags + 3 bytes length)
const BLOCK_HEADER_SIZE: usize = 4;

/// Full STREAMINFO block size (header + data)
const STREAMINFO_BLOCK_SIZE: usize = BLOCK_HEADER_SIZE + STREAMINFO_DATA_SIZE;

pub struct FlacHandler;

impl FlacHandler {
    pub fn new() -> Self {
        Self
    }

    /// Parse FLAC metadata block header.
    /// Returns (is_last, block_type, block_size).
    fn parse_block_header(data: &[u8]) -> Option<(bool, u8, usize)> {
        if data.len() < BLOCK_HEADER_SIZE {
            return None;
        }
        let is_last = (data[0] & 0x80) != 0;
        let block_type = data[0] & 0x7F;
        let block_size =
            ((data[1] as usize) << 16) | ((data[2] as usize) << 8) | (data[3] as usize);
        Some((is_last, block_type, block_size))
    }

    /// Write a metadata block header.
    fn write_block_header(is_last: bool, block_type: u8, size: usize) -> [u8; 4] {
        let type_byte = if is_last {
            block_type | 0x80
        } else {
            block_type
        };
        [
            type_byte,
            ((size >> 16) & 0xFF) as u8,
            ((size >> 8) & 0xFF) as u8,
            (size & 0xFF) as u8,
        ]
    }

    /// Build Vorbis comments from AudioMeta.
    fn build_vorbis_comments(metadata: &AudioMeta) -> VorbisComments {
        let mut tag = VorbisComments::default();

        // Basic fields (using Accessor trait)
        if let Some(ref title) = metadata.title {
            tag.set_title(title.clone());
        }
        if let Some(ref artist) = metadata.artist {
            tag.set_artist(artist.clone());
        }
        if let Some(ref album) = metadata.album {
            tag.set_album(album.clone());
        }
        if let Some(ref genre) = metadata.genre {
            tag.set_genre(genre.clone());
        }

        // Album artist
        if let Some(ref album_artist) = metadata.album_artist {
            tag.insert("ALBUMARTIST".to_string(), album_artist.clone());
        }

        // Year/Date
        if let Some(ref date) = metadata.date {
            tag.insert("DATE".to_string(), date.clone());
        } else if let Some(year) = metadata.year {
            tag.insert("DATE".to_string(), year.to_string());
        }

        // Track/Disc numbers
        if let Some(track) = metadata.track {
            tag.insert("TRACKNUMBER".to_string(), track.to_string());
        }
        if let Some(track_total) = metadata.track_total {
            tag.insert("TRACKTOTAL".to_string(), track_total.to_string());
        }
        if let Some(disc) = metadata.disc {
            tag.insert("DISCNUMBER".to_string(), disc.to_string());
        }
        if let Some(disc_total) = metadata.disc_total {
            tag.insert("DISCTOTAL".to_string(), disc_total.to_string());
        }

        // Extended metadata
        if let Some(ref composer) = metadata.composer {
            tag.insert("COMPOSER".to_string(), composer.clone());
        }
        if let Some(ref comment) = metadata.comment {
            tag.insert("COMMENT".to_string(), comment.clone());
        }
        if let Some(ref lyrics) = metadata.lyrics {
            tag.insert("LYRICS".to_string(), lyrics.clone());
        }
        if let Some(ref copyright) = metadata.copyright {
            tag.insert("COPYRIGHT".to_string(), copyright.clone());
        }
        if let Some(compilation) = metadata.compilation {
            tag.insert(
                "COMPILATION".to_string(),
                if compilation { "1" } else { "0" }.to_string(),
            );
        }

        // Sort fields
        if let Some(ref title_sort) = metadata.title_sort {
            tag.insert("TITLESORT".to_string(), title_sort.clone());
        }
        if let Some(ref artist_sort) = metadata.artist_sort {
            tag.insert("ARTISTSORT".to_string(), artist_sort.clone());
        }
        if let Some(ref album_sort) = metadata.album_sort {
            tag.insert("ALBUMSORT".to_string(), album_sort.clone());
        }
        if let Some(ref album_artist_sort) = metadata.album_artist_sort {
            tag.insert("ALBUMARTISTSORT".to_string(), album_artist_sort.clone());
        }

        // MusicBrainz IDs
        if let Some(ref mb_recording_id) = metadata.mb_recording_id {
            tag.insert("MUSICBRAINZ_TRACKID".to_string(), mb_recording_id.clone());
        }
        if let Some(ref mb_album_id) = metadata.mb_album_id {
            tag.insert("MUSICBRAINZ_ALBUMID".to_string(), mb_album_id.clone());
        }
        if let Some(ref mb_artist_id) = metadata.mb_artist_id {
            tag.insert("MUSICBRAINZ_ARTISTID".to_string(), mb_artist_id.clone());
        }
        if let Some(ref mb_album_artist_id) = metadata.mb_album_artist_id {
            tag.insert(
                "MUSICBRAINZ_ALBUMARTISTID".to_string(),
                mb_album_artist_id.clone(),
            );
        }
        if let Some(ref mb_release_group_id) = metadata.mb_release_group_id {
            tag.insert(
                "MUSICBRAINZ_RELEASEGROUPID".to_string(),
                mb_release_group_id.clone(),
            );
        }

        // ReplayGain
        if let Some(gain) = metadata.replaygain_track_gain {
            tag.insert(
                "REPLAYGAIN_TRACK_GAIN".to_string(),
                format!("{:.2} dB", gain),
            );
        }
        if let Some(peak) = metadata.replaygain_track_peak {
            tag.insert("REPLAYGAIN_TRACK_PEAK".to_string(), format!("{:.6}", peak));
        }
        if let Some(gain) = metadata.replaygain_album_gain {
            tag.insert(
                "REPLAYGAIN_ALBUM_GAIN".to_string(),
                format!("{:.2} dB", gain),
            );
        }
        if let Some(peak) = metadata.replaygain_album_peak {
            tag.insert("REPLAYGAIN_ALBUM_PEAK".to_string(), format!("{:.6}", peak));
        }

        // Encoder
        if let Some(ref encoder) = metadata.encoder {
            tag.insert("ENCODER".to_string(), encoder.clone());
        }

        tag
    }

    /// Serialize Vorbis comments to bytes (without block header).
    /// Format: vendor_length (4 LE) + vendor + comment_count (4 LE) + comments
    fn serialize_vorbis_comments(tag: &VorbisComments) -> Vec<u8> {
        let vendor = tag.vendor();
        let vendor = if vendor.is_empty() { "musicfs" } else { vendor };
        let mut data = Vec::new();

        // Vendor string (little-endian length + UTF-8 string)
        let vendor_bytes = vendor.as_bytes();
        data.extend_from_slice(&(vendor_bytes.len() as u32).to_le_bytes());
        data.extend_from_slice(vendor_bytes);

        // Collect all comments
        let comments: Vec<_> = tag.items().collect();
        data.extend_from_slice(&(comments.len() as u32).to_le_bytes());

        for (key, value) in comments {
            let comment = format!("{}={}", key, value);
            let comment_bytes = comment.as_bytes();
            data.extend_from_slice(&(comment_bytes.len() as u32).to_le_bytes());
            data.extend_from_slice(comment_bytes);
        }

        data
    }

    /// Extract metadata from Vorbis comments tag.
    fn extract_from_vorbis_comments(tag: &VorbisComments) -> AudioMeta {
        let mut meta = AudioMeta::default();
        meta.format = AudioFormat::Flac;

        // Basic fields (using Accessor trait)
        meta.title = tag.title().map(|c: Cow<'_, str>| c.into_owned());
        meta.artist = tag.artist().map(|c: Cow<'_, str>| c.into_owned());
        meta.album = tag.album().map(|c: Cow<'_, str>| c.into_owned());
        meta.genre = tag.genre().map(|c: Cow<'_, str>| c.into_owned());

        // Album artist
        meta.album_artist = tag.get("ALBUMARTIST").map(String::from);

        // Date/Year
        meta.date = tag.get("DATE").map(String::from);
        if let Some(ref date) = meta.date {
            if let Some(year_str) = date.split('-').next() {
                meta.year = year_str.parse().ok();
            }
        }

        // Track/Disc numbers
        meta.track = tag.get("TRACKNUMBER").and_then(|s| s.parse().ok());
        meta.track_total = tag.get("TRACKTOTAL").and_then(|s| s.parse().ok());
        meta.disc = tag.get("DISCNUMBER").and_then(|s| s.parse().ok());
        meta.disc_total = tag.get("DISCTOTAL").and_then(|s| s.parse().ok());

        // Extended metadata
        meta.composer = tag.get("COMPOSER").map(String::from);
        meta.comment = tag.get("COMMENT").map(String::from);
        meta.lyrics = tag.get("LYRICS").map(String::from);
        meta.copyright = tag.get("COPYRIGHT").map(String::from);
        meta.compilation = tag
            .get("COMPILATION")
            .map(|s| s == "1" || s.eq_ignore_ascii_case("true"));

        // Sort fields
        meta.title_sort = tag.get("TITLESORT").map(String::from);
        meta.artist_sort = tag.get("ARTISTSORT").map(String::from);
        meta.album_sort = tag.get("ALBUMSORT").map(String::from);
        meta.album_artist_sort = tag.get("ALBUMARTISTSORT").map(String::from);

        // MusicBrainz IDs
        meta.mb_recording_id = tag.get("MUSICBRAINZ_TRACKID").map(String::from);
        meta.mb_album_id = tag.get("MUSICBRAINZ_ALBUMID").map(String::from);
        meta.mb_artist_id = tag.get("MUSICBRAINZ_ARTISTID").map(String::from);
        meta.mb_album_artist_id = tag.get("MUSICBRAINZ_ALBUMARTISTID").map(String::from);
        meta.mb_release_group_id = tag.get("MUSICBRAINZ_RELEASEGROUPID").map(String::from);

        // ReplayGain
        meta.replaygain_track_gain = tag
            .get("REPLAYGAIN_TRACK_GAIN")
            .and_then(|s| Self::parse_replaygain_value(s));
        meta.replaygain_track_peak = tag
            .get("REPLAYGAIN_TRACK_PEAK")
            .and_then(|s| s.parse().ok());
        meta.replaygain_album_gain = tag
            .get("REPLAYGAIN_ALBUM_GAIN")
            .and_then(|s| Self::parse_replaygain_value(s));
        meta.replaygain_album_peak = tag
            .get("REPLAYGAIN_ALBUM_PEAK")
            .and_then(|s| s.parse().ok());

        // Encoder
        meta.encoder = tag.get("ENCODER").map(String::from);

        meta
    }

    /// Parse ReplayGain value, stripping optional "dB" suffix.
    fn parse_replaygain_value(value: &str) -> Option<f32> {
        value
            .trim()
            .trim_end_matches(" dB")
            .trim_end_matches("dB")
            .parse()
            .ok()
    }
}

impl Default for FlacHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatHandler for FlacHandler {
    fn id(&self) -> &'static str {
        "flac"
    }

    fn name(&self) -> &'static str {
        "FLAC"
    }

    fn extensions(&self) -> &[&'static str] {
        &["flac"]
    }

    fn mime_types(&self) -> &[&'static str] {
        &["audio/flac", "audio/x-flac"]
    }

    fn analyze(&self, data: &[u8], file_size: u64) -> Result<FormatLayout, FormatError> {
        // Verify FLAC marker
        if data.len() < FLAC_MARKER.len() || &data[0..4] != FLAC_MARKER {
            return Err(FormatError::InvalidData("Not a FLAC file".to_string()));
        }

        let mut offset = FLAC_MARKER.len();
        let mut streaminfo_data: Option<Vec<u8>> = None;

        // Parse metadata blocks to find audio_start and extract STREAMINFO
        loop {
            if offset + BLOCK_HEADER_SIZE > data.len() {
                return Err(FormatError::InvalidData(
                    "Truncated FLAC metadata".to_string(),
                ));
            }

            let (is_last, block_type, block_size) = Self::parse_block_header(&data[offset..])
                .ok_or_else(|| FormatError::InvalidData("Invalid block header".to_string()))?;

            // Extract STREAMINFO block data (without header)
            if block_type == BLOCK_TYPE_STREAMINFO {
                if block_size != STREAMINFO_DATA_SIZE {
                    return Err(FormatError::InvalidData(format!(
                        "Invalid STREAMINFO size: {} (expected {})",
                        block_size, STREAMINFO_DATA_SIZE
                    )));
                }
                let data_start = offset + BLOCK_HEADER_SIZE;
                let data_end = data_start + block_size;
                if data_end > data.len() {
                    return Err(FormatError::InvalidData(
                        "Truncated STREAMINFO block".to_string(),
                    ));
                }
                streaminfo_data = Some(data[data_start..data_end].to_vec());
            }

            offset += BLOCK_HEADER_SIZE + block_size;

            if is_last {
                break;
            }
        }

        let streaminfo = streaminfo_data
            .ok_or_else(|| FormatError::InvalidData("Missing STREAMINFO block".to_string()))?;

        Ok(FormatLayout {
            audio_start: offset as u64,
            audio_end: file_size,
            format: AudioFormat::Flac,
            format_data: Some(streaminfo),
        })
    }

    fn synthesize(
        &self,
        metadata: &AudioMeta,
        layout: &FormatLayout,
    ) -> Result<Vec<u8>, FormatError> {
        // STREAMINFO must be preserved from original
        let streaminfo_data = layout.format_data.as_ref().ok_or_else(|| {
            FormatError::SynthesisFailed("Missing STREAMINFO data in layout".to_string())
        })?;

        if streaminfo_data.len() != STREAMINFO_DATA_SIZE {
            return Err(FormatError::SynthesisFailed(format!(
                "Invalid STREAMINFO size: {} (expected {})",
                streaminfo_data.len(),
                STREAMINFO_DATA_SIZE
            )));
        }

        // Build Vorbis comments
        let vorbis_tag = Self::build_vorbis_comments(metadata);
        let vorbis_data = Self::serialize_vorbis_comments(&vorbis_tag);

        // Calculate total header size
        let total_size =
            FLAC_MARKER.len() + STREAMINFO_BLOCK_SIZE + BLOCK_HEADER_SIZE + vorbis_data.len();
        let mut buffer = Vec::with_capacity(total_size);

        // Write FLAC marker
        buffer.extend_from_slice(FLAC_MARKER);

        // Write STREAMINFO block (not last)
        let streaminfo_header =
            Self::write_block_header(false, BLOCK_TYPE_STREAMINFO, STREAMINFO_DATA_SIZE);
        buffer.extend_from_slice(&streaminfo_header);
        buffer.extend_from_slice(streaminfo_data);

        // Write VORBIS_COMMENT block (last)
        let vorbis_header =
            Self::write_block_header(true, BLOCK_TYPE_VORBIS_COMMENT, vorbis_data.len());
        buffer.extend_from_slice(&vorbis_header);
        buffer.extend_from_slice(&vorbis_data);

        Ok(buffer)
    }

    fn extract(&self, data: &[u8]) -> Result<AudioMeta, FormatError> {
        let mut cursor = Cursor::new(data);

        let flac_file = FlacFile::read_from(&mut cursor, ParseOptions::new())
            .map_err(|e| FormatError::InvalidData(e.to_string()))?;

        let tag = flac_file
            .vorbis_comments()
            .ok_or_else(|| FormatError::InvalidData("No Vorbis comments found".to_string()))?;

        Ok(Self::extract_from_vorbis_comments(tag))
    }

    fn estimate_header_size(&self, _metadata: &AudioMeta) -> usize {
        // fLaC (4) + STREAMINFO (38) + VORBIS_COMMENT header (4) + typical comments (~4KB)
        8192
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_meta() -> AudioMeta {
        AudioMeta {
            title: Some("Test Title".to_string()),
            artist: Some("Test Artist".to_string()),
            album: Some("Test Album".to_string()),
            album_artist: Some("Test Album Artist".to_string()),
            genre: Some("Rock".to_string()),
            year: Some(2024),
            track: Some(5),
            track_total: Some(12),
            disc: Some(1),
            disc_total: Some(2),
            format: AudioFormat::Flac,
            date: Some("2024-03-15".to_string()),
            composer: Some("Test Composer".to_string()),
            comment: Some("Test Comment".to_string()),
            lyrics: Some("Test Lyrics\nLine 2".to_string()),
            copyright: Some("2024 Test Copyright".to_string()),
            compilation: Some(false),
            title_sort: Some("Title, Test".to_string()),
            artist_sort: Some("Artist, Test".to_string()),
            album_sort: Some("Album, Test".to_string()),
            album_artist_sort: Some("Album Artist, Test".to_string()),
            mb_recording_id: Some("rec-12345".to_string()),
            mb_album_id: Some("alb-12345".to_string()),
            mb_artist_id: Some("art-12345".to_string()),
            mb_album_artist_id: Some("albart-12345".to_string()),
            mb_release_group_id: Some("rg-12345".to_string()),
            replaygain_track_gain: Some(-6.5),
            replaygain_track_peak: Some(0.987654),
            replaygain_album_gain: Some(-5.2),
            replaygain_album_peak: Some(0.999999),
            encoder: Some("FLAC 1.4.0".to_string()),
            ..Default::default()
        }
    }

    /// Create a minimal valid FLAC file header for testing.
    fn make_minimal_flac_header() -> Vec<u8> {
        let mut data = Vec::new();

        // FLAC marker
        data.extend_from_slice(b"fLaC");

        // STREAMINFO block (last=true for minimal file)
        // Header: type=0 (STREAMINFO), last=1, size=34
        data.push(0x80); // 0x80 = last flag set, type 0
        data.push(0x00);
        data.push(0x00);
        data.push(0x22); // 34 bytes

        // STREAMINFO data (34 bytes) - minimal valid values
        // min_block_size (16 bits) = 4096
        data.push(0x10);
        data.push(0x00);
        // max_block_size (16 bits) = 4096
        data.push(0x10);
        data.push(0x00);
        // min_frame_size (24 bits) = 0 (unknown)
        data.push(0x00);
        data.push(0x00);
        data.push(0x00);
        // max_frame_size (24 bits) = 0 (unknown)
        data.push(0x00);
        data.push(0x00);
        data.push(0x00);
        // sample_rate (20 bits) = 44100, channels-1 (3 bits) = 1, bits-1 (5 bits) = 15
        // 44100 = 0xAC44, channels=2 (1), bits=16 (15)
        // Packed: SSSS SSSS SSSS SSSS SSSS CCCC CBBB BB
        // 0xAC44 << 12 | (1 << 9) | (15 << 4) = ...
        // Let's use simpler encoding:
        // Byte 0-1: sample_rate high 16 bits of 20
        // Byte 2: sample_rate low 4 bits | channels 3 bits | bits high 1 bit
        // Byte 3: bits low 4 bits | total_samples high 4 bits
        // Actually the format is:
        // 20 bits sample rate, 3 bits channels-1, 5 bits bits-1, 36 bits total samples
        // 44100 = 0x0AC44
        data.push(0x0A); // sample_rate bits 19-12
        data.push(0xC4); // sample_rate bits 11-4
        data.push(0x42); // sample_rate bits 3-0 (0x4), channels-1 (0x1=stereo), bits-1 high bit (0)
        data.push(0xF0); // bits-1 low 4 bits (0xF=15, so 16 bits), total_samples high 4 bits (0)
                         // total_samples (remaining 32 bits) = 0
        data.push(0x00);
        data.push(0x00);
        data.push(0x00);
        data.push(0x00);
        // MD5 signature (128 bits = 16 bytes)
        data.extend_from_slice(&[0u8; 16]);

        data
    }

    /// Create a FLAC header with Vorbis comments for testing extract().
    fn make_flac_with_vorbis_comments() -> Vec<u8> {
        let mut data = Vec::new();

        // FLAC marker
        data.extend_from_slice(b"fLaC");

        // STREAMINFO block (not last)
        data.push(0x00); // type=0, last=0
        data.push(0x00);
        data.push(0x00);
        data.push(0x22); // 34 bytes

        // STREAMINFO data (34 bytes)
        data.push(0x10);
        data.push(0x00);
        data.push(0x10);
        data.push(0x00);
        data.extend_from_slice(&[0u8; 6]); // frame sizes
        data.push(0x0A);
        data.push(0xC4);
        data.push(0x42);
        data.push(0xF0);
        data.extend_from_slice(&[0u8; 4]); // total samples
        data.extend_from_slice(&[0u8; 16]); // MD5

        // VORBIS_COMMENT block (last)
        // Vendor: "test"
        // Comments: TITLE=Test Song, ARTIST=Test Artist
        let vendor = b"test";
        let comments = [
            b"TITLE=Test Song".as_slice(),
            b"ARTIST=Test Artist".as_slice(),
            b"ALBUM=Test Album".as_slice(),
            b"TRACKNUMBER=3".as_slice(),
            b"REPLAYGAIN_TRACK_GAIN=-5.50 dB".as_slice(),
        ];

        let mut vorbis_data = Vec::new();
        // Vendor length (LE)
        vorbis_data.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        vorbis_data.extend_from_slice(vendor);
        // Comment count (LE)
        vorbis_data.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for comment in &comments {
            vorbis_data.extend_from_slice(&(comment.len() as u32).to_le_bytes());
            vorbis_data.extend_from_slice(*comment);
        }

        // VORBIS_COMMENT header
        data.push(0x84); // type=4, last=1
        data.push(((vorbis_data.len() >> 16) & 0xFF) as u8);
        data.push(((vorbis_data.len() >> 8) & 0xFF) as u8);
        data.push((vorbis_data.len() & 0xFF) as u8);
        data.extend_from_slice(&vorbis_data);

        data
    }

    #[test]
    fn test_id_and_name() {
        let handler = FlacHandler::new();
        assert_eq!(handler.id(), "flac");
        assert_eq!(handler.name(), "FLAC");
    }

    #[test]
    fn test_extensions_and_mime_types() {
        let handler = FlacHandler::new();
        assert_eq!(handler.extensions(), &["flac"]);
        assert_eq!(handler.mime_types(), &["audio/flac", "audio/x-flac"]);
    }

    #[test]
    fn test_estimate_header_size() {
        let handler = FlacHandler::new();
        let meta = AudioMeta::default();
        assert_eq!(handler.estimate_header_size(&meta), 8192);
    }

    #[test]
    fn test_analyze_valid_flac() {
        let handler = FlacHandler::new();
        let data = make_minimal_flac_header();
        let file_size = data.len() as u64 + 1000; // Pretend there's audio data

        let result = handler.analyze(&data, file_size);
        assert!(result.is_ok(), "analyze failed: {:?}", result.err());

        let layout = result.unwrap();
        assert_eq!(layout.audio_start, 42); // 4 (marker) + 38 (STREAMINFO)
        assert_eq!(layout.audio_end, file_size);
        assert_eq!(layout.format, AudioFormat::Flac);
        assert!(layout.format_data.is_some());
        assert_eq!(
            layout.format_data.as_ref().unwrap().len(),
            STREAMINFO_DATA_SIZE
        );
    }

    #[test]
    fn test_analyze_invalid_marker() {
        let handler = FlacHandler::new();
        let data = b"ID3\x04\x00\x00"; // MP3 header, not FLAC

        let result = handler.analyze(data, 1000);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), FormatError::InvalidData(_)));
    }

    #[test]
    fn test_analyze_truncated() {
        let handler = FlacHandler::new();
        let data = b"fLaC"; // Just the marker, no blocks

        let result = handler.analyze(data, 4);
        assert!(result.is_err());
    }

    #[test]
    fn test_synthesize_creates_valid_flac_header() {
        let handler = FlacHandler::new();
        let meta = make_test_meta();

        // Create layout with STREAMINFO
        let original_data = make_minimal_flac_header();
        let layout = handler
            .analyze(&original_data, original_data.len() as u64)
            .unwrap();

        let result = handler.synthesize(&meta, &layout);
        assert!(result.is_ok(), "synthesize failed: {:?}", result.err());

        let bytes = result.unwrap();

        // Verify FLAC marker
        assert!(bytes.len() >= 4);
        assert_eq!(&bytes[0..4], b"fLaC");

        // Verify STREAMINFO block header
        assert_eq!(bytes[4] & 0x7F, BLOCK_TYPE_STREAMINFO); // Type 0
        assert_eq!(bytes[4] & 0x80, 0); // Not last

        // Verify STREAMINFO size
        let streaminfo_size =
            ((bytes[5] as usize) << 16) | ((bytes[6] as usize) << 8) | (bytes[7] as usize);
        assert_eq!(streaminfo_size, STREAMINFO_DATA_SIZE);

        // Verify VORBIS_COMMENT block follows
        let vorbis_offset = 4 + 4 + STREAMINFO_DATA_SIZE;
        assert_eq!(bytes[vorbis_offset] & 0x7F, BLOCK_TYPE_VORBIS_COMMENT);
        assert_eq!(bytes[vorbis_offset] & 0x80, 0x80); // Is last
    }

    #[test]
    fn test_synthesize_preserves_streaminfo() {
        let handler = FlacHandler::new();
        let meta = AudioMeta::default();

        // Create layout with specific STREAMINFO
        let original_data = make_minimal_flac_header();
        let layout = handler
            .analyze(&original_data, original_data.len() as u64)
            .unwrap();
        let original_streaminfo = layout.format_data.as_ref().unwrap().clone();

        let synthesized = handler.synthesize(&meta, &layout).unwrap();

        // Extract STREAMINFO from synthesized header
        let streaminfo_start = 4 + 4; // After marker and header
        let streaminfo_end = streaminfo_start + STREAMINFO_DATA_SIZE;
        let synthesized_streaminfo = &synthesized[streaminfo_start..streaminfo_end];

        assert_eq!(synthesized_streaminfo, original_streaminfo.as_slice());
    }

    #[test]
    fn test_synthesize_missing_streaminfo() {
        let handler = FlacHandler::new();
        let meta = AudioMeta::default();
        let layout = FormatLayout {
            audio_start: 42,
            audio_end: 1000,
            format: AudioFormat::Flac,
            format_data: None, // Missing STREAMINFO
        };

        let result = handler.synthesize(&meta, &layout);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            FormatError::SynthesisFailed(_)
        ));
    }

    #[test]
    fn test_extract_from_flac() {
        let handler = FlacHandler::new();
        let data = make_flac_with_vorbis_comments();

        let result = handler.extract(&data);
        assert!(result.is_ok(), "extract failed: {:?}", result.err());

        let meta = result.unwrap();
        assert_eq!(meta.title, Some("Test Song".to_string()));
        assert_eq!(meta.artist, Some("Test Artist".to_string()));
        assert_eq!(meta.album, Some("Test Album".to_string()));
        assert_eq!(meta.track, Some(3));
        assert_eq!(meta.format, AudioFormat::Flac);

        // Check ReplayGain parsing
        let gain = meta.replaygain_track_gain.unwrap();
        assert!((gain - (-5.5)).abs() < 0.01);
    }

    #[test]
    fn test_build_and_extract_vorbis_comments() {
        let original_meta = make_test_meta();
        let tag = FlacHandler::build_vorbis_comments(&original_meta);
        let extracted = FlacHandler::extract_from_vorbis_comments(&tag);

        assert_eq!(extracted.title, original_meta.title);
        assert_eq!(extracted.artist, original_meta.artist);
        assert_eq!(extracted.album, original_meta.album);
        assert_eq!(extracted.album_artist, original_meta.album_artist);
        assert_eq!(extracted.genre, original_meta.genre);
        assert_eq!(extracted.track, original_meta.track);
        assert_eq!(extracted.track_total, original_meta.track_total);
        assert_eq!(extracted.disc, original_meta.disc);
        assert_eq!(extracted.disc_total, original_meta.disc_total);
        assert_eq!(extracted.composer, original_meta.composer);
        assert_eq!(extracted.comment, original_meta.comment);
        assert_eq!(extracted.lyrics, original_meta.lyrics);
        assert_eq!(extracted.copyright, original_meta.copyright);
        assert_eq!(extracted.compilation, original_meta.compilation);
        assert_eq!(extracted.title_sort, original_meta.title_sort);
        assert_eq!(extracted.artist_sort, original_meta.artist_sort);
        assert_eq!(extracted.album_sort, original_meta.album_sort);
        assert_eq!(extracted.album_artist_sort, original_meta.album_artist_sort);
        assert_eq!(extracted.mb_recording_id, original_meta.mb_recording_id);
        assert_eq!(extracted.mb_album_id, original_meta.mb_album_id);
        assert_eq!(extracted.mb_artist_id, original_meta.mb_artist_id);
        assert_eq!(
            extracted.mb_album_artist_id,
            original_meta.mb_album_artist_id
        );
        assert_eq!(
            extracted.mb_release_group_id,
            original_meta.mb_release_group_id
        );
        assert_eq!(extracted.encoder, original_meta.encoder);

        // ReplayGain values (with tolerance for formatting)
        let orig_track_gain = original_meta.replaygain_track_gain.unwrap();
        let ext_track_gain = extracted.replaygain_track_gain.unwrap();
        assert!((orig_track_gain - ext_track_gain).abs() < 0.01);

        let orig_track_peak = original_meta.replaygain_track_peak.unwrap();
        let ext_track_peak = extracted.replaygain_track_peak.unwrap();
        assert!((orig_track_peak - ext_track_peak).abs() < 0.0001);
    }

    #[test]
    fn test_parse_replaygain_value() {
        assert_eq!(FlacHandler::parse_replaygain_value("-6.50 dB"), Some(-6.50));
        assert_eq!(FlacHandler::parse_replaygain_value("-6.50dB"), Some(-6.50));
        assert_eq!(FlacHandler::parse_replaygain_value("-6.50"), Some(-6.50));
        assert_eq!(FlacHandler::parse_replaygain_value("  3.2 dB  "), Some(3.2));
        assert_eq!(FlacHandler::parse_replaygain_value("invalid"), None);
    }

    #[test]
    fn test_parse_block_header() {
        // Not last, type 0, size 34
        let header = [0x00, 0x00, 0x00, 0x22];
        let (is_last, block_type, size) = FlacHandler::parse_block_header(&header).unwrap();
        assert!(!is_last);
        assert_eq!(block_type, 0);
        assert_eq!(size, 34);

        // Last, type 4, size 256
        let header = [0x84, 0x00, 0x01, 0x00];
        let (is_last, block_type, size) = FlacHandler::parse_block_header(&header).unwrap();
        assert!(is_last);
        assert_eq!(block_type, 4);
        assert_eq!(size, 256);
    }

    #[test]
    fn test_write_block_header() {
        let header = FlacHandler::write_block_header(false, 0, 34);
        assert_eq!(header, [0x00, 0x00, 0x00, 0x22]);

        let header = FlacHandler::write_block_header(true, 4, 256);
        assert_eq!(header, [0x84, 0x00, 0x01, 0x00]);
    }

    #[test]
    fn test_empty_metadata_produces_minimal_vorbis() {
        let handler = FlacHandler::new();
        let meta = AudioMeta::default();

        let original_data = make_minimal_flac_header();
        let layout = handler
            .analyze(&original_data, original_data.len() as u64)
            .unwrap();

        let result = handler.synthesize(&meta, &layout);
        assert!(result.is_ok());

        let bytes = result.unwrap();
        // Should have: fLaC (4) + STREAMINFO (38) + VORBIS_COMMENT (header + minimal data)
        assert!(bytes.len() >= 42 + 4 + 8); // At least vendor string overhead
    }

    #[test]
    fn test_round_trip_synthesize_analyze() {
        let handler = FlacHandler::new();
        let meta = make_test_meta();

        // Create initial layout
        let original_data = make_minimal_flac_header();
        let layout = handler
            .analyze(&original_data, original_data.len() as u64)
            .unwrap();

        // Synthesize new header
        let synthesized = handler.synthesize(&meta, &layout).unwrap();

        // Analyze synthesized header
        let new_layout = handler
            .analyze(&synthesized, synthesized.len() as u64)
            .unwrap();

        // STREAMINFO should be preserved
        assert_eq!(new_layout.format_data, layout.format_data);
        assert_eq!(new_layout.format, AudioFormat::Flac);
    }
}
