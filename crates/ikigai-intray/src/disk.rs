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
//!   passes through is opened refusing a link, and so is the file itself unless the operation
//!   acts on the name and never on what it points at (a rename, a removal, a new link's name).
//!   A link that is moved this way is moved as a link, and is never read through.
//!
//! The root itself may be a link (macOS's `/tmp` is one): the host bound it, and only what lies
//! below it is judged.
//!
//! **No gap between a check and an act (ledger #2959).** On unix there is no separate check. An
//! operation opens the root, opens each directory of the path relative to the one before it
//! (`openat` with `O_NOFOLLOW`, through `rustix`), and acts on the last name relative to the
//! last handle it holds. A link swapped into the tree at any moment is either refused when
//! the walk reaches it, or changes nothing: the operation holds the directory it already
//! opened, wherever that has been renamed to.
//!
//! ⚠ What this does NOT close:
//! - the root's own path is opened by path on every operation, so whoever can replace the root
//!   itself still chooses where it points. The host bound that path and owns what is above it.
//! - a hard link is not a symbolic link: someone who can write below the root and has a hard
//!   link to a file outside it has already been given that file.
//! - on Windows, which has no `openat`, the check and the operation are still two calls, so a
//!   link swapped in between them wins the race. No CI job here builds this crate there.
//! - on unix other than Linux, Android and FreeBSD there is no way to open a directory only to
//!   pass through it (`O_PATH`), so a directory the process can search but not read cannot be
//!   walked, and is reported as the filesystem's permission error.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Error, ErrorKind, Result};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

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
    kind: Option<Kind>,
}

/// What a name is, itself: a link is a link, never what it points at.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    Link,
    Other,
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
        self.kind == Some(Kind::Dir)
    }

    /// A regular file, not a link to one.
    pub(crate) fn is_file(&self) -> bool {
        self.kind == Some(Kind::File)
    }

    /// A symbolic link.
    pub(crate) fn is_link(&self) -> bool {
        self.kind == Some(Kind::Link)
    }
}

/// The permissions a space gives every file and directory it creates
/// ([`crate::SpaceEndpoint::with_modes`]): `(file, dir)`. `None`, the default, is the
/// platform's own (the process umask on unix), exactly as before the setting existed.
#[derive(Debug, Clone, Copy, Default)]
#[cfg_attr(not(unix), allow(dead_code))] // file modes are a unix notion; elsewhere they are held and unused
pub(crate) struct Modes(pub(crate) Option<(u32, u32)>);

/// What [`Root::metadata`] answers about a file.
#[derive(Debug)]
pub(crate) struct Meta {
    /// The device and inode that make a file itself on unix.
    #[cfg(unix)]
    identity: (u64, u64),
    #[cfg(unix)]
    modified: SystemTime,
    #[cfg(not(unix))]
    meta: std::fs::Metadata,
}

impl Meta {
    /// When the file was last modified.
    pub(crate) fn modified(&self) -> Result<SystemTime> {
        #[cfg(unix)]
        {
            Ok(self.modified)
        }
        #[cfg(not(unix))]
        {
            self.meta.modified()
        }
    }

    /// Whether this and `other` are the same file under two names. std has no stable file
    /// identity off unix: a note is written once and never modified, so two different notes
    /// are told apart there by their size and modification time.
    pub(crate) fn same_file(&self, other: &Meta) -> bool {
        #[cfg(unix)]
        {
            self.identity == other.identity
        }
        #[cfg(not(unix))]
        {
            (self.meta.len(), self.meta.modified().ok())
                == (other.meta.len(), other.meta.modified().ok())
        }
    }
}

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
#[cfg(not(unix))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Last {
    /// Followed: a read, an open, a stat, a directory created or listed. A link there is
    /// refused like one a path passes through.
    Followed,
    /// Acted on as a name: a rename, a removal, a new link's name. A link there is moved or
    /// removed as a link, never through.
    Named,
}

