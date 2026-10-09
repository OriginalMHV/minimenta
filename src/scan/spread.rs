//! Scheduler for scans on Linux that spreads the reads over the disk.
//!
//! A cold scan waits for the disk. The directories of one parent sit next to
//! each other on the disk, so threads that follow the tree read the same part
//! of the disk at the same time. On the virtual disks of cloud machines this
//! kept the request rate at about 75% of what the disk serves. Directories in
//! random order raised it.
//!
//! The scheduler keeps a pool of directories that wait for a scan. A thread
//! that lists a directory puts its subdirectories into the pool, and starts
//! one task for each. A task takes a random directory out of the pool, which
//! is not always the one that started it. A directory does not wait for its
//! children. The last child to finish attaches its tree to the parent and then
//! finishes the parent in the same way.
//!
//! A warm scan does not wait for the disk, and the random order costs it a few
//! percent. A detector tells the two cases apart. While the scan keeps the
//! CPUs busy, every subdirectory is a task of its own, and the last one runs
//! on the thread that listed its parent.

use std::io;
use std::sync::atomic::{
    AtomicBool, AtomicUsize,
    Ordering::{AcqRel, Relaxed},
};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rayon::Scope;

use super::{Ctx, NativePath, child_path, platform};
use crate::tree::{Dir, flag};

/// Where the pool stops growing. A directory that finds the pool full is
/// scanned by the thread that found it, so the memory stays bounded.
#[derive(Clone, Copy)]
struct Limits {
    entries: usize,
    path_bytes: usize,
}

const LIMITS: Limits = Limits {
    entries: 65_536,
    path_bytes: 32 << 20,
};

/// Items per window of the detector.
const WINDOW_ITEMS: u64 = 4096;
/// Weight of the windows before the last one in the smoothed busy share.
const KEEP: f64 = 0.5;
/// A scan turns cold when the smoothed busy share falls below this value.
/// Warm scans of `/usr` kept 85% or more of the CPUs busy in 95% of the
/// windows, and cold ones 24% to 40% in half of them.
const ENTER_COLD: f64 = 0.55;
/// A cold scan turns warm again above this value.
const LEAVE_COLD: f64 = 0.75;

type Slot = Mutex<Option<(Dir, io::Result<()>)>>;

/// Where a finished directory goes.
enum Up {
    Root(Arc<Slot>),
    Child(Child),
}

struct Child {
    parent: Arc<Node>,
    /// Position of the directory in the parent.
    index: usize,
    /// The blocks of the directory itself, from the listing of the parent.
    own_disk: u64,
}

/// A directory that waits for its children.
struct Node {
    /// Children that are not attached yet.
    left: AtomicUsize,
    inner: Mutex<Inner>,
}

struct Inner {
    dir: Dir,
    up: Option<Up>,
    result: io::Result<()>,
}

struct Pending {
    path: NativePath,
    ino: u64,
    child: Child,
}

struct Pool {
    items: Vec<Pending>,
    path_bytes: usize,
    rng: u64,
}

struct Scan<'a> {
    ctx: &'a Ctx<'a>,
    /// Tells the scan to spread the reads or not. `None` leaves the choice
    /// to the detector.
    forced: Option<bool>,
    limits: Limits,
    pool: Mutex<Pool>,
    detector: Detector,
}

/// Scans the tree below `root`. Returns when every directory in it is done, or
/// when the scan stops. Call it inside the thread pool of the scan.
pub(super) fn scan_tree(ctx: &Ctx, root: &NativePath, id: u64) -> (Dir, io::Result<()>) {
    scan_with(ctx, root, id, None, LIMITS)
}

fn scan_with(
    ctx: &Ctx,
    root: &NativePath,
    id: u64,
    forced: Option<bool>,
    limits: Limits,
) -> (Dir, io::Result<()>) {
    let scan = Scan {
        ctx,
        forced,
        limits,
        pool: Mutex::new(Pool {
            items: Vec::new(),
            path_bytes: 0,
            rng: 0x2545_F491_4F6C_DD1D,
        }),
        detector: Detector::new(),
    };
    let slot: Arc<Slot> = Arc::default();
    rayon::scope(|s| visit(s, &scan, root.clone(), id, Up::Root(Arc::clone(&slot))));
    let done = slot.lock().unwrap().take();
    // A scan that stopped early never finished the root.
    done.unwrap_or_else(|| {
        let empty = Dir {
            id,
            ..Dir::default()
        };
        (empty, Ok(()))
    })
}

