use image::ImageFormat;
use std::io::Cursor;
use symphonia::core::meta::Visual;
use tracing::debug;

#[derive(Debug, Clone)]
pub struct Artwork {
    pub art_type: ArtType,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtType {
    Front,
    Back,
    Other,
}

#[derive(Debug, Clone, Copy)]
pub enum ArtSize {
    Thumbnail,
    Medium,
    Full,
}

impl ArtSize {
    pub fn max_dimension(&self) -> Option<u32> {
        match self {
            ArtSize::Thumbnail => Some(150),
            ArtSize::Medium => Some(300),
            ArtSize::Full => None,
        }
    }
}

pub struct ArtworkExtractor;

impl ArtworkExtractor {
    pub fn extract_from_visual(visual: &Visual) -> Option<Artwork> {
        let data = visual.data.to_vec();

        let img = image::load_from_memory(&data).ok()?;

        let art_type = match visual.usage {
            Some(symphonia::core::meta::StandardVisualKey::FrontCover) => ArtType::Front,
            Some(symphonia::core::meta::StandardVisualKey::BackCover) => ArtType::Back,
            _ => ArtType::Other,
        };

        let mime_type = if visual.media_type.is_empty() {
            "image/jpeg".to_string()
        } else {
            visual.media_type.clone()
        };

        Some(Artwork {
            art_type,
            mime_type,
            width: img.width(),
            height: img.height(),
            data,
        })
    }

    pub fn resize(artwork: &Artwork, size: ArtSize) -> Option<Artwork> {
        let max_dim = size.max_dimension()?;

        if artwork.width <= max_dim && artwork.height <= max_dim {
            return Some(artwork.clone());
        }

        let img = image::load_from_memory(&artwork.data).ok()?;
        let resized = img.thumbnail(max_dim, max_dim);

        let mut output = Vec::new();
        let mut cursor = Cursor::new(&mut output);
        resized.write_to(&mut cursor, ImageFormat::Jpeg).ok()?;

        debug!(
            "Resized artwork from {}x{} to {}x{}",
            artwork.width,
            artwork.height,
            resized.width(),
            resized.height()
        );

        Some(Artwork {
            art_type: artwork.art_type,
            mime_type: "image/jpeg".to_string(),
            width: resized.width(),
            height: resized.height(),
            data: output,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_art_size_dimensions() {
        assert_eq!(ArtSize::Thumbnail.max_dimension(), Some(150));
        assert_eq!(ArtSize::Medium.max_dimension(), Some(300));
        assert_eq!(ArtSize::Full.max_dimension(), None);
    }

    #[test]
    fn test_art_type_equality() {
        assert_eq!(ArtType::Front, ArtType::Front);
        assert_ne!(ArtType::Front, ArtType::Back);
    }
}