/// Where a rival could change the tree under an operation: called wherever an operation has
/// finished looking at a name and is about to act on the next. Nothing outside this crate's
/// tests ever runs anything here.
#[cfg(all(unix, not(test)))]
fn race_point() {}

#[cfg(all(unix, test))]
thread_local! {
    static RACE_HOOK: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(unix, test))]
fn race_point() {
    RACE_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
}

/// The directory the intray works in, and the only door to it.
#[derive(Debug, Clone)]
pub(crate) struct Root {
    /// As the host bound it; never handed out to be opened (see [`Root::shown`]).
    path: PathBuf,
    /// The permissions of what this root's owner creates (the endpoint's; a reactor's are the
    /// default).
    #[cfg_attr(not(unix), allow(dead_code))] // file modes are a unix notion
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

    /// The refusal for `names`, the path to a symbolic link.
    fn refused_link(&self, names: &[OsString]) -> Error {
        refused(format!(
            "`{}` is a symbolic link, and the intray follows no link below its root",
            self.shown(&Rel(names.to_vec())).display()
        ))
    }
}

/// Every name of `rel` exactly one plain path segment.
fn judge(rel: &Rel) -> Result<()> {
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
    Ok(())
}

/// A directory opened to pass through it or act inside it, refusing a link.
#[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
const WALK: rustix::fs::OFlags = rustix::fs::OFlags::PATH.union(rustix::fs::OFlags::DIRECTORY);
#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_os = "freebsd"))
))]
const WALK: rustix::fs::OFlags = rustix::fs::OFlags::RDONLY.union(rustix::fs::OFlags::DIRECTORY);

/// `mode` as rustix takes it.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // `mode_t` is u32 on Linux and u16 on macOS
fn raw_mode(mode: u32) -> rustix::fs::Mode {
    rustix::fs::Mode::from_raw_mode(mode as rustix::fs::RawMode)
}

/// What `stat` says the name is.
#[cfg(unix)]
fn kind_of(file_type: rustix::fs::FileType) -> Kind {
    use rustix::fs::FileType;
    match file_type {
        FileType::Directory => Kind::Dir,
        FileType::RegularFile => Kind::File,
        FileType::Symlink => Kind::Link,
        _ => Kind::Other,
    }
}

#[cfg(unix)]
fn stat_kind(stat: &rustix::fs::Stat) -> Kind {
    kind_of(rustix::fs::FileType::from_raw_mode(stat.st_mode))
}

/// A `stat` as a [`Meta`].
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // these fields' types differ between Linux and macOS
fn meta_of(stat: &rustix::fs::Stat) -> Meta {
    use std::time::{Duration, UNIX_EPOCH};
    let (secs, nanos) = (stat.st_mtime as i64, stat.st_mtime_nsec as u32);
    let whole = Duration::from_secs(secs.unsigned_abs());
    let modified = if secs >= 0 {
        UNIX_EPOCH + whole
    } else {
        UNIX_EPOCH - whole
    };
    Meta {
        identity: (stat.st_dev as u64, stat.st_ino as u64),
        modified: modified + Duration::from_nanos(nanos.into()),
    }
}

#[cfg(unix)]
impl Root {
    /// The root, by the path the host bound: it may be a link, and only what lies below it is
    /// judged.
    fn open_root(&self) -> Result<std::os::fd::OwnedFd> {
        use rustix::fs::{openat, Mode, OFlags, CWD};
        Ok(openat(
            CWD,
            &self.path,
            WALK | OFlags::CLOEXEC,
            Mode::empty(),
        )?)
    }

