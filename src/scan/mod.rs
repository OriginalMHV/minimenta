use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Component, Path};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use rayon::prelude::*;

use crate::tree::{Dir, Sort, flag};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

// On 32-bit glibc, `fstatat` with `struct stat` fails with EOVERFLOW for large
// files, so those targets use the generic scanner.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod linux;
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
use linux as platform;

#[cfg(all(
    unix,
    not(any(
        target_os = "macos",
        all(target_os = "linux", target_pointer_width = "64")
    ))
))]
mod generic;
#[cfg(all(
    unix,
    not(any(
        target_os = "macos",
        all(target_os = "linux", target_pointer_width = "64")
    ))
))]
use generic as platform;

// The NTFS parser has no platform calls, so its tests run everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
mod ntfs;

#[cfg(windows)]
mod mft;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

/// The path type the platform scanner opens: a C string on Unix, where the
/// scanners call libc directly, and a `PathBuf` on Windows.
#[cfg(unix)]
pub(crate) type NativePath = std::ffi::CString;
#[cfg(windows)]
pub(crate) type NativePath = std::path::PathBuf;

#[cfg(unix)]
fn native(path: &Path) -> io::Result<NativePath> {
    Ok(std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?)
}

#[cfg(windows)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the same signature as the Unix version, which can fail"
)]
fn native(path: &Path) -> io::Result<NativePath> {
    Ok(path.to_path_buf())
}

/// Device and inode of a directory, used for `-x` and as its identity.
/// Windows scanners never follow reparse points, so they need neither.
#[cfg(unix)]
fn dev_ino(meta: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

#[cfg(windows)]
fn dev_ino(_: &fs::Metadata) -> (u64, u64) {
    (0, 0)
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub one_fs: bool,
    pub threads: usize,
    /// Load and save the cache (macOS only).
    pub cache: bool,
    /// Read the NTFS master file table when possible (Windows, administrator).
    pub mft: bool,
}

#[derive(Default)]
pub struct Progress {
    pub items: AtomicU64,
    pub disk: AtomicU64,
    pub errors: AtomicU64,
    pub cancel: AtomicBool,
    /// Set when the scan read the NTFS master file table.
    pub mft: AtomicBool,
    /// Set when a listing took several seconds without the NTFS master file
    /// table, but the same user could run minimenta as administrator to read
    /// it (Windows).
    pub elevate: AtomicBool,
}

// The Windows scanner never follows reparse points, so it needs no device
// check for `-x`, and its listing has no link count for hard links.
#[cfg_attr(windows, allow(dead_code))]
struct Ctx<'a> {
    one_fs: bool,
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    root_dev: u64,
    /// The scanned root. The macOS scanner counts firmlinks once.
    #[cfg(target_os = "macos")]
    root: &'a Path,
    progress: &'a Progress,
    hardlinks: Mutex<HashSet<(u64, u64)>>,
    /// Stops the scan without counting as a cancel, for example when another
    /// method finished first.
    stop: &'a AtomicBool,
}

impl Ctx<'_> {
    fn stopped(&self) -> bool {
        self.progress.cancel.load(Relaxed) || self.stop.load(Relaxed)
    }

    #[cfg_attr(windows, allow(dead_code))]
    fn first_link(&self, dev: u64, ino: u64) -> bool {
        self.hardlinks.lock().unwrap().insert((dev, ino))
    }
}

/// Scans `path` completely and returns its contents sorted by disk usage.
/// A symlink to a directory is followed for the root only.
pub fn scan(path: &Path, opts: &Options, progress: &Progress) -> io::Result<Dir> {
    // The scanners open directories with O_NOFOLLOW, so resolve the root first.
    let path = &fs::canonicalize(path)?;
    let meta = fs::metadata(path)?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "not a directory",
        ));
    }
    #[cfg(windows)]
    if opts.mft {
        return race(path, opts, progress, &meta);
    }
    list(path, opts, progress, &meta, &AtomicBool::new(false))
}

