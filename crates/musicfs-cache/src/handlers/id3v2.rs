use crate::{FormatError, FormatHandler, FormatLayout};
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::AudioFile;
use lofty::id3::v2::{CommentFrame, Frame, FrameId, Id3v2Tag, TextInformationFrame, UnsynchronizedTextFrame};
use lofty::mpeg::MpegFile;
use lofty::tag::{Accessor, TagExt};
use lofty::TextEncoding;
use musicfs_core::{AudioFormat, AudioMeta};
use std::borrow::Cow;
use std::io::Cursor;

const ID3V2_HEADER_SIZE: usize = 10;
const ID3V1_TAG_SIZE: usize = 128;

pub struct Id3v2Handler;

impl Id3v2Handler {
    pub fn new() -> Self {
        Self
    }

    fn parse_id3v2_header(data: &[u8]) -> Option<usize> {
        if data.len() < ID3V2_HEADER_SIZE {
            return None;
        }

        if &data[0..3] != b"ID3" {
            return None;
        }

        let size = syncsafe_decode(&data[6..10]);
        Some(ID3V2_HEADER_SIZE + size)
    }

    fn has_id3v1_tag(data: &[u8], file_size: u64) -> bool {
        if file_size < ID3V1_TAG_SIZE as u64 {
            return false;
        }

        let tag_start = (file_size as usize).saturating_sub(ID3V1_TAG_SIZE);
        if tag_start >= data.len() {
            return false;
        }

        &data[tag_start..tag_start + 3] == b"TAG"
    }

    fn set_text_frame(tag: &mut Id3v2Tag, frame_id: &'static str, value: &str) {
        let id = FrameId::Valid(Cow::Borrowed(frame_id));
        let frame = Frame::Text(TextInformationFrame::new(
            id,
            TextEncoding::UTF8,
            value.to_string(),
        ));
        tag.insert(frame);
    }

    fn set_track_disc_frame(tag: &mut Id3v2Tag, frame_id: &'static str, num: u32, total: Option<u32>) {
        let value = match total {
            Some(t) => format!("{}/{}", num, t),
            None => num.to_string(),
        };
        Self::set_text_frame(tag, frame_id, &value);
    }

    fn set_comment_frame(tag: &mut Id3v2Tag, value: &str) {
        let frame = Frame::Comment(CommentFrame::new(
            TextEncoding::UTF8,
            *b"eng",
            String::new(),
            value.to_string(),
        ));
        tag.insert(frame);
    }

    fn set_lyrics_frame(tag: &mut Id3v2Tag, value: &str) {
        let frame = Frame::UnsynchronizedText(UnsynchronizedTextFrame::new(
            TextEncoding::UTF8,
            *b"eng",
            String::new(),
            value.to_string(),
        ));
        tag.insert(frame);
    }

