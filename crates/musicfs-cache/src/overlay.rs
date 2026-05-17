//! OverlayReader: On-the-fly metadata overlay with header/audio splice logic.
//!
//! This module provides the core read path for metadata overlay. It synthesizes
//! headers on-the-fly from database metadata and splices them with original audio
//! data from the CAS.

use crate::{Database, FormatError, FormatHandlerRegistry};
use bytes::{Bytes, BytesMut};
use musicfs_cas::{FileReader, ReaderError};
use musicfs_core::{AudioFormat, FileId};
use std::sync::Arc;
use tracing::{debug, trace};

/// Error types for overlay operations
#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("Database error: {0}")]
    Database(#[from] musicfs_core::Error),

    #[error("Format handler error: {0}")]
    Handler(#[from] FormatError),

    #[error("CAS error: {0}")]
    Cas(#[from] ReaderError),

    #[error("File not found: {0:?}")]
    NotFound(FileId),

    #[error("No handler for format: {0:?}")]
    NoHandler(AudioFormat),
}

/// OverlayReader provides on-the-fly metadata overlay for audio files.
///
/// It synthesizes headers from database metadata and splices them with
/// original audio data from the CAS, presenting a virtual file that
/// reflects the current metadata state.
pub struct OverlayReader {
    db: Arc<Database>,
    registry: Arc<FormatHandlerRegistry>,
    cas_reader: Arc<FileReader>,
}

impl OverlayReader {
    /// Create a new OverlayReader with the given dependencies.
    pub fn new(
        db: Arc<Database>,
        registry: Arc<FormatHandlerRegistry>,
        cas_reader: Arc<FileReader>,
    ) -> Self {
        Self {
            db,
            registry,
            cas_reader,
        }
    }

    /// Read bytes from a virtual file with metadata overlay.
    ///
    /// This method implements the three-region splice logic:
    /// - Region 1: Synthetic header (offset < header_len)
    /// - Region 2: Audio data from CAS (offset >= header_len)
    /// - Region 3: Boundary crossing (spans header/audio)
    ///
    /// If no format_layout exists for the file, delegates directly to CAS reader.
    pub async fn read(
        &self,
        file_id: FileId,
        offset: u64,
        size: u32,
    ) -> Result<Bytes, OverlayError> {
        // Get format layout - if None, passthrough to CAS
        let layout = match self.db.get_format_layout(file_id)? {
            Some(layout) => layout,
            None => {
                trace!(file_id = ?file_id, "No format_layout, passthrough to CAS");
                return Ok(self.cas_reader.read(file_id, offset, size).await?);
            }
        };

        // Get metadata for synthesis
        let metadata = self.db.get_file_metadata_row(file_id)?;

        // Get handler for this format (handler IDs are lowercase)
        let format_id = format!("{:?}", layout.format).to_lowercase();
        let handler = self
            .registry
            .get_by_format(&format_id)
            .ok_or_else(|| OverlayError::NoHandler(layout.format))?;

        // Synthesize header on-the-fly
        let header = handler.synthesize(&metadata, &layout)?;
        let header_len = header.len() as u64;
        let audio_len = layout.audio_end - layout.audio_start;
        let virtual_size = header_len + audio_len;

        trace!(
            file_id = ?file_id,
            header_len,
            audio_len,
            virtual_size,
            offset,
            size,
            "Overlay read"
        );

        // Handle EOF
        if offset >= virtual_size {
            return Ok(Bytes::new());
        }

        let virtual_end = (offset + size as u64).min(virtual_size);
        let mut result = BytesMut::with_capacity((virtual_end - offset) as usize);

        // Region 1: Synthetic header
        if offset < header_len {
            let end = virtual_end.min(header_len);
            result.extend_from_slice(&header[offset as usize..end as usize]);
            trace!(
                file_id = ?file_id,
                start = offset,
                end,
                bytes = end - offset,
                "Read from synthetic header"
            );
        }

        // Region 2: Origin audio data (from CAS)
        if virtual_end > header_len {
            let audio_start_in_virtual = header_len.max(offset);
            let audio_offset_in_origin = layout.audio_start + (audio_start_in_virtual - header_len);
            let audio_bytes_needed = (virtual_end - audio_start_in_virtual) as u32;

            trace!(
                file_id = ?file_id,
                audio_offset_in_origin,
                audio_bytes_needed,
                "Read from CAS audio"
            );

            let audio = self
                .cas_reader
                .read(file_id, audio_offset_in_origin, audio_bytes_needed)
                .await?;
            result.extend_from_slice(&audio);
        }

        debug!(
            file_id = ?file_id,
            offset,
            size,
            returned = result.len(),
            "Overlay read complete"
        );

        Ok(result.freeze())
    }

    /// Estimate the virtual size of a file for getattr.
    ///
    /// Returns the estimated size based on format layout. If no layout exists,
    /// returns None to indicate the caller should use the original file size.
    pub fn estimate_virtual_size(&self, file_id: FileId) -> Result<Option<u64>, OverlayError> {
        // Get format layout - if None, return None to indicate passthrough
        let layout = match self.db.get_format_layout(file_id)? {
            Some(layout) => layout,
            None => return Ok(None),
        };

        // Get metadata for header size estimation
        let metadata = self.db.get_file_metadata_row(file_id)?;

        let format_id = format!("{:?}", layout.format).to_lowercase();
        let handler = self
            .registry
            .get_by_format(&format_id)
            .ok_or_else(|| OverlayError::NoHandler(layout.format))?;

        // Estimate header size
        let estimated_header = handler.estimate_header_size(&metadata) as u64;
        let audio_len = layout.audio_end - layout.audio_start;
        let virtual_size = estimated_header + audio_len;

        trace!(
            file_id = ?file_id,
            estimated_header,
            audio_len,
            virtual_size,
            "Estimated virtual size"
        );

        Ok(Some(virtual_size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::FlacHandler;
    use crate::FormatLayout;
    use musicfs_cas::{CasConfig, CasStore, ChunkManifest, ChunkRef};
    use musicfs_core::{AudioFormat, AudioMeta, OriginId, VirtualPath};
    use std::path::Path;
    use std::time::UNIX_EPOCH;
    use tempfile::TempDir;

    fn make_test_metadata() -> AudioMeta {
        AudioMeta {
            title: Some("Test Track".to_string()),
            artist: Some("Test Artist".to_string()),
            album: Some("Test Album".to_string()),
            track: Some(1),
            format: AudioFormat::Flac,
            sample_rate: Some(44100),
            bits_per_sample: Some(16),
            channels: Some(2),
            ..Default::default()
        }
    }

    fn make_test_layout() -> FormatLayout {
        // Simulate a file with minimal FLAC header, audio from 42 to 102442 (100KB audio)
        // STREAMINFO data (34 bytes) - minimal valid values for FLAC synthesis
        let streaminfo_data = vec![
            0x10, 0x00, // min_block_size = 4096
            0x10, 0x00, // max_block_size = 4096
            0x00, 0x00, 0x00, // min_frame_size = 0
            0x00, 0x00, 0x00, // max_frame_size = 0
            0x0A, 0xC4, 0x42, 0xF0, // sample_rate=44100, channels=2, bits=16
            0x00, 0x00, 0x00, 0x00, // total_samples
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // MD5 (16 bytes)
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        FormatLayout {
            audio_start: 42,            // fLaC (4) + STREAMINFO block (38)
            audio_end: 42 + 100 * 1024, // 100KB audio
            format: AudioFormat::Flac,
            format_data: Some(streaminfo_data),
        }
    }

    async fn setup_test_env() -> (
        TempDir,
        Arc<Database>,
        Arc<FormatHandlerRegistry>,
        Arc<FileReader>,
        FileId,
    ) {
        let dir = TempDir::new().unwrap();

        // Setup database
        let db = Arc::new(Database::open_memory().unwrap());

        // Setup registry with FLAC handler
        let mut registry = FormatHandlerRegistry::new();
        registry.register(Arc::new(FlacHandler::new()));
        let registry = Arc::new(registry);

        // Setup CAS store and reader
        let cas_config = CasConfig {
            chunks_dir: dir.path().join("chunks"),
            ..Default::default()
        };
        let store = Arc::new(CasStore::open(cas_config).await.unwrap());

        // Create test audio data (simulating 100KB of audio)
        let audio_data: Vec<u8> = (0..100 * 1024).map(|i| (i % 256) as u8).collect();
        let hash = store.put(&audio_data).await.unwrap();

        let reader = Arc::new(FileReader::new(store));

        // Register manifest for the test file
        // The manifest represents the ORIGINAL file in CAS, with audio starting at offset 42
        reader.register_manifest(ChunkManifest {
            file_id: FileId(1),
            total_size: 42 + 100 * 1024, // Original file size (42 byte header + 100KB audio)
            mtime: 0,
            chunks: vec![ChunkRef {
                hash,
                offset: 42, // Audio starts at offset 42 in the original file
                size: audio_data.len() as u32,
            }],
        });

        let file_id = db
            .upsert_file_with_layout(
                &OriginId::from("test"),
                Path::new("/test.flac"),
                &VirtualPath::new("/Test Artist/Test Album/01 - Test Track.flac"),
                &make_test_metadata(),
                UNIX_EPOCH,
                42 + 100 * 1024,
                Some(&make_test_layout()),
                None,
            )
            .unwrap();

        (dir, db, registry, reader, file_id)
    }

    #[tokio::test]
    async fn test_read_header_region() {
        let (_dir, db, registry, reader, file_id) = setup_test_env().await;
        let overlay = OverlayReader::new(db, registry, reader);

        // Read first 100 bytes (should be from synthetic header)
        let result = overlay.read(file_id, 0, 100).await.unwrap();

        // Should return data (synthetic header)
        assert!(!result.is_empty());
        assert!(result.len() <= 100);

        // FLAC files start with "fLaC" magic
        assert_eq!(&result[0..4], b"fLaC");
    }

    #[tokio::test]
    async fn test_read_audio_region() {
        let (_dir, db, registry, reader, file_id) = setup_test_env().await;
        let overlay = OverlayReader::new(db.clone(), registry.clone(), reader.clone());

        // First, get the actual header size by reading it
        let _header_result = overlay.read(file_id, 0, 64 * 1024).await.unwrap();

        // Get the layout to know where audio starts in virtual file
        let layout = db.get_format_layout(file_id).unwrap().unwrap();
        let metadata = db.get_file_metadata_row(file_id).unwrap();
        let handler = registry.get_by_format("flac").unwrap();
        let header = handler.synthesize(&metadata, &layout).unwrap();
        let header_len = header.len() as u64;

        // Read from well into the audio region
        let audio_offset = header_len + 1000;
        let result = overlay.read(file_id, audio_offset, 1000).await.unwrap();

        // Should return audio data
        assert!(!result.is_empty());
    }

    #[tokio::test]
    async fn test_read_boundary() {
        let (_dir, db, registry, reader, file_id) = setup_test_env().await;
        let overlay = OverlayReader::new(db.clone(), registry.clone(), reader.clone());

        // Get the actual header size
        let layout = db.get_format_layout(file_id).unwrap().unwrap();
        let metadata = db.get_file_metadata_row(file_id).unwrap();
        let handler = registry.get_by_format("flac").unwrap();
        let header = handler.synthesize(&metadata, &layout).unwrap();
        let header_len = header.len() as u64;

        // Read across the header/audio boundary
        let boundary_offset = header_len - 50;
        let result = overlay.read(file_id, boundary_offset, 100).await.unwrap();

        // Should return 100 bytes spanning both regions
        assert_eq!(result.len(), 100);

        // First 50 bytes should be from header
        assert_eq!(&result[0..50], &header[(header_len - 50) as usize..]);
    }

    #[tokio::test]
    async fn test_passthrough() {
        let dir = TempDir::new().unwrap();

        let db = Arc::new(Database::open_memory().unwrap());
        let registry = Arc::new(FormatHandlerRegistry::new());

        let cas_config = CasConfig {
            chunks_dir: dir.path().join("chunks"),
            ..Default::default()
        };
        let store = Arc::new(CasStore::open(cas_config).await.unwrap());

        let test_data = b"Hello, World! This is test data.";
        let hash = store.put(test_data).await.unwrap();

        // Insert file WITHOUT format_layout first to get the file_id
        let file_id = db
            .upsert_file(
                &OriginId::from("test"),
                Path::new("/test.txt"),
                &VirtualPath::new("/test.txt"),
                &AudioMeta::default(),
                UNIX_EPOCH,
                test_data.len() as u64,
            )
            .unwrap();

        let reader = Arc::new(FileReader::new(store));
        // Register manifest with the actual file_id from database
        reader.register_manifest(ChunkManifest {
            file_id,
            total_size: test_data.len() as u64,
            mtime: 0,
            chunks: vec![ChunkRef {
                hash,
                offset: 0,
                size: test_data.len() as u32,
            }],
        });

        let overlay = OverlayReader::new(db, registry, reader);

        let result = overlay
            .read(file_id, 0, test_data.len() as u32)
            .await
            .unwrap();
        assert_eq!(&result[..], test_data);
    }

    #[tokio::test]
    async fn test_estimate_virtual_size() {
        let (_dir, db, registry, reader, file_id) = setup_test_env().await;
        let overlay = OverlayReader::new(db, registry, reader);

        // Should return estimated size
        let size = overlay.estimate_virtual_size(file_id).unwrap();
        assert!(size.is_some());

        let virtual_size = size.unwrap();
        // Virtual size should be header + audio (100KB audio)
        assert!(virtual_size > 100 * 1024);
    }

    #[tokio::test]
    async fn test_estimate_virtual_size_passthrough() {
        let dir = TempDir::new().unwrap();
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = Arc::new(FormatHandlerRegistry::new());
        let cas_config = CasConfig {
            chunks_dir: dir.path().join("chunks"),
            ..Default::default()
        };
        let store = Arc::new(CasStore::open(cas_config).await.unwrap());
        let reader = Arc::new(FileReader::new(store));

        // Insert file WITHOUT format_layout
        let file_id = db
            .upsert_file(
                &OriginId::from("test"),
                Path::new("/test.txt"),
                &VirtualPath::new("/test.txt"),
                &AudioMeta::default(),
                UNIX_EPOCH,
                1000,
            )
            .unwrap();

        let overlay = OverlayReader::new(db, registry, reader);

        // Should return None for passthrough
        let size = overlay.estimate_virtual_size(file_id).unwrap();
        assert!(size.is_none());
    }

    #[tokio::test]
    async fn test_read_eof() {
        let (_dir, db, registry, reader, file_id) = setup_test_env().await;
        let overlay = OverlayReader::new(db, registry, reader);

        // Read past EOF
        let result = overlay.read(file_id, 1_000_000, 100).await.unwrap();
        assert!(result.is_empty());
    }
}