/// The MFT reader starts when the listing is slow: below `MFT_EARLY_RATE`
/// items/s over its first `MFT_EARLY`, or below `MFT_START_RATE` items/s over
/// a later `MFT_WINDOW`. The first full window counts from the start. On the
/// Windows runner, a warm listing of `C:\Program Files` ran at about 460,000
/// items/s and a cold one at about 18,000 items/s. Not starting the reader on
/// a large cold folder costs 3x to 5x. Starting it costs about 20% on a warm
/// disk, and up to 3x on a cold folder that the listing alone finishes within
/// a few seconds, because both then compete for the disk: a cold folder with
/// 32,000 items took 1.2 s to 1.9 s with the listing alone and 3.6 s to 3.9 s
/// in the race.
#[cfg(windows)]
const MFT_WINDOW: std::time::Duration = std::time::Duration::from_millis(250);
#[cfg(windows)]
const MFT_START_RATE: f64 = 150_000.0;
/// On the runner, cold listings of `C:\Program Files` had 600 to 1,400 items
/// after 50 ms, and warm ones about 36,000 after 70 ms.
#[cfg(windows)]
const MFT_EARLY: std::time::Duration = std::time::Duration::from_millis(50);
#[cfg(windows)]
const MFT_EARLY_RATE: f64 = 40_000.0;

/// Starts the MFT reader next to the directory listing and keeps the result
/// that finishes first. The MFT reader must read the table of the whole
/// volume (about 3 s for a 1.3 GB table), so a listing wins for small or warm
/// folders, and the MFT wins on a cold disk, where a listing waits for
/// thousands of small reads.
#[cfg(windows)]
fn race(path: &Path, opts: &Options, progress: &Progress, meta: &fs::Metadata) -> io::Result<Dir> {
    let start = std::time::Instant::now();
    let stop_listing = AtomicBool::new(false);
    let mft_progress = Progress::default();
    let elevation_helps = AtomicBool::new(false);
    let result = std::thread::scope(|s| {
        let reader = s.spawn(|| {
            if !wait_for_slow_listing(progress, &mft_progress.cancel) {
                return Ok(None);
            }
            let result = mft::scan(path, opts.threads, &mft_progress);
            if matches!(result, Ok(Some(_))) {
                stop_listing.store(true, Relaxed);
            } else if mft::elevation_helps(path) {
                elevation_helps.store(true, Relaxed);
            }
            result
        });
        let waiting = reader.thread().clone();
        let listed = list(path, opts, progress, meta, &stop_listing);
        if stop_listing.load(Relaxed)
            && let Ok(Ok(Some(mut dir))) = reader.join()
        {
            progress.mft.store(true, Relaxed);
            progress
                .items
                .store(mft_progress.items.load(Relaxed), Relaxed);
            dir.sort(Sort::default());
            return Ok(dir);
        }
        // The listing finished first, or the MFT reader could not run. Wake
        // the reader if it waits, so the scope does not wait for it.
        mft_progress.cancel.store(true, Relaxed);
        waiting.unpark();
        listed
    });
    if !progress.mft.load(Relaxed)
        && suggest_elevation(elevation_helps.load(Relaxed), start.elapsed())
    {
        progress.elevate.store(true, Relaxed);
    }
    result
}

/// The MFT read alone took about 3 s to 4 s on the runner, for a 1.3 GB table.
/// A listing that took longer than this floor could have been faster with
/// the MFT, so only then is an elevated run worth a hint.
#[cfg(windows)]
const ELEVATE_HINT_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

/// Whether to suggest an elevated run after a listing that took `took`.
/// `elevation_helps` says that the MFT reader could not run and that an
/// elevated run could read the MFT.
#[cfg(windows)]
fn suggest_elevation(elevation_helps: bool, took: std::time::Duration) -> bool {
    elevation_helps && took >= ELEVATE_HINT_AFTER
}

/// Decides from samples of the listed item count whether the listing is
/// slow, with the windows and rates above.
#[cfg(windows)]
struct RateCheck {
    /// Time and item count at the start of the current window.
    from: (std::time::Instant, u64),
    early: bool,
}