    fn build_tag_from_meta(metadata: &AudioMeta) -> Id3v2Tag {
        let mut tag = Id3v2Tag::new();

        if let Some(ref title) = metadata.title {
            tag.set_title(title.clone());
        }
        if let Some(ref artist) = metadata.artist {
            tag.set_artist(artist.clone());
        }
        if let Some(ref album) = metadata.album {
            tag.set_album(album.clone());
        }
        if let Some(ref album_artist) = metadata.album_artist {
            Self::set_text_frame(&mut tag, "TPE2", album_artist);
        }
        if let Some(year) = metadata.year {
            Self::set_text_frame(&mut tag, "TDRC", &year.to_string());
        }
        if let Some(ref genre) = metadata.genre {
            tag.set_genre(genre.clone());
        }

        if let Some(track) = metadata.track {
            Self::set_track_disc_frame(&mut tag, "TRCK", track, metadata.track_total);
        }
        if let Some(disc) = metadata.disc {
            Self::set_track_disc_frame(&mut tag, "TPOS", disc, metadata.disc_total);
        }

        if let Some(ref date) = metadata.date {
            Self::set_text_frame(&mut tag, "TDRC", date);
        }
        if let Some(ref composer) = metadata.composer {
            Self::set_text_frame(&mut tag, "TCOM", composer);
        }
        if let Some(ref comment) = metadata.comment {
            Self::set_comment_frame(&mut tag, comment);
        }
        if let Some(ref lyrics) = metadata.lyrics {
            Self::set_lyrics_frame(&mut tag, lyrics);
        }
        if let Some(ref copyright) = metadata.copyright {
            Self::set_text_frame(&mut tag, "TCOP", copyright);
        }
        if let Some(compilation) = metadata.compilation {
            Self::set_text_frame(&mut tag, "TCMP", if compilation { "1" } else { "0" });
        }

        if let Some(ref title_sort) = metadata.title_sort {
            Self::set_text_frame(&mut tag, "TSOT", title_sort);
        }
        if let Some(ref artist_sort) = metadata.artist_sort {
            Self::set_text_frame(&mut tag, "TSOP", artist_sort);
        }
        if let Some(ref album_sort) = metadata.album_sort {
            Self::set_text_frame(&mut tag, "TSOA", album_sort);
        }
        if let Some(ref album_artist_sort) = metadata.album_artist_sort {
            Self::set_text_frame(&mut tag, "TSO2", album_artist_sort);
        }

        if let Some(ref mb_recording_id) = metadata.mb_recording_id {
            tag.insert_user_text("MusicBrainz Recording Id".to_string(), mb_recording_id.clone());
        }
        if let Some(ref mb_album_id) = metadata.mb_album_id {
            tag.insert_user_text("MusicBrainz Album Id".to_string(), mb_album_id.clone());
        }
        if let Some(ref mb_artist_id) = metadata.mb_artist_id {
            tag.insert_user_text("MusicBrainz Artist Id".to_string(), mb_artist_id.clone());
        }
        if let Some(ref mb_album_artist_id) = metadata.mb_album_artist_id {
            tag.insert_user_text(
                "MusicBrainz Album Artist Id".to_string(),
                mb_album_artist_id.clone(),
            );
        }
        if let Some(ref mb_release_group_id) = metadata.mb_release_group_id {
            tag.insert_user_text(
                "MusicBrainz Release Group Id".to_string(),
                mb_release_group_id.clone(),
            );
        }

        if let Some(gain) = metadata.replaygain_track_gain {
            tag.insert_user_text(
                "REPLAYGAIN_TRACK_GAIN".to_string(),
                format!("{:.2} dB", gain),
            );
        }
        if let Some(peak) = metadata.replaygain_track_peak {
            tag.insert_user_text("REPLAYGAIN_TRACK_PEAK".to_string(), format!("{:.6}", peak));
        }
        if let Some(gain) = metadata.replaygain_album_gain {
            tag.insert_user_text(
                "REPLAYGAIN_ALBUM_GAIN".to_string(),
                format!("{:.2} dB", gain),
            );
        }
        if let Some(peak) = metadata.replaygain_album_peak {
            tag.insert_user_text("REPLAYGAIN_ALBUM_PEAK".to_string(), format!("{:.6}", peak));
        }

        if let Some(ref encoder) = metadata.encoder {
            Self::set_text_frame(&mut tag, "TSSE", encoder);
        }

        tag
    }

    fn extract_text_frame(tag: &Id3v2Tag, frame_id: &str) -> Option<String> {
        let id = FrameId::new(frame_id).ok()?;
        tag.get_text(&id).map(|s| s.to_string())
    }

    fn parse_track_disc(value: &str) -> (Option<u32>, Option<u32>) {
        let parts: Vec<&str> = value.split('/').collect();
        let num = parts.first().and_then(|s| s.parse().ok());
        let total = parts.get(1).and_then(|s| s.parse().ok());
        (num, total)
    }

    fn parse_replaygain_value(value: &str) -> Option<f32> {
        value
            .trim()
            .trim_end_matches(" dB")
            .trim_end_matches("dB")
            .parse()
            .ok()
    }

