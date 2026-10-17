use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::atomic::write_atomic;
use super::{Paths, StorageError};
use crate::model::{SessionKey, SessionMeta};

pub const SCHEMA_VERSION: u32 = 1;

/// `metadata.json`: the only data agnt owns. Conversation content never
/// lives here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataFile {
    pub schema_version: u32,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub sessions: Vec<SessionMeta>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for MetadataFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            sessions: Vec::new(),
            extra: BTreeMap::new(),
        }
    }
}

impl MetadataFile {
    pub fn get(&self, key: &SessionKey) -> Option<&SessionMeta> {
        self.sessions.iter().find(|m| &m.key == key)
    }

    /// Entry for `key`, created as default when absent.
    pub fn entry(&mut self, key: &SessionKey) -> &mut SessionMeta {
        if let Some(pos) = self.sessions.iter().position(|m| &m.key == key) {
            &mut self.sessions[pos]
        } else {
            self.sessions.push(SessionMeta::new(key.clone()));
            self.sessions.last_mut().expect("just pushed")
        }
    }

    /// Drop everything agnt recorded about `key`.
    pub fn remove(&mut self, key: &SessionKey) {
        self.sessions.retain(|m| &m.key != key);
    }

    pub fn by_key(&self) -> BTreeMap<SessionKey, SessionMeta> {
        self.sessions
            .iter()
            .cloned()
            .map(|m| (m.key.clone(), m))
            .collect()
    }
}

/// Reads and updates `metadata.json` with the discipline from the design doc:
/// take the lock file, reload, apply, write a temp file, fsync, rename, unlock.
#[derive(Debug, Clone)]
pub struct MetadataStore {
    data_path: PathBuf,
    lock_path: PathBuf,
    lock_timeout: Duration,
}

impl MetadataStore {
    pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

    pub fn new(paths: &Paths) -> Self {
        Self {
            data_path: paths.metadata_json(),
            lock_path: paths.metadata_lock(),
            lock_timeout: Self::DEFAULT_LOCK_TIMEOUT,
        }
    }

    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    pub fn path(&self) -> &PathBuf {
        &self.data_path
    }

    /// Read without locking. A missing file is an empty metadata set; a
    /// corrupt or newer-schema file is an error and is never overwritten.
    pub fn load(&self) -> Result<MetadataFile, StorageError> {
        let bytes = match std::fs::read(&self.data_path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(MetadataFile::default());
            }
            Err(e) => return Err(StorageError::io(&self.data_path, e)),
        };
        let file: MetadataFile = serde_json::from_slice(&bytes).map_err(|e| StorageError::Corrupt {
            path: self.data_path.clone(),
            source: e,
        })?;
        if file.schema_version > SCHEMA_VERSION {
            return Err(StorageError::UnsupportedSchema {
                path: self.data_path.clone(),
                found: file.schema_version,
                supported: SCHEMA_VERSION,
            });
        }
        Ok(file)
    }

    /// Apply `f` under the exclusive lock, on a freshly reloaded copy.
    /// Do not hold network waits inside `f`.
    pub fn update<T>(&self, f: impl FnOnce(&mut MetadataFile) -> T) -> Result<T, StorageError> {
        let dir = self
            .data_path
            .parent()
            .ok_or_else(|| StorageError::io(&self.data_path, std::io::Error::other("no parent dir")))?;
        std::fs::create_dir_all(dir).map_err(|e| StorageError::io(dir, e))?;

        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.lock_path)
            .map_err(|e| StorageError::io(&self.lock_path, e))?;
        let mut lock = fd_lock::RwLock::new(lock_file);

        // The guard stays inside this loop body: reload, apply, write, unlock.
        let start = Instant::now();
        loop {
            match lock.try_write() {
                Ok(_guard) => {
                    let mut file = self.load()?;
                    let out = f(&mut file);
                    file.revision += 1;
                    let bytes = serde_json::to_vec_pretty(&file).expect("metadata serializes");
                    write_atomic(&self.data_path, &bytes)?;
                    return Ok(out);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(StorageError::io(&self.lock_path, e)),
            }
            if start.elapsed() >= self.lock_timeout {
                return Err(StorageError::LockTimeout {
                    path: self.lock_path.clone(),
                    waited_ms: self.lock_timeout.as_millis() as u64,
                });
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Link, LinkSource, Provider, WorkflowState};

    fn store(dir: &tempfile::TempDir) -> MetadataStore {
        MetadataStore::new(&Paths::at(dir.path()))
    }

    #[test]
    fn missing_file_is_empty_and_first_update_creates_it() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        assert_eq!(s.load().unwrap(), MetadataFile::default());
        let key = SessionKey::new(Provider::Codex, "s1");
        s.update(|m| m.entry(&key).workflow = WorkflowState::Review)
            .unwrap();
        let loaded = s.load().unwrap();
        assert_eq!(loaded.revision, 1);
        assert_eq!(loaded.get(&key).unwrap().workflow, WorkflowState::Review);
    }

    #[test]
    fn corrupt_file_is_reported_and_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        std::fs::write(s.path(), b"{ not json").unwrap();
        assert!(matches!(s.load(), Err(StorageError::Corrupt { .. })));
        let key = SessionKey::new(Provider::Codex, "s1");
        let err = s.update(|m| m.entry(&key).workflow = WorkflowState::Done);
        assert!(matches!(err, Err(StorageError::Corrupt { .. })));
        assert_eq!(std::fs::read(s.path()).unwrap(), b"{ not json");
    }

    #[test]
    fn newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        std::fs::write(s.path(), br#"{"schema_version": 99, "sessions": []}"#).unwrap();
        assert!(matches!(
            s.load(),
            Err(StorageError::UnsupportedSchema { found: 99, .. })
        ));
    }

    #[test]
    fn unknown_top_level_fields_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(&dir);
        std::fs::write(
            s.path(),
            br#"{"schema_version": 1, "revision": 3, "sessions": [], "notes": "keep me"}"#,
        )
        .unwrap();
        s.update(|_| ()).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(s.path()).unwrap()).unwrap();
        assert_eq!(v["notes"], "keep me");
        assert_eq!(v["revision"], 4);
    }

    /// Two writers with independent file descriptors (as two processes would
    /// have) must not lose each other's updates.
    #[test]
    fn concurrent_updates_are_not_lost() {
        let dir = tempfile::tempdir().unwrap();
        let a = store(&dir).with_lock_timeout(Duration::from_secs(30));
        let b = a.clone();
        let key = SessionKey::new(Provider::Claude, "shared");
        const N: u64 = 40;

        let ta = std::thread::spawn(move || {
            for i in 0..N {
                a.update(|m| {
                    m.entry(&key).upsert_link(
                        Link::from_url(&format!("https://github.com/o/r/pull/{i}"), LinkSource::Manual)
                            .unwrap(),
                    )
                })
                .unwrap();
            }
        });
        let key_b = SessionKey::new(Provider::Claude, "shared");
        let tb = std::thread::spawn(move || {
            for i in 0..N {
                b.update(|m| {
                    m.entry(&key_b).workflow = if i % 2 == 0 {
                        WorkflowState::Review
                    } else {
                        WorkflowState::Done
                    }
                })
                .unwrap();
            }
        });
        ta.join().unwrap();
        tb.join().unwrap();

        let s = store(&dir);
        let file = s.load().unwrap();
        assert_eq!(file.revision, 2 * N);
        let meta = file.get(&SessionKey::new(Provider::Claude, "shared")).unwrap();
        assert_eq!(meta.links.len(), N as usize);
        assert_eq!(meta.workflow, WorkflowState::Done);
    }
}
