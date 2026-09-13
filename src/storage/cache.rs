use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::atomic::write_atomic;
use super::{Paths, StorageError};
use crate::model::Session;

/// `sessions-cache.json`: last observed list, capped, no conversation text.
/// Regenerable, so a corrupt cache is discarded rather than diagnosed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheFile {
    pub schema_version: u32,
    pub saved_at: DateTime<Utc>,
    #[serde(default)]
    pub sessions: Vec<Session>,
}

#[derive(Debug, Clone)]
pub struct CacheStore {
    path: PathBuf,
    max_rows: usize,
}

impl CacheStore {
    pub const SCHEMA_VERSION: u32 = 1;
    pub const DEFAULT_MAX_ROWS: usize = 1000;

    pub fn new(paths: &Paths) -> Self {
        Self {
            path: paths.sessions_cache_json(),
            max_rows: Self::DEFAULT_MAX_ROWS,
        }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// `None` when there is no usable cache (missing, unreadable, corrupt,
    /// or a different schema). The caller shows "previous observation" rows
    /// and never treats a cached state as confirmed.
    pub fn load(&self) -> Option<CacheFile> {
        let bytes = std::fs::read(&self.path).ok()?;
        let file: CacheFile = serde_json::from_slice(&bytes).ok()?;
        (file.schema_version == Self::SCHEMA_VERSION).then_some(file)
    }

    /// Save at most `max_rows` sessions, most recent activity first.
    pub fn save(&self, sessions: &[Session]) -> Result<(), StorageError> {
        let mut rows: Vec<Session> = sessions.to_vec();
        rows.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
        rows.truncate(self.max_rows);
        let file = CacheFile {
            schema_version: Self::SCHEMA_VERSION,
            saved_at: Utc::now(),
            sessions: rows,
        };
        let bytes = serde_json::to_vec_pretty(&file).expect("cache serializes");
        write_atomic(&self.path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Provider, RuntimeState, SessionKey};

    #[test]
    fn corrupt_cache_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let c = CacheStore::new(&Paths::at(dir.path()));
        assert!(c.load().is_none());
        std::fs::write(c.path(), b"garbage").unwrap();
        assert!(c.load().is_none());
    }

    #[test]
    fn round_trip_keeps_unknown_state_values_as_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let c = CacheStore::new(&Paths::at(dir.path()));
        let s = Session::new(SessionKey::new(Provider::Claude, "a"), "/tmp");
        c.save(std::slice::from_ref(&s)).unwrap();
        let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(c.path()).unwrap()).unwrap();
        v["sessions"][0]["runtime"]["state"] = "hibernating".into();
        std::fs::write(c.path(), serde_json::to_vec(&v).unwrap()).unwrap();
        let loaded = c.load().unwrap();
        assert_eq!(loaded.sessions[0].runtime.state, RuntimeState::Unknown);
        assert_eq!(loaded.sessions[0].key, s.key);
    }
}
