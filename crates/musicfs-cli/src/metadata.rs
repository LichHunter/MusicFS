//! CLI subcommands for metadata overlay management.

use anyhow::{Context, Result};
use clap::Subcommand;
use musicfs_grpc::proto::musicfs::v1::{
    metadata_service_client::MetadataServiceClient, ClearOverlayRequest, GetMetadataRequest,
    ImportMetadataRequest, UpdateMetadataRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio_stream::StreamExt;
use tonic::transport::Channel;
use tracing::{debug, info};

/// Metadata overlay management subcommands.
#[derive(Subcommand)]
pub enum MetadataCommand {
    /// Get metadata for a file (prints as JSON)
    Get {
        /// Virtual path of the file
        path: String,
        /// Print only a specific field
        #[arg(long)]
        field: Option<String>,
    },
    /// Set metadata fields for a file
    Set {
        /// Virtual path of the file
        path: String,
        /// Track title
        #[arg(long)]
        title: Option<String>,
        /// Artist name
        #[arg(long)]
        artist: Option<String>,
        /// Album name
        #[arg(long)]
        album: Option<String>,
        /// Album artist
        #[arg(long)]
        album_artist: Option<String>,
        /// Track number
        #[arg(long)]
        track: Option<u32>,
        /// Disc number
        #[arg(long)]
        disc: Option<u32>,
        /// Genre
        #[arg(long)]
        genre: Option<String>,
        /// Date (YYYY-MM-DD or YYYY)
        #[arg(long)]
        date: Option<String>,
        /// Composer
        #[arg(long)]
        composer: Option<String>,
        /// Comment
        #[arg(long)]
        comment: Option<String>,
        /// Set metadata from JSON string
        #[arg(long, conflicts_with_all = ["title", "artist", "album", "album_artist", "track", "disc", "genre", "date", "composer", "comment"])]
        json: Option<String>,
    },
    /// Clear metadata overlay (revert to original)
    Clear {
        /// Virtual path of the file
        path: String,
    },
    /// Show difference between current and original metadata
    Diff {
        /// Virtual path of the file
        path: String,
    },
    /// Import metadata from CSV or JSON file
    Import {
        /// Import file path
        file: PathBuf,
        /// File format (csv or json, auto-detected if not specified)
        #[arg(long)]
        format: Option<String>,
    },
    /// Export metadata to file
    Export {
        /// Output file path
        #[arg(long, short)]
        output: PathBuf,
        /// Filter by search query
        #[arg(long)]
        query: Option<String>,
        /// Output format (csv or json, auto-detected from extension)
        #[arg(long)]
        format: Option<String>,
    },
}

/// Metadata fields for JSON serialization.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MetadataFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disc: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_total: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disc_total: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub composer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lyrics: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copyright: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compilation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist_sort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_artist_sort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_sort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_sort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mb_recording_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mb_album_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mb_artist_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mb_album_artist_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mb_release_group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaygain_track_gain: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaygain_track_peak: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaygain_album_gain: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaygain_album_peak: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bits_per_sample: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoder: Option<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty", default)]
    pub custom_tags: HashMap<String, String>,
}

/// Execute a metadata subcommand.
pub async fn run_metadata(command: MetadataCommand, endpoint: &str) -> Result<()> {
    match command {
        MetadataCommand::Get { path, field } => run_get(endpoint, &path, field.as_deref()).await,
        MetadataCommand::Set {
            path,
            title,
            artist,
            album,
            album_artist,
            track,
            disc,
            genre,
            date,
            composer,
            comment,
            json,
        } => {
            run_set(
                endpoint,
                &path,
                title,
                artist,
                album,
                album_artist,
                track,
                disc,
                genre,
                date,
                composer,
                comment,
                json,
            )
            .await
        }
        MetadataCommand::Clear { path } => run_clear(endpoint, &path).await,
        MetadataCommand::Diff { path } => run_diff(endpoint, &path).await,
        MetadataCommand::Import { file, format } => run_import(endpoint, &file, format).await,
        MetadataCommand::Export {
            output,
            query,
            format,
        } => run_export(endpoint, &output, query, format).await,
    }
}

async fn connect(endpoint: &str) -> Result<MetadataServiceClient<Channel>> {
    MetadataServiceClient::connect(endpoint.to_string())
        .await
        .context("Failed to connect to gRPC server")
}