    /// Open the last of `names` inside `dir`, which holds the names before it, refusing a link.
    fn open_below(
        &self,
        dir: &std::os::fd::OwnedFd,
        names: &[OsString],
        flags: rustix::fs::OFlags,
        mode: rustix::fs::Mode,
    ) -> Result<std::os::fd::OwnedFd> {
        use rustix::fs::{openat, statat, AtFlags, OFlags};
        use rustix::io::Errno;
        let Some(name) = names.last() else {
            return Err(Error::from(Errno::INVAL));
        };
        let name = name.as_os_str();
        openat(dir, name, flags | OFlags::NOFOLLOW | OFlags::CLOEXEC, mode).map_err(|errno| {
            // Platforms answer a refused link differently (`ELOOP`, or `ENOTDIR` under
            // `O_DIRECTORY`), so ask the name itself. Nothing was followed either way.
            let link = errno != Errno::NOENT
                && statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)
                    .is_ok_and(|stat| stat_kind(&stat) == Kind::Link);
            if link {
                self.refused_link(names)
            } else {
                errno.into()
            }
        })
    }

    /// The directory holding `names`, opened one name at a time from the root.
    fn walk(&self, names: &[OsString]) -> Result<std::os::fd::OwnedFd> {
        let mut dir = self.open_root()?;
        for i in 0..names.len() {
            race_point();
            dir = self.open_below(&dir, &names[..=i], WALK, rustix::fs::Mode::empty())?;
        }
        race_point();
        Ok(dir)
    }

    /// The open directory `rel`'s last name is in, and that name. The root itself has no name
    /// to act on, so an operation on it is refused.
    fn parent<'r>(&self, rel: &'r Rel) -> Result<(std::os::fd::OwnedFd, &'r OsStr)> {
        judge(rel)?;
        let Some((last, above)) = rel.0.split_last() else {
            return Err(refused(
                "the intray's root has no name of its own to act on".to_string(),
            ));
        };
        Ok((self.walk(above)?, last.as_os_str()))
    }

    /// Open `rel` itself with `flags`, refusing a link at any name below the root.
    fn open(
        &self,
        rel: &Rel,
        flags: rustix::fs::OFlags,
        mode: rustix::fs::Mode,
    ) -> Result<std::os::fd::OwnedFd> {
        use rustix::fs::{openat, OFlags, CWD};
        judge(rel)?;
        let Some((_, above)) = rel.0.split_last() else {
            return Ok(openat(CWD, &self.path, flags | OFlags::CLOEXEC, mode)?);
        };
        let dir = self.walk(above)?;
        self.open_below(&dir, &rel.0, flags, mode)
    }

    /// `lstat` of `rel`, without following its last name.
    fn lstat(&self, rel: &Rel) -> Result<rustix::fs::Stat> {
        use rustix::fs::{statat, AtFlags, CWD};
        if rel.0.is_empty() {
            return Ok(statat(CWD, &self.path, AtFlags::empty())?);
        }
        let (dir, name) = self.parent(rel)?;
        Ok(statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW)?)
    }

    /// `lstat` of `rel`, refusing a link at its last name too.
    fn stat(&self, rel: &Rel) -> Result<rustix::fs::Stat> {
        let stat = self.lstat(rel)?;
        if stat_kind(&stat) == Kind::Link {
            return Err(self.refused_link(&rel.0));
        }
        Ok(stat)
    }
}

