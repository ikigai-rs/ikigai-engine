//! The intray's disk: every filesystem operation the library makes, CONFINED to the root the
//! host bound (ledger #2875).
//!
//! The intray is a PHYSICAL tuplespace. A space is a directory under the root the host bound
//! ([`crate::space`], [`crate::SpaceReactor::new`]); `out` writes a staging file and renames
//! it into the inbox, `take` and the reactor claim a tuple by renaming it, and notes, leases,
//! dead letters and each space's `cap` and `handler` files are files beside it. So the disk
//! is the crate's job, and Hypothesis H (ledger #2384) names that reach as ONE exception: the
//! `impl` of [`Root`] below. The rest of the library reaches the disk only through it, so a
//! direct `std::fs` call anywhere else is still refused by the ambient gate.
//!
//! **The confinement.** A [`Root`] holds the root's path, privately, and every operation takes
//! a [`Rel`]: a list of names below it. There is no way to hand it an absolute path, so the
//! library can name nothing outside the root, even by a bug in claim, publish or recovery:
//! - each name of a `Rel` must be exactly ONE plain path segment, judged when it is used, so
//!   `..`, `.`, an empty name, a name with a separator in it, an absolute path and (on Windows)
//!   a drive prefix are all refused before the disk is touched;
//! - no name below the root may be a SYMBOLIC LINK, wherever it points: every directory a path
//!   passes through is checked with `lstat`, and so is the file itself unless the operation
//!   acts on the name and never on what it points at (a rename, a removal, a new link's name).
//!   A link that is moved this way is moved as a link, and is never read through.
//!
//! The root itself may be a link (macOS's `/tmp` is one): the host bound it, and only what lies
//! below it is judged.
//!
//! ⚠ What this does NOT close: the check and the operation are two calls, so a link swapped
//! into the tree between them is a race this does not win. That needs every operation made
//! relative to an open directory handle (`openat` and friends), which std does not offer and
//! which needs a crate (`cap-std`, `rustix`) this crate does not carry. The same guarantee
//! ikigai-browse gives its checkouts (`src/checkout.rs`).

use std::ffi::{OsStr, OsString};
use std::fs::{File, FileType};
use std::io::{Error, ErrorKind, Result};
use std::path::{Component, Path, PathBuf};

/// A path below a [`Root`], as the names it passes through. Built only by [`Rel::top`] and
/// [`Rel::join`], and judged by the root each time it is used.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Rel(Vec<OsString>);

impl Rel {
    /// The root itself.
    pub(crate) fn top() -> Rel {
        Rel(Vec::new())
    }

    /// This path with one more name below it. Not judged here: a name that is not one plain
    /// segment makes every operation on the result refuse.
    pub(crate) fn join(&self, name: impl AsRef<OsStr>) -> Rel {
        let mut names = self.0.clone();
        names.push(name.as_ref().to_os_string());
        Rel(names)
    }

    /// The last name, `None` for the root.
    pub(crate) fn last(&self) -> Option<&OsStr> {
        self.0.last().map(OsString::as_os_str)
    }
}

/// One entry of a directory [`Root::list`] read: its name and its own type (a link is reported
/// as a link, never as what it points at).
pub(crate) struct Listed {
    name: OsString,
    kind: Option<FileType>,
}

impl Listed {
    /// The entry's name, when it is UTF-8.
    pub(crate) fn name(&self) -> Option<&str> {
        self.name.to_str()
    }

    /// The entry's name as the directory holds it.
    pub(crate) fn os_name(&self) -> &OsStr {
        &self.name
    }

    /// A directory, not a link to one.
    pub(crate) fn is_dir(&self) -> bool {
        self.kind.is_some_and(|k| k.is_dir())
    }

    /// A regular file, not a link to one.
    pub(crate) fn is_file(&self) -> bool {
        self.kind.is_some_and(|k| k.is_file())
    }

    /// A symbolic link.
    pub(crate) fn is_link(&self) -> bool {
        self.kind.is_some_and(|k| k.is_symlink())
    }
}

/// The permissions a space gives every file and directory it creates
/// ([`crate::SpaceEndpoint::with_modes`]): `(file, dir)`. `None`, the default, is the
/// platform's own (the process umask on unix), exactly as before the setting existed.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Modes(pub(crate) Option<(u32, u32)>);