async fn run_get(endpoint: &str, path: &str, field: Option<&str>) -> Result<()> {
    let mut client = connect(endpoint).await?;

    let response = client
        .get_metadata(GetMetadataRequest {
            virtual_path: path.to_string(),
        })
        .await
        .context("GetMetadata RPC failed")?;

    let meta = response.into_inner();
    let fields = MetadataFields {
        file_id: Some(meta.file_id),
        title: meta.title,
        artist: meta.artist,
        album: meta.album,
        album_artist: meta.album_artist,
        year: meta.year,
        track: meta.track,
        disc: meta.disc,
        genre: meta.genre,
        format: meta.format,
        duration_ms: meta.duration_ms,
        bitrate: meta.bitrate,
        track_total: meta.track_total,
        disc_total: meta.disc_total,
        date: meta.date,
        composer: meta.composer,
        comment: meta.comment,
        lyrics: meta.lyrics,
        copyright: meta.copyright,
        compilation: meta.compilation,
        artist_sort: meta.artist_sort,
        album_artist_sort: meta.album_artist_sort,
        album_sort: meta.album_sort,
        title_sort: meta.title_sort,
        mb_recording_id: meta.mb_recording_id,
        mb_album_id: meta.mb_album_id,
        mb_artist_id: meta.mb_artist_id,
        mb_album_artist_id: meta.mb_album_artist_id,
        mb_release_group_id: meta.mb_release_group_id,
        replaygain_track_gain: meta.replaygain_track_gain,
        replaygain_track_peak: meta.replaygain_track_peak,
        replaygain_album_gain: meta.replaygain_album_gain,
        replaygain_album_peak: meta.replaygain_album_peak,
        channels: meta.channels,
        bits_per_sample: meta.bits_per_sample,
        encoder: meta.encoder,
        custom_tags: meta.custom_tags,
    };

    if let Some(field_name) = field {
        let value = get_field_value(&fields, field_name)?;
        println!("{}", value);
    } else {
        let json = serde_json::to_string_pretty(&fields)?;
        println!("{}", json);
    }

    Ok(())
}

fn get_field_value(fields: &MetadataFields, field_name: &str) -> Result<String> {
    let value = match field_name {
        "file_id" => fields.file_id.map(|v| v.to_string()),
        "title" => fields.title.clone(),
        "artist" => fields.artist.clone(),
        "album" => fields.album.clone(),
        "album_artist" => fields.album_artist.clone(),
        "year" => fields.year.map(|v| v.to_string()),
        "track" => fields.track.map(|v| v.to_string()),
        "disc" => fields.disc.map(|v| v.to_string()),
        "genre" => fields.genre.clone(),
        "format" => fields.format.clone(),
        "duration_ms" => fields.duration_ms.map(|v| v.to_string()),
        "bitrate" => fields.bitrate.map(|v| v.to_string()),
        "track_total" => fields.track_total.map(|v| v.to_string()),
        "disc_total" => fields.disc_total.map(|v| v.to_string()),
        "date" => fields.date.clone(),
        "composer" => fields.composer.clone(),
        "comment" => fields.comment.clone(),
        "lyrics" => fields.lyrics.clone(),
        "copyright" => fields.copyright.clone(),
        "compilation" => fields.compilation.map(|v| v.to_string()),
        "artist_sort" => fields.artist_sort.clone(),
        "album_artist_sort" => fields.album_artist_sort.clone(),
        "album_sort" => fields.album_sort.clone(),
        "title_sort" => fields.title_sort.clone(),
        "mb_recording_id" => fields.mb_recording_id.clone(),
        "mb_album_id" => fields.mb_album_id.clone(),
        "mb_artist_id" => fields.mb_artist_id.clone(),
        "mb_album_artist_id" => fields.mb_album_artist_id.clone(),
        "mb_release_group_id" => fields.mb_release_group_id.clone(),
        "replaygain_track_gain" => fields.replaygain_track_gain.map(|v| v.to_string()),
        "replaygain_track_peak" => fields.replaygain_track_peak.map(|v| v.to_string()),
        "replaygain_album_gain" => fields.replaygain_album_gain.map(|v| v.to_string()),
        "replaygain_album_peak" => fields.replaygain_album_peak.map(|v| v.to_string()),
        "channels" => fields.channels.map(|v| v.to_string()),
        "bits_per_sample" => fields.bits_per_sample.map(|v| v.to_string()),
        "encoder" => fields.encoder.clone(),
        _ => return Err(anyhow::anyhow!("Unknown field: {}", field_name)),
    };

    Ok(value.unwrap_or_else(|| "null".to_string()))
}

