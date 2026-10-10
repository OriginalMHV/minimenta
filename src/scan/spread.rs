//! Scheduler for scans on Linux that spreads the reads over the disk.
//!
//! A cold scan waits for the disk. The directories of one parent sit next to
//! each other on the disk, so threads that follow the tree read the same part
//! of the disk at the same time. On the virtual disks of cloud machines this
//! kept the request rate at about 78% of what the disk serves. Directories in
//! random order raised it. A cold scan of `/usr` on GitHub runners took about
//! 0.92x (SCSI disk) and 0.89x (`NVMe` disk) of the time of the earlier scanner.
//! The row "Spread the directory reads with lazy paths and split ranges" in
//! `docs/experiments.md` has the runs. Spinning disks, network drives and local
//! `NVMe` disks were not measured.
//!
//! The scheduler keeps a pool of directories that wait for a scan. A thread
//! that lists a directory puts its subdirectories into the pool, and starts
//! one task for each. A task takes a random directory out of the pool, which
//! is not always the one that started it. A directory does not wait for its
//! children. The last child to finish attaches its tree to the parent and then
//! finishes the parent in the same way.
//!
//! A pool entry is a pointer to the parent and an index, and the path of a
//! subdirectory is built when its scan starts. The parent keeps one list of
//! its subdirectories, as the earlier scanner did. A subdirectory that finds
//! the pool full does not wait for a place. It runs as a plain task.
//!
//! A warm scan does not wait for the disk, and the random order costs it a few
//! percent. A detector tells the two cases apart. While the scan keeps the
//! CPUs busy, the subdirectories of a parent are plain tasks. A task splits
//! its range of subdirectories in halves and hands the upper half to another
//! thread, as `par_iter` does, so a wide parent needs no task per child.

use std::io;
use std::sync::atomic::{
    AtomicBool, AtomicUsize,
    Ordering::{AcqRel, Relaxed},
};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rayon::Scope;

use super::{Ctx, NativePath, SubDir, child_path, platform};
use crate::tree::{Dir, flag};

/// Where the pool stops growing.
#[derive(Clone, Copy)]
struct Limits {
    /// Entries in the pool. The peak of the pool in a cold scan of `/usr` was
    /// 14,989 to 35,719 entries (random pick, 16 to 64 threads, in the study
    /// in `docs/experiments.md`). The limit is about twice the highest peak.
    entries: usize,
    /// Each entry counts the length of the path of its parent. The parent
    /// keeps that path alive, so the limit bounds the paths that the pool
    /// keeps. With paths of 100 bytes the entry limit comes first.
    path_bytes: usize,
}

const LIMITS: Limits = Limits {
    entries: 65_536,
    path_bytes: 32 << 20,
};

/// Items per window of the detector. The limits below were fitted to windows
/// of this size, which gives about 180 windows for `/usr`. On the runners, a
/// cold scan of `/usr` took about 39 ms per window. A warm scan took about
/// 3.7 ms per window over 180 windows. It closes only about 120 windows, so a
/// closed warm window took about 6 ms. Runs 37986765908 and 37990715838 have
/// the data. The size was chosen and not tuned.
const WINDOW_ITEMS: u64 = 4096;
/// A window shorter than this is skipped and joins the next one. A warm scan of
/// `/usr` closed 120 of its 180 possible windows, so about a third of the warm
/// windows were this short. The value was chosen and not tuned.
const SHORTEST_WINDOW: f64 = 0.002;
/// Weight of the windows before the last one in the smoothed busy share.
const KEEP: f64 = 0.5;
/// A scan turns cold when the smoothed busy share falls below this value.
/// Warm scans of `/usr` kept at least 85% of the CPUs busy in 95% of the
/// windows. The median window of a cold scan kept 23% to 47% busy (240 scans
/// on 6 runners, 25% to 42% per runner). All data comes from runners with
/// 4 vCPUs. Machines with more CPUs, or with other busy threads in the
/// process, were not measured.
const ENTER_COLD: f64 = 0.55;
/// A cold scan turns warm again above this value.
const LEAVE_COLD: f64 = 0.75;
/// Any non-zero start works for the generator. A fixed start gives the same
/// picks for the same pool sizes.
const SEED: u64 = 0x2545_F491_4F6C_DD1D;

