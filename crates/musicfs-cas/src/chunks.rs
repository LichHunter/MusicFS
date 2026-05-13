use musicfs_core::ChunkHash;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkLocation {
    pub path: PathBuf,
    pub size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRef {
    pub hash: ChunkHash,
    pub offset: u64,
    pub size: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_hash_from_bytes() {
        let data = b"hello world";
        let hash = ChunkHash::from_bytes(data);
        assert_eq!(hash.as_hex().len(), 16);
    }

    #[test]
    fn test_chunk_hash_deterministic() {
        let data = b"test data";
        let hash1 = ChunkHash::from_bytes(data);
        let hash2 = ChunkHash::from_bytes(data);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_chunk_hash_hex_roundtrip() {
        let data = b"roundtrip test";
        let hash = ChunkHash::from_bytes(data);
        let hex = hash.as_hex();
        let restored = ChunkHash::from_hex(&hex).unwrap();
        assert_eq!(hash, restored);
    }
}
