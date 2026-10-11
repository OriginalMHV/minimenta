//! Stores a scanned tree on disk, so the next run can load it and re-list only
//! the directories that FSEvents reports as changed.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::scan::{Options, Progress};
use crate::tree::{Dir, Entry, Kind, flag};

const MAGIC: &[u8; 8] = b"MMCACHE\0";
const VERSION: u32 = 5;
/// Deeper trees are rejected when loading, so a damaged file cannot overflow
/// the stack. Paths on macOS are at most 1024 bytes, so real trees have at
/// most about 512 levels.
const MAX_DEPTH: usize = 1024;

/// Where a result came from, so the interface can say so.
pub enum Source {
    Scanned,
    /// Read from the NTFS master file table (Windows, administrator).
    MasterFileTable,
    /// A slow full scan that an elevated run could have read from the NTFS
    /// master file table (Windows, administrator without elevation).
    ScannedNotElevated,
    /// `listed` counts the directories that changed since the last run.
    /// `age_secs` is the time since the last full scan.
    Cached {
        listed: usize,
        age_secs: u64,
    },
}

/// Shown after a `ScannedNotElevated` scan.
pub const ELEVATE_HINT: &str = "Run as administrator to read the NTFS master file table, which is often several times faster on a cold disk.";

pub struct Scan {
    pub dir: Dir,
    pub source: Source,
    /// The header to save with when the tree is corrected later, for example
    /// by a rescan with `r`. It keeps the FSEvents position of this run, so
    /// changes after it are replayed again next time.
    pub session: Option<Header>,
}

/// Above this many changed directories, a full scan is simpler and about as fast.
#[cfg(target_os = "macos")]
const MAX_CHANGES: usize = 20_000;
/// A full scan at least this often heals rare cases that FSEvents cannot
/// describe, such as a folder renamed away, changed, and renamed back.
#[cfg(target_os = "macos")]
const MAX_AGE_SECS: u64 = 7 * 24 * 3600;
/// Saving a large tree takes time, so an unchanged tree is saved again only
/// after this long, to keep the next replay short.
#[cfg(target_os = "macos")]
const RESAVE_SECS: u64 = 3600;

/// Why the check could not use the cache. The progress screen shows the text
/// of [`Display`](std::fmt::Display) under its message. Each text says what the
/// check compared, so the reader can tell which limit it hit.
#[cfg(target_os = "macos")]
#[derive(Debug, PartialEq)]
pub enum CacheMiss {
    /// There is no cache file. This is a first scan, so the screen says nothing.
    NoFile,
    /// The last full scan is older than `MAX_AGE_SECS`.
    Expired,
    /// The cache was made for another root path or with another `-x` setting.
    OtherOptions,
    /// The root has another device or inode than at the last scan.
    FolderReplaced,
    FolderUnreadable,
    /// FSEvents gives no ID for the volume of the root.
    VolumeUnknown,
    VolumeChanged,
    /// The cache file cannot be read. Holds the error text.
    FileUnreadable(String),
    /// The magic bytes, the version, or the data do not match.
    FileDamaged,
    /// FSEvents cannot give the history of the root.
    Replay(crate::fsevents::ReplayError),
    /// The history names a path outside the root.
    OutsideRoot,
    /// The first pass found more than `MAX_CHANGES` entries. `changes` counts
    /// the entries that the check compared, `folders` the different paths
    /// among them.
    TooManyChanges {
        changes: usize,
        folders: usize,
        limit: usize,
    },
    /// The second pass found more than `MAX_CHANGES` folders with hard links.
    TooManyLinkedFolders {
        count: usize,
        limit: usize,
    },
    /// A folder could not be listed again. Holds the error text.
    ListFailed(String),
}

#[cfg(target_os = "macos")]
impl CacheMiss {
    /// A missing cache file is the normal case on the first scan of a folder.
    pub fn is_silent(&self) -> bool {
        matches!(self, Self::NoFile)
    }
}

#[cfg(target_os = "macos")]
impl std::fmt::Display for CacheMiss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoFile => f.write_str("There is no cache file."),
            Self::Expired => write!(
                f,
                "The last full scan is more than {} days old.",
                MAX_AGE_SECS / (24 * 3600)
            ),
            Self::OtherOptions => {
                f.write_str("The cache is from a scan with other options or another path.")
            }
            Self::FolderReplaced => f.write_str("The folder was replaced since the last scan."),
            Self::FolderUnreadable => f.write_str("The folder cannot be read."),
            Self::VolumeUnknown => {
                f.write_str("FSEvents cannot identify the volume of the folder.")
            }
            Self::VolumeChanged => {
                f.write_str("The volume of the folder changed since the last scan.")
            }
            Self::FileUnreadable(error) => write!(f, "The cache file cannot be read: {error}"),
            Self::FileDamaged => {
                f.write_str("The cache file is damaged or has another format version.")
            }
            Self::Replay(error) => write!(f, "{error}"),
            Self::OutsideRoot => {
                f.write_str("FSEvents named a path outside the folder. The folder may have moved.")
            }
            Self::TooManyChanges {
                changes,
                folders,
                limit,
            } => write!(
                f,
                "{changes} changes in {folders} folder{}. The limit is {limit} changes.",
                if *folders == 1 { "" } else { "s" }
            ),
            Self::TooManyLinkedFolders { count, limit } => write!(
                f,
                "{count} folders have hard links. The limit is {limit} folders."
            ),
            Self::ListFailed(error) => write!(f, "A folder could not be listed again: {error}"),
        }
    }
}

/// Scans `path`, or loads the cached tree and lists again only the directories
/// that changed since. Saves the result for the next run.
pub fn scan(path: &Path, opts: &Options, progress: &Progress) -> io::Result<Scan> {
    #[cfg(target_os = "macos")]
    let began = Instant::now();
    let root = path.canonicalize()?;
    let use_cache = opts.cache && cache_allowed();
    #[cfg(target_os = "macos")]
    if use_cache {
        match incremental(&root, opts, progress, began) {
            Ok(result) => return result,
            Err(miss) if miss.is_silent() => {}
            Err(miss) => {
                // The check can list folders before it stops, and the full
                // scan counts them again. The flag goes last, so the screen
                // never shows the old counts as part of the full scan. The
                // reason comes before `cache_unusable`, so the screen finds it
                // when it shows the message.
                progress.items.store(0, Ordering::Relaxed);
                progress.disk.store(0, Ordering::Relaxed);
                progress.errors.store(0, Ordering::Relaxed);
                let _ = progress.cache_reason.set(miss.to_string());
                progress.cache_unusable.store(true, Ordering::Relaxed);
                progress.checking_cache.store(false, Ordering::Release);
            }
        }
    }
    let event_id = if use_cache { current_event_id() } else { None };
    // Taken before the scan, so a mount that appears during the scan is
    // listed again next time.
    let mounts = if use_cache {
        mounts_under(&root)
    } else {
        Vec::new()
    };
    let start = Instant::now();
    let dir = crate::scan::scan(&root, opts, progress)?;
    let session = event_id.and_then(|event_id| {
        let secs = start.elapsed().as_secs_f64();
        let header = new_header(&root, opts, event_id, secs, mounts, dir.totals().items)?;
        save_in_background(&header, &dir);
        Some(header)
    });
    let source = if progress.mft.load(std::sync::atomic::Ordering::Relaxed) {
        Source::MasterFileTable
    } else if progress.elevate.load(std::sync::atomic::Ordering::Relaxed) {
        Source::ScannedNotElevated
    } else {
        Source::Scanned
    };
    Ok(Scan {
        dir,
        source,
        session,
    })
}