#[cfg(unix)]
#[allow(clippy::disallowed_methods)] // ambient: fs: the intray is a physical tuplespace, a directory per space under the root the host bound; every operation here takes a Rel opened relative to a handle on that root, so none reaches outside it (ledger #2418, #2875, #2959)
impl Root {
    /// [`std::fs::create_dir_all`] of the root and `rel` below it, giving each directory it
    /// CREATES this root's directory mode. One that already exists is left as it is: the
    /// space did not make it.
    pub(crate) fn create_dir_all(&self, rel: &Rel) -> Result<()> {
        judge(rel)?;
        self.create_root()?;
        let mut dir = self.open_root()?;
        for i in 0..rel.0.len() {
            let names = &rel.0[..=i];
            race_point();
            let below = self.open_below(&dir, names, WALK, rustix::fs::Mode::empty());
            dir = match below {
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    self.make_dir(&dir, names)?;
                    race_point();
                    self.open_below(&dir, names, WALK, rustix::fs::Mode::empty())?
                }
                below => below?,
            };
        }
        Ok(())
    }

    /// The root and what is missing above it, by path: all of that is the host's.
    fn create_root(&self) -> Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        let Some((_, mode)) = self.modes.0 else {
            return std::fs::create_dir_all(&self.path);
        };
        let mut missing = Vec::new();
        let mut at = Some(self.path.as_path());
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

    fn set_mode(path: &Path, mode: u32) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }

    /// Make the last of `names` inside `dir`. A rival creator that made it first sets the mode
    /// itself, and the caller's next open judges what it made.
    fn make_dir(&self, dir: &std::os::fd::OwnedFd, names: &[OsString]) -> Result<()> {
        use rustix::fs::{mkdirat, Mode, OFlags};
        use rustix::io::Errno;
        use std::os::unix::fs::PermissionsExt;
        let Some(name) = names.last() else {
            return Ok(());
        };
        let name = name.as_os_str();
        let wanted = self.modes.0.map(|(_, mode)| mode);
        // Created with the mode, so the umask can only NARROW it, and then set to it exactly
        // through a handle. The handle needs the owner's read for a moment when the mode
        // leaves it out, which widens nothing for anyone else.
        match mkdirat(dir, name, raw_mode(wanted.map_or(0o777, |m| m | 0o400))) {
            Ok(()) => {}
            Err(Errno::EXIST) => return Ok(()),
            Err(errno) => return Err(errno.into()),
        }
        let Some(mode) = wanted else {
            return Ok(());
        };
        let flags = OFlags::RDONLY | OFlags::DIRECTORY;
        File::from(self.open_below(dir, names, flags, Mode::empty())?)
            .set_permissions(std::fs::Permissions::from_mode(mode))
    }

    /// `std::fs::write` of a file the space creates (always under a fresh staging name), with
    /// this root's file mode.
    pub(crate) fn write(&self, rel: &Rel, bytes: &[u8]) -> Result<()> {
        use rustix::fs::OFlags;
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let wanted = self.modes.0.map(|(mode, _)| mode);
        let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC;
        let mut file = File::from(self.open(rel, flags, raw_mode(wanted.unwrap_or(0o666)))?);
        // As for a directory: created with the mode (never wider), then set exactly.
        if let Some(mode) = wanted {
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        file.write_all(bytes)
    }

    fn open_read(&self, rel: &Rel) -> Result<File> {
        let flags = rustix::fs::OFlags::RDONLY;
        Ok(File::from(self.open(
            rel,
            flags,
            rustix::fs::Mode::empty(),
        )?))
    }

    /// [`std::fs::read`].
    pub(crate) fn read(&self, rel: &Rel) -> Result<Vec<u8>> {
        use std::io::Read;
        let mut bytes = Vec::new();
        self.open_read(rel)?.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// [`std::fs::read_to_string`].
    pub(crate) fn read_to_string(&self, rel: &Rel) -> Result<String> {
        use std::io::Read;
        let mut text = String::new();
        self.open_read(rel)?.read_to_string(&mut text)?;
        Ok(text)
    }

    /// The entries of the directory `rel`, each with its own type (a link is a link).
    pub(crate) fn list(&self, rel: &Rel) -> Result<impl Iterator<Item = Result<Listed>>> {
        use rustix::fs::{statat, AtFlags, Dir, FileType, Mode, OFlags};
        use std::os::unix::ffi::OsStrExt;
        let flags = OFlags::RDONLY | OFlags::DIRECTORY;
        let dir = self.open(rel, flags, Mode::empty())?;
        let held = dir.try_clone()?;
        Ok(Dir::new(dir)?.filter_map(move |entry| {
            let entry = match entry {
                Ok(entry) => entry,
                Err(errno) => return Some(Err(errno.into())),
            };
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if name == "." || name == ".." {
                return None;
            }
            // Some filesystems do not say in the listing; as std does, ask the name then.
            let kind = match entry.file_type() {
                FileType::Unknown => statat(&held, name, AtFlags::SYMLINK_NOFOLLOW)
                    .ok()
                    .map(|stat| stat_kind(&stat)),
                known => Some(kind_of(known)),
            };
            Some(Ok(Listed {
                name: name.to_os_string(),
                kind,
            }))
        }))
    }

    /// [`std::fs::rename`]: the intray's compare-and-swap (a claim, a publish, a requeue). A
    /// link is moved as a link.
    pub(crate) fn rename(&self, from: &Rel, to: &Rel) -> Result<()> {
        let (from_dir, from_name) = self.parent(from)?;
        let (to_dir, to_name) = self.parent(to)?;
        Ok(rustix::fs::renameat(
            &from_dir, from_name, &to_dir, to_name,
        )?)
    }

    /// [`std::fs::remove_file`]. A link is removed as a link.
    pub(crate) fn remove_file(&self, rel: &Rel) -> Result<()> {
        let (dir, name) = self.parent(rel)?;
        Ok(rustix::fs::unlinkat(
            &dir,
            name,
            rustix::fs::AtFlags::empty(),
        )?)
    }

    /// [`std::fs::hard_link`]: `link` becomes a second name of `original`, which may not be a
    /// link itself. `linkat` without `AT_SYMLINK_FOLLOW` follows nothing, so a link swapped in
    /// after the refusal is only ever linked as a link.
    pub(crate) fn hard_link(&self, original: &Rel, link: &Rel) -> Result<()> {
        use rustix::fs::{linkat, statat, AtFlags};
        let (from_dir, from_name) = self.parent(original)?;
        let (to_dir, to_name) = self.parent(link)?;
        if statat(&from_dir, from_name, AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|stat| stat_kind(&stat) == Kind::Link)
        {
            return Err(self.refused_link(&original.0));
        }
        race_point();
        Ok(linkat(
            &from_dir,
            from_name,
            &to_dir,
            to_name,
            AtFlags::empty(),
        )?)
    }

    /// The file `rel`, never through a link.
    pub(crate) fn metadata(&self, rel: &Rel) -> Result<Meta> {
        Ok(meta_of(&self.stat(rel)?))
    }

    /// When the entry `rel` itself was last modified: a link's own time, not its target's.
    pub(crate) fn modified(&self, rel: &Rel) -> Result<SystemTime> {
        Ok(meta_of(&self.lstat(rel)?).modified)
    }

    /// [`Path::is_file`]; false when refused.
    pub(crate) fn is_file(&self, rel: &Rel) -> bool {
        self.stat(rel).is_ok_and(|s| stat_kind(&s) == Kind::File)
    }

    /// [`Path::exists`]; false when refused.
    pub(crate) fn exists(&self, rel: &Rel) -> bool {
        self.stat(rel).is_ok()
    }

    /// The root, canonical: what a filesystem watcher reports paths under.
    pub(crate) fn canonical(&self) -> Result<PathBuf> {
        self.path.canonicalize()
    }

    /// Open (creating, never truncating) the reactor lease file, for its lock.
    pub(crate) fn open_lease(&self, rel: &Rel) -> Result<File> {
        use rustix::fs::OFlags;
        let flags = OFlags::WRONLY | OFlags::CREATE;
        Ok(File::from(self.open(rel, flags, raw_mode(0o666))?))
    }
}

#[cfg(not(unix))]
impl Root {
    /// Judge `rel` and answer the path it names: every name one plain segment, and none of the
    /// existing names below the root a symbolic link (the last one too, when the operation
    /// follows it). A name that does not exist ends the check: nothing below it exists to be
    /// followed, and the operation reports the absence itself. The check and the operation
    /// that follows it are two calls, the race the module's docs name for this platform.
    fn resolve(&self, rel: &Rel, last: Last) -> Result<PathBuf> {
        judge(rel)?;
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
                    return Err(self.refused_link(&rel.0[..=i]));
                }
                Ok(_) => {}
                Err(_) => checking = false,
            }
        }
        Ok(path)
    }
}

