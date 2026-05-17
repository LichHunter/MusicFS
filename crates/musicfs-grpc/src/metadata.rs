//! MetadataService gRPC handlers for metadata overlay operations.

use crate::proto::musicfs::v1::{
    metadata_service_server::MetadataService, BatchUpdateProgress, BatchUpdateRequest,
    ClearOverlayRequest, ClearOverlayResponse, GetMetadataRequest, ImportMetadataRequest,
    ImportProgress, MetadataResponse, UpdateMetadataRequest, UpdateMetadataResponse,
};
use musicfs_cache::{Database, EnrichmentUpdate};
use musicfs_core::{AudioMeta, FileId, VirtualPath};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::{debug, info, instrument, warn};

/// gRPC service implementation for metadata operations.
pub struct MetadataServiceImpl {
    db: Arc<Database>,
}

impl MetadataServiceImpl {
    /// Create a new MetadataServiceImpl with the given database.
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Convert AudioMeta to MetadataResponse proto message.
    fn audio_meta_to_response(file_id: FileId, meta: &AudioMeta) -> MetadataResponse {
        MetadataResponse {
            file_id: file_id.0,
            title: meta.title.clone(),
            artist: meta.artist.clone(),
            album: meta.album.clone(),
            album_artist: meta.album_artist.clone(),
            year: meta.year,
            track: meta.track,
            disc: meta.disc,
            genre: meta.genre.clone(),
            format: Some(format!("{:?}", meta.format)),
            duration_ms: meta.duration_ms,
            bitrate: meta.bitrate.map(|b| b as u64),
            track_total: meta.track_total,
            disc_total: meta.disc_total,
            date: meta.date.clone(),
            composer: meta.composer.clone(),
            comment: meta.comment.clone(),
            lyrics: meta.lyrics.clone(),
            copyright: meta.copyright.clone(),
            compilation: meta.compilation,
            artist_sort: meta.artist_sort.clone(),
            album_artist_sort: meta.album_artist_sort.clone(),
            album_sort: meta.album_sort.clone(),
            title_sort: meta.title_sort.clone(),
            mb_recording_id: meta.mb_recording_id.clone(),
            mb_album_id: meta.mb_album_id.clone(),
            mb_artist_id: meta.mb_artist_id.clone(),
            mb_album_artist_id: meta.mb_album_artist_id.clone(),
            mb_release_group_id: meta.mb_release_group_id.clone(),
            replaygain_track_gain: meta.replaygain_track_gain,
            replaygain_track_peak: meta.replaygain_track_peak,
            replaygain_album_gain: meta.replaygain_album_gain,
            replaygain_album_peak: meta.replaygain_album_peak,
            channels: meta.channels,
            bits_per_sample: meta.bits_per_sample,
            encoder: meta.encoder.clone(),
            label: None,
            album_type: None,
            cover_url: None,
            custom_tags: Default::default(),
        }
    }

    /// Convert UpdateMetadataRequest to AudioMeta for database update.
    fn request_to_audio_meta(req: &UpdateMetadataRequest) -> AudioMeta {
        AudioMeta {
            title: req.title.clone(),
            artist: req.artist.clone(),
            album: req.album.clone(),
            album_artist: req.album_artist.clone(),
            genre: req.genre.clone(),
            year: None,
            track: req.track_number,
            disc: req.disc_number,
            duration_ms: None,
            bitrate: None,
            sample_rate: None,
            format: musicfs_core::AudioFormat::Unknown,
            track_total: None,
            disc_total: None,
            date: req.date.clone(),
            composer: req.composer.clone(),
            comment: req.comment.clone(),
            lyrics: req.lyrics.clone(),
            copyright: req.copyright.clone(),
            compilation: req.compilation,
            artist_sort: req.artist_sort.clone(),
            album_artist_sort: req.album_artist_sort.clone(),
            album_sort: req.album_sort.clone(),
            title_sort: req.title_sort.clone(),
            mb_recording_id: req.mb_recording_id.clone(),
            mb_album_id: req.mb_album_id.clone(),
            mb_artist_id: req.mb_artist_id.clone(),
            mb_album_artist_id: None,
            mb_release_group_id: None,
            replaygain_track_gain: req.replaygain_track_gain,
            replaygain_track_peak: req.replaygain_track_peak,
            replaygain_album_gain: req.replaygain_album_gain,
            replaygain_album_peak: req.replaygain_album_peak,
            channels: None,
            bits_per_sample: None,
            encoder: None,
        }
    }
}