    fn extract_from_tag(tag: &Id3v2Tag) -> AudioMeta {
        let mut meta = AudioMeta::default();
        meta.format = AudioFormat::Mp3;

        meta.title = tag.title().map(|c: Cow<'_, str>| c.into_owned());
        meta.artist = tag.artist().map(|c: Cow<'_, str>| c.into_owned());
        meta.album = tag.album().map(|c: Cow<'_, str>| c.into_owned());
        meta.album_artist = Self::extract_text_frame(tag, "TPE2");
        meta.genre = tag.genre().map(|c: Cow<'_, str>| c.into_owned());

        if let Some(track_str) = Self::extract_text_frame(tag, "TRCK") {
            let (track, track_total) = Self::parse_track_disc(&track_str);
            meta.track = track;
            meta.track_total = track_total;
        } else {
            meta.track = tag.track();
            meta.track_total = tag.track_total();
        }

        if let Some(disc_str) = Self::extract_text_frame(tag, "TPOS") {
            let (disc, disc_total) = Self::parse_track_disc(&disc_str);
            meta.disc = disc;
            meta.disc_total = disc_total;
        } else {
            meta.disc = tag.disk();
            meta.disc_total = tag.disk_total();
        }

        meta.date = Self::extract_text_frame(tag, "TDRC");
        if let Some(ref date) = meta.date {
            if let Some(year_str) = date.split('-').next() {
                meta.year = year_str.parse().ok();
            }
        }

        meta.composer = Self::extract_text_frame(tag, "TCOM");
        meta.comment = tag.comment().map(|c: Cow<'_, str>| c.into_owned());

        if let Some(uslt) = tag.unsync_text().next() {
            meta.lyrics = Some(uslt.content.to_string());
        }

        meta.copyright = Self::extract_text_frame(tag, "TCOP");

        if let Some(tcmp) = Self::extract_text_frame(tag, "TCMP") {
            meta.compilation = Some(tcmp == "1");
        }

        meta.title_sort = Self::extract_text_frame(tag, "TSOT");
        meta.artist_sort = Self::extract_text_frame(tag, "TSOP");
        meta.album_sort = Self::extract_text_frame(tag, "TSOA");
        meta.album_artist_sort = Self::extract_text_frame(tag, "TSO2");

        meta.mb_recording_id = tag.get_user_text("MusicBrainz Recording Id").map(String::from);
        meta.mb_album_id = tag.get_user_text("MusicBrainz Album Id").map(String::from);
        meta.mb_artist_id = tag.get_user_text("MusicBrainz Artist Id").map(String::from);
        meta.mb_album_artist_id = tag
            .get_user_text("MusicBrainz Album Artist Id")
            .map(String::from);
        meta.mb_release_group_id = tag
            .get_user_text("MusicBrainz Release Group Id")
            .map(String::from);

        if let Some(gain_str) = tag.get_user_text("REPLAYGAIN_TRACK_GAIN") {
            meta.replaygain_track_gain = Self::parse_replaygain_value(gain_str);
        }
        if let Some(peak_str) = tag.get_user_text("REPLAYGAIN_TRACK_PEAK") {
            meta.replaygain_track_peak = peak_str.parse::<f32>().ok();
        }
        if let Some(gain_str) = tag.get_user_text("REPLAYGAIN_ALBUM_GAIN") {
            meta.replaygain_album_gain = Self::parse_replaygain_value(gain_str);
        }
        if let Some(peak_str) = tag.get_user_text("REPLAYGAIN_ALBUM_PEAK") {
            meta.replaygain_album_peak = peak_str.parse::<f32>().ok();
        }

        meta.encoder = Self::extract_text_frame(tag, "TSSE");

        meta
    }
}

impl Default for Id3v2Handler {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatHandler for Id3v2Handler {
    fn id(&self) -> &'static str {
        "id3v2"
    }

    fn name(&self) -> &'static str {
        "ID3v2 (MP3)"
    }

    fn extensions(&self) -> &[&'static str] {
        &["mp3"]
    }

    fn mime_types(&self) -> &[&'static str] {
        &["audio/mpeg"]
    }

    fn analyze(&self, data: &[u8], file_size: u64) -> Result<FormatLayout, FormatError> {
        let audio_start = Self::parse_id3v2_header(data).unwrap_or(0) as u64;

        let audio_end = if Self::has_id3v1_tag(data, file_size) {
            file_size - ID3V1_TAG_SIZE as u64
        } else {
            file_size
        };

        Ok(FormatLayout {
            audio_start,
            audio_end,
            format: AudioFormat::Mp3,
            format_data: None,
        })
    }

    fn synthesize(
        &self,
        metadata: &AudioMeta,
        _layout: &FormatLayout,
    ) -> Result<Vec<u8>, FormatError> {
        let tag = Self::build_tag_from_meta(metadata);

        let mut buffer = Cursor::new(Vec::new());
        let write_options = WriteOptions::new().preferred_padding(1024);

        tag.dump_to(&mut buffer, write_options)
            .map_err(|e| FormatError::SynthesisFailed(e.to_string()))?;

        Ok(buffer.into_inner())
    }

    fn extract(&self, data: &[u8]) -> Result<AudioMeta, FormatError> {
        let mut cursor = Cursor::new(data);

        let mpeg_file = MpegFile::read_from(&mut cursor, ParseOptions::new())
            .map_err(|e| FormatError::InvalidData(e.to_string()))?;

        let tag = mpeg_file
            .id3v2()
            .ok_or_else(|| FormatError::InvalidData("No ID3v2 tag found".to_string()))?;

        Ok(Self::extract_from_tag(tag))
    }

    fn estimate_header_size(&self, _metadata: &AudioMeta) -> usize {
        4096 + 1024
    }
}

fn syncsafe_decode(bytes: &[u8]) -> usize {
    ((bytes[0] as usize) << 21)
        | ((bytes[1] as usize) << 14)
        | ((bytes[2] as usize) << 7)
        | (bytes[3] as usize)
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
            format: AudioFormat::Mp3,
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
            encoder: Some("LAME 3.100".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn test_id_and_name() {
        let handler = Id3v2Handler::new();
        assert_eq!(handler.id(), "id3v2");
        assert_eq!(handler.name(), "ID3v2 (MP3)");
    }

    #[test]
    fn test_extensions_and_mime_types() {
        let handler = Id3v2Handler::new();
        assert_eq!(handler.extensions(), &["mp3"]);
        assert_eq!(handler.mime_types(), &["audio/mpeg"]);
    }

    #[test]
    fn test_estimate_header_size() {
        let handler = Id3v2Handler::new();
        let meta = AudioMeta::default();
        assert_eq!(handler.estimate_header_size(&meta), 5120);
    }

    #[test]
    fn test_synthesize_creates_valid_id3v2() {
        let handler = Id3v2Handler::new();
        let meta = make_test_meta();
        let layout = FormatLayout {
            audio_start: 0,
            audio_end: 1000,
            format: AudioFormat::Mp3,
            format_data: None,
        };

        let result = handler.synthesize(&meta, &layout);
        assert!(result.is_ok());

        let bytes = result.unwrap();
        assert!(bytes.len() >= 10);
        assert_eq!(&bytes[0..3], b"ID3");
        assert_eq!(bytes[3], 0x04);
    }

    #[test]
    fn test_analyze_no_id3v2() {
        let handler = Id3v2Handler::new();
        let data = vec![0xFF, 0xFB, 0x90, 0x00];
        let file_size = 1000;

        let result = handler.analyze(&data, file_size);
        assert!(result.is_ok());

        let layout = result.unwrap();
        assert_eq!(layout.audio_start, 0);
        assert_eq!(layout.audio_end, 1000);
        assert_eq!(layout.format, AudioFormat::Mp3);
    }

    #[test]
    fn test_analyze_with_id3v2() {
        let handler = Id3v2Handler::new();

        let mut data = vec![
            b'I', b'D', b'3', 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64,
        ];
        data.extend(vec![0u8; 100]);
        let file_size = data.len() as u64;

        let result = handler.analyze(&data, file_size);
        assert!(result.is_ok());

        let layout = result.unwrap();
        assert_eq!(layout.audio_start, 110);
        assert_eq!(layout.audio_end, file_size);
    }

    #[test]
    fn test_analyze_with_id3v1() {
        let handler = Id3v2Handler::new();

        let mut data = vec![0xFF, 0xFB, 0x90, 0x00];
        data.extend(vec![0u8; 100]);
        data.extend(b"TAG");
        data.extend(vec![0u8; 125]);
        let file_size = data.len() as u64;

        let result = handler.analyze(&data, file_size);
        assert!(result.is_ok());

        let layout = result.unwrap();
        assert_eq!(layout.audio_start, 0);
        assert_eq!(layout.audio_end, file_size - 128);
    }

    #[test]
    fn test_syncsafe_decode() {
        assert_eq!(syncsafe_decode(&[0x00, 0x00, 0x00, 0x7F]), 127);
        assert_eq!(syncsafe_decode(&[0x00, 0x00, 0x01, 0x00]), 128);
        assert_eq!(syncsafe_decode(&[0x00, 0x00, 0x00, 0x64]), 100);
    }

    #[test]
    fn test_parse_track_disc() {
        assert_eq!(Id3v2Handler::parse_track_disc("5/12"), (Some(5), Some(12)));
        assert_eq!(Id3v2Handler::parse_track_disc("5"), (Some(5), None));
        assert_eq!(Id3v2Handler::parse_track_disc(""), (None, None));
    }

    #[test]
    fn test_parse_replaygain_value() {
        assert_eq!(
            Id3v2Handler::parse_replaygain_value("-6.50 dB"),
            Some(-6.50)
        );
        assert_eq!(Id3v2Handler::parse_replaygain_value("-6.50dB"), Some(-6.50));
        assert_eq!(Id3v2Handler::parse_replaygain_value("-6.50"), Some(-6.50));
        assert_eq!(Id3v2Handler::parse_replaygain_value("invalid"), None);
    }

    #[test]
    fn test_empty_metadata_produces_empty_tag() {
        let handler = Id3v2Handler::new();
        let meta = AudioMeta::default();
        let layout = FormatLayout {
            audio_start: 0,
            audio_end: 1000,
            format: AudioFormat::Mp3,
            format_data: None,
        };

        let result = handler.synthesize(&meta, &layout);
        assert!(result.is_ok());

        let bytes = result.unwrap();
        assert!(bytes.is_empty());
    }

    #[test]
    fn test_minimal_metadata_produces_valid_tag() {
        let handler = Id3v2Handler::new();
        let mut meta = AudioMeta::default();
        meta.title = Some("Test".to_string());
        let layout = FormatLayout {
            audio_start: 0,
            audio_end: 1000,
            format: AudioFormat::Mp3,
            format_data: None,
        };

        let result = handler.synthesize(&meta, &layout);
        assert!(result.is_ok());

        let bytes = result.unwrap();
        assert!(bytes.len() >= 10);
        assert_eq!(&bytes[0..3], b"ID3");
        assert_eq!(bytes[3], 0x04);
    }

    #[test]
    fn test_build_and_extract_tag() {
        let original_meta = make_test_meta();
        let tag = Id3v2Handler::build_tag_from_meta(&original_meta);
        let extracted = Id3v2Handler::extract_from_tag(&tag);

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
        assert_eq!(extracted.mb_album_artist_id, original_meta.mb_album_artist_id);
        assert_eq!(
            extracted.mb_release_group_id,
            original_meta.mb_release_group_id
        );
        assert_eq!(extracted.encoder, original_meta.encoder);

        let orig_track_gain = original_meta.replaygain_track_gain.unwrap();
        let ext_track_gain = extracted.replaygain_track_gain.unwrap();
        assert!((orig_track_gain - ext_track_gain).abs() < 0.01);

        let orig_track_peak = original_meta.replaygain_track_peak.unwrap();
        let ext_track_peak = extracted.replaygain_track_peak.unwrap();
        assert!((orig_track_peak - ext_track_peak).abs() < 0.0001);
    }
}