type Slot = Mutex<Option<(Dir, io::Result<()>)>>;

/// The path, the inode and the return address of a directory to scan.
type Next = (NativePath, u64, Up);

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
    /// The path of the directory. A child builds its own path from it.
    path: NativePath,
    /// The subdirectories from the listing. The list does not change.
    subdirs: Vec<SubDir>,
    inner: Mutex<Inner>,
}

struct Inner {
    dir: Dir,
    up: Option<Up>,
    result: io::Result<()>,
}

impl Node {
    fn new(
        path: NativePath,
        subdirs: Vec<SubDir>,
        dir: Dir,
        result: io::Result<()>,
        up: Option<Up>,
    ) -> Arc<Node> {
        Arc::new(Node {
            left: AtomicUsize::new(subdirs.len()),
            path,
            subdirs,
            inner: Mutex::new(Inner { dir, up, result }),
        })
    }

    /// What a thread needs to scan subdirectory `k`. The path exists from here
    /// on. Before, the subdirectory costs one list entry.
    fn child(self: &Arc<Self>, k: usize) -> Next {
        let sub = &self.subdirs[k];
        let (path, own_disk) = {
            let inner = self.inner.lock().unwrap();
            let entry = &inner.dir.entries[sub.index];
            (child_path(&self.path, inner.dir.name(entry)), entry.disk)
        };
        let up = Up::Child(Child {
            parent: Arc::clone(self),
            index: sub.index,
            own_disk,
        });
        (path, sub.ino, up)
    }
}

/// A subdirectory in the pool.
struct Waiting {
    node: Arc<Node>,
    /// Position of the subdirectory in `node.subdirs`.
    k: usize,
}

struct Pool {
    items: Vec<Waiting>,
    path_bytes: usize,
    rng: u64,
    /// Entries that were ever pushed.
    #[cfg(test)]
    pushed: usize,
}

impl Pool {
    fn new() -> Self {
        Pool {
            items: Vec::new(),
            path_bytes: 0,
            rng: SEED,
            #[cfg(test)]
            pushed: 0,
        }
    }

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

struct Scan<'a> {
    ctx: &'a Ctx<'a>,
    /// Tells the scan to spread the reads or not. `None` leaves the choice
    /// to the detector.
    forced: Option<bool>,
    limits: Limits,
    pool: Mutex<Pool>,
    detector: Detector,
    /// Ranges of subdirectories that went to another task.
    #[cfg(test)]
    splits: AtomicUsize,
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
    Scan::new(ctx, forced, limits).run(root, id)
}

impl<'a> Scan<'a> {
    fn new(ctx: &'a Ctx<'a>, forced: Option<bool>, limits: Limits) -> Self {
        Scan {
            ctx,
            forced,
            limits,
            pool: Mutex::new(Pool::new()),
            detector: Detector::new(),
            #[cfg(test)]
            splits: AtomicUsize::new(0),
        }
    }
}

impl Scan<'_> {
    fn run(&self, root: &NativePath, id: u64) -> (Dir, io::Result<()>) {
        let slot: Arc<Slot> = Arc::default();
        rayon::scope(|s| visit(s, self, root.clone(), id, Up::Root(Arc::clone(&slot))));
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

    fn spread(&self) -> bool {
        self.forced.unwrap_or_else(|| self.detector.cold())
    }

    /// Puts the subdirectories of `node` into the pool, from the first one on,
    /// until the pool is full. Returns how many went in.
    fn push_children(&self, node: &Arc<Node>) -> usize {
        let len = node.path.as_bytes().len().max(1);
        let mut pool = self.pool.lock().unwrap();
        let room = (self.limits.entries.saturating_sub(pool.items.len()))
            .min(self.limits.path_bytes.saturating_sub(pool.path_bytes) / len);
        let taken = room.min(node.subdirs.len());
        pool.items.extend((0..taken).map(|k| Waiting {
            node: Arc::clone(node),
            k,
        }));
        pool.path_bytes += taken * node.path.as_bytes().len();
        #[cfg(test)]
        {
            pool.pushed += taken;
        }
        taken
    }

    /// Takes a random directory out of the pool.
    fn pop(&self) -> Option<Waiting> {
        let mut pool = self.pool.lock().unwrap();
        let i = pool.next_index()?;
        let w = pool.items.swap_remove(i);
        pool.path_bytes -= w.node.path.as_bytes().len();
        Some(w)
    }
}