/// `began` is when the scan began. The progress screen counts its time from
/// there. Returns the reason when a full scan is needed.
#[cfg(target_os = "macos")]
fn incremental(
    root: &Path,
    opts: &Options,
    progress: &Progress,
    began: Instant,
) -> Result<io::Result<Scan>, CacheMiss> {
    use crate::fsevents::{self, ReplayError};
    use std::sync::atomic::Ordering::Relaxed;
    use std::time::Duration;

    let file = file_for(root).ok_or(CacheMiss::NoFile)?;
    fs::metadata(&file).map_err(|_| CacheMiss::NoFile)?;
    // A large cache file takes a moment to read, so the screen says so first.
    progress.checking_cache.store(true, Relaxed);
    let bytes =
        std::sync::Arc::new(fs::read(&file).map_err(|e| CacheMiss::FileUnreadable(e.to_string()))?);
    let (header, start) = decode_header(&bytes).ok_or(CacheMiss::FileDamaged)?;
    // Decoding a large tree takes about as long as the checks and the
    // FSEvents replay below, so both run at the same time if a thread starts.
    let shared = std::sync::Arc::clone(&bytes);
    let decoder = std::thread::Builder::new()
        .spawn(move || decode_tree(&shared[start..]))
        .ok();
    check_header(&header, root, opts, progress)?;
    // Taken before the replay: anything that happens from now on is replayed
    // next time. Listing a directory twice is harmless.
    let event_id =
        fsevents::current_event_id().ok_or(CacheMiss::Replay(ReplayError::Unavailable))?;
    let budget = Duration::from_secs_f64((header.scan_secs / 2.0).max(0.5));
    let mounts = mounts_under(root);

    // The replay for the root does not include other volumes, so each kept
    // volume with a history is asked on its own.
    let mut streams = vec![root.to_path_buf()];
    if !opts.one_fs {
        let kept = mounts
            .iter()
            .filter(|m| m.volume.is_some() && header.mounts.contains(m));
        streams.extend(kept.map(|m| m.path.clone()));
    }
    // Each replay waits at most `budget`. A replay for a mount that takes too
    // long does not end the check, so the next one can wait as long again.
    let limit = budget.saturating_mul(u32::try_from(streams.len()).unwrap_or(u32::MAX));
    let deadline = began.elapsed().saturating_add(limit).as_millis();
    let deadline = u64::try_from(deadline).unwrap_or(u64::MAX);
    progress.check_limit_ms.store(deadline, Relaxed);
    let mut changes: Vec<(PathBuf, bool)> = Vec::new();
    for stream in &streams {
        let events =
            match fsevents::changes_since(stream, header.event_id, budget, &progress.cancel) {
                Ok(events) => events,
                Err(error) => {
                    if progress.cancel.load(Relaxed) {
                        return Ok(Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "scan cancelled",
                        )));
                    }
                    if stream == root {
                        return Err(CacheMiss::Replay(error));
                    }
                    changes.push((stream.clone(), true));
                    continue;
                }
            };
        for event in events {
            let inside: Vec<PathBuf> = aliases(&event.path)
                .into_iter()
                .filter(|p| p.starts_with(root))
                .collect();
            // An event outside the root, such as one for its parent after the
            // root was renamed away and back, means the replay cannot describe
            // what happened.
            if inside.is_empty() {
                return Err(CacheMiss::OutsideRoot);
            }
            changes.extend(inside.into_iter().map(|p| (p, event.recursive)));
        }
    }
    progress.check_limit_ms.store(0, Relaxed);
    changes.extend(mount_changes(&header.mounts, &mounts, opts.one_fs));
    let listed = distinct_paths(&changes);

    let mut dir = match decoder {
        Some(thread) => thread.join().ok().flatten(),
        None => decode_tree(&bytes[start..]),
    }
    .ok_or(CacheMiss::FileDamaged)?;
    let before = (dir.totals(), error_count(&dir));
    if let Err(e) = apply(&mut dir, root, changes, opts, progress)? {
        return Ok(Err(e));
    }
    let changed = listed > 0 || (dir.totals(), error_count(&dir)) != before;
    let stale_file = now().saturating_sub(header.saved_at) > RESAVE_SECS;
    let age_secs = now().saturating_sub(header.full_scan_at);
    let session = Header {
        event_id,
        saved_at: now(),
        mounts,
        items: dir.totals().items,
        ..header
    };
    // An unchanged tree stays valid with the older position in the file:
    // replaying more history is harmless.
    if changed || stale_file {
        save_in_background(&session, &dir);
    }
    Ok(Ok(Scan {
        dir,
        source: Source::Cached { listed, age_secs },
        session: Some(session),
    }))
}

/// Checks that the cache is for this folder and these options, that the
/// folder and its volume are the same, and that the last full scan is recent.
/// Checks in that order, and returns the first reason that fails.
#[cfg(target_os = "macos")]
fn check_header(
    header: &Header,
    root: &Path,
    opts: &Options,
    progress: &Progress,
) -> Result<(), CacheMiss> {
    let (dev, ino) = identity(root).ok_or(CacheMiss::FolderUnreadable)?;
    let same_options = header.root == root && header.one_fs == opts.one_fs;
    let same_folder = (header.root_dev, header.root_ino) == (dev, ino);
    // The count of the same tree helps the estimate of a full scan, also when
    // the cache is too old to use.
    if same_options && same_folder {
        progress
            .expected_items
            .store(header.items, std::sync::atomic::Ordering::Relaxed);
    }
    if !same_options {
        return Err(CacheMiss::OtherOptions);
    }
    if !same_folder {
        return Err(CacheMiss::FolderReplaced);
    }
    // FSEvents reports nothing for a root that can no longer be read, so check
    // it directly. The full scan then reports the error.
    if fs::read_dir(root).is_err() {
        return Err(CacheMiss::FolderUnreadable);
    }
    match volume_of(root) {
        None => return Err(CacheMiss::VolumeUnknown),
        Some(volume) if volume != header.volume => return Err(CacheMiss::VolumeChanged),
        Some(_) => {}
    }
    if now().saturating_sub(header.full_scan_at) > MAX_AGE_SECS {
        return Err(CacheMiss::Expired);
    }
    Ok(())
}

/// The number of different paths in `changes`. It compares the paths as they
/// are and ignores the flags.
#[cfg(target_os = "macos")]
fn distinct_paths(changes: &[(PathBuf, bool)]) -> usize {
    let mut paths: Vec<&PathBuf> = changes.iter().map(|(p, _)| p).collect();
    paths.sort();
    paths.dedup();
    paths.len()
}

/// Lists the changed folders again, and the folders that FSEvents cannot
/// report on. Returns the reason when a full scan is simpler or needed.
/// A cancelled listing is the inner error.
#[cfg(target_os = "macos")]
fn apply(
    dir: &mut Dir,
    root: &Path,
    mut changes: Vec<(PathBuf, bool)>,
    opts: &Options,
    progress: &Progress,
) -> Result<io::Result<()>, CacheMiss> {
    let mut linked = Vec::new();
    refresh_dirs(dir, root, &mut changes, &mut linked);
    if changes.len() > MAX_CHANGES {
        return Err(CacheMiss::TooManyChanges {
            changes: changes.len(),
            folders: distinct_paths(&changes),
            limit: MAX_CHANGES,
        });
    }
    let found_links = match list_again(dir, root, &changes, opts, progress) {
        Ok(found) => found,
        Err(e) => return listing_stopped(e),
    };
    if changes.is_empty() {
        return Ok(Ok(()));
    }
    // Every link of a file must be found again in one pass, so it counts
    // once. That pass is needed when the listing found hard links, or when a
    // folder with hard links went away or lost them. Deleting one link sends
    // no event for the others.
    let mut now_linked = Vec::new();
    refresh_dirs(dir, root, &mut Vec::new(), &mut now_linked);
    linked.sort();
    now_linked.sort();
    if found_links || now_linked != linked {
        if now_linked.len() > MAX_CHANGES {
            return Err(CacheMiss::TooManyLinkedFolders {
                count: now_linked.len(),
                limit: MAX_CHANGES,
            });
        }
        let all: Vec<_> = now_linked.into_iter().map(|p| (p, false)).collect();
        if let Err(e) = list_again(dir, root, &all, opts, progress) {
            return listing_stopped(e);
        }
    }
    Ok(Ok(()))
}

/// A cancelled listing ends the scan. Any other error needs a full scan.
#[cfg(target_os = "macos")]
fn listing_stopped(e: io::Error) -> Result<io::Result<()>, CacheMiss> {
    if e.kind() == io::ErrorKind::Interrupted {
        Ok(Err(e))
    } else {
        Err(CacheMiss::ListFailed(e.to_string()))
    }
}

/// Adds the directories that FSEvents cannot report on to `out`:
/// - folders that could not be read (granting Full Disk Access sends no event),
/// - directories with entries that failed.
///
/// Adds the directories with hard-linked files to `linked`. Deleting one link
/// sends no event for the others.
fn refresh_dirs(dir: &Dir, path: &Path, out: &mut Vec<(PathBuf, bool)>, linked: &mut Vec<PathBuf>) {
    let (mut failed, mut multi_link) = (false, false);
    for e in &dir.entries {
        if e.kind == Kind::Dir {
            let child = path.join(crate::tree::os_name(dir.name(e)));
            if e.has(flag::ERROR) {
                out.push((child, true));
            } else if let Some(sub) = &e.dir {
                refresh_dirs(sub, &child, out, linked);
            }
        } else {
            failed |= e.has(flag::ERROR);
            multi_link |= e.has(flag::MULTI_LINK);
        }
    }
    if failed {
        out.push((path.to_path_buf(), false));
    }
    if multi_link {
        linked.push(path.to_path_buf());
    }
}

fn list_again(
    dir: &mut Dir,
    root: &Path,
    changes: &[(PathBuf, bool)],
    opts: &Options,
    progress: &Progress,
) -> io::Result<bool> {
    let refs: Vec<_> = changes
        .iter()
        .map(|(path, recursive)| crate::scan::Change {
            path,
            recursive: *recursive,
        })
        .collect();
    crate::scan::update(dir, root, &refs, opts, progress)
}

fn error_count(dir: &Dir) -> usize {
    dir.entries
        .iter()
        .map(|e| usize::from(e.has(flag::ERROR)) + e.dir.as_deref().map_or(0, error_count))
        .sum()
}

/// macOS shows the data volume through firmlinks: `/Users` is
/// `/System/Volumes/Data/Users`. FSEvents may report either form, so an event
/// counts for every form of its path.
fn aliases(path: &Path) -> Vec<PathBuf> {
    aliases_with(path, firmlinks())
}

