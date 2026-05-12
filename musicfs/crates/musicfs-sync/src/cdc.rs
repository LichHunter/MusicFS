use fastcdc::v2020::FastCDC;
use musicfs_core::ChunkHash;

pub struct CdcChunker {
    min_size: u32,
    avg_size: u32,
    max_size: u32,
}

impl Default for CdcChunker {
    fn default() -> Self {
        Self {
            min_size: 16 * 1024,
            avg_size: 64 * 1024,
            max_size: 256 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub hash: ChunkHash,
    pub offset: u64,
    pub length: u32,
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub struct ChunkRef<'a> {
    pub hash: ChunkHash,
    pub offset: u64,
    pub length: u32,
    pub data: &'a [u8],
}

impl CdcChunker {
    pub fn new(min_size: u32, avg_size: u32, max_size: u32) -> Self {
        Self {
            min_size,
            avg_size,
            max_size,
        }
    }

    pub fn chunk(&self, data: &[u8]) -> Vec<Chunk> {
        let chunker = FastCDC::new(data, self.min_size, self.avg_size, self.max_size);

        chunker
            .map(|c| {
                let chunk_data = &data[c.offset..c.offset + c.length];
                Chunk {
                    hash: ChunkHash::from_bytes(chunk_data),
                    offset: c.offset as u64,
                    length: c.length as u32,
                    data: chunk_data.to_vec(),
                }
            })
            .collect()
    }

    pub fn chunk_refs<'a>(&self, data: &'a [u8]) -> Vec<ChunkRef<'a>> {
        let chunker = FastCDC::new(data, self.min_size, self.avg_size, self.max_size);

        chunker
            .map(|c| {
                let chunk_data = &data[c.offset..c.offset + c.length];
                ChunkRef {
                    hash: ChunkHash::from_bytes(chunk_data),
                    offset: c.offset as u64,
                    length: c.length as u32,
                    data: chunk_data,
                }
            })
            .collect()
    }

    pub fn chunk_streaming<F>(&self, data: &[u8], mut processor: F) -> usize
    where
        F: FnMut(ChunkRef<'_>),
    {
        let chunker = FastCDC::new(data, self.min_size, self.avg_size, self.max_size);
        let mut count = 0;

        for c in chunker {
            let chunk_data = &data[c.offset..c.offset + c.length];
            processor(ChunkRef {
                hash: ChunkHash::from_bytes(chunk_data),
                offset: c.offset as u64,
                length: c.length as u32,
                data: chunk_data,
            });
            count += 1;
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cdc_basic() {
        let chunker = CdcChunker::default();
        let data = vec![0u8; 256 * 1024];

        let chunks = chunker.chunk(&data);

        assert!(!chunks.is_empty());

        let total: u64 = chunks.iter().map(|c| c.length as u64).sum();
        assert_eq!(total, data.len() as u64);

        let mut offset = 0u64;
        for chunk in &chunks {
            assert_eq!(chunk.offset, offset);
            offset += chunk.length as u64;
        }
    }

    #[test]
    fn test_cdc_stable_boundaries() {
        let chunker = CdcChunker::new(4 * 1024, 16 * 1024, 64 * 1024);

        let mut data1 = vec![0u8; 512 * 1024];
        for (i, b) in data1.iter_mut().enumerate() {
            *b = ((i * 17 + 31) % 256) as u8;
        }

        let mut data2 = vec![0xFFu8; 1024];
        data2.extend_from_slice(&data1);

        let chunks1 = chunker.chunk(&data1);
        let chunks2 = chunker.chunk(&data2);

        let hashes1: std::collections::HashSet<_> = chunks1.iter().map(|c| c.hash).collect();
        let hashes2: std::collections::HashSet<_> = chunks2.iter().map(|c| c.hash).collect();

        let shared = hashes1.intersection(&hashes2).count();

        assert!(shared > 0, "CDC should produce stable boundaries, got {} chunks in original, {} after prepend", chunks1.len(), chunks2.len());
    }

    #[test]
    fn test_cdc_chunk_sizes() {
        let chunker = CdcChunker::default();

        let data: Vec<u8> = (0..1024 * 1024).map(|i| ((i * 17 + 31) % 256) as u8).collect();

        let chunks = chunker.chunk(&data);

        for chunk in &chunks {
            if chunk.offset + chunk.length as u64 != data.len() as u64 {
                assert!(
                    chunk.length >= chunker.min_size / 2,
                    "Chunk too small: {}",
                    chunk.length
                );
                assert!(
                    chunk.length <= chunker.max_size * 2,
                    "Chunk too large: {}",
                    chunk.length
                );
            }
        }
    }

    #[test]
    fn test_cdc_streaming() {
        let chunker = CdcChunker::default();
        let data = vec![0u8; 256 * 1024];

        let mut streamed = Vec::new();
        let count = chunker.chunk_streaming(&data, |chunk| {
            streamed.push((chunk.hash, chunk.offset, chunk.length));
        });

        let batched = chunker.chunk(&data);

        assert_eq!(count, batched.len());
        for (i, chunk) in batched.iter().enumerate() {
            assert_eq!(streamed[i].0, chunk.hash);
            assert_eq!(streamed[i].1, chunk.offset);
            assert_eq!(streamed[i].2, chunk.length);
        }
    }

    #[test]
    fn test_bandwidth_reduction_metadata_edit() {
        let chunker = CdcChunker::new(4 * 1024, 16 * 1024, 64 * 1024);

        let mut state = 12345u64;
        let original: Vec<u8> = (0..2 * 1024 * 1024)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                (state >> 56) as u8
            })
            .collect();

        let chunks1 = chunker.chunk(&original);
        let hashes1: std::collections::HashSet<_> = chunks1.iter().map(|c| c.hash).collect();

        let mut modified = original.clone();
        let mid = modified.len() / 2;
        for i in mid..mid + 100 {
            modified[i] = 0xFF;
        }

        let chunks2 = chunker.chunk(&modified);
        let hashes2: std::collections::HashSet<_> = chunks2.iter().map(|c| c.hash).collect();

        let reused = hashes1.intersection(&hashes2).count();
        let reuse_ratio = reused as f64 / chunks2.len() as f64;

        // NFR-6.4 requires >90% bandwidth reduction for typical edits
        assert!(
            reuse_ratio > 0.90,
            "Expected >90% chunk reuse for mid-file edit (NFR-6.4). Reused {}/{} chunks ({:.1}%, total {} original)",
            reused,
            chunks2.len(),
            reuse_ratio * 100.0,
            chunks1.len()
        );
    }
}