/// Why a [`Root`] refused a path, carried inside the `io::Error` so a caller can tell a refusal
/// from the filesystem's own failures ([`is_refused`]).
#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

fn refused(why: String) -> Error {
    Error::new(ErrorKind::InvalidInput, Refused(why))
}

/// Whether `e` is a [`Root`] refusing a path (a name that is not one segment, or a symbolic
/// link below the root), rather than the filesystem failing an operation.
pub(crate) fn is_refused(e: &Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<Refused>())
}

/// Whether an operation follows a path's last name or acts on it as a name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Last {
    /// Followed: a read, an open, a stat, a directory created or listed. A link there is
    /// refused like one a path passes through.
    Followed,
    /// Acted on as a name: a rename, a removal, a new link's name. A link there is moved or
    /// removed as a link, never through.
    Named,
}

/// The directory the intray works in, and the only door to it.
#[derive(Debug, Clone)]
pub(crate) struct Root {
    /// As the host bound it; never handed out to be opened (see [`Root::shown`]).
    path: PathBuf,
    /// The permissions of what this root's owner creates (the endpoint's; a reactor's are the
    /// default).
    modes: Modes,
}

impl Root {
    pub(crate) fn new(path: PathBuf) -> Root {
        Root {
            path,
            modes: Modes::default(),
        }
    }

    /// This root, creating what it creates with `modes`.
    pub(crate) fn with_modes(self, modes: Modes) -> Root {
        Root { modes, ..self }
    }

    /// The root as the host bound it, for messages and for matching the paths a watcher
    /// reports. Reaching the disk through it is the violation this module exists to prevent.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Where `rel` is, for a message (and this crate's test seam). Not judged: a message may
    /// name a path the root refused.
    pub(crate) fn shown(&self, rel: &Rel) -> PathBuf {
        rel.0.iter().fold(self.path.clone(), |p, name| p.join(name))
    }

    /// Judge `rel` and answer the path it names: every name one plain segment, and none of the
    /// existing names below the root a symbolic link (the last one too, when the operation
    /// follows it). A name that does not exist ends the check: nothing below it exists to be
    /// followed, and the operation reports the absence itself.
    fn resolve(&self, rel: &Rel, last: Last) -> Result<PathBuf> {
        for name in &rel.0 {
            let mut parts = Path::new(name).components();
            let one = matches!(
                (parts.next(), parts.next()),
                (Some(Component::Normal(only)), None) if only == name.as_os_str()
            );
            if !one {
                return Err(refused(format!(
                    "`{}` is not a single name below the intray's root",
                    name.to_string_lossy()
                )));
            }
        }
        let mut path = self.path.clone();
        let mut checking = true;
        for (i, name) in rel.0.iter().enumerate() {
            path.push(name);
            let is_last = i + 1 == rel.0.len();
            if !checking || (is_last && last == Last::Named) {
                continue;
            }
            match self.lstat(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(refused(format!(
                        "`{}` is a symbolic link, and the intray follows no link below its root",
                        self.shown(&Rel(rel.0[..=i].to_vec())).display()
                    )));
                }
                Ok(_) => {}
                Err(_) => checking = false,
            }
        }
        Ok(path)
    }
}

#[allow(clippy::disallowed_methods)] // ambient: fs: the intray is a physical tuplespace, a directory per space under the root the host bound; every operation here takes a Rel judged against that root, so none reaches outside it (ledger #2418, #2875)
impl Root {
    fn lstat(&self, path: &Path) -> Result<std::fs::Metadata> {
        std::fs::symlink_metadata(path)
    }

