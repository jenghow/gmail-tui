//! Small filesystem helpers shared by the config and credential stores.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// Write `bytes` to `path` atomically.
///
/// The data lands in a sibling temporary file that is flushed to disk *before*
/// the rename, so an interrupted write can never leave a truncated config or a
/// half-written credential behind.
///
/// `private` restricts the file to its owner on Unix (`0600`). Windows has no
/// mode bits; the parent directory already inherits a per-user ACL from
/// `%APPDATA%` / `%LOCALAPPDATA%`.
pub fn atomic_write(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("could not create {}", parent.display()))?;

    let tmp = path.with_extension("tmp");
    {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        set_mode(&mut options, private);

        let mut file = options
            .open(&tmp)
            .with_context(|| format!("could not create {}", tmp.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("could not write {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("could not flush {}", tmp.display()))?;
    }

    // `fs::rename` replaces an existing destination on both Unix and Windows
    // (the latter via MoveFileEx with MOVEFILE_REPLACE_EXISTING).
    fs::rename(&tmp, path)
        .with_context(|| format!("could not move {} into place", tmp.display()))?;

    Ok(())
}

#[cfg(unix)]
fn set_mode(options: &mut OpenOptions, private: bool) {
    use std::os::unix::fs::OpenOptionsExt;
    if private {
        options.mode(0o600);
    }
}

#[cfg(not(unix))]
fn set_mode(_options: &mut OpenOptions, _private: bool) {}