#[allow(clippy::too_many_arguments)]
async fn run_set(
    endpoint: &str,
    path: &str,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    album_artist: Option<String>,
    track: Option<u32>,
    disc: Option<u32>,
    genre: Option<String>,
    date: Option<String>,
    composer: Option<String>,
    comment: Option<String>,
    json: Option<String>,
) -> Result<()> {
    let mut client = connect(endpoint).await?;

    let get_response = client
        .get_metadata(GetMetadataRequest {
            virtual_path: path.to_string(),
        })
        .await
        .context("Failed to get file metadata")?;

    let file_id = get_response.into_inner().file_id;

    let request = if let Some(json_str) = json {
        let fields: MetadataFields =
            serde_json::from_str(&json_str).context("Failed to parse JSON metadata")?;
        UpdateMetadataRequest {
            file_id,
            title: fields.title,
            artist: fields.artist,
            album: fields.album,
            album_artist: fields.album_artist,
            track_number: fields.track,
            disc_number: fields.disc,
            genre: fields.genre,
            date: fields.date,
            composer: fields.composer,
            comment: fields.comment,
            lyrics: fields.lyrics,
            copyright: fields.copyright,
            compilation: fields.compilation,
            artist_sort: fields.artist_sort,
            album_artist_sort: fields.album_artist_sort,
            album_sort: fields.album_sort,
            title_sort: fields.title_sort,
            mb_recording_id: fields.mb_recording_id,
            mb_album_id: fields.mb_album_id,
            mb_artist_id: fields.mb_artist_id,
            replaygain_track_gain: fields.replaygain_track_gain,
            replaygain_track_peak: fields.replaygain_track_peak,
            replaygain_album_gain: fields.replaygain_album_gain,
            replaygain_album_peak: fields.replaygain_album_peak,
            label: None,
            album_type: None,
            cover_url: None,
            custom_tags: fields.custom_tags,
        }
    } else {
        UpdateMetadataRequest {
            file_id,
            title,
            artist,
            album,
            album_artist,
            track_number: track,
            disc_number: disc,
            genre,
            date,
            composer,
            comment,
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
            replaygain_track_gain: None,
            replaygain_track_peak: None,
            replaygain_album_gain: None,
            replaygain_album_peak: None,
            label: None,
            album_type: None,
            cover_url: None,
            custom_tags: HashMap::new(),
        }
    };

    let response = client
        .update_metadata(request)
        .await
        .context("UpdateMetadata RPC failed")?;

    let result = response.into_inner();
    if result.success {
        info!(file_id = result.file_id, "Metadata updated successfully");
        println!("Metadata updated for file_id={}", result.file_id);
    } else {
        let msg = result
            .error_message
            .unwrap_or_else(|| "Unknown error".to_string());
        anyhow::bail!("Failed to update metadata: {}", msg);
    }

    Ok(())
}

async fn run_clear(endpoint: &str, path: &str) -> Result<()> {
    let mut client = connect(endpoint).await?;

    let get_response = client
        .get_metadata(GetMetadataRequest {
            virtual_path: path.to_string(),
        })
        .await
        .context("Failed to get file metadata")?;

    let file_id = get_response.into_inner().file_id;

    let response = client
        .clear_overlay(ClearOverlayRequest { file_id })
        .await
        .context("ClearOverlay RPC failed")?;

    let result = response.into_inner();
    if result.success {
        info!(file_id = result.file_id, "Overlay cleared successfully");
        println!("Metadata overlay cleared for file_id={}", result.file_id);
    } else {
        let msg = result
            .error_message
            .unwrap_or_else(|| "Unknown error".to_string());
        anyhow::bail!("Failed to clear overlay: {}", msg);
    }

    Ok(())
}