    /// [`std::fs::create_dir_all`] of the root and `rel` below it, giving each directory it
    /// CREATES this root's directory mode. One that already exists is left as it is: the
    /// space did not make it.
    pub(crate) fn create_dir_all(&self, rel: &Rel) -> Result<()> {
        let dir = self.resolve(rel, Last::Followed)?;
        let Some((_, mode)) = self.modes.0 else {
            return std::fs::create_dir_all(dir);
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut missing = Vec::new();
            let mut at = Some(dir.as_path());
            while let Some(d) = at.filter(|d| !d.as_os_str().is_empty() && !d.is_dir()) {
                missing.push(d);
                at = d.parent();
            }
            for d in missing.into_iter().rev() {
                // Created with the mode, so the umask can only NARROW it, and then set to
                // it exactly: the directory is never wider than asked, even for a moment.
                match std::fs::DirBuilder::new().mode(mode).create(d) {
                    Ok(()) => Self::set_mode(d, mode)?,
                    // A rival creator made it first; it sets the mode itself.
                    Err(e) if e.kind() == ErrorKind::AlreadyExists && d.is_dir() => {}
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
    /// this root's file mode.
    pub(crate) fn write(&self, rel: &Rel, bytes: &[u8]) -> Result<()> {
        let path = self.resolve(rel, Last::Followed)?;
        let Some((mode, _)) = self.modes.0 else {
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
                .open(&path)?;
            Self::set_mode(&path, mode)?;
            file.write_all(bytes)
        }
        #[cfg(not(unix))]
        {
            let _ = mode;
            std::fs::write(path, bytes)
        }
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }

    /// [`std::fs::read`].
    pub(crate) fn read(&self, rel: &Rel) -> Result<Vec<u8>> {
        std::fs::read(self.resolve(rel, Last::Followed)?)
    }

    /// [`std::fs::read_to_string`].
    pub(crate) fn read_to_string(&self, rel: &Rel) -> Result<String> {
        std::fs::read_to_string(self.resolve(rel, Last::Followed)?)
    }

    /// The entries of the directory `rel`, each with its own type (a link is a link).
    pub(crate) fn list(&self, rel: &Rel) -> Result<impl Iterator<Item = Result<Listed>>> {
        let entries = std::fs::read_dir(self.resolve(rel, Last::Followed)?)?;
        Ok(entries.map(|entry| {
            entry.map(|entry| Listed {
                name: entry.file_name(),
                kind: entry.file_type().ok(),
            })
        }))
    }

    /// [`std::fs::rename`]: the intray's compare-and-swap (a claim, a publish, a requeue). A
    /// link is moved as a link.
    pub(crate) fn rename(&self, from: &Rel, to: &Rel) -> Result<()> {
        std::fs::rename(
            self.resolve(from, Last::Named)?,
            self.resolve(to, Last::Named)?,
        )
    }

    /// [`std::fs::remove_file`]. A link is removed as a link.
    pub(crate) fn remove_file(&self, rel: &Rel) -> Result<()> {
        std::fs::remove_file(self.resolve(rel, Last::Named)?)
    }

    /// [`std::fs::hard_link`]: `link` becomes a second name of `original`, which may not be a
    /// link itself.
    pub(crate) fn hard_link(&self, original: &Rel, link: &Rel) -> Result<()> {
        std::fs::hard_link(
            self.resolve(original, Last::Followed)?,
            self.resolve(link, Last::Named)?,
        )
    }

    /// [`std::fs::metadata`].
    pub(crate) fn metadata(&self, rel: &Rel) -> Result<std::fs::Metadata> {
        std::fs::metadata(self.resolve(rel, Last::Followed)?)
    }

    /// When the entry `rel` itself was last modified: a link's own time, not its target's.
    pub(crate) fn modified(&self, rel: &Rel) -> Result<std::time::SystemTime> {
        self.lstat(&self.resolve(rel, Last::Named)?)?.modified()
    }

    /// [`Path::is_file`]; false when refused.
    pub(crate) fn is_file(&self, rel: &Rel) -> bool {
        self.resolve(rel, Last::Followed)
            .is_ok_and(|path| path.is_file())
    }

    /// [`Path::exists`]; false when refused.
    pub(crate) fn exists(&self, rel: &Rel) -> bool {
        self.resolve(rel, Last::Followed)
            .is_ok_and(|path| path.exists())
    }

    /// The root, canonical: what a filesystem watcher reports paths under.
    pub(crate) fn canonical(&self) -> Result<PathBuf> {
        self.path.canonicalize()
    }

    /// Open (creating, never truncating) the reactor lease file, for its lock.
    pub(crate) fn open_lease(&self, rel: &Rel) -> Result<File> {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.resolve(rel, Last::Followed)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("ikigai-intray-disk")
            .join(format!("{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("root").join("jobs")).unwrap();
        std::fs::write(dir.join("secret"), b"outside").unwrap();
        dir
    }

    #[test]
    fn a_name_that_is_not_one_segment_is_refused_before_the_disk_is_touched() {
        let base = scratch("lexical");
        let root = Root::new(base.join("root"));
        let secret = base.join("secret");
        for bad in [
            "..",
            ".",
            "",
            "jobs/..",
            "a/b",
            "jobs/",
            "./jobs",
            secret.to_str().unwrap(),
        ] {
            let rel = Rel::top().join("jobs").join(bad);
            let e = root.read(&rel).expect_err(bad);
            assert!(is_refused(&e), "`{bad}`: {e}");
            assert!(e.to_string().contains("not a single name"), "{e}");
            assert!(!root.exists(&rel) && !root.is_file(&rel));
        }
        let climbing = Rel::top().join("..").join("secret");
        assert!(is_refused(&root.read(&climbing).unwrap_err()));
        assert!(is_refused(
            &root
                .rename(&climbing, &Rel::top().join("stolen"))
                .unwrap_err()
        ));
        assert!(is_refused(&root.create_dir_all(&climbing).unwrap_err()));
        assert!(is_refused(&root.write(&climbing, b"x").unwrap_err()));
        assert!(is_refused(&root.remove_file(&climbing).unwrap_err()));
        assert_eq!(std::fs::read(&secret).unwrap(), b"outside");
    }

    #[test]
    fn a_missing_name_is_the_filesystems_answer_not_a_refusal() {
        let base = scratch("missing");
        let root = Root::new(base.join("root"));
        let e = root
            .read(&Rel::top().join("jobs").join("inbox").join("x.tuple"))
            .unwrap_err();
        assert_eq!(e.kind(), ErrorKind::NotFound);
        assert!(!is_refused(&e));
        let e = root
            .rename(
                &Rel::top().join("jobs").join("gone"),
                &Rel::top().join("jobs").join("here"),
            )
            .unwrap_err();
        assert_eq!(e.kind(), ErrorKind::NotFound, "a lost claim reads as gone");
    }

    #[cfg(unix)]
    #[test]
    fn no_link_below_the_root_is_followed() {
        use std::os::unix::fs::symlink;
        let base = scratch("links");
        let root = Root::new(base.join("root"));
        symlink(&base, base.join("root").join("jobs").join("out")).unwrap();
        symlink(
            base.join("secret"),
            base.join("root").join("jobs").join("s"),
        )
        .unwrap();

        // A link a path passes through, and a link it ends at when the operation follows it.
        let through = Rel::top().join("jobs").join("out").join("secret");
        let at = Rel::top().join("jobs").join("s");
        for rel in [&through, &at] {
            for e in [
                root.read(rel).unwrap_err(),
                root.metadata(rel).unwrap_err(),
                root.write(rel, b"x").unwrap_err(),
                root.list(rel).err().unwrap(),
            ] {
                assert!(is_refused(&e), "{e}");
                assert!(e.to_string().contains("symbolic link"), "{e}");
            }
            assert!(!root.exists(rel) && !root.is_file(rel));
        }
        assert!(is_refused(
            &root.create_dir_all(&through.join("more")).unwrap_err()
        ));
        assert!(is_refused(&root.remove_file(&through).unwrap_err()));
        assert!(is_refused(
            &root.hard_link(&at, &Rel::top().join("held")).unwrap_err()
        ));

        // A link is moved and removed as a NAME, never through: the target stays as it was.
        let moved = Rel::top().join("jobs").join("moved");
        root.rename(&at, &moved).unwrap();
        assert!(root.modified(&moved).is_ok(), "its own time");
        root.remove_file(&moved).unwrap();
        assert_eq!(std::fs::read(base.join("secret")).unwrap(), b"outside");

        // Listing reports a link as a link.
        let listed: Vec<Listed> = root
            .list(&Rel::top().join("jobs"))
            .unwrap()
            .map(|e| e.unwrap())
            .collect();
        assert!(listed.iter().all(|e| e.is_link() && !e.is_dir()));
    }

    #[cfg(unix)]
    #[test]
    fn the_root_itself_may_be_a_link() {
        use std::os::unix::fs::symlink;
        let base = scratch("root-link");
        symlink(base.join("root"), base.join("bound")).unwrap();
        let root = Root::new(base.join("bound"));
        let file = Rel::top().join("jobs").join("t");
        root.write(&file, b"in").unwrap();
        assert_eq!(root.read(&file).unwrap(), b"in");
    }
}