#[cfg(not(unix))]
#[allow(clippy::disallowed_methods)] // ambient: fs: the intray is a physical tuplespace, a directory per space under the root the host bound; every operation here takes a Rel judged against that root, so none reaches outside it (ledger #2418, #2875)
impl Root {
    fn lstat(&self, path: &Path) -> Result<std::fs::Metadata> {
        std::fs::symlink_metadata(path)
    }

    /// [`std::fs::create_dir_all`] of the root and `rel` below it.
    pub(crate) fn create_dir_all(&self, rel: &Rel) -> Result<()> {
        std::fs::create_dir_all(self.resolve(rel, Last::Followed)?)
    }

    /// `std::fs::write` of a file the space creates (always under a fresh staging name).
    pub(crate) fn write(&self, rel: &Rel, bytes: &[u8]) -> Result<()> {
        std::fs::write(self.resolve(rel, Last::Followed)?, bytes)
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
                kind: entry.file_type().ok().map(|kind| {
                    if kind.is_symlink() {
                        Kind::Link
                    } else if kind.is_dir() {
                        Kind::Dir
                    } else if kind.is_file() {
                        Kind::File
                    } else {
                        Kind::Other
                    }
                }),
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
    pub(crate) fn metadata(&self, rel: &Rel) -> Result<Meta> {
        let meta = std::fs::metadata(self.resolve(rel, Last::Followed)?)?;
        Ok(Meta { meta })
    }

    /// When the entry `rel` itself was last modified: a link's own time, not its target's.
    pub(crate) fn modified(&self, rel: &Rel) -> Result<SystemTime> {
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

    /// What a rival does to the tree between a check and an act.
    #[cfg(unix)]
    #[derive(Clone, Copy, Debug)]
    enum Swap {
        /// `jobs`, a directory the path passes through, becomes a link to `outside`.
        Component,
        /// `jobs/t`, the path's last name, becomes a link to `outside/t`.
        Leaf,
    }

    #[cfg(unix)]
    impl Swap {
        fn apply(self, base: &Path) {
            use std::os::unix::fs::symlink;
            let jobs = base.join("root").join("jobs");
            match self {
                Swap::Component => {
                    std::fs::rename(&jobs, base.join("root").join("jobs.real")).unwrap();
                    symlink(base.join("outside"), &jobs).unwrap();
                }
                Swap::Leaf => {
                    std::fs::rename(jobs.join("t"), jobs.join("t.real")).unwrap();
                    symlink(base.join("outside").join("t"), jobs.join("t")).unwrap();
                }
            }
        }
    }

    /// A time no file here has but `outside/t`, so a stat can show which file it reached.
    #[cfg(unix)]
    fn outside_time() -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000)
    }

