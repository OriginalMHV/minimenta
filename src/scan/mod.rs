use std::collections::HashSet;
use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use rayon::prelude::*;

use crate::tree::{Dir, Sort, flag};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod generic;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
use generic as platform;

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub one_fs: bool,
    pub threads: usize,
}

#[derive(Default)]
pub struct Progress {
    pub items: AtomicU64,
    pub disk: AtomicU64,
    pub errors: AtomicU64,
    pub cancel: AtomicBool,
}

struct Ctx<'a> {
    one_fs: bool,
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    root_dev: u64,
    progress: &'a Progress,
    hardlinks: Mutex<HashSet<(u64, u64)>>,
}

impl Ctx<'_> {
    fn first_link(&self, dev: u64, ino: u64) -> bool {
        self.hardlinks.lock().unwrap().insert((dev, ino))
    }
}

/// Scans `path` completely and returns its contents sorted by disk usage.
pub fn scan(path: &Path, opts: &Options, progress: &Progress) -> io::Result<Dir> {
    let meta = fs::metadata(path)?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "not a directory",
        ));
    }
    let ctx = Ctx {
        one_fs: opts.one_fs,
        root_dev: meta.dev(),
        progress,
        hardlinks: Mutex::default(),
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads)
        .stack_size(16 << 20)
        .build()
        .map_err(io::Error::other)?;
    let root = CString::new(path.as_os_str().as_bytes())?;
    let (dir, result) = pool.install(|| scan_dir(&ctx, root, 0));
    if progress.cancel.load(Relaxed) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "scan cancelled"));
    }
    match result {
        Err(e) if dir.entries.is_empty() => Err(e),
        _ => Ok(dir),
    }
}

/// `expected` is the entry count from the parent listing, or 0 when unknown.
fn scan_dir(ctx: &Ctx, path: CString, expected: u32) -> (Dir, io::Result<()>) {
    let mut dir = Dir::default();
    if ctx.progress.cancel.load(Relaxed) {
        return (dir, Ok(()));
    }
    let mut subdirs = Vec::new();
    let result = platform::read_dir(ctx, &path, &mut dir, &mut subdirs, expected);
    if result.is_err() {
        ctx.progress.errors.fetch_add(1, Relaxed);
    }
    ctx.progress
        .items
        .fetch_add(dir.entries.len() as u64, Relaxed);
    ctx.progress.disk.fetch_add(dir.totals().disk, Relaxed);

    if !subdirs.is_empty() {
        let parent = path.as_bytes();
        let scanned: Vec<_> = subdirs
            .par_iter()
            .map(|&(i, expected)| {
                scan_dir(ctx, child_path(parent, dir.name(&dir.entries[i])), expected)
            })
            .collect();
        for (&(i, _), (sub, result)) in subdirs.iter().zip(scanned) {
            dir.attach(i, sub, if result.is_err() { flag::ERROR } else { 0 });
        }
    }
    dir.sort(Sort::default());
    (dir, result)
}

fn child_path(parent: &[u8], name: &[u8]) -> CString {
    let mut path = Vec::with_capacity(parent.len() + name.len() + 2);
    path.extend_from_slice(parent);
    if !parent.ends_with(b"/") {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    // SAFETY: Unix file names and the root path cannot contain NUL bytes.
    unsafe { CString::from_vec_unchecked(path) }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn as_path(path: &std::ffi::CStr) -> &Path {
    Path::new(std::ffi::OsStr::from_bytes(path.to_bytes()))
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

    #[test]
    fn rejects_a_file_as_root() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("file"), 1);
        let err = scan(&tmp.path().join("file"), &opts(), &Progress::default())
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::NotADirectory);
    }
}