#[tonic::async_trait]
impl MetadataService for MetadataServiceImpl {
    #[instrument(level = "debug", skip(self, request), fields(method = "get_metadata"))]
    async fn get_metadata(
        &self,
        request: Request<GetMetadataRequest>,
    ) -> Result<Response<MetadataResponse>, Status> {
        let req = request.into_inner();
        debug!(virtual_path = %req.virtual_path, "GetMetadata request");

        if req.virtual_path.is_empty() {
            return Err(Status::invalid_argument("virtual_path cannot be empty"));
        }

        let vpath = VirtualPath::new(&req.virtual_path);

        let file_meta = self
            .db
            .get_file_by_virtual_path(&vpath)
            .map_err(|e| Status::internal(format!("Database error: {}", e)))?
            .ok_or_else(|| Status::not_found(format!("File not found: {}", req.virtual_path)))?;

        let audio_meta = self
            .db
            .get_file_metadata_row(file_meta.id)
            .map_err(|e| Status::internal(format!("Failed to get metadata: {}", e)))?;

        let response = Self::audio_meta_to_response(file_meta.id, &audio_meta);
        Ok(Response::new(response))
    }

    #[instrument(
        level = "info",
        skip(self, request),
        fields(method = "update_metadata")
    )]
    async fn update_metadata(
        &self,
        request: Request<UpdateMetadataRequest>,
    ) -> Result<Response<UpdateMetadataResponse>, Status> {
        let req = request.into_inner();
        let file_id = FileId(req.file_id);
        info!(file_id = req.file_id, "UpdateMetadata request");

        if req.file_id <= 0 {
            return Err(Status::invalid_argument("file_id must be positive"));
        }

        let audio_meta = Self::request_to_audio_meta(&req);

        if let Err(e) = self.db.update_metadata(file_id, &audio_meta) {
            warn!(file_id = req.file_id, error = %e, "Failed to update metadata");
            return Ok(Response::new(UpdateMetadataResponse {
                file_id: req.file_id,
                success: false,
                error_message: Some(e.to_string()),
            }));
        }

        if req.label.is_some() || req.album_type.is_some() || req.cover_url.is_some() {
            let enrichment = EnrichmentUpdate {
                label: req.label.clone(),
                album_type: req.album_type.clone(),
                cover_url: req.cover_url.clone(),
                genres_json: None,
                primary_genre: None,
                source: "orchestrator".to_string(),
            };
            if let Err(e) = self.db.update_enrichment(file_id, &enrichment) {
                warn!(file_id = req.file_id, error = %e, "Failed to update enrichment");
                return Ok(Response::new(UpdateMetadataResponse {
                    file_id: req.file_id,
                    success: false,
                    error_message: Some(e.to_string()),
                }));
            }
        }

        debug!(file_id = req.file_id, "Metadata updated successfully");
        Ok(Response::new(UpdateMetadataResponse {
            file_id: req.file_id,
            success: true,
            error_message: None,
        }))
    }

    #[instrument(level = "info", skip(self, request), fields(method = "clear_overlay"))]
    async fn clear_overlay(
        &self,
        request: Request<ClearOverlayRequest>,
    ) -> Result<Response<ClearOverlayResponse>, Status> {
        let req = request.into_inner();
        let file_id = FileId(req.file_id);
        info!(file_id = req.file_id, "ClearOverlay request");

        if req.file_id <= 0 {
            return Err(Status::invalid_argument("file_id must be positive"));
        }

        match self.db.clear_overlay(file_id) {
            Ok(()) => {
                debug!(file_id = req.file_id, "Overlay cleared successfully");
                Ok(Response::new(ClearOverlayResponse {
                    file_id: req.file_id,
                    success: true,
                    error_message: None,
                }))
            }
            Err(e) => {
                warn!(file_id = req.file_id, error = %e, "Failed to clear overlay");
                Ok(Response::new(ClearOverlayResponse {
                    file_id: req.file_id,
                    success: false,
                    error_message: Some(e.to_string()),
                }))
            }
        }
    }

    type BatchUpdateMetadataStream = ReceiverStream<Result<BatchUpdateProgress, Status>>;

    #[instrument(
        level = "info",
        skip(self, request),
        fields(method = "batch_update_metadata")
    )]
    async fn batch_update_metadata(
        &self,
        request: Request<BatchUpdateRequest>,
    ) -> Result<Response<Self::BatchUpdateMetadataStream>, Status> {
        let req = request.into_inner();
        let total = req.items.len() as u32;
        info!(item_count = total, "BatchUpdateMetadata request");

        let (tx, rx) = mpsc::channel(32);
        let db = Arc::clone(&self.db);

        tokio::spawn(async move {
            for (i, item) in req.items.into_iter().enumerate() {
                let file_id = FileId(item.file_id);
                let completed = (i + 1) as u32;

                let error_message = if let Some(ref metadata_req) = item.metadata {
                    let audio_meta = MetadataServiceImpl::request_to_audio_meta(metadata_req);
                    match db.update_metadata(file_id, &audio_meta) {
                        Ok(()) => {
                            if metadata_req.label.is_some()
                                || metadata_req.album_type.is_some()
                                || metadata_req.cover_url.is_some()
                            {
                                let enrichment = EnrichmentUpdate {
                                    label: metadata_req.label.clone(),
                                    album_type: metadata_req.album_type.clone(),
                                    cover_url: metadata_req.cover_url.clone(),
                                    genres_json: None,
                                    primary_genre: None,
                                    source: "orchestrator".to_string(),
                                };
                                if let Err(e) = db.update_enrichment(file_id, &enrichment) {
                                    Some(e.to_string())
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        }
                        Err(e) => Some(e.to_string()),
                    }
                } else {
                    Some("Missing metadata in batch item".to_string())
                };

                let progress = BatchUpdateProgress {
                    completed,
                    total,
                    current_file_id: Some(item.file_id),
                    error_message,
                };

                if tx.send(Ok(progress)).await.is_err() {
                    break;
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    type ImportMetadataStream = ReceiverStream<Result<ImportProgress, Status>>;

    #[instrument(
        level = "info",
        skip(self, request),
        fields(method = "import_metadata")
    )]
    async fn import_metadata(
        &self,
        request: Request<ImportMetadataRequest>,
    ) -> Result<Response<Self::ImportMetadataStream>, Status> {
        let req = request.into_inner();
        info!(source_path = %req.source_path, format = ?req.format, "ImportMetadata request");

        if req.source_path.is_empty() {
            return Err(Status::invalid_argument("source_path cannot be empty"));
        }

        let (tx, rx) = mpsc::channel(32);
        let db = Arc::clone(&self.db);
        let source_path = req.source_path.clone();
        let format = req.format.clone();

        tokio::spawn(async move {
            let file_format = format.as_deref().unwrap_or_else(|| {
                if source_path.ends_with(".csv") {
                    "csv"
                } else if source_path.ends_with(".json") {
                    "json"
                } else {
                    "unknown"
                }
            });

            let content = match tokio::fs::read_to_string(&source_path).await {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx
                        .send(Ok(ImportProgress {
                            imported: 0,
                            total: 0,
                            current_file: None,
                            error_message: Some(format!("Failed to read file: {}", e)),
                        }))
                        .await;
                    return;
                }
            };

            let entries: Vec<ImportEntry> = match file_format {
                "json" => match serde_json::from_str::<Vec<ImportEntry>>(&content) {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = tx
                            .send(Ok(ImportProgress {
                                imported: 0,
                                total: 0,
                                current_file: None,
                                error_message: Some(format!("Failed to parse JSON: {}", e)),
                            }))
                            .await;
                        return;
                    }
                },
                "csv" => match parse_csv_entries(&content) {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = tx
                            .send(Ok(ImportProgress {
                                imported: 0,
                                total: 0,
                                current_file: None,
                                error_message: Some(format!("Failed to parse CSV: {}", e)),
                            }))
                            .await;
                        return;
                    }
                },
                _ => {
                    let _ = tx
                        .send(Ok(ImportProgress {
                            imported: 0,
                            total: 0,
                            current_file: None,
                            error_message: Some(format!("Unsupported format: {}", file_format)),
                        }))
                        .await;
                    return;
                }
            };

            let total = entries.len() as u32;
            let mut imported = 0u32;

            for entry in entries {
                let vpath = VirtualPath::new(&entry.virtual_path);

                let file_meta = match db.get_file_by_virtual_path(&vpath) {
                    Ok(Some(f)) => f,
                    Ok(None) => {
                        let progress = ImportProgress {
                            imported,
                            total,
                            current_file: Some(entry.virtual_path.clone()),
                            error_message: Some(format!("File not found: {}", entry.virtual_path)),
                        };
                        if tx.send(Ok(progress)).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    Err(e) => {
                        let progress = ImportProgress {
                            imported,
                            total,
                            current_file: Some(entry.virtual_path.clone()),
                            error_message: Some(format!("Database error: {}", e)),
                        };
                        if tx.send(Ok(progress)).await.is_err() {
                            break;
                        }
                        continue;
                    }
                };

                let audio_meta = entry.to_audio_meta();
                let error_message = match db.update_metadata(file_meta.id, &audio_meta) {
                    Ok(()) => {
                        imported += 1;
                        None
                    }
                    Err(e) => Some(e.to_string()),
                };

                let progress = ImportProgress {
                    imported,
                    total,
                    current_file: Some(entry.virtual_path),
                    error_message,
                };

                if tx.send(Ok(progress)).await.is_err() {
                    break;
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

/// Entry from import file (CSV or JSON).
#[derive(Debug, Clone, serde::Deserialize)]
struct ImportEntry {
    virtual_path: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    artist: Option<String>,
    #[serde(default)]
    album: Option<String>,
    #[serde(default)]
    album_artist: Option<String>,
    #[serde(default)]
    genre: Option<String>,
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    track: Option<u32>,
    #[serde(default)]
    disc: Option<u32>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    composer: Option<String>,
    #[serde(default)]
    comment: Option<String>,
}

impl ImportEntry {
    fn to_audio_meta(&self) -> AudioMeta {
        AudioMeta {
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            album_artist: self.album_artist.clone(),
            genre: self.genre.clone(),
            year: self.year,
            track: self.track,
            disc: self.disc,
            duration_ms: None,
            bitrate: None,
            sample_rate: None,
            format: musicfs_core::AudioFormat::Unknown,
            track_total: None,
            disc_total: None,
            date: self.date.clone(),
            composer: self.composer.clone(),
            comment: self.comment.clone(),
            lyrics: None,
            copyright: None,
            compilation: None,
            artist_sort: None,
            album_artist_sort: None,
            album_sort: None,
            title_sort: None,
            mb_recording_id: None,
            mb_album_id: None,
            mb_artist_id: None,
            mb_album_artist_id: None,
            mb_release_group_id: None,
            replaygain_track_gain: None,
            replaygain_track_peak: None,
            replaygain_album_gain: None,
            replaygain_album_peak: None,
            channels: None,
            bits_per_sample: None,
            encoder: None,
        }
    }
}

/// Parse CSV content into ImportEntry list.
fn parse_csv_entries(content: &str) -> Result<Vec<ImportEntry>, String> {
    let mut reader = csv::Reader::from_reader(content.as_bytes());
    let mut entries = Vec::new();

    for result in reader.deserialize() {
        let entry: ImportEntry = result.map_err(|e| format!("CSV parse error: {}", e))?;
        entries.push(entry);
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::musicfs::v1::BatchUpdateItem;
    use musicfs_core::{AudioFormat, OriginId};
    use std::path::Path;
    use std::time::UNIX_EPOCH;
    use tempfile::TempDir;
    use tokio_stream::StreamExt;

    fn create_test_db() -> (TempDir, Arc<Database>) {
        let dir = TempDir::new().unwrap();
        let db = Arc::new(Database::open_memory().unwrap());
        (dir, db)
    }

    fn insert_test_file(db: &Database, vpath: &str) -> FileId {
        let real_path = format!("/music{}", vpath);
        db.upsert_file(
            &OriginId::from("local"),
            Path::new(&real_path),
            &VirtualPath::new(vpath),
            &AudioMeta {
                title: Some("Test Track".to_string()),
                artist: Some("Test Artist".to_string()),
                album: Some("Test Album".to_string()),
                format: AudioFormat::Flac,
                ..Default::default()
            },
            UNIX_EPOCH,
            1000,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn test_get_metadata_success() {
        let (_dir, db) = create_test_db();
        let vpath = "/Artist/Album/Track.flac";
        insert_test_file(&db, vpath);

        let service = MetadataServiceImpl::new(db);
        let request = Request::new(GetMetadataRequest {
            virtual_path: vpath.to_string(),
        });

        let response = service.get_metadata(request).await.unwrap();
        let meta = response.into_inner();

        assert_eq!(meta.title, Some("Test Track".to_string()));
        assert_eq!(meta.artist, Some("Test Artist".to_string()));
        assert_eq!(meta.album, Some("Test Album".to_string()));
    }

    #[tokio::test]
    async fn test_get_metadata_not_found() {
        let (_dir, db) = create_test_db();
        let service = MetadataServiceImpl::new(db);

        let request = Request::new(GetMetadataRequest {
            virtual_path: "/nonexistent.flac".to_string(),
        });

        let result = service.get_metadata(request).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn test_get_metadata_empty_path() {
        let (_dir, db) = create_test_db();
        let service = MetadataServiceImpl::new(db);

        let request = Request::new(GetMetadataRequest {
            virtual_path: String::new(),
        });

        let result = service.get_metadata(request).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_update_metadata_success() {
        let (_dir, db) = create_test_db();
        let vpath = "/Artist/Album/Track.flac";
        let file_id = insert_test_file(&db, vpath);

        let service = MetadataServiceImpl::new(db.clone());
        let request = Request::new(UpdateMetadataRequest {
            file_id: file_id.0,
            title: Some("Updated Title".to_string()),
            artist: Some("Updated Artist".to_string()),
            ..Default::default()
        });

        let response = service.update_metadata(request).await.unwrap();
        let result = response.into_inner();

        assert!(result.success);
        assert!(result.error_message.is_none());

        let meta = db.get_file_metadata_row(file_id).unwrap();
        assert_eq!(meta.title, Some("Updated Title".to_string()));
        assert_eq!(meta.artist, Some("Updated Artist".to_string()));
    }

    #[tokio::test]
    async fn test_update_metadata_invalid_id() {
        let (_dir, db) = create_test_db();
        let service = MetadataServiceImpl::new(db);

        let request = Request::new(UpdateMetadataRequest {
            file_id: 0,
            title: Some("Title".to_string()),
            ..Default::default()
        });

        let result = service.update_metadata(request).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_clear_overlay_success() {
        let (_dir, db) = create_test_db();
        let vpath = "/Artist/Album/Track.flac";
        let file_id = insert_test_file(&db, vpath);

        let service = MetadataServiceImpl::new(db.clone());
        let request = Request::new(ClearOverlayRequest { file_id: file_id.0 });

        let response = service.clear_overlay(request).await.unwrap();
        let result = response.into_inner();

        assert!(result.success);
        assert!(result.error_message.is_none());

        let meta = db.get_file_metadata_row(file_id).unwrap();
        assert!(meta.title.is_none());
        assert!(meta.artist.is_none());
    }

    #[tokio::test]
    async fn test_clear_overlay_invalid_id() {
        let (_dir, db) = create_test_db();
        let service = MetadataServiceImpl::new(db);

        let request = Request::new(ClearOverlayRequest { file_id: -1 });

        let result = service.clear_overlay(request).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_batch_update_metadata() {
        let (_dir, db) = create_test_db();
        let file_id1 = insert_test_file(&db, "/Track1.flac");
        let file_id2 = insert_test_file(&db, "/Track2.flac");

        let service = MetadataServiceImpl::new(db.clone());
        let request = Request::new(BatchUpdateRequest {
            items: vec![
                BatchUpdateItem {
                    file_id: file_id1.0,
                    metadata: Some(UpdateMetadataRequest {
                        file_id: file_id1.0,
                        title: Some("Batch Title 1".to_string()),
                        ..Default::default()
                    }),
                },
                BatchUpdateItem {
                    file_id: file_id2.0,
                    metadata: Some(UpdateMetadataRequest {
                        file_id: file_id2.0,
                        title: Some("Batch Title 2".to_string()),
                        ..Default::default()
                    }),
                },
            ],
        });

        let response = service.batch_update_metadata(request).await.unwrap();
        let mut stream = response.into_inner();

        let mut progress_count = 0;
        while let Some(Ok(result)) = stream.next().await {
            progress_count += 1;
            assert!(result.error_message.is_none());
        }

        assert_eq!(progress_count, 2);

        let meta1 = db.get_file_metadata_row(file_id1).unwrap();
        assert_eq!(meta1.title, Some("Batch Title 1".to_string()));

        let meta2 = db.get_file_metadata_row(file_id2).unwrap();
        assert_eq!(meta2.title, Some("Batch Title 2".to_string()));
    }

    #[tokio::test]
    async fn test_import_metadata_empty_path() {
        let (_dir, db) = create_test_db();
        let service = MetadataServiceImpl::new(db);

        let request = Request::new(ImportMetadataRequest {
            source_path: String::new(),
            format: None,
        });

        let result = service.import_metadata(request).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn test_parse_csv_entries() {
        let csv_content = r#"virtual_path,title,artist,album
/Track1.flac,Title 1,Artist 1,Album 1
/Track2.flac,Title 2,Artist 2,Album 2"#;

        let entries = parse_csv_entries(csv_content).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].virtual_path, "/Track1.flac");
        assert_eq!(entries[0].title, Some("Title 1".to_string()));
        assert_eq!(entries[1].virtual_path, "/Track2.flac");
        assert_eq!(entries[1].artist, Some("Artist 2".to_string()));
    }

    #[test]
    fn test_import_entry_to_audio_meta() {
        let entry = ImportEntry {
            virtual_path: "/test.flac".to_string(),
            title: Some("Test".to_string()),
            artist: Some("Artist".to_string()),
            album: None,
            album_artist: None,
            genre: Some("Rock".to_string()),
            year: Some(2024),
            track: Some(1),
            disc: None,
            date: None,
            composer: None,
            comment: None,
        };

        let meta = entry.to_audio_meta();
        assert_eq!(meta.title, Some("Test".to_string()));
        assert_eq!(meta.artist, Some("Artist".to_string()));
        assert_eq!(meta.genre, Some("Rock".to_string()));
        assert_eq!(meta.year, Some(2024));
        assert_eq!(meta.track, Some(1));
    }
}