#[cfg(windows)]
impl RateCheck {
    fn new(start: std::time::Instant) -> Self {
        RateCheck {
            from: (start, 0),
            early: true,
        }
    }

    fn window(&self) -> std::time::Duration {
        if self.early { MFT_EARLY } else { MFT_WINDOW }
    }

    /// Whether the listing is slow at `now` with `items` listed. Before the
    /// current window has passed, it is not.
    fn is_slow(&mut self, now: std::time::Instant, items: u64) -> bool {
        let threshold = if self.early {
            MFT_EARLY_RATE
        } else {
            MFT_START_RATE
        };
        let elapsed = now.saturating_duration_since(self.from.0);
        if elapsed < self.window() {
            return false;
        }
        let rate = items.saturating_sub(self.from.1) as f64 / elapsed.as_secs_f64();
        if rate < threshold {
            return true;
        }
        if !self.early {
            self.from = (now, items);
        }
        self.early = false;
        false
    }
}

/// Waits until the listing is slow, and returns false when `done` is set
/// first. Small folders finish, and warm ones list fast enough, so they pay
/// nothing for the race.
#[cfg(windows)]
fn wait_for_slow_listing(progress: &Progress, done: &AtomicBool) -> bool {
    let mut check = RateCheck::new(std::time::Instant::now());
    loop {
        std::thread::park_timeout(check.window());
        if done.load(Relaxed) {
            return false;
        }
        if check.is_slow(std::time::Instant::now(), progress.items.load(Relaxed)) {
            return true;
        }
    }
}

fn list(
    path: &Path,
    opts: &Options,
    progress: &Progress,
    meta: &fs::Metadata,
    stop: &AtomicBool,
) -> io::Result<Dir> {
    let ctx = Ctx {
        one_fs: opts.one_fs,
        root_dev: dev_ino(meta).0,
        #[cfg(target_os = "macos")]
        root: path,
        progress,
        hardlinks: Mutex::default(),
        stop,
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads)
        .stack_size(16 << 20)
        .build()
        .map_err(io::Error::other)?;
    let root = native(path)?;
    let (mut dir, result) = pool.install(|| scan_dir(&ctx, &root, 0, dev_ino(meta).1));
    // The macOS scanner does not count directory blocks, so the root does not either.
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::os::unix::fs::MetadataExt;
        dir.own_disk = meta.blocks() * 512;
    }
    if ctx.stopped() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "scan cancelled"));
    }
    match result {
        Err(e) if dir.entries.is_empty() => Err(e),
        _ => {
            // Only the root is sorted here. The browser sorts each directory
            // when it opens it, so most directories are never sorted at all.
            dir.sort(Sort::default());
            Ok(dir)
        }
    }
}

/// A subdirectory found by `read_dir`, to be scanned next.
pub struct SubDir {
    /// Position of the entry in the listed directory.
    pub index: usize,
    /// Entry count from the listing, or 0 when unknown.
    pub expected: u32,
    /// Inode of the subdirectory, or 0 when unknown.
    pub ino: u64,
}

/// A directory below the root that must be listed again. The incremental
/// update is only used with the cache, which needs FSEvents (macOS).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct Change<'a> {
    pub path: &'a Path,
    /// List the whole subtree again, not only the directory itself.
    pub recursive: bool,
}

