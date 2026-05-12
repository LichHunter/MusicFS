use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Clone)]
pub struct CredentialStore {
    cache: HashMap<String, Credential>,
}

impl std::fmt::Debug for CredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialStore")
            .field("cache_keys", &self.cache.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Credential {
    Basic {
        username: String,
        #[serde(skip_serializing)]
        password: String,
    },

    AwsKey {
        access_key_id: String,
        #[serde(skip_serializing)]
        secret_access_key: String,
        session_token: Option<String>,
        region: String,
    },

    SshKey {
        username: String,
        private_key_path: PathBuf,
        #[serde(skip_serializing)]
        passphrase: Option<String>,
    },

    EnvVar {
        var_name: String,
    },
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &"[REDACTED]")
                .finish(),
            Self::AwsKey {
                access_key_id,
                session_token,
                region,
                ..
            } => {
                let key_preview = if access_key_id.len() > 4 {
                    format!("{}...", &access_key_id[..4])
                } else {
                    "****".to_string()
                };
                let token_display = if session_token.is_some() {
                    "[REDACTED]"
                } else {
                    "None"
                };
                f.debug_struct("AwsKey")
                    .field("access_key_id", &key_preview)
                    .field("secret_access_key", &"[REDACTED]")
                    .field("session_token", &token_display)
                    .field("region", region)
                    .finish()
            }
            Self::SshKey {
                username,
                private_key_path,
                ..
            } => f
                .debug_struct("SshKey")
                .field("username", username)
                .field("private_key_path", private_key_path)
                .field("passphrase", &"[REDACTED]")
                .finish(),
            Self::EnvVar { var_name } => f
                .debug_struct("EnvVar")
                .field("var_name", var_name)
                .finish(),
        }
    }
}

impl CredentialStore {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    pub fn load(
        &mut self,
        origin_id: &str,
        config: &CredentialConfig,
    ) -> Result<Credential, CredentialError> {
        if let Some(cred) = self.cache.get(origin_id) {
            return Ok(cred.clone());
        }

        let cred = match config {
            CredentialConfig::Environment { prefix } => self.load_from_env(prefix)?,
            CredentialConfig::File { path } => self.load_from_file(path)?,
            CredentialConfig::Inline(cred) => cred.clone(),
        };

        self.cache.insert(origin_id.to_string(), cred.clone());
        Ok(cred)
    }

    fn load_from_env(&self, prefix: &str) -> Result<Credential, CredentialError> {
        if let (Ok(key), Ok(secret)) = (
            std::env::var(format!("{}_ACCESS_KEY_ID", prefix)),
            std::env::var(format!("{}_SECRET_ACCESS_KEY", prefix)),
        ) {
            return Ok(Credential::AwsKey {
                access_key_id: key,
                secret_access_key: secret,
                session_token: std::env::var(format!("{}_SESSION_TOKEN", prefix)).ok(),
                region: std::env::var(format!("{}_REGION", prefix))
                    .unwrap_or_else(|_| "us-east-1".to_string()),
            });
        }

        if let (Ok(user), Ok(pass)) = (
            std::env::var(format!("{}_USERNAME", prefix)),
            std::env::var(format!("{}_PASSWORD", prefix)),
        ) {
            return Ok(Credential::Basic {
                username: user,
                password: pass,
            });
        }

        Err(CredentialError::NotFound(format!(
            "No credentials found with prefix {}",
            prefix
        )))
    }

    fn load_from_file(&self, path: &PathBuf) -> Result<Credential, CredentialError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| CredentialError::FileRead(e.to_string()))?;

        if path.extension().map(|e| e == "json").unwrap_or(false) {
            serde_json::from_str(&content).map_err(|e| CredentialError::Parse(e.to_string()))
        } else {
            toml::from_str(&content).map_err(|e| CredentialError::Parse(e.to_string()))
        }
    }

    pub fn clear(&mut self) {
        self.cache.clear();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source")]
pub enum CredentialConfig {
    Environment { prefix: String },
    File { path: PathBuf },
    Inline(Credential),
}

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("Credential not found: {0}")]
    NotFound(String),

    #[error("Failed to read credential file: {0}")]
    FileRead(String),

    #[error("Failed to parse credential: {0}")]
    Parse(String),
}

impl Default for CredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_credential_debug_redacted() {
        let cred = Credential::Basic {
            username: "user".to_string(),
            password: "secret123".to_string(),
        };

        let debug_output = format!("{:?}", cred);
        assert!(debug_output.contains("user"));
        assert!(!debug_output.contains("secret123"));
        assert!(debug_output.contains("[REDACTED]"));
    }

    #[test]
    fn test_aws_credential_debug_redacted() {
        let cred = Credential::AwsKey {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
        };

        let debug_output = format!("{:?}", cred);
        assert!(debug_output.contains("AKIA..."));
        assert!(!debug_output.contains("wJalrXUtnFEMI"));
        assert!(debug_output.contains("[REDACTED]"));
    }

    #[test]
    fn test_credential_store_debug() {
        let mut store = CredentialStore::new();
        store.cache.insert(
            "test".to_string(),
            Credential::Basic {
                username: "user".to_string(),
                password: "secret".to_string(),
            },
        );

        let debug_output = format!("{:?}", store);
        assert!(debug_output.contains("test"));
        assert!(!debug_output.contains("secret"));
    }

    #[test]
    fn test_load_from_env() {
        std::env::set_var("TEST_ORIGIN_USERNAME", "testuser");
        std::env::set_var("TEST_ORIGIN_PASSWORD", "testpass");

        let store = CredentialStore::new();
        let cred = store.load_from_env("TEST_ORIGIN").unwrap();

        match cred {
            Credential::Basic { username, password } => {
                assert_eq!(username, "testuser");
                assert_eq!(password, "testpass");
            }
            _ => panic!("Expected Basic credential"),
        }

        std::env::remove_var("TEST_ORIGIN_USERNAME");
        std::env::remove_var("TEST_ORIGIN_PASSWORD");
    }
}
