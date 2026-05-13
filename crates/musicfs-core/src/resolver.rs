use crate::{AudioMeta, VirtualPath};

#[derive(Debug, Clone)]
pub struct PathTemplate {
    pub pattern: String,
    pub fallback_artist: String,
    pub fallback_album: String,
    pub fallback_title: String,
    pub fallback_year: String,
}

impl Default for PathTemplate {
    fn default() -> Self {
        Self {
            pattern: "$artist/$album ($year) [$format_upper]/$track - $title.$format".to_string(),
            fallback_artist: "Unknown Artist".to_string(),
            fallback_album: "Unknown Album".to_string(),
            fallback_title: "Unknown Track".to_string(),
            fallback_year: "Unknown".to_string(),
        }
    }
}

pub struct PathResolver {
    template: PathTemplate,
}

impl PathResolver {
    pub fn new(template: PathTemplate) -> Self {
        Self { template }
    }

    pub fn resolve(&self, meta: &AudioMeta, extension: &str) -> VirtualPath {
        let artist = meta
            .artist
            .as_deref()
            .unwrap_or(&self.template.fallback_artist);
        let album = meta
            .album
            .as_deref()
            .unwrap_or(&self.template.fallback_album);
        let title = meta
            .title
            .as_deref()
            .unwrap_or(&self.template.fallback_title);
        let year = meta
            .year
            .map(|y| y.to_string())
            .unwrap_or_else(|| self.template.fallback_year.clone());
        let track = meta.track.unwrap_or(0);
        let disc = meta.disc.unwrap_or(1);
        let genre = meta.genre.as_deref().unwrap_or("Unknown");
        let format = extension.to_lowercase();
        let format_upper = extension.to_uppercase();

        let artist = sanitize_path_component(artist);
        let album = sanitize_path_component(album);
        let title = sanitize_path_component(title);
        let genre = sanitize_path_component(genre);

        let path = self
            .template
            .pattern
            .replace("$artist", &artist)
            .replace("$album", &album)
            .replace("$title", &title)
            .replace("$track", &format!("{:02}", track))
            .replace("$disc", &disc.to_string())
            .replace("$year", &year)
            .replace("$genre", &genre)
            .replace("$format_upper", &format_upper)
            .replace("$format", &format);

        VirtualPath::new(path)
    }
}

impl Default for PathResolver {
    fn default() -> Self {
        Self::new(PathTemplate::default())
    }
}

fn sanitize_path_component(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AudioFormat;

    #[test]
    fn test_resolve_complete_metadata() {
        let resolver = PathResolver::default();
        let meta = AudioMeta {
            artist: Some("Metallica".to_string()),
            album: Some("Master of Puppets".to_string()),
            title: Some("Battery".to_string()),
            track: Some(1),
            year: Some(1986),
            format: AudioFormat::Flac,
            ..Default::default()
        };

        let path = resolver.resolve(&meta, "flac");
        assert_eq!(
            path.as_str(),
            "Metallica/Master of Puppets (1986) [FLAC]/01 - Battery.flac"
        );
    }

    #[test]
    fn test_resolve_missing_album() {
        let resolver = PathResolver::default();
        let meta = AudioMeta {
            artist: Some("Artist".to_string()),
            title: Some("Track".to_string()),
            track: Some(5),
            ..Default::default()
        };

        let path = resolver.resolve(&meta, "mp3");
        assert_eq!(
            path.as_str(),
            "Artist/Unknown Album (Unknown) [MP3]/05 - Track.mp3"
        );
    }

    #[test]
    fn test_sanitize_special_chars() {
        let resolver = PathResolver::default();
        let meta = AudioMeta {
            artist: Some("AC/DC".to_string()),
            album: Some("Who Made Who?".to_string()),
            title: Some("Test:Track".to_string()),
            track: Some(1),
            year: Some(1986),
            ..Default::default()
        };

        let path = resolver.resolve(&meta, "flac");
        assert!(!path.as_str().contains(':'));
        assert!(!path.as_str().contains('?'));
        assert!(path.as_str().contains("AC_DC"));
    }

    #[test]
    fn test_custom_template() {
        let template = PathTemplate {
            pattern: "$genre/$artist - $album/$track $title.$format".to_string(),
            ..Default::default()
        };
        let resolver = PathResolver::new(template);
        let meta = AudioMeta {
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            title: Some("Song".to_string()),
            genre: Some("Rock".to_string()),
            track: Some(3),
            ..Default::default()
        };

        let path = resolver.resolve(&meta, "flac");
        assert_eq!(path.as_str(), "Rock/Artist - Album/03 Song.flac");
    }
}
