//! Persistence: user metadata (source of truth), a regenerable sessions
//! cache, and the lock/atomic-write discipline both use.

pub mod atomic;
pub mod cache;
pub mod metadata;
pub mod paths;

pub use cache::{CacheFile, CacheStore};
pub use metadata::{MetadataFile, MetadataStore, SCHEMA_VERSION};
pub use paths::Paths;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file exists but is not valid JSON for this schema. The original is
    /// left untouched; metadata editing is disabled until the user fixes it.
    #[error("{path} is corrupt: {source}")]
    Corrupt {
        path: std::path::PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{path} has schema_version {found}; this agnt supports {supported}")]
    UnsupportedSchema {
        path: std::path::PathBuf,
        found: u32,
        supported: u32,
    },
    #[error("could not acquire {path} within {waited_ms} ms")]
    LockTimeout {
        path: std::path::PathBuf,
        waited_ms: u64,
    },
}

impl StorageError {
    pub fn io(path: impl Into<std::path::PathBuf>, source: std::io::Error) -> Self {
        StorageError::Io {
            path: path.into(),
            source,
        }
    }
}