    /// `<base>/root` holding `jobs/t` and `keep`, and `<base>/outside` holding its own `t` and
    /// `only`, each marked so reaching it shows.
    #[cfg(unix)]
    fn race_tree(tag: &str) -> PathBuf {
        let base = std::env::temp_dir()
            .join("ikigai-intray-disk")
            .join(format!("race-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (jobs, outside) = (base.join("root").join("jobs"), base.join("outside"));
        std::fs::create_dir_all(&jobs).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(jobs.join("t"), b"inside").unwrap();
        std::fs::write(base.join("root").join("keep"), b"inside").unwrap();
        std::fs::write(outside.join("t"), b"outside").unwrap();
        std::fs::write(outside.join("only"), b"outside").unwrap();
        std::fs::File::options()
            .write(true)
            .open(outside.join("t"))
            .unwrap()
            .set_modified(outside_time())
            .unwrap();
        base
    }

    /// Everything in `outside`: each name, its bytes when it is a file, and its mode.
    #[cfg(unix)]
    fn outside_now(base: &Path) -> Vec<(OsString, Vec<u8>, u32)> {
        use std::os::unix::fs::PermissionsExt;
        let mut seen: Vec<_> = std::fs::read_dir(base.join("outside"))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let meta = entry.metadata().unwrap();
                let bytes = std::fs::read(entry.path()).unwrap_or_default();
                (entry.file_name(), bytes, meta.permissions().mode())
            })
            .collect();
        seen.sort();
        seen
    }

