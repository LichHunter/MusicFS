use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, SystemTime};
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartCollection {
    pub id: i64,
    pub name: String,
    pub query: CollectionQuery,
    pub created_at: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CollectionQuery {
    Match {
        field: String,
        pattern: String,
    },
    DateRange {
        field: String,
        start: i32,
        end: i32,
    },
    RecentlyAdded {
        days: u32,
    },
    RecentlyPlayed {
        days: u32,
    },
    MostPlayed {
        limit: u32,
    },
    Genre {
        genre: String,
    },
    Compound {
        op: BoolOp,
        children: Vec<CollectionQuery>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum BoolOp {
    And,
    Or,
}

impl CollectionQuery {
    pub fn to_tantivy_query(&self) -> String {
        match self {
            CollectionQuery::Match { field, pattern } => {
                format!("{}:{}", field, pattern)
            }
            CollectionQuery::DateRange { field, start, end } => {
                format!("{}:[{} TO {}]", field, start, end)
            }
            CollectionQuery::Genre { genre } => {
                format!("genre:{}", genre)
            }
            CollectionQuery::Compound { op, children } => {
                let sep = match op {
                    BoolOp::And => " AND ",
                    BoolOp::Or => " OR ",
                };
                let parts: Vec<_> = children
                    .iter()
                    .map(|c| format!("({})", c.to_tantivy_query()))
                    .collect();
                parts.join(sep)
            }
            _ => String::new(),
        }
    }

    pub fn is_dynamic(&self) -> bool {
        matches!(
            self,
            CollectionQuery::RecentlyAdded { .. }
                | CollectionQuery::RecentlyPlayed { .. }
                | CollectionQuery::MostPlayed { .. }
        )
    }
}

pub struct CollectionStore {
    db: Mutex<rusqlite::Connection>,
}

impl CollectionStore {
    pub fn new(db_path: &Path) -> Result<Self, CollectionError> {
        let db = rusqlite::Connection::open(db_path)?;

        db.execute(
            "CREATE TABLE IF NOT EXISTS collections (
                id INTEGER PRIMARY KEY,
                name TEXT UNIQUE NOT NULL,
                query_json TEXT NOT NULL,
                created_at INTEGER NOT NULL
            )",
            [],
        )?;

        info!(path = ?db_path, "Collection store opened");
        Ok(Self { db: Mutex::new(db) })
    }

    pub fn create(
        &self,
        name: &str,
        query: CollectionQuery,
    ) -> Result<SmartCollection, CollectionError> {
        info!(name = %name, "Creating collection");
        let query_json = serde_json::to_string(&query)?;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let db = self.db.lock();
        db.execute(
            "INSERT INTO collections (name, query_json, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![name, query_json, now],
        )?;

        let id = db.last_insert_rowid();
        debug!(id = id, name = %name, "Collection created");

        Ok(SmartCollection {
            id,
            name: name.to_string(),
            query,
            created_at: SystemTime::UNIX_EPOCH + Duration::from_secs(now as u64),
        })
    }

    pub fn list(&self) -> Result<Vec<SmartCollection>, CollectionError> {
        let db = self.db.lock();
        let mut stmt = db.prepare("SELECT id, name, query_json, created_at FROM collections")?;

        let collections = stmt.query_map([], |row| {
            let query_json: String = row.get(2)?;
            let created_secs: i64 = row.get(3)?;

            let query = match serde_json::from_str(&query_json) {
                Ok(q) => q,
                Err(e) => {
                    warn!("Failed to parse collection query JSON: {}", e);
                    CollectionQuery::Match {
                        field: "title".to_string(),
                        pattern: "*".to_string(),
                    }
                }
            };

            Ok(SmartCollection {
                id: row.get(0)?,
                name: row.get(1)?,
                query,
                created_at: SystemTime::UNIX_EPOCH + Duration::from_secs(created_secs as u64),
            })
        })?;

        collections
            .collect::<Result<Vec<_>, _>>()
            .map_err(CollectionError::from)
    }

    pub fn get(&self, name: &str) -> Result<Option<SmartCollection>, CollectionError> {
        let db = self.db.lock();
        let mut stmt =
            db.prepare("SELECT id, name, query_json, created_at FROM collections WHERE name = ?1")?;

        let result = stmt
            .query_row([name], |row| {
                let query_json: String = row.get(2)?;
                let created_secs: i64 = row.get(3)?;

                let query = match serde_json::from_str(&query_json) {
                    Ok(q) => q,
                    Err(e) => {
                        warn!("Failed to parse collection query JSON: {}", e);
                        CollectionQuery::Match {
                            field: "title".to_string(),
                            pattern: "*".to_string(),
                        }
                    }
                };

                Ok(SmartCollection {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    query,
                    created_at: SystemTime::UNIX_EPOCH + Duration::from_secs(created_secs as u64),
                })
            })
            .ok();

        Ok(result)
    }

    pub fn delete(&self, name: &str) -> Result<(), CollectionError> {
        info!(name = %name, "Deleting collection");
        let db = self.db.lock();
        db.execute("DELETE FROM collections WHERE name = ?1", [name])?;
        Ok(())
    }
}

pub fn builtin_collections() -> Vec<SmartCollection> {
    vec![
        SmartCollection {
            id: -1,
            name: "Recently Added".to_string(),
            query: CollectionQuery::RecentlyAdded { days: 30 },
            created_at: SystemTime::UNIX_EPOCH,
        },
        SmartCollection {
            id: -2,
            name: "80s Music".to_string(),
            query: CollectionQuery::DateRange {
                field: "year".to_string(),
                start: 1980,
                end: 1989,
            },
            created_at: SystemTime::UNIX_EPOCH,
        },
        SmartCollection {
            id: -3,
            name: "90s Music".to_string(),
            query: CollectionQuery::DateRange {
                field: "year".to_string(),
                start: 1990,
                end: 1999,
            },
            created_at: SystemTime::UNIX_EPOCH,
        },
    ]
}

#[derive(Debug, thiserror::Error)]
pub enum CollectionError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_collection_crud() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("collections.db");
        let store = CollectionStore::new(&db_path).unwrap();

        let collection = store
            .create("Jazz", CollectionQuery::Genre { genre: "Jazz".to_string() })
            .unwrap();

        assert_eq!(collection.name, "Jazz");

        let collections = store.list().unwrap();
        assert_eq!(collections.len(), 1);

        store.delete("Jazz").unwrap();
        let collections = store.list().unwrap();
        assert_eq!(collections.len(), 0);
    }

    #[test]
    fn test_compound_query() {
        let query = CollectionQuery::Compound {
            op: BoolOp::And,
            children: vec![
                CollectionQuery::Genre { genre: "Metal".to_string() },
                CollectionQuery::DateRange {
                    field: "year".to_string(),
                    start: 1980,
                    end: 1989,
                },
            ],
        };

        let tantivy_query = query.to_tantivy_query();
        assert!(tantivy_query.contains("genre:Metal"));
        assert!(tantivy_query.contains("year:[1980 TO 1989]"));
        assert!(tantivy_query.contains(" AND "));
    }

    #[test]
    fn test_builtin_collections() {
        let builtins = builtin_collections();
        assert_eq!(builtins.len(), 3);
        assert!(builtins.iter().any(|c| c.name == "Recently Added"));
    }

    #[test]
    fn test_dynamic_query_detection() {
        assert!(CollectionQuery::RecentlyAdded { days: 30 }.is_dynamic());
        assert!(CollectionQuery::RecentlyPlayed { days: 7 }.is_dynamic());
        assert!(CollectionQuery::MostPlayed { limit: 100 }.is_dynamic());
        assert!(!CollectionQuery::Genre { genre: "Rock".to_string() }.is_dynamic());
    }
}
