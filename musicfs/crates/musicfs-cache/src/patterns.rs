use musicfs_core::FileId;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct AccessPattern {
    pub file_id: FileId,
    pub timestamp: SystemTime,
    pub context: AccessContext,
    pub hour_of_day: u8,
}

#[derive(Debug, Clone, Default)]
pub struct AccessContext {
    pub album_id: Option<i64>,
    pub track_number: Option<u32>,
    pub artist: Option<String>,
}

pub struct PatternStore {
    db: Mutex<rusqlite::Connection>,
    sequence_counts: RwLock<HashMap<(FileId, FileId), u32>>,
    time_patterns: RwLock<HashMap<u8, Vec<FileId>>>,
    max_history: usize,
}

impl PatternStore {
    pub fn new(db_path: &Path, max_history: usize) -> Result<Self, PatternError> {
        let db = rusqlite::Connection::open(db_path)?;

        db.execute(
            "CREATE TABLE IF NOT EXISTS access_log (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL,
                access_time INTEGER NOT NULL,
                hour_of_day INTEGER NOT NULL
            )",
            [],
        )?;

        db.execute(
            "CREATE INDEX IF NOT EXISTS idx_access_log_file ON access_log(file_id)",
            [],
        )?;

        db.execute(
            "CREATE INDEX IF NOT EXISTS idx_access_log_time ON access_log(access_time)",
            [],
        )?;

