use std::path::{Path, PathBuf};

use directories::ProjectDirs;

/// Logical file layout from the design doc (section 8). The base directory
/// follows the OS convention via `directories`; `AGNT_DATA_DIR` overrides it
/// (used by tests and for demos).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub base: PathBuf,
}

impl Paths {
    pub const ENV_OVERRIDE: &'static str = "AGNT_DATA_DIR";

    pub fn resolve() -> Option<Self> {
        if let Some(dir) = std::env::var_os(Self::ENV_OVERRIDE).filter(|v| !v.is_empty()) {
            return Some(Self::at(dir));
        }
        ProjectDirs::from("", "", "agnt").map(|d| Self::at(d.data_dir()))
    }

    pub fn at(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    pub fn config_toml(&self) -> PathBuf {
        self.base.join("config.toml")
    }

    pub fn metadata_json(&self) -> PathBuf {
        self.base.join("metadata.json")
    }

    /// Locked instead of `metadata.json` itself, because rename replaces the
    /// data file and would drop a lock held on the old inode.
    pub fn metadata_lock(&self) -> PathBuf {
        self.base.join("metadata.lock")
    }

    pub fn sessions_cache_json(&self) -> PathBuf {
        self.base.join("sessions-cache.json")
    }

    pub fn ensure_base(&self) -> std::io::Result<&Path> {
        std::fs::create_dir_all(&self.base)?;
        Ok(&self.base)
    }
}
