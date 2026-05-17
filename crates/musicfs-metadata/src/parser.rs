use musicfs_core::{AudioFormat, AudioMeta, Error, Result};
use std::fs::File;
use std::path::Path;
use symphonia::core::codecs::CODEC_TYPE_NULL;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tracing::debug;

pub struct MetadataParser;

impl MetadataParser {
    pub fn new() -> Self {
        Self
    }

    pub fn parse_file(&self, path: &Path) -> Result<AudioMeta> {
        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        let mut hint = Hint::new();
        if !ext.is_empty() {
            hint.with_extension(ext);
        }

        let fmt_opts = FormatOptions::default();
        let meta_opts = MetadataOptions::default();

        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &fmt_opts, &meta_opts)
            .map_err(|e| Error::Metadata(format!("Failed to probe format: {}", e)))?;
        let mut format = probed.format;

        let mut audio_meta = AudioMeta {
            format: AudioFormat::from_extension(ext),
            ..Default::default()
        };

        if let Some(metadata) = format.metadata().current() {
            self.extract_tags(&mut audio_meta, metadata);
        }

        if let Some(track) = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        {
            let params = &track.codec_params;

            if let Some(n_frames) = params.n_frames {
                if let Some(sample_rate) = params.sample_rate {
                    audio_meta.duration_ms = Some((n_frames as u64 * 1000) / sample_rate as u64);
                    audio_meta.sample_rate = Some(sample_rate);
                }
            }

            if let Some(channels) = params.channels {
                audio_meta.channels = Some(channels.count() as u32);
            }

            if let Some(bits_per_sample) = params.bits_per_sample {
                audio_meta.bits_per_sample = Some(bits_per_sample);
                if let Some(sample_rate) = params.sample_rate {
                    if let Some(channels) = params.channels {
                        audio_meta.bitrate =
                            Some(bits_per_sample * sample_rate * channels.count() as u32 / 1000);
                    }
                }
            }
        }

        debug!(?audio_meta, "Parsed metadata");
        Ok(audio_meta)
    }

    fn extract_tags(
        &self,
        meta: &mut AudioMeta,
        metadata: &symphonia::core::meta::MetadataRevision,
    ) {
        use symphonia::core::meta::StandardTagKey;

        for tag in metadata.tags() {
            if let Some(std_key) = tag.std_key {
                let value = tag.value.to_string();
                match std_key {
                    // Basic metadata
                    StandardTagKey::TrackTitle => meta.title = Some(value),
                    StandardTagKey::Artist => meta.artist = Some(value),
                    StandardTagKey::Album => meta.album = Some(value),
                    StandardTagKey::AlbumArtist => meta.album_artist = Some(value),
                    StandardTagKey::Genre => meta.genre = Some(value),

                    // Track/disc with totals (parse "X/Y" format)
                    StandardTagKey::TrackNumber => {
                        let parts: Vec<&str> = value.split('/').collect();
                        meta.track = parts.first().and_then(|s| s.trim().parse().ok());
                        if parts.len() > 1 {
                            meta.track_total = parts.get(1).and_then(|s| s.trim().parse().ok());
                        }
                    }
                    StandardTagKey::DiscNumber => {
                        let parts: Vec<&str> = value.split('/').collect();
                        meta.disc = parts.first().and_then(|s| s.trim().parse().ok());
                        if parts.len() > 1 {
                            meta.disc_total = parts.get(1).and_then(|s| s.trim().parse().ok());
                        }
                    }
                    StandardTagKey::TrackTotal => {
                        meta.track_total = value.trim().parse().ok();
                    }
                    StandardTagKey::DiscTotal => {
                        meta.disc_total = value.trim().parse().ok();
                    }

                    // Date handling: store full date string, extract year
                    StandardTagKey::Date | StandardTagKey::ReleaseDate => {
                        meta.date = Some(value.clone());
                        meta.year = value.chars().take(4).collect::<String>().parse().ok();
                    }

                    // Additional metadata
                    StandardTagKey::Composer => meta.composer = Some(value),
                    StandardTagKey::Comment => meta.comment = Some(value),
                    StandardTagKey::Lyrics => meta.lyrics = Some(value),
                    StandardTagKey::Copyright => meta.copyright = Some(value),
                    StandardTagKey::Compilation => {
                        meta.compilation = Some(value == "1" || value.eq_ignore_ascii_case("true"));
                    }
                    StandardTagKey::Encoder => meta.encoder = Some(value),

                    // Sort keys
                    StandardTagKey::SortTrackTitle => meta.title_sort = Some(value),
                    StandardTagKey::SortArtist => meta.artist_sort = Some(value),
                    StandardTagKey::SortAlbum => meta.album_sort = Some(value),
                    StandardTagKey::SortAlbumArtist => meta.album_artist_sort = Some(value),

                    // MusicBrainz IDs
                    StandardTagKey::MusicBrainzRecordingId => meta.mb_recording_id = Some(value),
                    StandardTagKey::MusicBrainzAlbumId => meta.mb_album_id = Some(value),
                    StandardTagKey::MusicBrainzArtistId => meta.mb_artist_id = Some(value),
                    StandardTagKey::MusicBrainzAlbumArtistId => {
                        meta.mb_album_artist_id = Some(value)
                    }
                    StandardTagKey::MusicBrainzReleaseGroupId => {
                        meta.mb_release_group_id = Some(value)
                    }

                    // ReplayGain (parse as f32, values may have "dB" suffix)
                    StandardTagKey::ReplayGainTrackGain => {
                        meta.replaygain_track_gain = parse_replaygain(&value);
                    }
                    StandardTagKey::ReplayGainTrackPeak => {
                        meta.replaygain_track_peak = value.trim().parse().ok();
                    }
                    StandardTagKey::ReplayGainAlbumGain => {
                        meta.replaygain_album_gain = parse_replaygain(&value);
                    }
                    StandardTagKey::ReplayGainAlbumPeak => {
                        meta.replaygain_album_peak = value.trim().parse().ok();
                    }

                    _ => {}
                }
            }
        }
    }
}

/// Parse ReplayGain value, stripping optional "dB" suffix
fn parse_replaygain(value: &str) -> Option<f32> {
    let trimmed = value.trim();
    let without_db = trimmed
        .strip_suffix("dB")
        .or_else(|| trimmed.strip_suffix(" dB"))
        .unwrap_or(trimmed);
    without_db.trim().parse().ok()
}

impl Default for MetadataParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_format_detection() {
        assert_eq!(AudioFormat::from_extension("flac"), AudioFormat::Flac);
        assert_eq!(AudioFormat::from_extension("mp3"), AudioFormat::Mp3);
        assert_eq!(AudioFormat::from_extension("opus"), AudioFormat::Opus);
        assert_eq!(AudioFormat::from_extension("ogg"), AudioFormat::Vorbis);
        assert_eq!(AudioFormat::from_extension("m4a"), AudioFormat::Aac);
        assert_eq!(AudioFormat::from_extension("wav"), AudioFormat::Wav);
    }

    #[test]
    fn test_parser_creation() {
        let parser = MetadataParser::new();
        let default_parser = MetadataParser::default();
        assert!(std::mem::size_of_val(&parser) == std::mem::size_of_val(&default_parser));
    }
}