/// Updates a cached tree in place. Each changed directory is listed again,
/// one level deep. Unchanged subdirectories keep their cached subtrees, and new
/// subdirectories are scanned completely. Returns whether the listing found
/// files with more than one hard link.
///
/// Fails when the root cannot be listed again, so the caller can scan fully.
///
/// Limits: hard links found again are counted as first links, so a file with
/// several links can be counted twice. A directory that is renamed away,
/// changed, and renamed back keeps its cached contents. The cache age limit
/// and a rescan with `r` repair both.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn update(
    dir: &mut Dir,
    root: &Path,
    changes: &[Change],
    opts: &Options,
    progress: &Progress,
) -> io::Result<bool> {
    let meta = fs::metadata(root)?;
    let ctx = Ctx {
        one_fs: opts.one_fs,
        root_dev: dev_ino(&meta).0,
        #[cfg(target_os = "macos")]
        root,
        progress,
        hardlinks: Mutex::default(),
        stop: &AtomicBool::new(false),
    };

    // Parents first, so a child change lands in the freshly listed parent.
    // Changes below a recursive change are covered by it.
    let mut targets: Vec<(Vec<&[u8]>, bool)> = changes
        .iter()
        .filter_map(|c| Some((relative(root, c.path)?, c.recursive)))
        .collect();
    targets.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(&b.0)));
    targets.dedup_by(|later, earlier| {
        earlier.0 == later.0 && {
            earlier.1 |= later.1;
            true
        }
    });
    let recursive: Vec<Vec<&[u8]>> = targets
        .iter()
        .filter(|t| t.1)
        .map(|t| t.0.clone())
        .collect();
    targets.retain(|(path, _)| {
        !recursive
            .iter()
            .any(|r| r.len() < path.len() && path.starts_with(r))
    });
    // A repeat scan often finds nothing to list, and then needs no threads.
    if targets.is_empty() {
        if ctx.stopped() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "scan cancelled"));
        }
        fix_totals(dir);
        dir.sort(Sort::default());
        return Ok(false);
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads)
        .stack_size(16 << 20)
        .build()
        .map_err(io::Error::other)?;

    let native_root = native(root)?;
    pool.install(|| {
        for (components, recursive) in &targets {
            if ctx.stopped() {
                break;
            }
            let path = components
                .iter()
                .fold(native_root.clone(), |path, c| child_path(&path, c));
            let Some((target, entry_flags)) = locate(dir, components) else {
                continue;
            };
            let old = std::mem::take(target);
            // The parent listing holds a directory's own blocks, so keep them.
            let (id, own_disk) = (old.id, old.own_disk);
            let (mut fresh, result) = if *recursive {
                scan_dir(&ctx, &path, 0, id)
            } else {
                relist(&ctx, &path, old)
            };
            fresh.own_disk = own_disk;
            *target = fresh;
            match entry_flags {
                Some(flags) => {
                    *flags &= !flag::ERROR;
                    if result.is_err() {
                        *flags |= flag::ERROR;
                    }
                }
                None => result?,
            }
        }
        Ok::<_, io::Error>(())
    })?;
    if ctx.stopped() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "scan cancelled"));
    }
    fix_totals(dir);
    dir.sort(Sort::default());
    Ok(!ctx.hardlinks.lock().unwrap().is_empty())
}

/// The path components of `path` below `root`, or `None` when it is outside.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn relative<'p>(root: &Path, path: &'p Path) -> Option<Vec<&'p [u8]>> {
    let rest = path.strip_prefix(root).ok()?;
    rest.components()
        .map(|c| match c {
            Component::Normal(name) => Some(name.as_encoded_bytes()),
            _ => None,
        })
        .collect()
}

/// Finds the directory at `components` and marks every entry on the way as
/// dirty. Returns the directory and the flags of its entry in the parent
/// (`None` for the root). Missing directories are skipped: the change event
/// of their parent covers them.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn locate<'d>(dir: &'d mut Dir, components: &[&[u8]]) -> Option<(&'d mut Dir, Option<&'d mut u8>)> {
    let Some((last, ancestors)) = components.split_last() else {
        return Some((dir, None));
    };
    let mut current = dir;
    for name in ancestors {
        let i = current.find(name)?;
        let e = &mut current.entries[i];
        e.flags |= flag::DIRTY;
        current = e.dir.as_deref_mut()?;
    }
    let i = current.find(last)?;
    let e = &mut current.entries[i];
    e.flags |= flag::DIRTY;
    let sub = e.dir.as_deref_mut()?;
    Some((sub, Some(&mut e.flags)))
}