impl Scan<'_> {
    fn spread(&self) -> bool {
        self.forced.unwrap_or_else(|| self.detector.cold())
    }

    /// Puts the directory into the pool, or gives it back when the pool is full.
    fn push(&self, p: Pending) -> Result<(), Pending> {
        let mut pool = self.pool.lock().unwrap();
        let len = p.path.as_bytes().len();
        if pool.items.len() >= self.limits.entries || pool.path_bytes + len > self.limits.path_bytes
        {
            return Err(p);
        }
        pool.path_bytes += len;
        pool.items.push(p);
        Ok(())
    }

    /// Takes a random directory out of the pool.
    fn pop(&self) -> Option<Pending> {
        let mut pool = self.pool.lock().unwrap();
        let i = pool.next_index()?;
        let p = pool.items.swap_remove(i);
        pool.path_bytes -= p.path.as_bytes().len();
        Some(p)
    }
}

impl Pool {
    /// The position of the next entry to take, from a xorshift generator.
    fn next_index(&mut self) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        Some((x % self.items.len() as u64) as usize)
    }
}

/// The task of one pool entry.
fn token<'s, 'a: 's>(s: &Scope<'s>, scan: &'s Scan<'a>) {
    if let Some(p) = scan.pop() {
        visit(s, scan, p.path, p.ino, Up::Child(p.child));
    }
}

/// Lists a directory and hands its subdirectories on. A directory without
/// subdirectories is done at once.
fn visit<'s, 'a: 's>(
    s: &Scope<'s>,
    scan: &'s Scan<'a>,
    mut path: NativePath,
    mut ino: u64,
    mut up: Up,
) {
    let ctx = scan.ctx;
    loop {
        if ctx.stopped() {
            return;
        }
        let mut dir = Dir {
            id: ino,
            ..Dir::default()
        };
        let mut subdirs = Vec::new();
        let result = platform::read_dir(ctx, &path, &mut dir, &mut subdirs, 0);
        if result.is_err() {
            ctx.progress.errors.fetch_add(1, Relaxed);
        }
        let items = dir.entries.len() as u64;
        let before = ctx.progress.items.fetch_add(items, Relaxed);
        ctx.progress.disk.fetch_add(dir.totals().disk, Relaxed);
        if scan.forced.is_none() {
            scan.detector.count(before, before + items);
        }
        if subdirs.is_empty() {
            finish(up, dir, result);
            return;
        }

        let node = Arc::new(Node {
            left: AtomicUsize::new(subdirs.len()),
            inner: Mutex::new(Inner {
                dir: Dir::default(),
                up: Some(up),
                result: Ok(()),
            }),
        });
        let mut children: Vec<Pending> = subdirs
            .iter()
            .map(|sub| {
                let entry = &dir.entries[sub.index];
                Pending {
                    path: child_path(&path, dir.name(entry)),
                    ino: sub.ino,
                    child: Child {
                        parent: Arc::clone(&node),
                        index: sub.index,
                        own_disk: entry.disk,
                    },
                }
            })
            .collect();
        {
            let mut inner = node.inner.lock().unwrap();
            inner.dir = dir;
            inner.result = result;
        }
        drop(node);

        if scan.spread() {
            // The pool may refuse a directory. This thread scans it.
            let mut refused = Vec::new();
            for p in children {
                match scan.push(p) {
                    Ok(()) => s.spawn(move |s| token(s, scan)),
                    Err(p) => refused.push(p),
                }
            }
            children = refused;
        } else {
            let last = children.pop();
            for p in children {
                s.spawn(move |s| visit(s, scan, p.path, p.ino, Up::Child(p.child)));
            }
            children = last.into_iter().collect();
        }
        let Some(next) = children.pop() else {
            return;
        };
        for p in children {
            visit(s, scan, p.path, p.ino, Up::Child(p.child));
        }
        (path, ino, up) = (next.path, next.ino, Up::Child(next.child));
    }
}