        db.execute(
            "CREATE TABLE IF NOT EXISTS sequence_counts (
                from_file_id INTEGER NOT NULL,
                to_file_id INTEGER NOT NULL,
                count INTEGER NOT NULL DEFAULT 1,
                PRIMARY KEY (from_file_id, to_file_id)
            )",
            [],
        )?;

        let sequence_counts = {
            let mut map = HashMap::new();
            let mut stmt = db.prepare("SELECT from_file_id, to_file_id, count FROM sequence_counts")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    (
                        FileId(row.get::<_, i64>(0)?),
                        FileId(row.get::<_, i64>(1)?),
                    ),
                    row.get::<_, u32>(2)?,
                ))
            })?;
            for row in rows {
                let (key, count) = row?;
                map.insert(key, count);
            }
            map
        };

        Ok(Self {
            db: Mutex::new(db),
            sequence_counts: RwLock::new(sequence_counts),
            time_patterns: RwLock::new(HashMap::new()),
            max_history,
        })
    }

    pub fn record(&self, file_id: FileId, _context: AccessContext) -> Result<(), PatternError> {
        let now = SystemTime::now();
        let timestamp = now.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let hour = (timestamp / 3600 % 24) as u8;

        let db = self.db.lock();

        db.execute(
            "INSERT INTO access_log (file_id, access_time, hour_of_day) VALUES (?1, ?2, ?3)",
            rusqlite::params![file_id.0, timestamp, hour],
        )?;

        {
            let mut time_patterns = self.time_patterns.write();
            time_patterns.entry(hour).or_default().push(file_id);
        }

        let prev_file_id: Option<i64> = db
            .query_row(
                "SELECT file_id FROM access_log WHERE id = (SELECT MAX(id) - 1 FROM access_log)",
                [],
                |row| row.get(0),
            )
            .ok();

        if let Some(prev_id) = prev_file_id {
            let prev = FileId(prev_id);

            {
                let mut sequences = self.sequence_counts.write();
                *sequences.entry((prev, file_id)).or_insert(0) += 1;
            }

            db.execute(
                "INSERT INTO sequence_counts (from_file_id, to_file_id, count) 
                 VALUES (?1, ?2, 1)
                 ON CONFLICT(from_file_id, to_file_id) DO UPDATE SET count = count + 1",
                rusqlite::params![prev_id, file_id.0],
            )?;
        }

        let cutoff = timestamp - (self.max_history as i64 * 86400);
        db.execute("DELETE FROM access_log WHERE access_time < ?1", [cutoff])?;

        Ok(())
    }

    pub fn predict_next(&self, current: FileId, limit: usize) -> Vec<FileId> {
        let sequences = self.sequence_counts.read();

        let mut predictions: Vec<_> = sequences
            .iter()
            .filter(|((from, _), count)| *from == current && **count >= 2)
            .map(|((_, to), count)| (*to, *count))
            .collect();

        predictions.sort_by(|a, b| b.1.cmp(&a.1));
        predictions
            .into_iter()
            .take(limit)
            .map(|(id, _)| id)
            .collect()
    }

    pub fn predict_for_time(&self, hour: u8, limit: usize) -> Vec<FileId> {
        let time_patterns = self.time_patterns.read();

        time_patterns
            .get(&hour)
            .map(|files| files.iter().rev().take(limit).copied().collect())
            .unwrap_or_default()
    }

    pub fn recently_played(&self, days: u32) -> Result<Vec<FileId>, PatternError> {
        let cutoff = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - (days as i64 * 86400);

        let db = self.db.lock();
        let mut stmt = db.prepare(
            "SELECT DISTINCT file_id FROM access_log WHERE access_time >= ?1 ORDER BY access_time DESC",
        )?;

        let files: Vec<FileId> = stmt
            .query_map([cutoff], |row| Ok(FileId(row.get(0)?)))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(files)
    }

    pub fn most_played(&self, limit: u32) -> Result<Vec<FileId>, PatternError> {
        let db = self.db.lock();
        let mut stmt = db.prepare(
            "SELECT file_id, COUNT(*) as play_count FROM access_log 
             GROUP BY file_id ORDER BY play_count DESC LIMIT ?1",
        )?;

        let files: Vec<FileId> = stmt
            .query_map([limit], |row| Ok(FileId(row.get(0)?)))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(files)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PatternError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_pattern_prediction() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("patterns.db");
        let store = PatternStore::new(&db_path, 30).unwrap();
        let ctx = AccessContext::default();

        for _ in 0..5 {
            store.record(FileId(1), ctx.clone()).unwrap();
            store.record(FileId(2), ctx.clone()).unwrap();
            store.record(FileId(3), ctx.clone()).unwrap();
        }

        let predictions = store.predict_next(FileId(1), 3);
        assert!(!predictions.is_empty());
        assert_eq!(predictions[0], FileId(2));
    }

    #[test]
    fn test_pattern_persistence() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("patterns.db");
        let ctx = AccessContext::default();

        {
            let store = PatternStore::new(&db_path, 30).unwrap();
            for _ in 0..3 {
                store.record(FileId(1), ctx.clone()).unwrap();
                store.record(FileId(2), ctx.clone()).unwrap();
            }
        }

        {
            let store = PatternStore::new(&db_path, 30).unwrap();
            let predictions = store.predict_next(FileId(1), 3);
            assert!(!predictions.is_empty());
            assert_eq!(predictions[0], FileId(2));
        }
    }

    #[test]
    fn test_recently_played() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("patterns.db");
        let store = PatternStore::new(&db_path, 30).unwrap();
        let ctx = AccessContext::default();

        store.record(FileId(100), ctx.clone()).unwrap();
        store.record(FileId(200), ctx.clone()).unwrap();

        let recent = store.recently_played(7).unwrap();
        assert!(recent.contains(&FileId(100)));
        assert!(recent.contains(&FileId(200)));
    }

    #[test]
    fn test_most_played() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("patterns.db");
        let store = PatternStore::new(&db_path, 30).unwrap();
        let ctx = AccessContext::default();

        for _ in 0..5 {
            store.record(FileId(1), ctx.clone()).unwrap();
        }
        for _ in 0..2 {
            store.record(FileId(2), ctx.clone()).unwrap();
        }

        let most = store.most_played(10).unwrap();
        assert_eq!(most[0], FileId(1));
    }
}
