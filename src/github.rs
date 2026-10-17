//! GitHub enrichment through an authenticated `gh` (design doc §9).
//!
//! Only version detection exists so far; branch-to-PR candidate lookup is
//! P2 work. When it lands it must stay read-only, treat results as inferred
//! candidates (never overwriting manual links), and refuse to pick one PR
//! when several match the same branch.

use std::path::PathBuf;

use crate::providers::AdapterError;
use crate::providers::process::{find_in_path, version_of};

pub const BINARY: &str = "gh";

pub fn gh_binary() -> Option<PathBuf> {
    find_in_path(BINARY)
}

pub async fn gh_version() -> Result<String, AdapterError> {
    version_of(gh_binary().as_deref(), BINARY).await
}
