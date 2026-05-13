use crate::OriginId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub mount_point: PathBuf,
    pub cache_dir: PathBuf,
    pub origins: Vec<OriginConfig>,

    #[serde(default)]
    pub cache: CacheConfig,

    #[serde(default)]
    pub health: HealthConfig,

    #[serde(default)]
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OriginConfig {
    pub id: String,
    pub origin_type: OriginType,
    pub priority: u8,

    #[serde(default = "default_enabled")]
    pub enabled: bool,

    #[serde(flatten)]
    pub settings: HashMap<String, toml::Value>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum OriginType {
    Local,
    Nfs,
    Smb,
    S3,
    Sftp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    #[serde(default = "default_metadata_cache_mb")]
    pub metadata_cache_mb: u64,

    #[serde(default = "default_content_cache_gb")]
    pub content_cache_gb: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            metadata_cache_mb: default_metadata_cache_mb(),
            content_cache_gb: default_content_cache_gb(),
        }
    }
}

fn default_metadata_cache_mb() -> u64 {
    100
}
fn default_content_cache_gb() -> u64 {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthConfig {
    #[serde(default = "default_check_interval_secs")]
    pub check_interval_secs: u64,

    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,

    #[serde(default = "default_unhealthy_threshold")]
    pub unhealthy_threshold: u32,

    #[serde(default)]
    pub per_origin_thresholds: HashMap<OriginType, u32>,
}

impl Default for HealthConfig {
    fn default() -> Self {
        let mut per_origin = HashMap::new();
        per_origin.insert(OriginType::Local, 1);
        per_origin.insert(OriginType::Nfs, 3);
        per_origin.insert(OriginType::Smb, 3);
        per_origin.insert(OriginType::S3, 3);
        per_origin.insert(OriginType::Sftp, 3);

        Self {
            check_interval_secs: default_check_interval_secs(),
            timeout_ms: default_timeout_ms(),
            unhealthy_threshold: default_unhealthy_threshold(),
            per_origin_thresholds: per_origin,
        }
    }
}

impl HealthConfig {
    pub fn threshold_for(&self, origin_type: OriginType) -> u32 {
        self.per_origin_thresholds
            .get(&origin_type)
            .copied()
            .unwrap_or(self.unhealthy_threshold)
    }
}

fn default_check_interval_secs() -> u64 {
    30
}
fn default_timeout_ms() -> u64 {
    5000
}
fn default_unhealthy_threshold() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_dir")]
    pub log_dir: PathBuf,

    #[serde(default)]
    pub json_output: bool,

    #[serde(default = "default_true")]
    pub journald: bool,

    #[serde(default = "default_log_level")]
    pub level: String,

    #[serde(default = "default_sample_rate")]
    pub trace_sample_rate: f32,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            log_dir: default_log_dir(),
            json_output: false,
            journald: true,
            level: default_log_level(),
            trace_sample_rate: default_sample_rate(),
        }
    }
}

fn default_log_dir() -> PathBuf {
    PathBuf::from("/var/log/musicfs")
}

fn default_log_level() -> String {
    "musicfs=info,warn".to_string()
}

fn default_true() -> bool {
    true
}

fn default_sample_rate() -> f32 {
    1.0
}

impl Config {
    pub fn from_file(path: &std::path::Path) -> Result<Self, ConfigError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| ConfigError::Read(e.to_string()))?;
        toml::from_str(&content).map_err(|e| ConfigError::Parse(e.to_string()))
    }

    pub fn origin_id(&self, id: &str) -> Option<OriginId> {
        self.origins
            .iter()
            .find(|o| o.id == id)
            .map(|_| OriginId::from(id))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Failed to read config: {0}")]
    Read(String),

    #[error("Failed to parse config: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_config() {
        let toml = r#"
mount_point = "/mnt/music"
cache_dir = "/home/user/.cache/musicfs"

[[origins]]
id = "local"
origin_type = "local"
priority = 1
path = "/mnt/nas/music"

[[origins]]
id = "backup"
origin_type = "s3"
priority = 2
bucket = "music-backup"
region = "us-east-1"
"#;

        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.origins.len(), 2);
        assert_eq!(config.origins[0].priority, 1);
        assert_eq!(config.origins[1].origin_type, OriginType::S3);
    }

    #[test]
    fn test_default_health_thresholds() {
        let health = HealthConfig::default();
        assert_eq!(health.threshold_for(OriginType::Local), 1);
        assert_eq!(health.threshold_for(OriginType::Sftp), 3);
    }

    #[test]
    fn test_cache_defaults() {
        let cache = CacheConfig::default();
        assert_eq!(cache.metadata_cache_mb, 100);
        assert_eq!(cache.content_cache_gb, 10);
    }
}