/// Lists one directory again and moves the cached subtrees of its unchanged
/// subdirectories into the new listing. A subtree is unchanged when the name
/// and the inode match and it was readable last time. Anything else, such as
/// a directory replaced by another one with the same name, is scanned fully.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn relist(ctx: &Ctx, path: &NativePath, mut old: Dir) -> (Dir, io::Result<()>) {
    let mut fresh = Dir {
        id: old.id,
        ..Dir::default()
    };
    let mut subdirs = Vec::new();
    let result = platform::read_dir(ctx, path, &mut fresh, &mut subdirs, 0);
    ctx.progress
        .items
        .fetch_add(fresh.entries.len() as u64, Relaxed);

    // A map keeps this linear for directories with many subdirectories.
    let Dir { names, entries, .. } = &mut old;
    let by_name: std::collections::HashMap<&[u8], usize> = entries
        .iter()
        .enumerate()
        .map(|(j, e)| (&names[e.name_start as usize..][..e.name_len as usize], j))
        .collect();
    let mut new_subdirs = Vec::new();
    for sub in subdirs {
        let cached = by_name
            .get(fresh.name(&fresh.entries[sub.index]))
            .and_then(|&j| {
                let e = &mut entries[j];
                let same = sub.ino != 0 && e.dir.as_ref()?.id == sub.ino;
                if !same || e.has(flag::ERROR) {
                    return None;
                }
                e.dir.take()
            });
        match cached {
            Some(mut tree) => {
                tree.own_disk = fresh.entries[sub.index].disk;
                fresh.attach(sub.index, *tree, 0);
            }
            None => new_subdirs.push(sub),
        }
    }
    let scanned: Vec<_> = new_subdirs
        .par_iter()
        .map(|sub| {
            let name = fresh.name(&fresh.entries[sub.index]);
            scan_dir(ctx, &child_path(path, name), sub.expected, sub.ino)
        })
        .collect();
    for (sub, (mut tree, result)) in new_subdirs.iter().zip(scanned) {
        tree.own_disk = fresh.entries[sub.index].disk;
        fresh.attach(
            sub.index,
            tree,
            if result.is_err() { flag::ERROR } else { 0 },
        );
    }
    fresh.sort(Sort::default());
    (fresh, result)
}

/// Recomputes the totals of dirty entries from their contents, deepest first,
/// and sorts the directories whose entries changed.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn fix_totals(dir: &mut Dir) {
    let mut changed = false;
    for e in &mut dir.entries {
        if !e.has(flag::DIRTY) {
            continue;
        }
        e.flags &= !(flag::DIRTY | flag::SUB_ERROR);
        if let Some(sub) = e.dir.as_deref_mut() {
            fix_totals(sub);
            let t = sub.totals();
            (e.disk, e.apparent, e.items) = (t.disk, t.apparent, 1 + t.items);
            if sub.has_error() {
                e.flags |= flag::SUB_ERROR;
            }
        }
        changed = true;
    }
    if changed {
        dir.sort(Sort::default());
    }
}

/// `expected` is the entry count from the parent listing, or 0 when unknown.
/// `id` is the inode of the directory, or 0 when unknown.
fn scan_dir(ctx: &Ctx, path: &NativePath, expected: u32, id: u64) -> (Dir, io::Result<()>) {
    let mut dir = Dir {
        id,
        ..Dir::default()
    };
    if ctx.stopped() {
        return (dir, Ok(()));
    }
    let mut subdirs = Vec::new();
    let result = platform::read_dir(ctx, path, &mut dir, &mut subdirs, expected);
    if result.is_err() {
        ctx.progress.errors.fetch_add(1, Relaxed);
    }
    ctx.progress
        .items
        .fetch_add(dir.entries.len() as u64, Relaxed);
    ctx.progress.disk.fetch_add(dir.totals().disk, Relaxed);

    if !subdirs.is_empty() {
        let scanned: Vec<_> = subdirs
            .par_iter()
            .map(|sub| {
                let name = dir.name(&dir.entries[sub.index]);
                scan_dir(ctx, &child_path(path, name), sub.expected, sub.ino)
            })
            .collect();
        for (sub, (mut tree, result)) in subdirs.iter().zip(scanned) {
            // The listing stored the directory's own blocks in its entry.
            tree.own_disk = dir.entries[sub.index].disk;
            dir.attach(
                sub.index,
                tree,
                if result.is_err() { flag::ERROR } else { 0 },
            );
        }
    }
    (dir, result)
}