/// The task of one pool entry.
fn token<'s, 'a: 's>(s: &Scope<'s>, scan: &'s Scan<'a>) {
    let Some(w) = scan.pop() else {
        return;
    };
    if scan.ctx.stopped() {
        return;
    }
    let (path, ino, up) = w.node.child(w.k);
    drop(w);
    visit(s, scan, path, ino, up);
}

/// Puts the subdirectories of `node` into the pool and starts one task for
/// each. Returns how many went in.
fn pool_children<'s, 'a: 's>(s: &Scope<'s>, scan: &'s Scan<'a>, node: &Arc<Node>) -> usize {
    let pooled = scan.push_children(node);
    for _ in 0..pooled {
        s.spawn(move |s| token(s, scan));
    }
    pooled
}

/// Gives the children `lo..hi` of `node` to the threads. It starts a task for
/// the upper half of the range, again and again, and returns the first child
/// for the calling thread. An idle thread that takes such a task splits it in
/// the same way. A wide parent so needs only a few tasks at a time, and a
/// path exists only for the children that run.
fn hand_on<'s, 'a: 's>(
    s: &Scope<'s>,
    scan: &'s Scan<'a>,
    node: &Arc<Node>,
    lo: usize,
    mut hi: usize,
) -> Option<Next> {
    while hi.saturating_sub(lo) > 1 {
        if scan.ctx.stopped() {
            return None;
        }
        let mid = lo + (hi - lo) / 2;
        let node = Arc::clone(node);
        #[cfg(test)]
        scan.splits.fetch_add(1, Relaxed);
        s.spawn(move |s| run_range(s, scan, &node, mid, hi));
        hi = mid;
    }
    (lo < hi && !scan.ctx.stopped()).then(|| node.child(lo))
}