    /// Every operation, each answering whether it SAW something from outside the root (what it
    /// changed there shows in [`outside_now`]).
    #[cfg(unix)]
    #[allow(clippy::type_complexity)]
    fn operations() -> Vec<(&'static str, fn(&Root) -> bool)> {
        fn jobs() -> Rel {
            Rel::top().join("jobs")
        }
        fn t() -> Rel {
            jobs().join("t")
        }
        vec![
            ("read", |r| r.read(&t()).is_ok_and(|b| b == b"outside")),
            ("read_to_string", |r| {
                r.read_to_string(&t()).is_ok_and(|b| b == "outside")
            }),
            ("metadata", |r| {
                r.metadata(&t())
                    .and_then(|m| m.modified())
                    .is_ok_and(|at| at == outside_time())
            }),
            ("modified", |r| {
                r.modified(&t()).is_ok_and(|at| at == outside_time())
            }),
            ("is_file", |r| r.is_file(&jobs().join("only"))),
            ("exists", |r| r.exists(&jobs().join("only"))),
            ("list", |r| {
                r.list(&jobs()).is_ok_and(|mut entries| {
                    entries.any(|e| e.is_ok_and(|e| e.name() == Some("only")))
                })
            }),
            ("write", |r| {
                let _ = r.write(&t(), b"written");
                false
            }),
            ("write with modes", |r| {
                let r = r.clone().with_modes(Modes(Some((0o666, 0o777))));
                let _ = r.write(&t(), b"written");
                false
            }),
            ("write a new name", |r| {
                let _ = r.write(&jobs().join("new"), b"written");
                false
            }),
            ("rename from", |r| {
                let _ = r.rename(&t(), &Rel::top().join("stolen"));
                false
            }),
            ("rename to", |r| {
                let _ = r.rename(&Rel::top().join("keep"), &jobs().join("planted"));
                false
            }),
            ("remove_file", |r| {
                let _ = r.remove_file(&t());
                false
            }),
            ("hard_link original", |r| {
                let _ = r.hard_link(&t(), &Rel::top().join("held"));
                r.read(&Rel::top().join("held"))
                    .is_ok_and(|b| b == b"outside")
            }),
            ("hard_link name", |r| {
                let _ = r.hard_link(&Rel::top().join("keep"), &jobs().join("held"));
                false
            }),
            ("open_lease", |r| {
                let _ = r.open_lease(&jobs().join("lease"));
                false
            }),
            ("create_dir_all", |r| {
                let _ = r.create_dir_all(&jobs().join("made").join("deep"));
                false
            }),
            ("create_dir_all with modes", |r| {
                let r = r.clone().with_modes(Modes(Some((0o666, 0o777))));
                let _ = r.create_dir_all(&jobs().join("made").join("deep"));
                false
            }),
        ]
    }