/// Attaches a finished directory to its parent. The last child of a parent
/// finishes the parent too, and so on up the tree.
fn finish(mut up: Up, mut dir: Dir, mut result: io::Result<()>) {
    loop {
        let child = match up {
            Up::Root(slot) => {
                *slot.lock().unwrap() = Some((dir, result));
                return;
            }
            Up::Child(child) => child,
        };
        let node = child.parent;
        {
            let mut inner = node.inner.lock().unwrap();
            dir.own_disk = child.own_disk;
            let flags = if result.is_err() { flag::ERROR } else { 0 };
            inner.dir.attach(child.index, dir, flags);
        }
        if node.left.fetch_sub(1, AcqRel) != 1 {
            return;
        }
        let mut inner = node.inner.lock().unwrap();
        let Some(parent_up) = inner.up.take() else {
            return;
        };
        dir = std::mem::take(&mut inner.dir);
        result = std::mem::replace(&mut inner.result, Ok(()));
        up = parent_up;
    }
}

fn process_cpu_seconds() -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage fills the struct, and a zeroed struct is a valid one.
    let usage = unsafe {
        libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr());
        usage.assume_init()
    };
    let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    seconds(usage.ru_utime) + seconds(usage.ru_stime)
}

/// Tells whether the scan waits for the disk. After every 4096 items it
/// compares the CPU time that the process used with the CPU time that its
/// threads could use.
struct Detector {
    /// CPUs that the threads can use at the same time.
    capacity: f64,
    cold: AtomicBool,
    last: Mutex<Window>,
}

struct Window {
    at: Instant,
    cpu: f64,
    /// Smoothed share of the busy CPUs. A scan starts out warm.
    busy: f64,
}

impl Detector {
    fn new() -> Self {
        let cores = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
        Detector {
            capacity: cores.min(rayon::current_num_threads()) as f64,
            cold: AtomicBool::new(false),
            last: Mutex::new(Window {
                at: Instant::now(),
                cpu: process_cpu_seconds(),
                busy: 1.0,
            }),
        }
    }

    fn cold(&self) -> bool {
        self.cold.load(Relaxed)
    }

    /// Takes the item counter before and after one directory. Ends a window
    /// when the counter crosses a multiple of the window size.
    fn count(&self, before: u64, after: u64) {
        if before / WINDOW_ITEMS == after / WINDOW_ITEMS {
            return;
        }
        // Another thread that ends a window at the same time does the work.
        let Ok(mut last) = self.last.try_lock() else {
            return;
        };
        let now = Instant::now();
        let cpu = process_cpu_seconds();
        let wall = now.duration_since(last.at).as_secs_f64();
        if wall < 0.002 {
            return;
        }
        let busy = (cpu - last.cpu) / wall / self.capacity;
        let (smoothed, cold) = smooth(last.busy, self.cold(), busy);
        *last = Window {
            at: now,
            cpu,
            busy: smoothed,
        };
        self.cold.store(cold, Relaxed);
    }
}

