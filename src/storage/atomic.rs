use std::io::Write;
use std::path::Path;

use super::StorageError;

/// Write `bytes` to a temp file in the target's directory, fsync it, then
/// rename over `path`. A failure at any step leaves the previous file intact.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| StorageError::io(dir, e))?;

    let mut tmp = tempfile::Builder::new()
        .prefix(".agnt-")
        .suffix(".tmp")
        .tempfile_in(dir)
        .map_err(|e| StorageError::io(dir, e))?;
    tmp.write_all(bytes)
        .map_err(|e| StorageError::io(tmp.path(), e))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| StorageError::io(tmp.path(), e))?;
    tmp.persist(path).map_err(|e| StorageError::io(path, e.error))?;

    // Directory fsync makes the rename durable on POSIX; not all platforms
    // allow opening a directory for sync, so failure here is not fatal.
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_content_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("data.json");
        write_atomic(&target, b"one").unwrap();
        write_atomic(&target, b"two").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"two");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("data.json")]);
    }

    #[test]
    fn failed_write_keeps_original() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("data.json");
        write_atomic(&target, b"original").unwrap();
        // Target is a directory now -> rename must fail, original dir untouched.
        let blocked = dir.path().join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::create_dir(blocked.join("child")).unwrap();
        assert!(write_atomic(&blocked, b"x").is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        assert!(blocked.join("child").exists());
    }
}