    /// The check-then-act race (ledger #2959), made deterministic: each operation runs once per
    /// [`race_point`] it passes, with a rival swapping a link in at exactly that point. Whatever
    /// the operation then answers, it must not have seen or changed anything outside the root.
    #[cfg(unix)]
    #[test]
    fn a_link_swapped_in_mid_operation_reaches_nothing_outside_the_root() {
        let mut escapes = Vec::new();
        for (name, operation) in operations() {
            for swap in [Swap::Component, Swap::Leaf] {
                for point in 0..32 {
                    let base = race_tree(&format!("{}-{swap:?}-{point}", name.replace(' ', "-")));
                    let before = outside_now(&base);
                    let (fired, rival_base) =
                        (std::rc::Rc::new(std::cell::Cell::new(0)), base.clone());
                    let count = fired.clone();
                    RACE_HOOK.with(|hook| {
                        *hook.borrow_mut() = Some(Box::new(move || {
                            if count.get() == point {
                                swap.apply(&rival_base);
                            }
                            count.set(count.get() + 1);
                        }));
                    });
                    let saw = operation(&Root::new(base.join("root")));
                    RACE_HOOK.with(|hook| *hook.borrow_mut() = None);
                    if saw {
                        escapes.push(format!("{name}, {swap:?} at point {point}: read outside"));
                    }
                    if outside_now(&base) != before {
                        escapes.push(format!(
                            "{name}, {swap:?} at point {point}: changed outside"
                        ));
                    }
                    let _ = std::fs::remove_dir_all(&base);
                    if fired.get() <= point {
                        break;
                    }
                }
            }
        }
        assert!(
            escapes.is_empty(),
            "{} escapes from the root:\n{}",
            escapes.len(),
            escapes.join("\n")
        );
    }

    /// The same race with no seam: a real rival thread flipping `jobs` between the directory and
    /// a link to `outside`, while this thread reads and writes through it. It only ever finds
    /// an escape by luck, but no run may find one.
    #[cfg(unix)]
    #[test]
    fn a_rival_thread_flipping_a_component_never_gets_a_read_or_write_out() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let base = race_tree("rival");
        let before = outside_now(&base);
        let stop = Arc::new(AtomicBool::new(false));
        let rival = {
            let (base, stop) = (base.clone(), stop.clone());
            std::thread::spawn(move || {
                let (jobs, real) = (
                    base.join("root").join("jobs"),
                    base.join("root").join("jobs.real"),
                );
                while !stop.load(Ordering::Relaxed) {
                    std::fs::rename(&jobs, &real).unwrap();
                    std::os::unix::fs::symlink(base.join("outside"), &jobs).unwrap();
                    std::thread::yield_now();
                    std::fs::remove_file(&jobs).unwrap();
                    std::fs::rename(&real, &jobs).unwrap();
                    std::thread::yield_now();
                }
            })
        };
        let root = Root::new(base.join("root"));
        let t = Rel::top().join("jobs").join("t");
        let mut read_outside = 0;
        for i in 0..20_000 {
            if i % 2 == 0 {
                read_outside += root.read(&t).is_ok_and(|b| b == b"outside") as u32;
            } else {
                let _ = root.write(&t, b"inside");
            }
        }
        stop.store(true, Ordering::Relaxed);
        rival.join().unwrap();
        assert_eq!(
            read_outside, 0,
            "reads that came back from outside the root"
        );
        assert_eq!(outside_now(&base), before, "outside the root is as it was");
    }

    /// A directory its mode lets the owner search but not read (a drop box) is still passed
    /// through: walking opens it only to pass through, with `O_PATH`.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_directory_that_can_be_searched_but_not_read_is_walked() {
        use std::os::unix::fs::PermissionsExt;
        let base = scratch("search-only");
        let root = Root::new(base.join("root")).with_modes(Modes(Some((0o600, 0o300))));
        let drop_box = Rel::top().join("drop");
        let file = drop_box.join("box").join("t");
        root.create_dir_all(&drop_box.join("box")).unwrap();
        root.write(&file, b"in").unwrap();
        let held = root.read(&file);
        let mode = std::fs::metadata(root.shown(&drop_box)).unwrap();
        for dir in [drop_box.join("box"), drop_box] {
            let open = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(root.shown(&dir), open).unwrap();
        }
        assert_eq!(held.unwrap(), b"in");
        assert_eq!(mode.permissions().mode() & 0o777, 0o300);
    }
}