/// The firmlinks of the system volume and the data folders they lead to.
/// Empty before macOS 10.15, which has no firmlinks.
pub fn firmlinks() -> &'static [(PathBuf, PathBuf)] {
    static TABLE: std::sync::OnceLock<Vec<(PathBuf, PathBuf)>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        fs::read_to_string("/usr/share/firmlinks")
            .map_or_else(|_| Vec::new(), |text| parse_firmlinks(&text))
    })
}

fn parse_firmlinks(text: &str) -> Vec<(PathBuf, PathBuf)> {
    text.lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(link, data)| {
            (
                PathBuf::from(link),
                Path::new("/System/Volumes/Data").join(data),
            )
        })
        .collect()
}

fn aliases_with(path: &Path, table: &[(PathBuf, PathBuf)]) -> Vec<PathBuf> {
    let mut out = vec![path.to_path_buf()];
    for (link, data) in table {
        if let Ok(rest) = path.strip_prefix(link) {
            out.push(data.join(rest));
        } else if let Ok(rest) = path.strip_prefix(data) {
            out.push(link.join(rest));
        }
    }
    out
}

/// A file system mounted below the scanned root.
#[derive(Clone, Debug, PartialEq)]
pub struct Mount {
    pub path: PathBuf,
    /// The FSEvents history of the volume, or `None` when it keeps none.
    pub volume: Option<[u8; 16]>,
    /// Identifies the mounted file system, so a different one at the same
    /// path is noticed.
    pub fsid: u64,
    pub read_only: bool,
}

/// Mounts that appeared, disappeared, or changed must be listed again:
/// FSEvents does not replay mount changes. So must kept mounts without a
/// history, unless they are read-only and cannot change. With `-x`, mount
/// points are only flagged, so listing the parent again is enough.
fn mount_changes(before: &[Mount], now: &[Mount], one_fs: bool) -> Vec<(PathBuf, bool)> {
    let trusted =
        |m: &Mount| (m.volume.is_some() || m.read_only) && before.contains(m) && now.contains(m);
    let mut paths: Vec<&PathBuf> = before
        .iter()
        .chain(now)
        .filter(|m| !trusted(m))
        .map(|m| &m.path)
        .collect();
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .filter_map(|p| {
            if one_fs {
                Some((p.parent()?.to_path_buf(), false))
            } else {
                Some((p.clone(), true))
            }
        })
        .collect()
}

/// Running as root (for example with `sudo -E`) would create root-owned cache
/// files in the user's home folder and block later saves.
#[cfg(target_os = "macos")]
fn cache_allowed() -> bool {
    let uid = unsafe { libc::geteuid() };
    uid != 0
}

/// The cache needs FSEvents, which only macOS has.
#[cfg(not(target_os = "macos"))]
fn cache_allowed() -> bool {
    false
}

#[cfg(unix)]
fn identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn identity(_: &Path) -> Option<(u64, u64)> {
    None
}

fn new_header(
    root: &Path,
    opts: &Options,
    event_id: u64,
    scan_secs: f64,
    mounts: Vec<Mount>,
    items: u64,
) -> Option<Header> {
    let (root_dev, root_ino) = identity(root)?;
    Some(Header {
        root: root.to_path_buf(),
        root_dev,
        root_ino,
        one_fs: opts.one_fs,
        volume: volume_of(root)?,
        mounts,
        event_id,
        scan_secs,
        saved_at: now(),
        full_scan_at: now(),
        items,
    })
}

#[cfg(target_os = "macos")]
fn current_event_id() -> Option<u64> {
    crate::fsevents::current_event_id()
}

#[cfg(target_os = "macos")]
fn volume_of(path: &Path) -> Option<[u8; 16]> {
    crate::fsevents::volume_uuid(identity(path)?.0)
}