async fn run_diff(endpoint: &str, path: &str) -> Result<()> {
    let mut client = connect(endpoint).await?;

    let response = client
        .get_metadata(GetMetadataRequest {
            virtual_path: path.to_string(),
        })
        .await
        .context("GetMetadata RPC failed")?;

    let meta = response.into_inner();
    debug!(file_id = meta.file_id, "Retrieved metadata for diff");

    println!("Current metadata for: {}", path);
    println!("---");

    let fields = MetadataFields {
        file_id: Some(meta.file_id),
        title: meta.title,
        artist: meta.artist,
        album: meta.album,
        album_artist: meta.album_artist,
        year: meta.year,
        track: meta.track,
        disc: meta.disc,
        genre: meta.genre,
        format: meta.format,
        duration_ms: meta.duration_ms,
        bitrate: meta.bitrate,
        track_total: meta.track_total,
        disc_total: meta.disc_total,
        date: meta.date,
        composer: meta.composer,
        comment: meta.comment,
        lyrics: meta.lyrics,
        copyright: meta.copyright,
        compilation: meta.compilation,
        artist_sort: meta.artist_sort,
        album_artist_sort: meta.album_artist_sort,
        album_sort: meta.album_sort,
        title_sort: meta.title_sort,
        mb_recording_id: meta.mb_recording_id,
        mb_album_id: meta.mb_album_id,
        mb_artist_id: meta.mb_artist_id,
        mb_album_artist_id: meta.mb_album_artist_id,
        mb_release_group_id: meta.mb_release_group_id,
        replaygain_track_gain: meta.replaygain_track_gain,
        replaygain_track_peak: meta.replaygain_track_peak,
        replaygain_album_gain: meta.replaygain_album_gain,
        replaygain_album_peak: meta.replaygain_album_peak,
        channels: meta.channels,
        bits_per_sample: meta.bits_per_sample,
        encoder: meta.encoder,
        custom_tags: meta.custom_tags,
    };

    let json = serde_json::to_string_pretty(&fields)?;
    println!("{}", json);
    println!("---");
    println!("Note: Original metadata comparison requires re-parsing the source file.");
    println!("Use 'musicfs metadata clear <path>' to revert to original metadata.");

    Ok(())
}

async fn run_import(endpoint: &str, file: &PathBuf, format: Option<String>) -> Result<()> {
    let mut client = connect(endpoint).await?;

    let file_format = format.or_else(|| {
        file.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
    });

    let source_path = file
        .canonicalize()
        .unwrap_or_else(|_| file.clone())
        .to_string_lossy()
        .to_string();

    info!(source_path = %source_path, format = ?file_format, "Starting metadata import");

    let response = client
        .import_metadata(ImportMetadataRequest {
            source_path,
            format: file_format,
        })
        .await
        .context("ImportMetadata RPC failed")?;

    let mut stream = response.into_inner();
    let mut last_imported = 0u32;
    let mut last_total = 0u32;
    let mut errors = Vec::new();

    while let Some(progress) = stream.next().await {
        let progress = progress.context("Stream error")?;
        last_imported = progress.imported;
        last_total = progress.total;

        if let Some(ref err) = progress.error_message {
            let file = progress.current_file.as_deref().unwrap_or("unknown");
            errors.push(format!("{}: {}", file, err));
        }

        if let Some(ref current) = progress.current_file {
            print!(
                "\rImporting: {}/{} - {}",
                progress.imported, progress.total, current
            );
            std::io::Write::flush(&mut std::io::stdout())?;
        }
    }

    println!();
    println!(
        "Import complete: {}/{} files imported",
        last_imported, last_total
    );

    if !errors.is_empty() {
        println!("\nErrors ({}):", errors.len());
        for err in errors.iter().take(10) {
            println!("  - {}", err);
        }
        if errors.len() > 10 {
            println!("  ... and {} more", errors.len() - 10);
        }
    }

    Ok(())
}

async fn run_export(
    _endpoint: &str,
    output: &PathBuf,
    query: Option<String>,
    format: Option<String>,
) -> Result<()> {
    let output_format = format.or_else(|| {
        output
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
    });

    println!("Export metadata to: {}", output.display());
    if let Some(ref q) = query {
        println!("Filter query: {}", q);
    }
    println!("Format: {}", output_format.as_deref().unwrap_or("json"));
    println!();
    println!("Note: Export requires file listing capability.");
    println!("This feature requires integration with the Search service.");
    println!(
        "Use 'musicfs search <query>' to find files, then 'musicfs metadata get <path>' for each."
    );

    Ok(())
}
