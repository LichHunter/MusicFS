use crate::proto::musicfs::v1::{
    music_fs_server::MusicFs, SearchRequest, SearchResponse, SearchResult,
};
use musicfs_search::SearchIndex;
use std::sync::Arc;
use std::time::Instant;
use tonic::{Request, Response, Status};
use tracing::debug;

pub struct SearchService {
    index: Arc<SearchIndex>,
}

impl SearchService {
    pub fn new(index: Arc<SearchIndex>) -> Self {
        Self { index }
    }
}

#[tonic::async_trait]
impl MusicFs for SearchService {
    async fn search(
        &self,
        request: Request<SearchRequest>,
    ) -> Result<Response<SearchResponse>, Status> {
        let start = Instant::now();
        let req = request.into_inner();

        if req.query.is_empty() {
            return Err(Status::invalid_argument("Query cannot be empty"));
        }

        if req.query.len() > 256 {
            return Err(Status::invalid_argument("Query exceeds maximum length (256)"));
        }

        let limit = req.limit.unwrap_or(100).min(10000) as usize;
        let offset = req.offset.unwrap_or(0) as usize;

        let results = self
            .index
            .search(&req.query, limit + offset)
            .map_err(|e| Status::internal(format!("Search failed: {}", e)))?;

        let hits: Vec<SearchResult> = results
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|hit| SearchResult {
                file_id: hit.file_id.0,
                virtual_path: hit.virtual_path.as_str().to_string(),
                artist: hit.artist,
                album: hit.album,
                title: hit.title,
                score: hit.score,
                highlights: Default::default(),
            })
            .collect();

        let total_matches = self.index.count();
        let query_time_ms = start.elapsed().as_millis() as u32;

        debug!(
            "Search '{}' returned {} results in {}ms",
            req.query,
            hits.len(),
            query_time_ms
        );

        Ok(Response::new(SearchResponse {
            results: hits,
            total_matches,
            query_time_ms,
        }))
    }

    type SearchStreamStream = tokio_stream::wrappers::ReceiverStream<Result<SearchResult, Status>>;

    async fn search_stream(
        &self,
        request: Request<SearchRequest>,
    ) -> Result<Response<Self::SearchStreamStream>, Status> {
        let req = request.into_inner();

        if req.query.is_empty() {
            return Err(Status::invalid_argument("Query cannot be empty"));
        }

        let limit = req.limit.unwrap_or(1000).min(10000) as usize;

        let results = self
            .index
            .search(&req.query, limit)
            .map_err(|e| Status::internal(format!("Search failed: {}", e)))?;

        let (tx, rx) = tokio::sync::mpsc::channel(100);

        tokio::spawn(async move {
            for hit in results {
                let result = SearchResult {
                    file_id: hit.file_id.0,
                    virtual_path: hit.virtual_path.as_str().to_string(),
                    artist: hit.artist,
                    album: hit.album,
                    title: hit.title,
                    score: hit.score,
                    highlights: Default::default(),
                };
                if tx.send(Ok(result)).await.is_err() {
                    break;
                }
            }
        });

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_grpc_search_empty_query() {
        let dir = TempDir::new().unwrap();
        let index = Arc::new(SearchIndex::open(dir.path()).unwrap());
        let service = SearchService::new(index);

        let request = Request::new(SearchRequest {
            query: String::new(),
            limit: Some(10),
            offset: None,
            origin_id: None,
        });

        let result = service.search(request).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_grpc_search_returns_response() {
        let dir = TempDir::new().unwrap();
        let index = Arc::new(SearchIndex::open(dir.path()).unwrap());
        let service = SearchService::new(index);

        let request = Request::new(SearchRequest {
            query: "test".to_string(),
            limit: Some(10),
            offset: None,
            origin_id: None,
        });

        let response = service.search(request).await.unwrap();
        assert!(response.get_ref().results.is_empty());
    }
}