/// Adds the busy share of one window to the smoothed share. Returns the new
/// smoothed share and whether the scan is cold. The two limits leave a gap, so
/// the answer does not flip back and forth.
fn smooth(smoothed: f64, cold: bool, busy: f64) -> (f64, bool) {
    let smoothed = KEEP * smoothed + (1.0 - KEEP) * busy.min(1.0);
    if smoothed < ENTER_COLD {
        (smoothed, true)
    } else if smoothed > LEAVE_COLD {
        (smoothed, false)
    } else {
        (smoothed, cold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Progress;
    use crate::tree::Kind;
    use std::ffi::CString;
    use std::fs::{self, File};
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn ctx<'a>(progress: &'a Progress, stop: &'a AtomicBool) -> Ctx<'a> {
        Ctx {
            one_fs: false,
            root_dev: 0,
            progress,
            hardlinks: Mutex::default(),
            stop,
        }
    }

    fn native(path: &Path) -> NativePath {
        CString::new(path.as_os_str().as_bytes()).unwrap()
    }

    /// A deep chain, a wide directory, empty directories and a few files.
    fn build(root: &Path) {
        fs::create_dir_all(root.join("a/b/c/d/e")).unwrap();
        File::create(root.join("a/b/c/d/e/deep.bin"))
            .unwrap()
            .write_all(&[1; 9000])
            .unwrap();
        for i in 0..150 {
            let dir = root.join(format!("wide/{i:03}"));
            fs::create_dir_all(&dir).unwrap();
            File::create(dir.join("one.bin"))
                .unwrap()
                .write_all(&[2; 100])
                .unwrap();
            File::create(dir.join("two.bin")).unwrap();
        }
        for i in 0..20 {
            fs::create_dir_all(root.join(format!("empty/{i}"))).unwrap();
        }
        File::create(root.join("top.bin"))
            .unwrap()
            .write_all(&[3; 5000])
            .unwrap();
        std::os::unix::fs::symlink("a", root.join("alias")).unwrap();
    }

    /// One line for every entry in the tree, sorted. Two scans of the same
    /// tree give the same lines.
    fn lines(dir: &Dir, prefix: &str, out: &mut Vec<String>) {
        for e in &dir.entries {
            let name = format!("{prefix}/{}", String::from_utf8_lossy(dir.name(e)));
            out.push(format!(
                "{name} {:?} disk {} apparent {} items {} flags {}",
                e.kind, e.disk, e.apparent, e.items, e.flags
            ));
            if let Some(sub) = &e.dir {
                out.push(format!("{name} own {} id {}", sub.own_disk, sub.id));
                lines(sub, &name, out);
            }
        }
    }

    fn shape(dir: &Dir) -> Vec<String> {
        let mut out = Vec::new();
        lines(dir, "", &mut out);
        out.sort_unstable();
        out
    }

    fn pool(threads: usize) -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
    }

    fn plain(root: &Path) -> Dir {
        let (progress, stop) = (Progress::default(), AtomicBool::new(false));
        let ctx = ctx(&progress, &stop);
        let (dir, result) = pool(4).install(|| crate::scan::scan_dir(&ctx, &native(root), 0, 7));
        result.unwrap();
        dir
    }

    fn scan_spread(root: &Path, threads: usize, forced: Option<bool>, limits: Limits) -> Dir {
        let (progress, stop) = (Progress::default(), AtomicBool::new(false));
        let ctx = ctx(&progress, &stop);
        let (dir, result) =
            pool(threads).install(|| scan_with(&ctx, &native(root), 7, forced, limits));
        result.unwrap();
        assert_eq!(progress.errors.load(Relaxed), 0);
        assert_eq!(progress.items.load(Relaxed), dir.totals().items);
        dir
    }

    #[test]
    fn every_way_to_hand_on_builds_the_tree_of_the_plain_scan() {
        let tmp = tempfile::tempdir().unwrap();
        build(tmp.path());
        let want = shape(&plain(tmp.path()));
        assert!(want.len() > 400, "{} lines", want.len());

        for threads in [1, 4] {
            for forced in [None, Some(true), Some(false)] {
                let got = scan_spread(tmp.path(), threads, forced, LIMITS);
                assert_eq!(shape(&got), want, "{threads} threads, forced {forced:?}");
            }
        }
    }

    #[test]
    fn a_full_pool_leaves_the_directories_to_the_thread_that_found_them() {
        let tmp = tempfile::tempdir().unwrap();
        build(tmp.path());
        let want = shape(&plain(tmp.path()));

        for (entries, path_bytes) in [(0, LIMITS.path_bytes), (3, LIMITS.path_bytes), (50, 400)] {
            let limits = Limits {
                entries,
                path_bytes,
            };
            let got = scan_spread(tmp.path(), 4, Some(true), limits);
            assert_eq!(shape(&got), want, "{entries} entries, {path_bytes} bytes");
        }
    }

    #[test]
    fn the_root_keeps_its_inode_and_a_directory_sums_its_children() {
        let tmp = tempfile::tempdir().unwrap();
        build(tmp.path());

        let got = scan_spread(tmp.path(), 4, Some(true), LIMITS);

        assert_eq!(got.id, 7);
        let wide = got.find(b"wide").unwrap();
        let sub = got.entries[wide].dir.as_ref().unwrap();
        assert_eq!(got.entries[wide].kind, Kind::Dir);
        assert_eq!(got.entries[wide].disk, sub.totals().disk);
        assert_eq!(sub.entries.len(), 150);
        assert_eq!(got.entries[wide].items, 1 + 150 * 3);
    }

    #[test]
    fn a_cancelled_scan_returns_an_empty_root_without_listing() {
        let tmp = tempfile::tempdir().unwrap();
        build(tmp.path());
        for cancel in [true, false] {
            let (progress, stop) = (Progress::default(), AtomicBool::new(!cancel));
            progress.cancel.store(cancel, Relaxed);
            let ctx = ctx(&progress, &stop);

            let (dir, result) =
                pool(4).install(|| scan_with(&ctx, &native(tmp.path()), 7, Some(true), LIMITS));

            result.unwrap();
            assert!(dir.entries.is_empty());
            assert_eq!(progress.items.load(Relaxed), 0);
        }
    }

    #[test]
    fn a_directory_that_cannot_be_read_is_flagged_and_counted() {
        // SAFETY: geteuid has no arguments and cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        build(tmp.path());
        let locked = tmp.path().join("wide/000");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let (progress, stop) = (Progress::default(), AtomicBool::new(false));
        let ctx = ctx(&progress, &stop);

        let (dir, result) =
            pool(4).install(|| scan_with(&ctx, &native(tmp.path()), 7, Some(true), LIMITS));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        result.unwrap();
        assert_eq!(progress.errors.load(Relaxed), 1);
        let wide = dir.entries[dir.find(b"wide").unwrap()]
            .dir
            .as_ref()
            .unwrap();
        let entry = &wide.entries[wide.find(b"000").unwrap()];
        assert!(entry.has(flag::ERROR));
        assert!(dir.has_error());
    }

    fn pending(ino: u64, node: &Arc<Node>) -> Pending {
        Pending {
            path: CString::new(format!("/d/{ino}")).unwrap(),
            ino,
            child: Child {
                parent: Arc::clone(node),
                index: 0,
                own_disk: 0,
            },
        }
    }

    #[test]
    fn the_pool_gives_out_every_entry_once_in_a_mixed_order() {
        let node = Arc::new(Node {
            left: AtomicUsize::new(0),
            inner: Mutex::new(Inner {
                dir: Dir::default(),
                up: None,
                result: Ok(()),
            }),
        });
        let (progress, stop) = (Progress::default(), AtomicBool::new(false));
        let ctx = ctx(&progress, &stop);
        let scan = Scan {
            ctx: &ctx,
            forced: Some(true),
            limits: LIMITS,
            pool: Mutex::new(Pool {
                items: Vec::new(),
                path_bytes: 0,
                rng: 0x2545_F491_4F6C_DD1D,
            }),
            detector: Detector::new(),
        };
        for ino in 0..500 {
            assert!(scan.push(pending(ino, &node)).is_ok());
        }

        let order: Vec<u64> = std::iter::from_fn(|| scan.pop()).map(|p| p.ino).collect();

        assert_eq!(order.len(), 500);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..500).collect::<Vec<_>>());
        assert_ne!(order, sorted, "the order must not follow the pushes");
        assert!(scan.pop().is_none());
        assert_eq!(scan.pool.lock().unwrap().path_bytes, 0);
    }

    #[test]
    fn a_scan_turns_cold_after_busy_shares_fall_and_warm_after_they_rise() {
        let mut state = (1.0, false);
        // A warm scan keeps the CPUs busy, with one bad window.
        for busy in [0.95, 0.9, 0.3, 0.95, 0.9, 0.97] {
            state = smooth(state.0, state.1, busy);
            assert!(!state.1, "busy {busy}");
        }
        // A cold scan leaves the CPUs idle. Two windows are enough.
        state = smooth(state.0, state.1, 0.3);
        assert!(!state.1);
        state = smooth(state.0, state.1, 0.3);
        assert!(state.1);
        // One busy window in the middle of a cold scan changes nothing.
        state = smooth(state.0, state.1, 0.9);
        assert!(state.1);
        // A scan that stays busy turns warm.
        for _ in 0..4 {
            state = smooth(state.0, state.1, 0.95);
        }
        assert!(!state.1);
    }

    #[test]
    fn a_busy_share_above_one_counts_as_one() {
        assert_eq!(smooth(1.0, false, 3.0), (1.0, false));
    }
}