fn run_range<'s, 'a: 's>(
    s: &Scope<'s>,
    scan: &'s Scan<'a>,
    node: &Arc<Node>,
    lo: usize,
    hi: usize,
) {
    if let Some((path, ino, up)) = hand_on(s, scan, node, lo, hi) {
        visit(s, scan, path, ino, up);
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

        let node = Node::new(path, subdirs, dir, result, Some(up));
        let pooled = if scan.spread() {
            pool_children(s, scan, &node)
        } else {
            0
        };
        let Some(next) = hand_on(s, scan, &node, pooled, node.subdirs.len()) else {
            return;
        };
        (path, ino, up) = next;
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
/// threads could use. The CPU time counts every thread of the process.
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

impl Window {
    /// The state after the window that ends at `now` with `cpu` seconds of
    /// CPU time used, and whether the scan is cold. `None` for a window that
    /// is too short to count.
    fn step(&self, now: Instant, cpu: f64, capacity: f64, cold: bool) -> Option<(Window, bool)> {
        let wall = now.duration_since(self.at).as_secs_f64();
        if wall < SHORTEST_WINDOW {
            return None;
        }
        let busy = (cpu - self.cpu) / wall / capacity;
        let (smoothed, cold) = smooth(self.busy, cold, busy);
        let next = Window {
            at: now,
            cpu,
            busy: smoothed,
        };
        Some((next, cold))
    }
}

/// Tells whether the item counter passed a multiple of the window size
/// between two values.
fn ends_window(before: u64, after: u64) -> bool {
    before / WINDOW_ITEMS != after / WINDOW_ITEMS
}

impl Detector {
    fn new() -> Self {
        let cores = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
        Self::starting(
            cores.min(rayon::current_num_threads()) as f64,
            Instant::now(),
        )
    }

    fn starting(capacity: f64, at: Instant) -> Self {
        Detector {
            capacity,
            cold: AtomicBool::new(false),
            last: Mutex::new(Window {
                at,
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
        if !ends_window(before, after) {
            return;
        }
        // Another thread that ends a window at the same time does the work.
        let Ok(mut last) = self.last.try_lock() else {
            return;
        };
        let step = last.step(
            Instant::now(),
            process_cpu_seconds(),
            self.capacity,
            self.cold(),
        );
        if let Some((window, cold)) = step {
            *last = window;
            self.cold.store(cold, Relaxed);
        }
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
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;
    use std::time::Duration;

    fn ctx_on<'a>(
        progress: &'a Progress,
        stop: &'a AtomicBool,
        one_fs: bool,
        root_dev: u64,
    ) -> Ctx<'a> {
        Ctx {
            one_fs,
            root_dev,
            progress,
            hardlinks: Mutex::default(),
            stop,
        }
    }

    fn ctx<'a>(progress: &'a Progress, stop: &'a AtomicBool) -> Ctx<'a> {
        ctx_on(progress, stop, false, 0)
    }

    fn native(path: &Path) -> NativePath {
        CString::new(path.as_os_str().as_bytes()).unwrap()
    }

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

    /// Hard links in three directories, and a directory whose listing needs
    /// several `getdents64` batches. Six of its entries are directories, and
    /// they come in the later batches.
    fn build_links_and_a_big_directory(root: &Path) {
        let big = root.join("big");
        fs::create_dir(&big).unwrap();
        for i in 0..3000 {
            let path = big.join(format!("{i:0>60}"));
            if i % 500 == 499 {
                fs::create_dir(&path).unwrap();
                File::create(path.join("inner.bin"))
                    .unwrap()
                    .write_all(&[4; 100])
                    .unwrap();
            } else {
                File::create(path).unwrap().write_all(&[1; 5]).unwrap();
            }
        }
        fs::create_dir_all(root.join("links/x")).unwrap();
        File::create(root.join("links/x/one.bin"))
            .unwrap()
            .write_all(&[5; 7000])
            .unwrap();
        fs::hard_link(root.join("links/x/one.bin"), root.join("links/two.bin")).unwrap();
        fs::hard_link(root.join("links/x/one.bin"), big.join("three.bin")).unwrap();
    }

    /// Many directories, each with files and one subdirectory. A scan passes
    /// the first 4096 items early, and the later directories find the
    /// detector in its new state.
    fn build_comb(root: &Path) {
        for i in 0..400 {
            let dir = root.join(format!("d{i:03}"));
            fs::create_dir_all(dir.join("e")).unwrap();
            for j in 0..28 {
                File::create(dir.join(format!("f{j}"))).unwrap();
            }
            File::create(dir.join("e/g")).unwrap();
        }
    }

    fn flat(root: &Path, n: usize) {
        for i in 0..n {
            fs::create_dir(root.join(format!("{i}"))).unwrap();
        }
    }

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

    /// Entries below `dir` with the flag.
    fn flagged(dir: &Dir, f: u8) -> usize {
        dir.entries
            .iter()
            .map(|e| usize::from(e.has(f)) + e.dir.as_ref().map_or(0, |sub| flagged(sub, f)))
            .sum()
    }

    fn pool(threads: usize) -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
    }

    fn plain_on(root: &Path, one_fs: bool, root_dev: u64) -> Dir {
        let (progress, stop) = (Progress::default(), AtomicBool::new(false));
        let ctx = ctx_on(&progress, &stop, one_fs, root_dev);
        let (dir, result) = pool(4).install(|| crate::scan::scan_dir(&ctx, &native(root), 0, 7));
        result.unwrap();
        dir
    }

    fn plain(root: &Path) -> Dir {
        plain_on(root, false, 0)
    }

    /// What a scan did, besides the tree.
    struct Ran {
        dir: Dir,
        /// Ranges of subdirectories that went to another task.
        splits: usize,
        /// Subdirectories that went through the pool.
        pushed: usize,
        cold: bool,
    }

    /// Runs a scan of `root`. `setup` changes the scan before it starts.
    /// Every scan must leave the pool empty.
    fn run_on(
        root: &Path,
        threads: usize,
        one_fs: bool,
        root_dev: u64,
        setup: impl FnOnce(&mut Scan<'_>),
    ) -> Ran {
        let (progress, stop) = (Progress::default(), AtomicBool::new(false));
        let ctx = ctx_on(&progress, &stop, one_fs, root_dev);
        let mut scan = Scan::new(&ctx, None, LIMITS);
        setup(&mut scan);
        let (dir, result) = pool(threads).install(|| scan.run(&native(root), 7));
        result.unwrap();
        assert_eq!(progress.errors.load(Relaxed), 0);
        assert_eq!(progress.items.load(Relaxed), dir.totals().items);
        let waiting = scan.pool.lock().unwrap();
        assert!(waiting.items.is_empty(), "the pool must be empty");
        assert_eq!(waiting.path_bytes, 0);
        Ran {
            dir,
            splits: scan.splits.load(Relaxed),
            pushed: waiting.pushed,
            cold: scan.detector.cold(),
        }
    }

    fn run(root: &Path, threads: usize, setup: impl FnOnce(&mut Scan<'_>)) -> Ran {
        run_on(root, threads, false, 0, setup)
    }

    fn scan_spread(root: &Path, threads: usize, forced: Option<bool>, limits: Limits) -> Dir {
        run(root, threads, |scan| {
            scan.forced = forced;
            scan.limits = limits;
        })
        .dir
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
    fn a_full_pool_sends_the_rest_to_tasks_that_other_threads_can_take() {
        let tmp = tempfile::tempdir().unwrap();
        build(tmp.path());
        let want = shape(&plain(tmp.path()));

        for (entries, path_bytes) in [(0, LIMITS.path_bytes), (3, LIMITS.path_bytes), (50, 400)] {
            let limits = Limits {
                entries,
                path_bytes,
            };
            let ran = run(tmp.path(), 4, |scan| {
                scan.forced = Some(true);
                scan.limits = limits;
            });
            assert_eq!(
                shape(&ran.dir),
                want,
                "{entries} entries, {path_bytes} bytes"
            );
            // The 150 directories below `wide` do not fit, so they were split.
            assert!(ran.splits >= 2, "{} splits", ran.splits);
            if entries == 0 {
                assert_eq!(ran.pushed, 0);
            }
        }
    }

    #[test]
    fn every_sibling_runs_once_however_the_range_is_split() {
        for n in [1, 2, 3, 5, 64, 1000] {
            let tmp = tempfile::tempdir().unwrap();
            flat(tmp.path(), n);
            let want = shape(&plain(tmp.path()));
            assert_eq!(want.len(), 2 * n);

            for forced in [Some(false), Some(true)] {
                for entries in [0, 1, 7, LIMITS.entries] {
                    let limits = Limits { entries, ..LIMITS };
                    let got = scan_spread(tmp.path(), 4, forced, limits);
                    assert_eq!(shape(&got), want, "{n} siblings, {forced:?}, {entries}");
                }
            }
        }
    }

    #[test]
    fn a_directory_with_more_subdirectories_than_the_pool_holds_gets_every_one() {
        let tmp = tempfile::tempdir().unwrap();
        let n = LIMITS.entries + 5_000;
        flat(tmp.path(), n);

        let ran = run(tmp.path(), 4, |scan| scan.forced = Some(true));

        assert_eq!(ran.dir.entries.len(), n);
        assert!(ran.dir.entries.iter().all(|e| e.dir.is_some()));
        assert!(ran.pushed >= LIMITS.entries, "{} pushed", ran.pushed);
        // The 5,000 that found the pool full were split, not scanned in a row.
        assert!(ran.splits >= 12, "{} splits", ran.splits);
    }

    #[test]
    fn a_waiting_directory_holds_a_pointer_and_an_index_and_no_path() {
        assert_eq!(size_of::<Waiting>(), 2 * size_of::<usize>());
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
    fn the_pool_path_counts_links_big_directories_and_other_file_systems_like_the_plain_scan() {
        let tmp = tempfile::tempdir().unwrap();
        build(tmp.path());
        build_links_and_a_big_directory(tmp.path());
        let dev = fs::metadata(tmp.path()).unwrap().dev();

        // The last case treats every directory as another file system.
        for (one_fs, root_dev) in [(false, 0), (true, dev), (true, dev + 1)] {
            let want = plain_on(tmp.path(), one_fs, root_dev);
            for threads in [1, 4] {
                for forced in [Some(true), Some(false)] {
                    let got = run_on(tmp.path(), threads, one_fs, root_dev, |scan| {
                        scan.forced = forced;
                    })
                    .dir;
                    let case =
                        format!("{threads} threads, {forced:?}, one_fs {one_fs}, {root_dev}");
                    assert_eq!(got.totals(), want.totals(), "{case}");
                    assert_eq!(
                        flagged(&got, flag::HARDLINK),
                        flagged(&want, flag::HARDLINK)
                    );
                    assert_eq!(
                        flagged(&got, flag::OTHER_FS),
                        flagged(&want, flag::OTHER_FS)
                    );
                    assert_eq!(got.entries.len(), want.entries.len(), "{case}");
                }
            }
        }
        let all = plain(tmp.path());
        assert_eq!(flagged(&all, flag::HARDLINK), 2);
        let big = all.find(b"big").unwrap();
        assert_eq!(all.entries[big].dir.as_ref().unwrap().entries.len(), 3001);
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
    fn a_scan_that_stops_with_directories_waiting_drains_the_pool() {
        let tmp = tempfile::tempdir().unwrap();
        build(tmp.path());
        for forced in [true, false] {
            let (progress, stop) = (Progress::default(), AtomicBool::new(false));
            let ctx = ctx(&progress, &stop);
            let scan = Scan::new(&ctx, Some(forced), LIMITS);
            let slot: Arc<Slot> = Arc::default();

            // One thread lists the root and cannot start the tasks it made
            // before the closure ends. The stop comes at that point.
            pool(1).install(|| {
                rayon::scope(|s| {
                    visit(s, &scan, native(tmp.path()), 7, Up::Root(Arc::clone(&slot)));
                    if forced {
                        assert_eq!(scan.pool.lock().unwrap().items.len(), 3);
                    }
                    progress.cancel.store(true, Relaxed);
                });
            });

            assert!(slot.lock().unwrap().is_none(), "forced {forced}");
            let waiting = scan.pool.lock().unwrap();
            assert!(waiting.items.is_empty(), "forced {forced}");
            assert_eq!(waiting.path_bytes, 0);
        }
    }

    #[test]
    fn a_scan_that_stops_from_another_thread_returns_and_drains_the_pool() {
        let tmp = tempfile::tempdir().unwrap();
        build_comb(tmp.path());
        let want = plain(tmp.path()).totals();

        for forced in [Some(true), Some(false)] {
            let (progress, stop) = (Progress::default(), AtomicBool::new(false));
            let ctx = ctx(&progress, &stop);
            let mut scan = Scan::new(&ctx, None, LIMITS);
            scan.forced = forced;
            let done = AtomicBool::new(false);

            let (dir, result) = std::thread::scope(|threads| {
                threads.spawn(|| {
                    while !done.load(Relaxed) {
                        if progress.items.load(Relaxed) >= 1000 {
                            progress.cancel.store(true, Relaxed);
                            return;
                        }
                        std::thread::yield_now();
                    }
                });
                let out = pool(4).install(|| scan.run(&native(tmp.path()), 7));
                done.store(true, Relaxed);
                out
            });

            result.unwrap();
            // The stop may come after the last directory. Then the tree is whole.
            if !dir.entries.is_empty() {
                assert_eq!(dir.totals(), want, "forced {forced:?}");
            }
            let waiting = scan.pool.lock().unwrap();
            assert!(waiting.items.is_empty(), "forced {forced:?}");
            assert_eq!(waiting.path_bytes, 0);
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

    fn node_with(children: usize, path: &str) -> Arc<Node> {
        let subdirs = (0..children)
            .map(|i| SubDir {
                index: i,
                expected: 0,
                ino: i as u64,
            })
            .collect();
        Node::new(
            CString::new(path).unwrap(),
            subdirs,
            Dir::default(),
            Ok(()),
            None,
        )
    }

    fn scan_for<'a>(ctx: &'a Ctx<'a>, limits: Limits) -> Scan<'a> {
        Scan::new(ctx, Some(true), limits)
    }

    #[test]
    fn the_pool_gives_out_every_entry_once_in_a_random_order() {
        let (progress, stop) = (Progress::default(), AtomicBool::new(false));
        let ctx = ctx(&progress, &stop);
        let scan = scan_for(&ctx, LIMITS);
        let node = node_with(500, "/d");
        assert_eq!(scan.push_children(&node), 500);

        let order: Vec<usize> = std::iter::from_fn(|| scan.pop()).map(|w| w.k).collect();

        assert_eq!(order.len(), 500);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..500).collect::<Vec<_>>());
        // Oldest first and newest first are the two orders that lost. The
        // seed is fixed, so the count of rising neighbours does not vary.
        assert_ne!(order, sorted, "the order must not follow the pushes");
        sorted.reverse();
        assert_ne!(order, sorted, "the order must not reverse the pushes");
        let rising = order.windows(2).filter(|w| w[1] > w[0]).count();
        assert!(
            (200..=300).contains(&rising),
            "{rising} of 499 neighbours rise"
        );
        assert!(scan.pop().is_none());
        assert_eq!(scan.pool.lock().unwrap().path_bytes, 0);
    }

    #[test]
    fn the_pool_stops_at_the_entry_limit_and_at_the_path_limit() {
        let (progress, stop) = (Progress::default(), AtomicBool::new(false));
        let ctx = ctx(&progress, &stop);
        let node = node_with(10, "/d");

        let scan = scan_for(
            &ctx,
            Limits {
                entries: 4,
                ..LIMITS
            },
        );
        assert_eq!(scan.push_children(&node), 4);
        assert_eq!(scan.push_children(&node), 0);
        assert!(scan.pop().is_some());
        assert_eq!(scan.push_children(&node), 1);

        // A path of two bytes counts two bytes for each entry.
        let scan = scan_for(
            &ctx,
            Limits {
                path_bytes: 5,
                ..LIMITS
            },
        );
        assert_eq!(scan.push_children(&node), 2);
        assert_eq!(scan.pool.lock().unwrap().path_bytes, 4);
        assert_eq!(scan.push_children(&node), 0);
    }

    fn windows(start: (f64, bool), busy: &[f64]) -> (f64, bool) {
        busy.iter()
            .fold(start, |state, &b| smooth(state.0, state.1, b))
    }

    #[test]
    fn one_idle_window_does_not_make_a_warm_scan_cold() {
        let mut state = (1.0, false);
        for busy in [0.95, 0.9, 0.3, 0.95, 0.9, 0.97] {
            state = smooth(state.0, state.1, busy);
            assert!(!state.1, "busy {busy}");
        }
    }

    #[test]
    fn two_idle_windows_make_a_scan_cold_and_one_busy_window_does_not_undo_it() {
        assert!(!windows((1.0, false), &[0.3]).1);
        let cold = windows((1.0, false), &[0.3, 0.3]);
        assert!(cold.1);
        assert!(windows(cold, &[0.9]).1);
    }

    #[test]
    fn a_cold_scan_that_keeps_the_cpus_busy_turns_warm() {
        let cold = windows((1.0, false), &[0.3, 0.3, 0.3]);
        assert!(cold.1);
        assert!(!windows(cold, &[0.95; 4]).1);
    }

    #[test]
    fn a_busy_share_above_one_counts_as_one() {
        assert_eq!(smooth(1.0, false, 3.0), (1.0, false));
    }

    #[test]
    fn a_window_ends_when_the_counter_passes_a_multiple_of_the_window_size() {
        assert!(!ends_window(0, 4095));
        assert!(ends_window(4095, 4096));
        assert!(!ends_window(4096, 8191));
        assert!(ends_window(8191, 8192));
        assert!(ends_window(10, 3 * WINDOW_ITEMS));
        assert!(!ends_window(5000, 5000));
    }

    fn window_at(at: Instant) -> Window {
        Window {
            at,
            cpu: 1.0,
            busy: 1.0,
        }
    }

    #[test]
    fn a_window_is_the_cpu_time_over_the_wall_time_and_the_cpus() {
        let at = Instant::now();
        let last = window_at(at);
        let later = at + Duration::from_millis(100);

        // 0.1 s of CPU time in 0.1 s on 4 CPUs is a busy share of 0.25.
        let (next, cold) = last.step(later, 1.1, 4.0, false).unwrap();
        assert!(
            (next.busy - (0.5 + 0.5 * 0.25)).abs() < 1e-9,
            "{}",
            next.busy
        );
        assert_eq!((next.at, next.cpu, cold), (later, 1.1, false));

        let (next, cold) = last.step(later, 1.0, 4.0, false).unwrap();
        assert!((next.busy - 0.5).abs() < 1e-9);
        assert!(cold, "an idle window pulls a warm scan below the limit");

        // More CPU time than the CPUs allow counts as fully busy.
        let (next, cold) = last.step(later, 3.0, 4.0, true).unwrap();
        assert!((next.busy - 1.0).abs() < 1e-9);
        assert!(!cold);
    }

    #[test]
    fn a_window_shorter_than_two_milliseconds_is_skipped() {
        let at = Instant::now();
        let last = window_at(at);
        assert!(
            last.step(at + Duration::from_millis(1), 1.5, 4.0, false)
                .is_none()
        );
        assert!(
            last.step(at + Duration::from_millis(3), 1.0, 4.0, false)
                .is_some()
        );
    }

    #[test]
    fn a_cold_detector_sends_the_directories_through_the_pool_and_a_warm_scan_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        build_comb(tmp.path());
        let want = shape(&plain(tmp.path()));

        let ran = run(tmp.path(), 4, |scan| {
            scan.detector.cold.store(true, Relaxed);
        });

        assert_eq!(shape(&ran.dir), want);
        assert!(ran.pushed >= 400, "{} pushed", ran.pushed);
        let ran = run(tmp.path(), 4, |scan| scan.forced = Some(false));
        assert_eq!(ran.pushed, 0, "a warm scan does not use the pool");
    }

    #[test]
    fn a_scan_that_leaves_the_cpus_idle_turns_cold_and_uses_the_pool() {
        let tmp = tempfile::tempdir().unwrap();
        build_comb(tmp.path());
        let want = shape(&plain(tmp.path()));

        let ran = run(tmp.path(), 4, |scan| {
            // A huge capacity makes every window look idle. A start in the
            // past makes the first window long enough to count.
            let start = Instant::now()
                .checked_sub(Duration::from_secs(5))
                .unwrap_or_else(Instant::now);
            scan.detector = Detector::starting(1e9, start);
        });

        assert_eq!(shape(&ran.dir), want);
        assert!(ran.cold, "the detector must have seen the idle windows");
        assert!(ran.pushed > 0, "the later directories must use the pool");
    }

    #[test]
    fn a_scan_that_changes_from_warm_to_cold_and_back_builds_the_same_tree() {
        let tmp = tempfile::tempdir().unwrap();
        build_comb(tmp.path());
        let want = shape(&plain(tmp.path()));

        for _ in 0..3 {
            let ran = run(tmp.path(), 4, |scan| scan.forced = None);
            assert_eq!(shape(&ran.dir), want);

            let (progress, stop) = (Progress::default(), AtomicBool::new(false));
            let ctx = ctx(&progress, &stop);
            let scan = Scan::new(&ctx, None, LIMITS);
            let done = AtomicBool::new(false);
            let (dir, result) = std::thread::scope(|threads| {
                threads.spawn(|| {
                    while !done.load(Relaxed) {
                        scan.detector.cold.fetch_xor(true, Relaxed);
                        std::thread::sleep(Duration::from_micros(50));
                    }
                });
                let out = pool(4).install(|| scan.run(&native(tmp.path()), 7));
                done.store(true, Relaxed);
                out
            });
            result.unwrap();
            assert_eq!(shape(&dir), want);
            assert!(scan.pool.lock().unwrap().items.is_empty());
        }
    }
}