#[cfg(target_os = "macos")]
fn mounts_under(root: &Path) -> Vec<Mount> {
    const MNT_RDONLY: u32 = 0x1;
    // getmntinfo shares one global buffer between threads, so use getfsstat
    // with a buffer of our own.
    let mut list: Vec<libc::statfs> = Vec::new();
    for _ in 0..4 {
        let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
        if count <= 0 {
            return Vec::new();
        }
        // Room for mounts that appear between the two calls.
        let capacity = count as usize + 8;
        list = Vec::with_capacity(capacity);
        let bytes = (capacity * size_of::<libc::statfs>()) as libc::c_int;
        let filled = unsafe { libc::getfsstat(list.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
        if filled < 0 {
            return Vec::new();
        }
        if (filled as usize) < capacity {
            // SAFETY: getfsstat wrote `filled` complete entries.
            unsafe { list.set_len(filled as usize) };
            break;
        }
        list.clear();
    }
    let mut mounts: Vec<Mount> = list
        .iter()
        .filter_map(|fs| {
            // SAFETY: f_mntonname is a NUL-terminated C string written by the kernel.
            let name = unsafe { std::ffi::CStr::from_ptr(fs.f_mntonname.as_ptr()) };
            let path = PathBuf::from(crate::tree::os_name(name.to_bytes()));
            // SAFETY: fsid_t is two 32-bit integers with a private field.
            let fsid: [u32; 2] = unsafe { std::mem::transmute_copy(&fs.f_fsid) };
            (path != root && path.starts_with(root)).then(|| Mount {
                volume: volume_of(&path),
                fsid: u64::from(fsid[0]) << 32 | u64::from(fsid[1]),
                read_only: fs.f_flags & MNT_RDONLY != 0,
                path,
            })
        })
        .collect();
    mounts.sort_by(|a, b| a.path.cmp(&b.path));
    mounts
}

#[cfg(not(target_os = "macos"))]
fn current_event_id() -> Option<u64> {
    None
}

#[cfg(not(target_os = "macos"))]
fn volume_of(_: &Path) -> Option<[u8; 16]> {
    None
}

#[cfg(not(target_os = "macos"))]
fn mounts_under(_: &Path) -> Vec<Mount> {
    Vec::new()
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub root: PathBuf,
    /// Device and inode of the root, so a replaced root is not mistaken for the old one.
    pub root_dev: u64,
    pub root_ino: u64,
    pub one_fs: bool,
    pub volume: [u8; 16],
    pub mounts: Vec<Mount>,
    /// FSEvents position taken before the scan started, so changes made
    /// during the scan are replayed next time.
    pub event_id: u64,
    pub scan_secs: f64,
    /// Seconds since the Unix epoch.
    pub saved_at: u64,
    /// When the whole tree was last scanned. Incremental updates keep it, so
    /// the age limit also applies to roots that are scanned every day.
    pub full_scan_at: u64,
    /// Items in the saved tree. [`encode`] writes the count of the tree that
    /// it saves, so the progress screen can estimate the time of a full scan.
    pub items: u64,
}

fn path_hash(root: &Path) -> u64 {
    root.as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// `~/Library/Caches/minimenta/<hash of the root path>.bin`
pub fn file_for(root: &Path) -> Option<PathBuf> {
    let home = std::env::home_dir()?;
    Some(home.join(format!(
        "Library/Caches/minimenta/{:016x}.bin",
        path_hash(root)
    )))
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

pub fn encode(header: &Header, dir: &Dir) -> Vec<u8> {
    let mut out = Vec::with_capacity(128 + dir.names.len() + dir.entries.len() * 40);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    put_bytes(&mut out, header.root.as_os_str().as_encoded_bytes());
    out.extend_from_slice(&header.root_dev.to_le_bytes());
    out.extend_from_slice(&header.root_ino.to_le_bytes());
    out.push(u8::from(header.one_fs));
    out.extend_from_slice(&header.volume);
    out.extend_from_slice(&(header.mounts.len() as u32).to_le_bytes());
    for m in &header.mounts {
        put_bytes(&mut out, m.path.as_os_str().as_encoded_bytes());
        out.push(u8::from(m.volume.is_some()));
        out.extend_from_slice(&m.volume.unwrap_or_default());
        out.extend_from_slice(&m.fsid.to_le_bytes());
        out.push(u8::from(m.read_only));
    }
    out.extend_from_slice(&header.event_id.to_le_bytes());
    out.extend_from_slice(&header.scan_secs.to_le_bytes());
    out.extend_from_slice(&header.saved_at.to_le_bytes());
    out.extend_from_slice(&header.full_scan_at.to_le_bytes());
    out.extend_from_slice(&dir.totals().items.to_le_bytes());
    encode_dir(&mut out, dir);
    out
}

fn encode_dir(out: &mut Vec<u8>, dir: &Dir) {
    out.extend_from_slice(&dir.id.to_le_bytes());
    out.extend_from_slice(&dir.own_disk.to_le_bytes());
    put_bytes(out, &dir.names);
    out.extend_from_slice(&(dir.entries.len() as u32).to_le_bytes());
    for e in &dir.entries {
        out.extend_from_slice(&e.name_start.to_le_bytes());
        out.extend_from_slice(&e.name_len.to_le_bytes());
        out.extend_from_slice(&e.disk.to_le_bytes());
        out.extend_from_slice(&e.apparent.to_le_bytes());
        out.extend_from_slice(&e.items.to_le_bytes());
        out.push(e.kind as u8);
        out.push(e.flags & flag::PERSISTENT);
        out.push(u8::from(e.dir.is_some()));
        if let Some(sub) = &e.dir {
            encode_dir(out, sub);
        }
    }
}

#[cfg(test)]
fn decode(bytes: &[u8]) -> Option<(Header, Dir)> {
    let (header, start) = decode_header(bytes)?;
    Some((header, decode_tree(&bytes[start..])?))
}

/// Returns the header and the position where the tree starts.
fn decode_header(bytes: &[u8]) -> Option<(Header, usize)> {
    let mut r = Reader { bytes, pos: 0 };
    if r.take(8)? != MAGIC || r.u32()? != VERSION {
        return None;
    }
    let root = r.path()?;
    let (root_dev, root_ino) = (r.u64()?, r.u64()?);
    let one_fs = r.u8()? != 0;
    let volume = r.take(16)?.try_into().ok()?;
    let mount_count = r.u32()? as usize;
    let mut mounts = Vec::with_capacity(mount_count.min(r.remaining() / 30));
    for _ in 0..mount_count {
        let path = r.path()?;
        let known = r.u8()? != 0;
        let uuid: [u8; 16] = r.take(16)?.try_into().ok()?;
        let fsid = r.u64()?;
        let read_only = r.u8()? != 0;
        mounts.push(Mount {
            path,
            volume: known.then_some(uuid),
            fsid,
            read_only,
        });
    }
    let header = Header {
        root,
        root_dev,
        root_ino,
        one_fs,
        volume,
        mounts,
        event_id: r.u64()?,
        scan_secs: f64::from_le_bytes(r.take(8)?.try_into().ok()?),
        saved_at: r.u64()?,
        full_scan_at: r.u64()?,
        items: r.u64()?,
    };
    Some((header, r.pos))
}

/// Decodes the tree that follows the header. The tree must fill `bytes`.
fn decode_tree(bytes: &[u8]) -> Option<Dir> {
    let mut r = Reader { bytes, pos: 0 };
    let dir = decode_dir(&mut r, 0)?;
    (r.pos == bytes.len()).then_some(dir)
}

/// Bytes that no stored name may contain. One check covers all names of a
/// directory.
fn valid_names(names: &[u8]) -> bool {
    !names.contains(&b'/') && !names.contains(&0)
}

/// A valid single path component, taken from names that passed
/// [`valid_names`]: not empty, not `.` or `..`. Outside Unix the name must
/// also be UTF-8, because there not every byte string is a valid `OsStr`.
fn valid_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name != b"."
        && name != b".."
        && (cfg!(unix) || std::str::from_utf8(name).is_ok())
}

/// The fixed part of an encoded entry: name start and length, disk,
/// apparent size, items, kind, flags, and whether a subtree follows.
const ENTRY_BYTES: usize = 35;

fn decode_dir(r: &mut Reader, depth: usize) -> Option<Dir> {
    if depth > MAX_DEPTH {
        return None;
    }
    let id = r.u64()?;
    let own_disk = r.u64()?;
    let names_len = r.u32()? as usize;
    let names = r.take(names_len)?;
    if !valid_names(names) {
        return None;
    }
    let count = r.u32()? as usize;
    let mut entries = Vec::with_capacity(count.min(r.remaining() / ENTRY_BYTES));
    for _ in 0..count {
        // One bounds check per entry instead of one per field.
        let e: &[u8; ENTRY_BYTES] = r.take(ENTRY_BYTES)?.try_into().ok()?;
        let u32_at = |o: usize| u32::from_le_bytes([e[o], e[o + 1], e[o + 2], e[o + 3]]);
        let u64_at = |o: usize| u64::from(u32_at(o)) | u64::from(u32_at(o + 4)) << 32;
        let (name_start, name_len) = (u32_at(0), u32_at(4));
        let name = names
            .get(name_start as usize..(name_start as usize).checked_add(name_len as usize)?)?;
        if !valid_name(name) {
            return None;
        }
        let (disk, apparent, items) = (u64_at(8), u64_at(16), u64_at(24));
        let kind = match e[32] {
            0 => Kind::File,
            1 => Kind::Dir,
            2 => Kind::Symlink,
            3 => Kind::Other,
            _ => return None,
        };
        let flags = e[33] & flag::PERSISTENT;
        let dir = if e[34] != 0 {
            Some(Box::new(decode_dir(r, depth + 1)?))
        } else {
            None
        };
        entries.push(Entry {
            name_start,
            name_len,
            disk,
            apparent,
            items,
            kind,
            flags,
            dir,
        });
    }
    // The order on disk may come from any sort, so mark it as stale.
    Some(Dir {
        names: names.to_vec(),
        entries,
        sort: None,
        id,
        own_disk,
    })
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn path(&mut self) -> Option<PathBuf> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        if cfg!(unix) {
            Some(PathBuf::from(crate::tree::os_name(bytes)))
        } else {
            Some(PathBuf::from(std::str::from_utf8(bytes).ok()?))
        }
    }
}

#[cfg(test)]
fn load(root: &Path) -> Option<(Header, Dir)> {
    decode(&fs::read(file_for(root)?).ok()?)
}

static PENDING: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

/// Writes the cache on a background thread. Call [`flush`] before the process
/// exits. The file is replaced atomically, so a crash never leaves half a file.
/// A damaged file after a power loss is rejected on load and rebuilt.
pub fn save_in_background(header: &Header, dir: &Dir) {
    let Some(path) = file_for(&header.root) else {
        return;
    };
    let bytes = encode(header, dir);
    let mut pending = PENDING.lock().unwrap();
    if let Some(previous) = pending.take() {
        let _ = previous.join();
    }
    *pending = Some(std::thread::spawn(move || {
        let _ = write_atomically(&path, &bytes);
    }));
}

pub fn flush() {
    if let Some(handle) = PENDING.lock().unwrap().take() {
        let _ = handle.join();
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("cache path has no parent"))?;
    fs::create_dir_all(dir)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let result = fs::File::create(&tmp)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Sort;

    #[test]
    fn a_scan_that_an_elevated_run_could_speed_up_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = Options {
            one_fs: false,
            threads: 2,
            cache: false,
            mft: false,
            spread: true,
        };
        let progress = Progress::default();
        progress.elevate.store(true, Ordering::Relaxed);
        let scan = scan(tmp.path(), &opts, &progress).unwrap();
        assert!(matches!(scan.source, Source::ScannedNotElevated));
    }

    fn sample() -> (Header, Dir) {
        let mut sub = Dir {
            id: 77,
            ..Dir::default()
        };
        sub.push(b"file.bin", Kind::File, 4096, 1000, 0);
        sub.push(b"link", Kind::Symlink, 0, 12, flag::HARDLINK);
        let mut root = Dir::default();
        root.push(b"sub", Kind::Dir, 0, 0, 0);
        root.push(b"top.txt", Kind::File, 8192, 5000, flag::SELECTED);
        root.attach(0, sub, 0);
        root.sort(Sort::default());
        let header = Header {
            root: PathBuf::from("/Users/me/code"),
            root_dev: 16_777_234,
            root_ino: 99,
            one_fs: true,
            volume: [7; 16],
            mounts: vec![
                Mount {
                    path: PathBuf::from("/Users/me/code/disk"),
                    volume: None,
                    fsid: 5,
                    read_only: true,
                },
                Mount {
                    path: PathBuf::from("/Users/me/code/data"),
                    volume: Some([3; 16]),
                    fsid: 6,
                    read_only: false,
                },
            ],
            event_id: 42,
            scan_secs: 1.5,
            saved_at: 1_700_000_000,
            full_scan_at: 1_699_000_000,
            items: root.totals().items,
        };
        (header, root)
    }

    /// One line per entry with its path, sizes, kind, and flags, sorted, so
    /// trees compare equal regardless of the order inside a directory.
    fn describe(dir: &Dir) -> Vec<String> {
        fn walk(dir: &Dir, prefix: &str, out: &mut Vec<String>) {
            for e in &dir.entries {
                let name = format!("{prefix}{}", String::from_utf8_lossy(dir.name(e)));
                out.push(format!(
                    "{name} {} {} {} {:?} {}",
                    e.disk, e.apparent, e.items, e.kind, e.flags
                ));
                if let Some(sub) = &e.dir {
                    walk(sub, &format!("{name}/"), out);
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, "", &mut out);
        out.sort();
        out
    }

    #[test]
    fn round_trips_the_tree_and_header_without_transient_flags() {
        let (header, dir) = sample();
        let (h2, d2) = decode(&encode(&header, &dir)).unwrap();
        assert_eq!(h2, header);
        let expected: Vec<String> = describe(&dir)
            .into_iter()
            .map(|l| l.replace(" 16", " 0"))
            .collect();
        assert_eq!(describe(&d2), expected);
        assert_eq!(
            d2.entries.iter().find_map(|e| e.dir.as_ref()).unwrap().id,
            77
        );
        assert_eq!(d2.sort, None, "the stored order must count as stale");
    }

    /// The browser saves its session header with a tree that can have
    /// changed since the scan.
    #[test]
    fn stores_the_item_count_of_the_saved_tree() {
        let (mut header, dir) = sample();
        header.items = 999;
        let (h2, _) = decode(&encode(&header, &dir)).unwrap();
        assert_eq!(h2.items, dir.totals().items);
    }

    #[test]
    fn rejects_truncated_or_foreign_files() {
        let (header, dir) = sample();
        let bytes = encode(&header, &dir);
        for cut in [0, 7, 20, bytes.len() / 2, bytes.len() - 1] {
            assert!(
                decode(&bytes[..cut]).is_none(),
                "accepted a file cut at {cut}"
            );
        }
        let mut foreign = bytes.clone();
        foreign[0] = b'X';
        assert!(decode(&foreign).is_none());
    }

    #[test]
    fn rejects_names_that_are_not_single_path_components() {
        for bad in [&b"a/b"[..], b"..", b".", b"", b"nul\0"] {
            let mut dir = Dir::default();
            dir.push(bad, Kind::File, 1, 1, 0);
            let (header, _) = sample();
            assert!(decode(&encode(&header, &dir)).is_none(), "accepted {bad:?}");
        }
    }

    #[test]
    fn rejects_trees_deeper_than_the_limit() {
        let mut dir = Dir::default();
        for _ in 0..MAX_DEPTH + 2 {
            let mut parent = Dir::default();
            parent.push(b"d", Kind::Dir, 0, 0, 0);
            parent.attach(0, dir, 0);
            dir = parent;
        }
        let (header, _) = sample();
        assert!(decode(&encode(&header, &dir)).is_none());
    }

    #[test]
    fn mounts_without_history_or_with_changes_are_listed_again() {
        let m = |p: &str, v: Option<u8>| Mount {
            path: PathBuf::from(p),
            volume: v.map(|b| [b; 16]),
            fsid: 1,
            read_only: false,
        };
        let before = [
            m("/r/kept", Some(1)),
            m("/r/gone", Some(2)),
            m("/r/nolog", None),
            m("/r/swapped", Some(3)),
        ];
        let now = [
            m("/r/kept", Some(1)),
            m("/r/new", Some(4)),
            m("/r/nolog", None),
            m("/r/swapped", Some(5)),
        ];
        let paths = |one_fs| {
            let mut c: Vec<_> = mount_changes(&before, &now, one_fs)
                .into_iter()
                .map(|(p, r)| (p.display().to_string(), r))
                .collect();
            c.sort();
            c
        };
        let full = ["gone", "new", "nolog", "swapped"].map(|n| (format!("/r/{n}"), true));
        assert_eq!(paths(false), full);
        assert!(
            paths(true).iter().all(|(p, r)| p == "/r" && !r),
            "with -x the parent is listed again"
        );
    }

    #[test]
    fn read_only_mounts_without_history_are_trusted_until_they_change() {
        let sealed = Mount {
            path: PathBuf::from("/r/sealed"),
            volume: None,
            fsid: 9,
            read_only: true,
        };
        let remounted = Mount {
            fsid: 10,
            ..sealed.clone()
        };
        let unchanged = std::slice::from_ref(&sealed);
        assert_eq!(mount_changes(unchanged, unchanged, false), Vec::new());
        assert_eq!(
            mount_changes(&[sealed], &[remounted], false),
            [(PathBuf::from("/r/sealed"), true)]
        );
    }

    #[test]
    fn events_count_for_both_firmlink_forms() {
        let table = parse_firmlinks("/Users\tUsers\n/usr/local\tusr/local\n");
        let forms = |p: &str| aliases_with(Path::new(p), &table);
        assert_eq!(
            forms("/Users/me/a"),
            [
                PathBuf::from("/Users/me/a"),
                PathBuf::from("/System/Volumes/Data/Users/me/a")
            ]
        );
        assert_eq!(
            forms("/System/Volumes/Data/usr/local/x"),
            [
                PathBuf::from("/System/Volumes/Data/usr/local/x"),
                PathBuf::from("/usr/local/x")
            ]
        );
        assert_eq!(forms("/tmp/x"), [PathBuf::from("/tmp/x")]);
    }

    #[test]
    fn cache_files_differ_per_root() {
        assert_ne!(file_for(Path::new("/a")), file_for(Path::new("/b")));
    }

    /// The progress screen shows `Reason: ` and the text on one line of an
    /// 80-column terminal. The tests of the progress screen draw the longest
    /// texts, so they check the real prefix.
    #[cfg(target_os = "macos")]
    fn assert_text(cases: impl IntoIterator<Item = (CacheMiss, &'static str)>) {
        for (miss, text) in cases {
            assert_eq!(miss.to_string(), text);
            let line = format!("  Reason: {text}");
            assert!(
                line.chars().count() <= 80,
                "does not fit 80 columns: {line}"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_reasons_about_the_cache_and_the_folder_say_what_is_wrong() {
        assert_text([
            (CacheMiss::NoFile, "There is no cache file."),
            (
                CacheMiss::Expired,
                "The last full scan is more than 7 days old.",
            ),
            (
                CacheMiss::OtherOptions,
                "The cache is from a scan with other options or another path.",
            ),
            (
                CacheMiss::FolderReplaced,
                "The folder was replaced since the last scan.",
            ),
            (CacheMiss::FolderUnreadable, "The folder cannot be read."),
            (
                CacheMiss::VolumeUnknown,
                "FSEvents cannot identify the volume of the folder.",
            ),
            (
                CacheMiss::VolumeChanged,
                "The volume of the folder changed since the last scan.",
            ),
            (
                CacheMiss::FileUnreadable("Permission denied (os error 13)".into()),
                "The cache file cannot be read: Permission denied (os error 13)",
            ),
            (
                CacheMiss::FileDamaged,
                "The cache file is damaged or has another format version.",
            ),
        ]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_reasons_about_fsevents_name_the_limit_or_the_gap() {
        use crate::fsevents::ReplayError::{
            EventsDropped, IdsWrapped, RootChanged, StreamFailed, TimedOut, Unavailable,
        };
        use std::time::Duration;
        assert_text([
            (CacheMiss::Replay(Unavailable), "FSEvents is not available."),
            (
                CacheMiss::Replay(StreamFailed),
                "FSEvents could not start a stream for the folder.",
            ),
            (
                CacheMiss::Replay(TimedOut(Duration::from_millis(39_480))),
                "FSEvents did not send the changes within 39.5 s. That is the limit.",
            ),
            (
                CacheMiss::Replay(TimedOut(Duration::from_secs(1800))),
                "FSEvents did not send the changes within 1800.0 s. That is the limit.",
            ),
            (
                CacheMiss::Replay(EventsDropped),
                "FSEvents dropped events, so its history has gaps.",
            ),
            (
                CacheMiss::Replay(IdsWrapped),
                "The FSEvents event IDs wrapped around.",
            ),
            (
                CacheMiss::Replay(RootChanged),
                "FSEvents reported a change in the path to the folder.",
            ),
            (
                CacheMiss::OutsideRoot,
                "FSEvents named a path outside the folder. The folder may have moved.",
            ),
        ]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_reasons_about_the_limits_name_the_counts_that_the_check_compared() {
        let too_many = |changes, folders| CacheMiss::TooManyChanges {
            changes,
            folders,
            limit: 20_000,
        };
        assert_text([
            (
                too_many(31_204, 2_113),
                "31204 changes in 2113 folders. The limit is 20000 changes.",
            ),
            (
                too_many(1_234_567, 123_456),
                "1234567 changes in 123456 folders. The limit is 20000 changes.",
            ),
            (
                too_many(20_001, 1),
                "20001 changes in 1 folder. The limit is 20000 changes.",
            ),
            (
                CacheMiss::TooManyLinkedFolders {
                    count: 25_000,
                    limit: 20_000,
                },
                "25000 folders have hard links. The limit is 20000 folders.",
            ),
            (
                CacheMiss::ListFailed("Permission denied (os error 13)".into()),
                "A folder could not be listed again: Permission denied (os error 13)",
            ),
        ]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn only_a_missing_cache_file_gives_no_message() {
        use crate::fsevents::ReplayError;
        assert!(CacheMiss::NoFile.is_silent());
        for miss in [
            CacheMiss::Expired,
            CacheMiss::FileDamaged,
            CacheMiss::Replay(ReplayError::Cancelled),
            CacheMiss::ListFailed(String::new()),
        ] {
            assert!(!miss.is_silent(), "{miss:?}");
        }
    }

    /// Each test reproduces a case where an earlier version showed different
    /// data from the cache than a fresh scan.
    #[cfg(target_os = "macos")]
    mod incremental {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;
        use std::time::{Duration, Instant};

        struct Fixture {
            tmp: tempfile::TempDir,
            root: PathBuf,
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_file(file_for(&self.root).unwrap());
                // Make every folder writable again, so the temporary folder can be removed.
                let _ = Command::new("chmod")
                    .args(["-R", "u+rwx"])
                    .arg(self.tmp.path())
                    .status();
            }
        }

        /// FSEvents does not record every temporary folder, so stay below the
        /// project directory, where it always records changes.
        fn fixture() -> Fixture {
            let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
            fs::create_dir_all(&base).unwrap();
            let tmp = tempfile::tempdir_in(&base).unwrap();
            let root = tmp.path().canonicalize().unwrap().join("root");
            fs::create_dir(&root).unwrap();
            Fixture { tmp, root }
        }

        fn write(root: &Path, rel: &str, len: usize) {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![1u8; len]).unwrap();
        }

        fn opts() -> Options {
            Options {
                one_fs: false,
                threads: 4,
                cache: true,
                mft: false,
                spread: true,
            }
        }

        /// Waits until FSEvents reports a change, or a short while for changes
        /// that produce no event at all.
        fn settle(root: &Path, since: u64) {
            let deadline = Instant::now() + Duration::from_secs(3);
            let never = std::sync::atomic::AtomicBool::new(false);
            while Instant::now() < deadline {
                let seen =
                    crate::fsevents::changes_since(root, since, Duration::from_secs(2), &never);
                if seen.is_ok_and(|c| !c.is_empty()) {
                    std::thread::sleep(Duration::from_millis(300));
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }

        /// Scans fully to write the cache, applies `change`, then compares a
        /// run that may use the cache with a fresh full scan.
        fn check(root: &Path, change: impl FnOnce()) -> Source {
            check_with_reason(root, change).0
        }

        /// Like [`check`], and also returns why the run could not use the cache.
        fn check_with_reason(root: &Path, change: impl FnOnce()) -> (Source, Option<String>) {
            // Setup events can get IDs after the cache position. Let them settle
            // first, or the replay sees the root being created.
            std::thread::sleep(Duration::from_millis(1500));
            let _ = fs::remove_file(file_for(root).unwrap());
            let first = scan(root, &opts(), &Progress::default()).unwrap();
            assert!(matches!(first.source, Source::Scanned));
            flush();
            let (header, _) = load(root).expect("the first scan writes the cache");
            change();
            settle(root, header.event_id);
            let progress = Progress::default();
            let cached = scan(root, &opts(), &progress).unwrap();
            flush();
            let fresh = crate::scan::scan(root, &opts(), &Progress::default()).unwrap();
            assert_eq!(describe(&cached.dir), describe(&fresh));
            assert_eq!(cached.dir.totals(), fresh.totals());
            (cached.source, progress.cache_reason.get().cloned())
        }

        #[test]
        fn files_and_folders_added_grown_and_removed() {
            let f = fixture();
            write(&f.root, "a/one.bin", 1000);
            write(&f.root, "b/c/x.bin", 30_000);
            write(&f.root, "d/gone.bin", 50_000);
            let source = check(&f.root, || {
                write(&f.root, "a/new.bin", 5000);
                write(&f.root, "a/one.bin", 70_000);
                fs::remove_dir_all(f.root.join("d")).unwrap();
                write(&f.root, "e/f/g.bin", 9000);
                fs::remove_file(f.root.join("b/c/x.bin")).unwrap();
            });
            assert!(matches!(source, Source::Cached { listed, .. } if listed >= 4));
        }

        #[test]
        fn a_folder_swapped_with_a_new_build_is_not_reused_by_name() {
            let f = fixture();
            write(&f.root, "build/lib/big.bin", 300_000);
            write(&f.root, "build.new/lib/small.bin", 10);
            let source = check(&f.root, || {
                fs::rename(f.root.join("build"), f.root.join("build.old")).unwrap();
                fs::rename(f.root.join("build.new"), f.root.join("build")).unwrap();
            });
            assert!(matches!(source, Source::Cached { .. }));
        }

        #[test]
        fn swapped_sibling_folders_keep_their_own_contents() {
            let f = fixture();
            write(&f.root, "a/x/deep/big.bin", 200_000);
            write(&f.root, "a/y/deep/small.bin", 20);
            check(&f.root, || {
                let a = f.root.join("a");
                fs::rename(a.join("x"), a.join("tmp")).unwrap();
                fs::rename(a.join("y"), a.join("x")).unwrap();
                fs::rename(a.join("tmp"), a.join("y")).unwrap();
            });
        }

        /// A cache that is used every day still gets a full scan after the age limit.
        #[test]
        fn the_age_limit_counts_from_the_last_full_scan() {
            let f = fixture();
            write(&f.root, "a.bin", 1000);
            scan(&f.root, &opts(), &Progress::default()).unwrap();
            flush();
            let (mut header, dir) = load(&f.root).unwrap();
            header.full_scan_at -= MAX_AGE_SECS + 1;
            save_in_background(&header, &dir);
            flush();
            let run = scan(&f.root, &opts(), &Progress::default()).unwrap();
            flush();
            assert!(
                matches!(run.source, Source::Scanned),
                "used a cache older than the limit"
            );
        }

        /// The volume ID comes from FSEvents. A cache of a volume with another
        /// ID has event numbers that mean nothing now.
        #[test]
        fn a_cache_of_another_volume_gives_a_reason() {
            use std::sync::atomic::Ordering::Relaxed;
            let f = fixture();
            write(&f.root, "a.bin", 1000);
            scan(&f.root, &opts(), &Progress::default()).unwrap();
            flush();
            let (mut header, dir) = load(&f.root).unwrap();
            header.volume[0] ^= 1;
            save_in_background(&header, &dir);
            flush();
            let progress = Progress::default();
            let run = scan(&f.root, &opts(), &progress).unwrap();
            flush();
            assert!(matches!(run.source, Source::Scanned));
            assert!(progress.cache_unusable.load(Relaxed));
            assert_eq!(
                progress.cache_reason.get().map(String::as_str),
                Some("The volume of the folder changed since the last scan.")
            );
        }

        /// The scan marks that the cache could not be used, and the full scan
        /// counts from 0 again. The item count of the cached scan
        /// stays for the estimate of the time left.
        #[test]
        fn a_cache_that_cannot_be_used_restarts_the_counters() {
            use std::sync::atomic::Ordering::Relaxed;
            let f = fixture();
            write(&f.root, "a/b.bin", 1000);
            let first = Progress::default();
            let before = scan(&f.root, &opts(), &first).unwrap().dir.totals().items;
            flush();
            assert!(!first.checking_cache.load(Relaxed));
            assert!(
                !first.cache_unusable.load(Relaxed),
                "there was no cache yet"
            );
            assert_eq!(first.cache_reason.get(), None, "there was no cache yet");
            let (mut header, dir) = load(&f.root).unwrap();
            header.full_scan_at -= MAX_AGE_SECS + 1;
            save_in_background(&header, &dir);
            flush();
            let progress = Progress::default();
            // Stands for the folders that a check lists before it stops.
            progress.items.store(1000, Relaxed);
            progress.errors.store(1, Relaxed);
            let run = scan(&f.root, &opts(), &progress).unwrap();
            flush();
            assert!(matches!(run.source, Source::Scanned));
            assert!(!progress.checking_cache.load(Relaxed));
            assert!(progress.cache_unusable.load(Relaxed));
            assert_eq!(
                progress.cache_reason.get().map(String::as_str),
                Some("The last full scan is more than 7 days old.")
            );
            assert_eq!(progress.items.load(Relaxed), run.dir.totals().items);
            assert_eq!(progress.errors.load(Relaxed), 0);
            assert_eq!(progress.expected_items.load(Relaxed), before);
        }

        /// A count from a scan with other options would make the estimate wrong.
        #[test]
        fn a_cache_of_another_scan_scope_gives_no_count() {
            use std::sync::atomic::Ordering::Relaxed;
            let f = fixture();
            write(&f.root, "a/b.bin", 1000);
            scan(&f.root, &opts(), &Progress::default()).unwrap();
            flush();
            let progress = Progress::default();
            let one_fs = Options {
                one_fs: true,
                ..opts()
            };
            let run = scan(&f.root, &one_fs, &progress).unwrap();
            flush();
            assert!(matches!(run.source, Source::Scanned));
            assert!(progress.cache_unusable.load(Relaxed));
            assert_eq!(
                progress.cache_reason.get().map(String::as_str),
                Some("The cache is from a scan with other options or another path.")
            );
            assert_eq!(progress.expected_items.load(Relaxed), 0);
        }

        /// FSEvents reports `/Users/...` paths for a root given as
        /// `/System/Volumes/Data/Users/...`.
        #[test]
        fn a_root_given_through_the_data_volume_sees_changes() {
            let f = fixture();
            let data_form =
                Path::new("/System/Volumes/Data").join(f.root.strip_prefix("/").unwrap());
            if !data_form.exists() {
                eprintln!("skipped: the fixture is not on a firmlinked path");
                return;
            }
            write(&f.root, "a/one.bin", 1000);
            let source = check(&data_form, || write(&f.root, "a/two.bin", 50_000));
            assert!(
                matches!(source, Source::Cached { listed, .. } if listed >= 1),
                "missed the change"
            );
            let _ = fs::remove_file(file_for(&data_form).unwrap());
        }

        #[test]
        fn a_root_renamed_away_changed_and_renamed_back_is_scanned_fully() {
            let f = fixture();
            write(&f.root, "a.bin", 1000);
            let away = f.tmp.path().join("away");
            let (source, reason) = check_with_reason(&f.root, || {
                fs::rename(&f.root, &away).unwrap();
                write(&away, "b.bin", 70_000);
                fs::rename(&away, &f.root).unwrap();
            });
            assert!(matches!(source, Source::Scanned));
            let reason = reason.expect("a full scan after a failed check has a reason");
            assert!(
                [
                    "FSEvents named a path outside the folder. The folder may have moved.",
                    "FSEvents reported a change in the path to the folder.",
                ]
                .contains(&reason.as_str()),
                "unexpected reason: {reason}"
            );
        }

        /// Writes the cache of `root`, then lets `damage` change the file.
        fn cache_then(root: &Path, damage: impl FnOnce(&Path)) -> Progress {
            write(root, "a/b.bin", 1000);
            scan(root, &opts(), &Progress::default()).unwrap();
            flush();
            let file = file_for(root).unwrap();
            damage(&file);
            let progress = Progress::default();
            let run = scan(root, &opts(), &progress).unwrap();
            flush();
            assert!(matches!(run.source, Source::Scanned));
            progress
        }

        #[test]
        fn a_damaged_cache_file_gives_a_reason() {
            let f = fixture();
            let progress = cache_then(&f.root, |file| fs::write(file, b"not a cache").unwrap());
            assert!(
                progress
                    .cache_unusable
                    .load(std::sync::atomic::Ordering::Relaxed)
            );
            assert_eq!(
                progress.cache_reason.get().map(String::as_str),
                Some("The cache file is damaged or has another format version.")
            );
        }

        #[test]
        fn a_cache_file_that_cannot_be_read_gives_the_error() {
            let f = fixture();
            let progress = cache_then(&f.root, |file| {
                fs::set_permissions(file, fs::Permissions::from_mode(0o000)).unwrap();
            });
            assert_eq!(
                progress.cache_reason.get().map(String::as_str),
                Some("The cache file cannot be read: Permission denied (os error 13)")
            );
        }

        /// The header is good, so the checks and the replay pass. Only the
        /// decoder of the tree finds the damage.
        #[test]
        fn a_cache_file_with_a_damaged_tree_gives_a_reason() {
            let f = fixture();
            write(&f.root, "a/b.bin", 1000);
            // Let the setup events get IDs before the cache position, so the
            // replay sees no event at all.
            std::thread::sleep(Duration::from_millis(1500));
            scan(&f.root, &opts(), &Progress::default()).unwrap();
            flush();
            let file = file_for(&f.root).unwrap();
            let bytes = fs::read(&file).unwrap();
            let (_, tree_start) = decode_header(&bytes).unwrap();
            fs::write(&file, &bytes[..tree_start + 3]).unwrap();
            let progress = Progress::default();
            let run = scan(&f.root, &opts(), &progress).unwrap();
            flush();
            assert!(matches!(run.source, Source::Scanned));
            assert_eq!(
                progress.cache_reason.get().map(String::as_str),
                Some("The cache file is damaged or has another format version.")
            );
        }

        /// The user cancels while the check replays the history. The scan stops,
        /// so the screen has no message and no reason.
        #[test]
        fn a_cancelled_check_gives_no_reason() {
            use std::sync::atomic::Ordering::Relaxed;
            let f = fixture();
            write(&f.root, "a/b.bin", 1000);
            scan(&f.root, &opts(), &Progress::default()).unwrap();
            flush();
            let progress = Progress::default();
            progress.cancel.store(true, Relaxed);
            let error = scan(&f.root, &opts(), &progress)
                .err()
                .expect("a cancelled scan fails");
            flush();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            assert!(!progress.cache_unusable.load(Relaxed));
            assert_eq!(progress.cache_reason.get(), None);
        }

        /// Entries that the first pass compared: many events for a few folders,
        /// and the folder that the tree remembers as failed.
        #[test]
        fn too_many_changes_for_few_folders_give_both_counts_and_list_nothing() {
            use std::sync::atomic::Ordering::Relaxed;
            let f = fixture();
            let (_, mut dir) = sample();
            dir.push(b"broken.bin", Kind::File, 0, 0, flag::ERROR);
            let before = describe(&dir);
            let raw = MAX_CHANGES + 5_000;
            let changes: Vec<(PathBuf, bool)> = (0..raw)
                .map(|i| (f.root.join(format!("d{}", i % 3)), i % 2 == 0))
                .collect();
            let progress = Progress::default();
            let miss = apply(&mut dir, &f.root, changes, &opts(), &progress)
                .expect_err("more changes than the limit need a full scan");
            // The failed file adds the root as a fourth path and a 25,001st entry.
            assert_eq!(
                miss,
                CacheMiss::TooManyChanges {
                    changes: raw + 1,
                    folders: 4,
                    limit: MAX_CHANGES
                }
            );
            assert_eq!(
                miss.to_string(),
                "25001 changes in 4 folders. The limit is 20000 changes."
            );
            assert_eq!(progress.items.load(Relaxed), 0, "listed a folder");
            assert_eq!(describe(&dir), before);
        }

        #[test]
        fn as_many_changes_as_the_limit_are_still_listed_again() {
            let f = fixture();
            let changes: Vec<(PathBuf, bool)> = (0..MAX_CHANGES)
                .map(|i| (f.root.join(format!("missing/{}", i % 3)), false))
                .collect();
            let mut dir = Dir::default();
            let result = apply(&mut dir, &f.root, changes, &opts(), &Progress::default());
            assert!(matches!(result, Ok(Ok(()))), "{result:?}");
        }

        #[test]
        fn a_folder_that_cannot_be_listed_again_gives_the_error() {
            let f = fixture();
            let gone = f.root.join("gone");
            let changes = vec![(gone.join("a"), false)];
            let mut dir = Dir::default();
            let miss = apply(&mut dir, &gone, changes, &opts(), &Progress::default())
                .expect_err("a root that is gone needs a full scan");
            assert_eq!(
                miss,
                CacheMiss::ListFailed("No such file or directory (os error 2)".into())
            );
        }

        /// The user presses Esc while the check lists the changed folders again.
        /// The cancel is an error of the scan and not a reason.
        #[test]
        fn a_cancel_while_listing_again_is_not_a_cache_miss() {
            let f = fixture();
            let progress = Progress::default();
            progress
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let changes = vec![(f.root.join("a"), false)];
            let mut dir = Dir::default();
            let result = apply(&mut dir, &f.root, changes, &opts(), &progress);
            assert!(
                matches!(&result, Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted),
                "{result:?}"
            );
        }

        /// A change removes the links of one folder, so the check must list
        /// every folder with links again. One folder more than the limit has links.
        #[test]
        fn more_folders_with_links_than_the_limit_give_the_count_and_the_limit() {
            let f = fixture();
            let mut dir = Dir::default();
            for i in 0..=MAX_CHANGES + 1 {
                let mut sub = Dir::default();
                sub.push(b"f", Kind::File, 0, 1, flag::MULTI_LINK);
                dir.push(format!("d{i}").as_bytes(), Kind::Dir, 0, 0, 0);
                dir.attach(i, sub, 0);
            }
            // The folder is not on disk, so listing it again gives an error
            // and the tree no longer counts its links.
            let changes = vec![(f.root.join("d0"), true)];
            let miss = apply(&mut dir, &f.root, changes, &opts(), &Progress::default())
                .expect_err("more folders with links than the limit need a full scan");
            assert_eq!(
                miss,
                CacheMiss::TooManyLinkedFolders {
                    count: MAX_CHANGES + 1,
                    limit: MAX_CHANGES
                }
            );
            assert_eq!(
                miss.to_string(),
                "20001 folders have hard links. The limit is 20000 folders."
            );
        }

        #[test]
        fn distinct_paths_count_each_path_once_whatever_the_flag() {
            let path = |p: &str, recursive| (PathBuf::from(p), recursive);
            let changes = [
                path("/r/a", false),
                path("/r/a", true),
                path("/r/b", false),
                path("/r/a", false),
            ];
            assert_eq!(distinct_paths(&changes), 2);
            assert_eq!(distinct_paths(&[]), 0);
        }

        /// pnpm stores and local git clones create hard links like this.
        #[test]
        fn deleting_the_counted_hard_link_counts_the_other_one() {
            let f = fixture();
            write(&f.root, "a/f.bin", 700_000);
            fs::create_dir_all(f.root.join("b")).unwrap();
            fs::hard_link(f.root.join("a/f.bin"), f.root.join("b/g.bin")).unwrap();
            check(&f.root, || fs::remove_file(f.root.join("a/f.bin")).unwrap());
        }

        /// Writes the cache, applies `change`, and returns the cached result
        /// with the number of items listed again.
        fn cached_after(root: &Path, change: impl FnOnce()) -> (Scan, u64) {
            std::thread::sleep(Duration::from_millis(1500));
            let _ = fs::remove_file(file_for(root).unwrap());
            scan(root, &opts(), &Progress::default()).unwrap();
            flush();
            let (header, _) = load(root).expect("the first scan writes the cache");
            change();
            settle(root, header.event_id);
            let progress = Progress::default();
            let cached = scan(root, &opts(), &progress).unwrap();
            flush();
            assert!(matches!(cached.source, Source::Cached { .. }));
            assert_eq!(
                progress
                    .check_limit_ms
                    .load(std::sync::atomic::Ordering::Relaxed),
                0,
                "the time limit shows only during the replay"
            );
            (
                cached,
                progress.items.load(std::sync::atomic::Ordering::Relaxed),
            )
        }

        #[test]
        fn a_change_elsewhere_does_not_list_hard_linked_folders_again() {
            let f = fixture();
            write(&f.root, "a/f.bin", 700_000);
            fs::create_dir_all(f.root.join("b")).unwrap();
            fs::hard_link(f.root.join("a/f.bin"), f.root.join("b/g.bin")).unwrap();
            write(&f.root, "c/x.bin", 10);

            let (cached, listed) = cached_after(&f.root, || write(&f.root, "c/y.bin", 10));

            assert_eq!(listed, 2, "only the two files in c are listed again");
            let fresh = crate::scan::scan(&f.root, &opts(), &Progress::default()).unwrap();
            assert_eq!(cached.dir.totals(), fresh.totals());
        }

        #[test]
        fn a_change_in_the_root_does_not_list_hard_linked_folders_again() {
            let f = fixture();
            write(&f.root, "a/f.bin", 700_000);
            fs::create_dir_all(f.root.join("b")).unwrap();
            fs::hard_link(f.root.join("a/f.bin"), f.root.join("b/g.bin")).unwrap();

            let (cached, listed) = cached_after(&f.root, || write(&f.root, "new.bin", 10));

            assert_eq!(listed, 3, "only the root is listed again");
            assert_eq!(cached.dir.totals().apparent, 700_010);
        }

        #[test]
        fn moving_the_counted_link_out_of_the_root_counts_the_other_one() {
            let f = fixture();
            write(&f.root, "a/f.bin", 700_000);
            fs::create_dir_all(f.root.join("b")).unwrap();
            fs::hard_link(f.root.join("a/f.bin"), f.root.join("b/g.bin")).unwrap();
            write(&f.root, "c/x.bin", 10);

            let (cached, _) = cached_after(&f.root, || {
                let (_, tree) = load(&f.root).unwrap();
                let counted = ["a", "b"]
                    .into_iter()
                    .find(|name| {
                        let sub = tree.entries[tree.find(name.as_bytes()).unwrap()]
                            .dir
                            .as_ref();
                        sub.unwrap().entries.iter().any(|e| !e.has(flag::HARDLINK))
                    })
                    .unwrap();
                fs::rename(f.root.join(counted), f.tmp.path().join("moved")).unwrap();
            });

            assert_eq!(cached.dir.totals().apparent, 700_010);
        }

        #[test]
        fn a_new_link_in_a_folder_without_links_counts_once() {
            let f = fixture();
            write(&f.root, "a/f.bin", 700_000);
            fs::create_dir_all(f.root.join("b")).unwrap();
            fs::hard_link(f.root.join("a/f.bin"), f.root.join("b/g.bin")).unwrap();
            write(&f.root, "c/x.bin", 10);

            let (cached, _) = cached_after(&f.root, || {
                fs::hard_link(f.root.join("a/f.bin"), f.root.join("c/h.bin")).unwrap();
            });

            let fresh = crate::scan::scan(&f.root, &opts(), &Progress::default()).unwrap();
            assert_eq!(cached.dir.totals(), fresh.totals());
            assert_eq!(cached.dir.totals().apparent, 700_010);
        }

        #[test]
        fn an_unchanged_tree_is_not_saved_again() {
            let f = fixture();
            write(&f.root, "a.bin", 1000);
            std::thread::sleep(Duration::from_millis(1500));
            scan(&f.root, &opts(), &Progress::default()).unwrap();
            flush();
            let file = file_for(&f.root).unwrap();
            let written = fs::metadata(&file).unwrap().modified().unwrap();
            std::thread::sleep(Duration::from_millis(1100));
            let run = scan(&f.root, &opts(), &Progress::default()).unwrap();
            flush();
            assert!(matches!(run.source, Source::Cached { listed: 0, .. }));
            assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), written);
        }

        #[test]
        fn a_replaced_root_is_scanned_fully() {
            let f = fixture();
            write(&f.root, "src/big.bin", 100_000);
            let other = f.tmp.path().join("other");
            write(&other, "tiny.bin", 5);
            let (source, reason) = check_with_reason(&f.root, || {
                fs::rename(&f.root, f.tmp.path().join("root.old")).unwrap();
                fs::rename(&other, &f.root).unwrap();
            });
            assert!(matches!(source, Source::Scanned));
            assert_eq!(
                reason.as_deref(),
                Some("The folder was replaced since the last scan.")
            );
        }

        #[test]
        fn a_folder_that_becomes_readable_is_listed_again() {
            let f = fixture();
            write(&f.root, "a/x/big.bin", 300_000);
            fs::set_permissions(f.root.join("a/x"), fs::Permissions::from_mode(0o000)).unwrap();
            check(&f.root, || {
                fs::set_permissions(f.root.join("a/x"), fs::Permissions::from_mode(0o755)).unwrap();
            });
        }

        /// Granting Full Disk Access sends no event, so a folder that was
        /// unreadable in the cache must be listed again on every load.
        #[test]
        fn a_folder_cached_as_unreadable_is_listed_again_without_any_event() {
            let f = fixture();
            write(&f.root, "a/x/big.bin", 300_000);
            write(&f.root, "a/other.bin", 10);
            // Let the setup events get IDs before the cache position, so this
            // run really sees no event at all.
            std::thread::sleep(Duration::from_millis(1500));
            scan(&f.root, &opts(), &Progress::default()).unwrap();
            flush();
            let (header, mut dir) = load(&f.root).unwrap();
            let a = dir.find(b"a").unwrap();
            dir.entries[a].flags |= flag::SUB_ERROR;
            let a = dir.entries[a].dir.as_deref_mut().unwrap();
            let x = a.find(b"x").unwrap();
            let id = a.entries[x].dir.as_ref().unwrap().id;
            a.entries[x].dir = Some(Box::new(Dir {
                id,
                ..Dir::default()
            }));
            a.entries[x].flags |= flag::ERROR;
            save_in_background(&header, &dir);
            flush();

            let cached = scan(&f.root, &opts(), &Progress::default()).unwrap();
            flush();
            let fresh = crate::scan::scan(&f.root, &opts(), &Progress::default()).unwrap();
            assert!(matches!(cached.source, Source::Cached { .. }));
            assert_eq!(describe(&cached.dir), describe(&fresh));
        }

        #[test]
        fn an_unreadable_root_fails_instead_of_saving_an_empty_tree() {
            let f = fixture();
            write(&f.root, "keep.bin", 1000);
            scan(&f.root, &opts(), &Progress::default()).unwrap();
            flush();
            let (header, _) = load(&f.root).unwrap();
            write(&f.root, "new.bin", 10);
            settle(&f.root, header.event_id);
            fs::set_permissions(&f.root, fs::Permissions::from_mode(0o000)).unwrap();
            let progress = Progress::default();
            let result = scan(&f.root, &opts(), &progress);
            flush();
            fs::set_permissions(&f.root, fs::Permissions::from_mode(0o755)).unwrap();
            assert!(result.is_err(), "showed a tree for an unreadable root");
            assert_eq!(
                progress.cache_reason.get().map(String::as_str),
                Some("The folder cannot be read.")
            );
            let (_, saved) = load(&f.root).unwrap();
            assert_eq!(saved.entries.len(), 1, "saved a damaged tree");
        }

        #[test]
        fn changes_on_a_mounted_disk_image_are_seen() {
            // Unmounts even when an assertion fails, so no test image stays mounted.
            struct Mounted(PathBuf);
            impl Drop for Mounted {
                fn drop(&mut self) {
                    let _ = Command::new("hdiutil")
                        .args(["detach", "-quiet", "-force"])
                        .arg(&self.0)
                        .status();
                }
            }
            let f = fixture();
            let image = f.tmp.path().join("disk.dmg");
            let created = Command::new("hdiutil")
                .args([
                    "create", "-quiet", "-size", "4m", "-fs", "APFS", "-volname", "mmtest",
                ])
                .arg(&image)
                .status();
            let mount = f.root.join("mnt");
            fs::create_dir(&mount).unwrap();
            let attached = created.is_ok_and(|s| s.success())
                && Command::new("hdiutil")
                    .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
                    .arg(&mount)
                    .arg(&image)
                    .status()
                    .is_ok_and(|s| s.success());
            if !attached {
                eprintln!("skipped: hdiutil could not attach a disk image");
                return;
            }
            // Wait until the image keeps an FSEvents history, so this test
            // covers the separate query for a volume with history. A volume
            // without history is always listed again (see mount_changes).
            write(&mount, "before.bin", 1000);
            let deadline = Instant::now() + Duration::from_secs(10);
            while volume_of(&mount).is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(200));
            }
            if volume_of(&mount).is_none() {
                let _ = Command::new("hdiutil")
                    .args(["detach", "-quiet", "-force"])
                    .arg(&mount)
                    .status();
                eprintln!("skipped: the disk image keeps no FSEvents history");
                return;
            }
            let mounted = Mounted(mount.clone());
            let detach = || {
                let _ = Command::new("hdiutil")
                    .args(["detach", "-quiet", "-force"])
                    .arg(&mount)
                    .status();
            };
            check(&f.root, || write(&mount, "written.bin", 400_000));
            check(&f.root, detach);
            drop(mounted);
        }
    }
}