#[cfg(unix)]
fn child_path(parent: &NativePath, name: &[u8]) -> NativePath {
    let parent = parent.as_bytes();
    let mut path = Vec::with_capacity(parent.len() + name.len() + 2);
    path.extend_from_slice(parent);
    if !parent.ends_with(b"/") {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    // SAFETY: Unix file names and the root path cannot contain NUL bytes.
    unsafe { std::ffi::CString::from_vec_unchecked(path) }
}

#[cfg(windows)]
fn child_path(parent: &NativePath, name: &[u8]) -> NativePath {
    parent.join(crate::tree::os_name(name))
}

#[cfg(all(
    unix,
    not(any(
        target_os = "macos",
        all(target_os = "linux", target_pointer_width = "64")
    ))
))]
fn as_path(path: &std::ffi::CStr) -> &Path {
    Path::new(crate::tree::os_name(path.to_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Kind;
    use std::fs::File;
    use std::io::Write;

    fn opts() -> Options {
        Options {
            one_fs: false,
            threads: 4,
            cache: false,
            mft: false,
        }
    }

    fn write(path: &Path, bytes: usize) {
        File::create(path)
            .unwrap()
            .write_all(&vec![7u8; bytes])
            .unwrap();
    }

    fn find<'a>(dir: &'a Dir, name: &str) -> &'a crate::tree::Entry {
        dir.entries
            .iter()
            .find(|e| dir.name(e) == name.as_bytes())
            .unwrap()
    }

    #[test]
    fn scans_nested_directories_and_rolls_up_sizes() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("a/b/c")).unwrap();
        write(&tmp.path().join("top.bin"), 100);
        write(&tmp.path().join("a/one.bin"), 1000);
        write(&tmp.path().join("a/b/c/deep.bin"), 10_000);

        let progress = Progress::default();
        let dir = scan(tmp.path(), &opts(), &progress).unwrap();
        let totals = dir.totals();

        assert_eq!(totals.apparent, 11_100);
        assert_eq!(totals.items, 6);
        assert!(totals.disk >= 11_100);
        let a = find(&dir, "a");
        assert_eq!(a.kind, Kind::Dir);
        assert_eq!(a.apparent, 11_000);
        assert_eq!(a.items, 5);
        assert_eq!(dir.name(&dir.entries[0]), b"a", "largest entry comes first");
        assert_eq!(progress.items.load(Relaxed), 6);
    }

    #[cfg(unix)]
    #[test]
    fn counts_hard_links_once() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("original"), 50_000);
        fs::hard_link(tmp.path().join("original"), tmp.path().join("link")).unwrap();

        let dir = scan(tmp.path(), &opts(), &Progress::default()).unwrap();

        assert_eq!(dir.totals().apparent, 50_000);
        let flagged = dir.entries.iter().filter(|e| e.has(flag::HARDLINK)).count();
        assert_eq!(flagged, 1);
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("real")).unwrap();
        write(&tmp.path().join("real/big.bin"), 20_000);
        std::os::unix::fs::symlink(tmp.path().join("real"), tmp.path().join("alias")).unwrap();

        let dir = scan(tmp.path(), &opts(), &Progress::default()).unwrap();

        let alias = find(&dir, "alias");
        assert_eq!(alias.kind, Kind::Symlink);
        assert!(alias.dir.is_none());
        assert!(dir.totals().apparent < 40_000);
    }

    /// A parent that is listed again must not reuse a child subtree that
    /// could not be read last time, even when the name and inode match.
    #[test]
    fn relisting_a_parent_scans_a_child_that_was_unreadable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("a/x")).unwrap();
        write(&root.join("a/x/big.bin"), 30_000);
        let mut dir = scan(&root, &opts(), &Progress::default()).unwrap();
        let a = dir.find(b"a").unwrap();
        let a_dir = dir.entries[a].dir.as_deref_mut().unwrap();
        let x = a_dir.find(b"x").unwrap();
        let id = a_dir.entries[x].dir.as_ref().unwrap().id;
        a_dir.entries[x].dir = Some(Box::new(Dir {
            id,
            ..Dir::default()
        }));
        a_dir.entries[x].flags |= flag::ERROR;

        let a_path = root.join("a");
        let changes = [Change {
            path: &a_path,
            recursive: false,
        }];
        update(&mut dir, &root, &changes, &opts(), &Progress::default()).unwrap();

        assert_eq!(dir.totals().apparent, 30_000);
        assert!(!dir.has_error());
    }

    #[test]
    fn an_update_with_nothing_to_list_still_stops_on_cancel() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let mut dir = scan(&root, &opts(), &Progress::default()).unwrap();
        let progress = Progress::default();
        progress.cancel.store(true, Relaxed);

        let err = update(&mut dir, &root, &[], &opts(), &progress)
            .err()
            .unwrap();

        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn rejects_a_file_as_root() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("file"), 1);
        let err = scan(&tmp.path().join("file"), &opts(), &Progress::default())
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::NotADirectory);
    }

    #[cfg(unix)]
    #[test]
    fn follows_a_symlink_given_as_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("real/sub")).unwrap();
        write(&tmp.path().join("real/sub/data.bin"), 3000);
        std::os::unix::fs::symlink(tmp.path().join("real"), tmp.path().join("alias")).unwrap();

        let via_link = scan(&tmp.path().join("alias"), &opts(), &Progress::default()).unwrap();
        let direct = scan(&tmp.path().join("real"), &opts(), &Progress::default()).unwrap();

        assert_eq!(via_link.totals(), direct.totals());
        assert_eq!(via_link.totals().apparent, 3000);
        assert_eq!(via_link.totals().items, 2);
    }

    #[cfg(windows)]
    #[test]
    fn the_mft_reader_starts_early_for_a_clearly_cold_listing() {
        let start = std::time::Instant::now();
        let at = |ms| start + std::time::Duration::from_millis(ms);
        let mut check = RateCheck::new(start);
        assert!(!check.is_slow(at(10), 0), "the first window has not passed");
        // 1,000 items in 50 ms is 20,000 items/s.
        assert!(check.is_slow(at(50), 1_000));
    }

    #[cfg(windows)]
    #[test]
    fn the_mft_reader_waits_for_a_full_window_after_a_fast_start() {
        let start = std::time::Instant::now();
        let at = |ms| start + std::time::Duration::from_millis(ms);
        let mut check = RateCheck::new(start);
        // 400,000 items/s, then 200,000 items/s over the first full window.
        assert!(!check.is_slow(at(50), 20_000));
        assert!(!check.is_slow(at(300), 60_000));
        assert!(!check.is_slow(at(400), 60_000), "the window has not passed");
        // 10,000 items in the next 300 ms is about 33,000 items/s.
        assert!(check.is_slow(at(600), 70_000));
    }

    #[cfg(windows)]
    #[test]
    fn a_stalled_listing_starts_the_mft_reader_and_a_finished_one_never_does() {
        assert!(wait_for_slow_listing(
            &Progress::default(),
            &AtomicBool::new(false)
        ));
        assert!(!wait_for_slow_listing(
            &Progress::default(),
            &AtomicBool::new(true)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn only_a_long_listing_suggests_an_elevated_run() {
        let ms = std::time::Duration::from_millis;
        assert!(suggest_elevation(true, ms(6_000)));
        assert!(
            !suggest_elevation(true, ms(400)),
            "the MFT could not win against a short listing"
        );
        assert!(!suggest_elevation(false, ms(60_000)));
    }

    #[test]
    fn refreshing_a_scanned_tree_keeps_its_totals() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("a/b/c")).unwrap();
        write(&tmp.path().join("a/b/c/deep.bin"), 10_000);
        let dir = scan(tmp.path(), &opts(), &Progress::default()).unwrap();
        let mut tree = crate::tree::Tree {
            path: tmp.path().to_path_buf(),
            dir: Box::new(dir),
        };
        let before = tree.dir.totals();

        tree.refresh_totals(&[0, 0, 0]);

        assert_eq!(tree.dir.totals(), before);
    }
}
