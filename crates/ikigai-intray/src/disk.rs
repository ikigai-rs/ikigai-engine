//! The intray's disk: every filesystem operation the library makes, in one place.
//!
//! The intray is a PHYSICAL tuplespace. A space is a directory under the root the host bound
//! ([`crate::space`], [`crate::SpaceReactor::new`]); `out` writes a staging file and renames
//! it into the inbox, `take` and the reactor claim a tuple by renaming it, and notes, leases,
//! dead letters and each space's `cap` and `handler` files are files beside it. So the disk
//! is the crate's job, and Hypothesis H (ledger #2384) names that reach as ONE exception: this
//! module. The rest of the library reaches the disk only through these functions, so a
//! direct `std::fs` call anywhere else is still refused by the ambient gate.
//!
//! What this module does NOT do is confine a path: every path it is handed is built by the
//! caller from that host-bound root. The functions are thin on purpose, each the `std`
//! operation of the same name, so the rename and claim protocols stay readable where they
//! are written.
#![allow(clippy::disallowed_methods)] // ambient: fs: the intray is a physical tuplespace, a directory per space under the root the host bound; these are its only disk operations (ledger #2418)

use std::fs::{File, ReadDir};
use std::io::Result;
use std::path::{Path, PathBuf};

/// [`std::fs::create_dir_all`].
pub(crate) fn create_dir_all(dir: impl AsRef<Path>) -> Result<()> {
    std::fs::create_dir_all(dir)
}

/// [`std::fs::read`].
pub(crate) fn read(path: impl AsRef<Path>) -> Result<Vec<u8>> {
    std::fs::read(path)
}

/// [`std::fs::read_to_string`].
pub(crate) fn read_to_string(path: impl AsRef<Path>) -> Result<String> {
    std::fs::read_to_string(path)
}

/// [`std::fs::read_dir`].
pub(crate) fn read_dir(dir: impl AsRef<Path>) -> Result<ReadDir> {
    std::fs::read_dir(dir)
}

/// [`std::fs::rename`]: the intray's compare-and-swap (a claim, a publish, a requeue).
pub(crate) fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> Result<()> {
    std::fs::rename(from, to)
}

/// [`std::fs::remove_file`].
pub(crate) fn remove_file(path: impl AsRef<Path>) -> Result<()> {
    std::fs::remove_file(path)
}

/// [`std::fs::hard_link`].
pub(crate) fn hard_link(original: impl AsRef<Path>, link: impl AsRef<Path>) -> Result<()> {
    std::fs::hard_link(original, link)
}

/// [`std::fs::metadata`].
pub(crate) fn metadata(path: impl AsRef<Path>) -> Result<std::fs::Metadata> {
    std::fs::metadata(path)
}

/// [`Path::is_dir`].
pub(crate) fn is_dir(path: impl AsRef<Path>) -> bool {
    path.as_ref().is_dir()
}

/// [`Path::is_file`].
pub(crate) fn is_file(path: impl AsRef<Path>) -> bool {
    path.as_ref().is_file()
}

/// [`Path::exists`].
pub(crate) fn exists(path: impl AsRef<Path>) -> bool {
    path.as_ref().exists()
}

/// [`Path::canonicalize`].
pub(crate) fn canonicalize(path: impl AsRef<Path>) -> Result<PathBuf> {
    path.as_ref().canonicalize()
}

/// Open (creating, never truncating) the reactor lease file, for its lock.
pub(crate) fn open_lease(path: impl AsRef<Path>) -> Result<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
}

/// The permissions a space gives every file and directory it creates
/// ([`crate::SpaceEndpoint::with_modes`]). `None`, the default, is the platform's own (the
/// process umask on unix), exactly as before the setting existed.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Modes(pub(crate) Option<(u32, u32)>);

impl Modes {
    /// `std::fs::create_dir_all`, giving each directory it CREATES the chosen mode. One that
    /// already exists is left as it is: the space did not make it.
    pub(crate) fn create_dir_all(&self, dir: &Path) -> Result<()> {
        let Some((_, mode)) = self.0 else {
            return std::fs::create_dir_all(dir);
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut missing = Vec::new();
            let mut at = Some(dir);
            while let Some(d) = at.filter(|d| !d.as_os_str().is_empty() && !d.is_dir()) {
                missing.push(d);
                at = d.parent();
            }
            for d in missing.into_iter().rev() {
                // Created with the mode, so the umask can only NARROW it, and then set to
                // it exactly: the directory is never wider than asked, even for a moment.
                match std::fs::DirBuilder::new().mode(mode).create(d) {
                    Ok(()) => set_mode(d, mode)?,
                    // A rival creator made it first; it sets the mode itself.
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && d.is_dir() => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = mode;
            std::fs::create_dir_all(dir)
        }
    }

    /// `std::fs::write` of a file the space creates (always under a fresh staging name), with
    /// the chosen mode.
    pub(crate) fn write(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let Some((mode, _)) = self.0 else {
            return std::fs::write(path, bytes);
        };
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            // As for a directory: created with the mode (never wider), then set exactly.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(mode)
                .open(path)?;
            set_mode(path, mode)?;
            file.write_all(bytes)
        }
        #[cfg(not(unix))]
        {
            let _ = mode;
            std::fs::write(path, bytes)
        }
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}
