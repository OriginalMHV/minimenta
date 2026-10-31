//! Delete-speed experiment. Not part of the product. It times several ways to
//! delete a folder and to move it to the Trash, on a fresh generated tree
//! before every round. Run it only in CI (see .github/workflows/delete-speed.yml).
//!
//! Usage: delete_bench --tree a|b|c --scratch DIR [--rounds N] [--cold] [--trash]
#![allow(clippy::all, clippy::pedantic, dead_code)]

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rayon::prelude::*;

#[cfg(target_os = "macos")]
#[path = "nsfm.rs"]
mod file_manager;

// ---------------------------------------------------------------- trees

const BODY: [u8; 1024] = [b'x'; 1024];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tree {
    /// node_modules like: 10^4 leaf folders with 10 files, 1 KiB each.
    Wide,
    /// 20 files of 50 MB.
    Large,
    /// One folder with 50,000 files.
    Flat,
}

/// Creates the tree and returns (files, folders).
fn generate(tree: Tree, root: &Path) -> io::Result<(usize, usize)> {
    match tree {
        Tree::Wide => {
            let files = AtomicUsize::new(0);
            let dirs = AtomicUsize::new(0);
            gen_wide(root, 0, &files, &dirs)?;
            Ok((files.into_inner(), dirs.into_inner()))
        }
        Tree::Large => {
            fs::create_dir_all(root)?;
            let chunk = vec![b'y'; 1 << 20];
            (0..20).into_par_iter().try_for_each(|i| {
                use io::Write;
                let mut f = fs::File::create(root.join(format!("big{i:02}.bin")))?;
                for _ in 0..50 {
                    f.write_all(&chunk)?;
                }
                Ok::<_, io::Error>(())
            })?;
            Ok((20, 1))
        }
        Tree::Flat => {
            fs::create_dir_all(root)?;
            (0..50_000).into_par_iter().try_for_each(|i| {
                fs::write(root.join(format!("f{i:05}.dat")), BODY)
            })?;
            Ok((50_000, 1))
        }
    }
}

fn gen_wide(path: &Path, level: u32, files: &AtomicUsize, dirs: &AtomicUsize) -> io::Result<()> {
    fs::create_dir_all(path)?;
    dirs.fetch_add(1, Ordering::Relaxed);
    if level == 4 {
        for i in 0..10 {
            fs::write(path.join(format!("file{i:02}.dat")), BODY)?;
        }
        files.fetch_add(10, Ordering::Relaxed);
        return Ok(());
    }
    (0..10).into_par_iter().try_for_each(|i| {
        gen_wide(&path.join(format!("d{i}")), level + 1, files, dirs)
    })
}

// ------------------------------------------------- portable primitives

#[cfg(unix)]
mod sys {
    use super::*;
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    pub type Name = CString;

    pub fn name_of(s: &[u8]) -> Name {
        CString::new(s).unwrap()
    }

    pub struct Handle {
        pub fd: OwnedFd,
        pub dev: u64,
    }

    fn fstat_dev(fd: RawFd) -> io::Result<u64> {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut st) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(st.st_dev as u64)
    }

    pub fn open_root_parent(path: &Path) -> io::Result<Handle> {
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let c = CString::new(parent.as_os_str().as_bytes()).unwrap();
        let fd = unsafe {
            libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC)
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let dev = fstat_dev(fd.as_raw_fd())?;
        Ok(Handle { fd, dev })
    }

    /// Opens a child folder without following a symlink. A folder on another
    /// file system (a mount point) is refused, `same_dev` is the root's device.
    pub fn open_child(parent: &Handle, name: &Name, same_dev: u64) -> io::Result<Handle> {
        let fd = unsafe {
            libc::openat(
                parent.fd.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let dev = fstat_dev(fd.as_raw_fd())?;
        if dev != same_dev {
            return Err(io::Error::other("another file system"));
        }
        Ok(Handle { fd, dev })
    }

    /// Lists a folder: (files and other non-folders, folders). Never follows links.
    pub fn list(h: &Handle) -> io::Result<(Vec<Name>, Vec<Name>)> {
        let dup = unsafe { libc::dup(h.fd.as_raw_fd()) };
        if dup < 0 {
            return Err(io::Error::last_os_error());
        }
        let dir = unsafe { libc::fdopendir(dup) };
        if dir.is_null() {
            let e = io::Error::last_os_error();
            unsafe { libc::close(dup) };
            return Err(e);
        }
        unsafe { libc::rewinddir(dir) };
        let (mut files, mut dirs) = (Vec::new(), Vec::new());
        loop {
            let ent = unsafe { libc::readdir(dir) };
            if ent.is_null() {
                break;
            }
            let ent = unsafe { &*ent };
            let name = unsafe { std::ffi::CStr::from_ptr(ent.d_name.as_ptr()) };
            let b = name.to_bytes();
            if b == b"." || b == b".." {
                continue;
            }
            let is_dir = if ent.d_type == libc::DT_DIR {
                true
            } else if ent.d_type == libc::DT_UNKNOWN {
                let mut st: libc::stat = unsafe { std::mem::zeroed() };
                let r = unsafe {
                    libc::fstatat(h.fd.as_raw_fd(), name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW)
                };
                r == 0 && (st.st_mode & libc::S_IFMT) == libc::S_IFDIR
            } else {
                false
            };
            let owned = name.to_owned();
            if is_dir { dirs.push(owned) } else { files.push(owned) }
        }
        unsafe { libc::closedir(dir) };
        Ok((files, dirs))
    }

    pub fn unlink(h: &Handle, name: &Name) -> io::Result<()> {
        if unsafe { libc::unlinkat(h.fd.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn rmdir(parent: &Handle, name: &Name) -> io::Result<()> {
        if unsafe { libc::unlinkat(parent.fd.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn not_empty(e: &io::Error) -> bool {
        matches!(e.raw_os_error(), Some(libc::ENOTEMPTY) | Some(libc::EEXIST))
    }

    pub fn raise_nofile() {
        let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        unsafe {
            if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) == 0 {
                #[cfg(target_os = "macos")]
                let want = lim.rlim_max.min(10_240);
                #[cfg(not(target_os = "macos"))]
                let want = lim.rlim_max.min(65_536);
                lim.rlim_cur = want;
                libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
            }
        }
    }

    pub fn os_name(n: &Name) -> OsString {
        OsString::from_vec(n.as_bytes().to_vec())
    }
}

#[cfg(windows)]
mod sys {
    use super::*;

    pub type Name = OsString;

    pub struct Handle {
        pub path: PathBuf,
    }

    pub fn open_root_parent(path: &Path) -> io::Result<Handle> {
        Ok(Handle { path: path.parent().unwrap().to_path_buf() })
    }

    pub fn open_child(parent: &Handle, name: &Name, _same_dev: u64) -> io::Result<Handle> {
        Ok(Handle { path: parent.path.join(name) })
    }

    pub fn list(h: &Handle) -> io::Result<(Vec<Name>, Vec<Name>)> {
        let (mut files, mut dirs) = (Vec::new(), Vec::new());
        for e in fs::read_dir(&h.path)? {
            let e = e?;
            if e.file_type()?.is_dir() {
                dirs.push(e.file_name())
            } else {
                files.push(e.file_name())
            }
        }
        Ok((files, dirs))
    }

    pub fn unlink(h: &Handle, name: &Name) -> io::Result<()> {
        let p = h.path.join(name);
        match fs::remove_file(&p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                if let Ok(m) = fs::metadata(&p) {
                    let mut perm = m.permissions();
                    perm.set_readonly(false);
                    let _ = fs::set_permissions(&p, perm);
                }
                fs::remove_file(&p).or_else(|_| fs::remove_dir(&p))
            }
            Err(e) => fs::remove_dir(&p).map_err(|_| e),
        }
    }

    pub fn rmdir(parent: &Handle, name: &Name) -> io::Result<()> {
        fs::remove_dir(parent.path.join(name))
    }

    pub fn not_empty(e: &io::Error) -> bool {
        // ERROR_DIR_NOT_EMPTY
        e.raw_os_error() == Some(145)
    }

    pub fn raise_nofile() {}

    pub fn os_name(n: &Name) -> OsString {
        n.clone()
    }
}

use sys::{Handle, Name};

// ------------------------------------------------------ parallel delete

/// A folder that is open for deleting. When the last reference drops, every
/// child is finished, and the folder removes itself from its parent. So the
/// bottom-up `rmdir` runs on whichever thread ends last, with no extra pass.
struct Node {
    h: Handle,
    name: Name,
    parent: Option<Arc<Node>>,
    errors: Arc<AtomicUsize>,
    /// Device of the root folder.
    dev: u64,
}

impl Drop for Node {
    fn drop(&mut self) {
        let Some(parent) = &self.parent else { return };
        match sys::rmdir(&parent.h, &self.name) {
            Ok(()) => {}
            Err(e) if sys::not_empty(&e) => {
                // A file appeared that the tree did not know. Sweep, retry once.
                sweep(&self.h, self.dev, &self.errors);
                if sys::rmdir(&parent.h, &self.name).is_err() {
                    self.errors.fetch_add(1, Ordering::Relaxed);
                }
            }
            Err(_) => {
                self.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Single-thread cleanup of whatever is left in `h`.
fn sweep(h: &Handle, dev: u64, errors: &AtomicUsize) {
    let Ok((files, dirs)) = sys::list(h) else {
        errors.fetch_add(1, Ordering::Relaxed);
        return;
    };
    for f in &files {
        if sys::unlink(h, f).is_err() {
            errors.fetch_add(1, Ordering::Relaxed);
        }
    }
    for d in &dirs {
        match sys::open_child(h, d, dev) {
            Ok(c) => {
                sweep(&c, dev, errors);
                if sys::rmdir(h, d).is_err() {
                    errors.fetch_add(1, Ordering::Relaxed);
                }
            }
            Err(_) => {
                errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Folders and file names that the scan already knows.
struct Known {
    files: Vec<Name>,
    dirs: Vec<(Name, Known)>,
}

fn scan_known(h: &Handle, dev: u64) -> io::Result<Known> {
    let (files, dirs) = sys::list(h)?;
    let mut out = Known { files, dirs: Vec::new() };
    for d in dirs {
        let c = sys::open_child(h, &d, dev)?;
        let k = scan_known(&c, dev)?;
        out.dirs.push((d, k));
    }
    Ok(out)
}

const CHUNK: usize = 1024;

fn unlink_many(h: &Handle, files: &[Name], errors: &AtomicUsize) {
    let one = |chunk: &[Name]| {
        for f in chunk {
            if sys::unlink(h, f).is_err() {
                errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    };
    if files.len() > 2 * CHUNK {
        files.par_chunks(CHUNK).for_each(one);
    } else {
        one(files);
    }
}

fn spawn_child<'s>(
    scope: &rayon::Scope<'s>,
    node: &Arc<Node>,
    name: Name,
    known: Option<&'s Known>,
) {
    match sys::open_child(&node.h, &name, node.dev) {
        Ok(h) => {
            let child = Arc::new(Node {
                h,
                name,
                parent: Some(node.clone()),
                errors: node.errors.clone(),
                dev: node.dev,
            });
            scope.spawn(move |s| run_dir(s, child, known));
        }
        Err(_) => {
            node.errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn run_dir<'s>(scope: &rayon::Scope<'s>, node: Arc<Node>, known: Option<&'s Known>) {
    match known {
        Some(k) => {
            for (name, sub) in &k.dirs {
                spawn_child(scope, &node, name.clone(), Some(sub));
            }
            unlink_many(&node.h, &k.files, &node.errors);
        }
        None => match sys::list(&node.h) {
            Ok((files, dirs)) => {
                for name in dirs {
                    spawn_child(scope, &node, name, None);
                }
                unlink_many(&node.h, &files, &node.errors);
            }
            Err(_) => {
                node.errors.fetch_add(1, Ordering::Relaxed);
            }
        },
    }
}

/// Deletes `root` with `threads` threads. `known` skips the first readdir of
/// each folder. Returns the number of errors.
fn par_delete(root: &Path, threads: usize, known: Option<&Known>) -> io::Result<usize> {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
    let ph = sys::open_root_parent(root)?;
    let errors = Arc::new(AtomicUsize::new(0));
    #[cfg(unix)]
    let dev = ph.dev;
    #[cfg(not(unix))]
    let dev = 0;
    let name = root_name(root);
    let ch = sys::open_child(&ph, &name, dev)?;
    let parent = Arc::new(Node {
        h: ph,
        name: root_name(Path::new("p")),
        parent: None,
        errors: errors.clone(),
        dev,
    });
    let node = Arc::new(Node {
        h: ch,
        name,
        parent: Some(parent),
        errors: errors.clone(),
        dev,
    });
    pool.scope(|s| run_dir(s, node, known));
    Ok(errors.load(Ordering::Relaxed))
}

fn root_name(root: &Path) -> Name {
    let n = root.file_name().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        sys::name_of(n.as_bytes())
    }
    #[cfg(windows)]
    {
        n.to_os_string()
    }
}

/// Opens `root` for a scan that gives names to method 3.
fn scan_tree(root: &Path) -> io::Result<Known> {
    let ph = sys::open_root_parent(root)?;
    #[cfg(unix)]
    let dev = ph.dev;
    #[cfg(not(unix))]
    let dev = 0;
    let ch = sys::open_child(&ph, &root_name(root), dev)?;
    scan_known(&ch, dev)
}

// ----------------------------------------------------------- methods

type Outcome = (Duration, bool, String);

fn gone(p: &Path) -> bool {
    p.symlink_metadata().is_err()
}

fn time_baseline(root: &Path) -> Outcome {
    let t = Instant::now();
    let r = fs::remove_dir_all(root);
    let d = t.elapsed();
    (d, r.is_ok() && gone(root), r.err().map(|e| e.to_string()).unwrap_or_default())
}

fn time_par(root: &Path, threads: usize) -> Outcome {
    let t = Instant::now();
    let r = par_delete(root, threads, None);
    let d = t.elapsed();
    match r {
        Ok(0) => (d, gone(root), String::new()),
        Ok(n) => (d, false, format!("{n} errors")),
        Err(e) => (d, false, e.to_string()),
    }
}

/// Scans first (not timed), then deletes with the known names.
fn time_known(root: &Path, threads: usize) -> Outcome {
    let known = match scan_tree(root) {
        Ok(k) => k,
        Err(e) => return (Duration::ZERO, false, format!("scan: {e}")),
    };
    let t = Instant::now();
    let r = par_delete(root, threads, Some(&known));
    let d = t.elapsed();
    match r {
        Ok(0) => (d, gone(root), String::new()),
        Ok(n) => (d, false, format!("{n} errors")),
        Err(e) => (d, false, e.to_string()),
    }
}

/// Only the rename is timed. The delete afterwards is not.
fn time_rename(root: &Path) -> Outcome {
    let hidden = root.with_file_name(format!(".{}.deleting", root.file_name().unwrap().to_string_lossy()));
    let t = Instant::now();
    let r = fs::rename(root, &hidden);
    let d = t.elapsed();
    let ok = r.is_ok() && gone(root) && hidden.exists();
    let _ = fs::remove_dir_all(&hidden);
    (d, ok, r.err().map(|e| e.to_string()).unwrap_or_default())
}

#[cfg(unix)]
fn time_rm(root: &Path) -> Outcome {
    let t = Instant::now();
    let r = Command::new("rm").arg("-rf").arg(root).status();
    let d = t.elapsed();
    let ok = r.as_ref().is_ok_and(|s| s.success()) && gone(root);
    (d, ok, r.err().map(|e| e.to_string()).unwrap_or_default())
}

#[cfg(target_os = "macos")]
fn time_removefile(root: &Path) -> Outcome {
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn removefile(path: *const libc::c_char, state: *mut libc::c_void, flags: u32) -> libc::c_int;
    }
    const REMOVEFILE_RECURSIVE: u32 = 1;
    let c = std::ffi::CString::new(root.as_os_str().as_bytes()).unwrap();
    let t = Instant::now();
    let rc = unsafe { removefile(c.as_ptr(), std::ptr::null_mut(), REMOVEFILE_RECURSIVE) };
    let d = t.elapsed();
    let note = if rc == 0 { String::new() } else { io::Error::last_os_error().to_string() };
    (d, rc == 0 && gone(root), note)
}

#[cfg(windows)]
fn time_cmd_rd(root: &Path) -> Outcome {
    let t = Instant::now();
    let r = Command::new("cmd").args(["/C", "rd", "/s", "/q"]).arg(root).status();
    let d = t.elapsed();
    let ok = r.as_ref().is_ok_and(|s| s.success()) && gone(root);
    (d, ok, r.err().map(|e| e.to_string()).unwrap_or_default())
}

// ------------------------------------------------------------- trash

/// Runs `f` on a thread. Returns None when it takes longer than `limit`.
fn with_timeout<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(limit).ok()
}

#[cfg(target_os = "macos")]
fn time_finder(root: &Path) -> Outcome {
    use std::os::unix::ffi::OsStrExt;
    use std::process::Stdio;
    let script = "on run argv
  set theFiles to {}
  repeat with p in argv
    set end of theFiles to POSIX file (contents of p)
  end repeat
  tell application \"Finder\" to set trashed to delete theFiles
  if class of trashed is not list then set trashed to {trashed}
  set out to \"\"
  repeat with t in trashed
    set out to out & POSIX path of (t as alias) & (character id 0)
  end repeat
  return out
end run";
    let t = Instant::now();
    let mut child = match Command::new("osascript")
        .arg("-e")
        .arg(script)
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (t.elapsed(), false, format!("spawn: {e}")),
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return (t.elapsed(), false, "TIMEOUT 30 s (Finder not usable on the runner)".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return (t.elapsed(), false, e.to_string()),
        }
    }
    let d = t.elapsed();
    let out = child.wait_with_output().unwrap();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return (d, false, format!("osascript failed: {err}"));
    }
    let text = out.stdout.strip_suffix(b"\n").unwrap_or(&out.stdout).to_vec();
    let loc = text.split(|&b| b == 0).find(|p| !p.is_empty()).map(|p| PathBuf::from(std::ffi::OsStr::from_bytes(p)));
    let ok = gone(root) && loc.as_ref().is_some_and(|l| l.symlink_metadata().is_ok());
    if let Some(l) = loc {
        let _ = fs::remove_dir_all(l);
    }
    (d, ok, String::new())
}

#[cfg(target_os = "macos")]
fn time_nsfm(root: &Path) -> Outcome {
    let t = Instant::now();
    let r = file_manager::trash(root);
    let d = t.elapsed();
    match r {
        Ok(Some(loc)) => {
            let ok = gone(root) && loc.symlink_metadata().is_ok();
            let _ = fs::remove_dir_all(loc);
            (d, ok, String::new())
        }
        Ok(None) => (d, gone(root), "no resulting URL".into()),
        Err(e) => (d, false, e),
    }
}

#[cfg(not(target_os = "macos"))]
fn time_trash_crate(root: &Path) -> Outcome {
    let name = root.file_name().unwrap().to_os_string();
    let parent = std::fs::canonicalize(root.parent().unwrap()).ok();
    let p = root.to_path_buf();
    let t = Instant::now();
    let r = with_timeout(Duration::from_secs(300), move || trash::delete(&p));
    let d = t.elapsed();
    let r = match r {
        None => return (d, false, "TIMEOUT 300 s".into()),
        Some(r) => r,
    };
    if let Err(e) = r {
        return (d, false, e.to_string());
    }
    let mut note = String::new();
    let mut in_trash = false;
    match trash::os_limited::list() {
        Ok(items) => {
            let mine: Vec<_> = items
                .into_iter()
                .filter(|i| i.name == name && std::fs::canonicalize(&i.original_parent).ok() == parent)
                .collect();
            in_trash = !mine.is_empty();
            if let Err(e) = trash::os_limited::purge_all(mine) {
                note = format!("purge: {e}");
            }
        }
        Err(e) => note = format!("list: {e}"),
    }
    (d, gone(root) && in_trash, note)
}

// ---------------------------------------------------------- harness

struct Row {
    method: String,
    round: usize,
    secs: f64,
    files: usize,
    ok: bool,
    note: String,
}

fn drop_caches() {
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("sudo")
            .args(["sh", "-c", "sync; echo 3 > /proc/sys/vm/drop_caches"])
            .status();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("sync").status();
        let _ = Command::new("sudo").arg("purge").status();
    }
}

fn median(v: &mut Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n == 0 {
        f64::NAN
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let tree = match get("--tree").as_deref() {
        Some("a") => Tree::Wide,
        Some("b") => Tree::Large,
        Some("c") => Tree::Flat,
        _ => panic!("--tree a|b|c"),
    };
    let scratch = PathBuf::from(get("--scratch").expect("--scratch DIR"));
    let rounds: usize = get("--rounds").map_or(5, |s| s.parse().unwrap());
    let cold = args.iter().any(|a| a == "--cold");
    let do_trash = args.iter().any(|a| a == "--trash");
    sys::raise_nofile();
    rayon::ThreadPoolBuilder::new().num_threads(16).build_global().unwrap();
    fs::create_dir_all(&scratch).unwrap();
    let root = scratch.join("tree");
    let os = std::env::consts::OS;
    println!("BENCH os={os} tree={tree:?} rounds={rounds} cold={cold} cpus={}", std::thread::available_parallelism().map_or(0, |n| n.get()));

    type Method = (String, Box<dyn Fn(&Path) -> Outcome>);
    let mut methods: Vec<Method> = Vec::new();
    let mut m = |n: &str, f: Box<dyn Fn(&Path) -> Outcome>| methods.push((n.to_string(), f));
    if do_trash {
        #[cfg(target_os = "macos")]
        {
            m("trash:nsfm", Box::new(time_nsfm));
            m("trash:finder", Box::new(time_finder));
        }
        #[cfg(not(target_os = "macos"))]
        m("trash:crate", Box::new(time_trash_crate));
    } else {
        m("baseline remove_dir_all", Box::new(time_baseline));
        let thread_set: &[usize] = if tree == Tree::Large { &[8] } else { &[1, 4, 8, 16] };
        for &n in thread_set {
            m(&format!("par-walk t={n}"), Box::new(move |p| time_par(p, n)));
        }
        if tree != Tree::Large {
            for n in [8, 16] {
                m(&format!("par-known t={n}"), Box::new(move |p| time_known(p, n)));
            }
            m("rename only (UX)", Box::new(time_rename));
        }
        #[cfg(unix)]
        m("rm -rf (process)", Box::new(time_rm));
        #[cfg(target_os = "macos")]
        m("removefile RECURSIVE", Box::new(time_removefile));
        #[cfg(windows)]
        m("cmd rd /s /q", Box::new(time_cmd_rd));
    }

    let mut rows: Vec<Row> = Vec::new();
    let mut skip: Vec<String> = Vec::new();
    for round in 0..rounds {
        for (name, f) in &methods {
            if skip.contains(name) {
                continue;
            }
            let _ = fs::remove_dir_all(&root);
            let g = Instant::now();
            let (files, dirs) = generate(tree, &root).expect("generate");
            let gen_s = g.elapsed().as_secs_f64();
            if cold {
                drop_caches();
            }
            let (d, ok, note) = f(&root);
            let secs = d.as_secs_f64();
            println!(
                "RESULT\t{os}\t{tree:?}\t{name}\t{round}\t{secs:.4}\t{files}\t{dirs}\t{ok}\t{note}\tgen={gen_s:.1}s",
            );
            if note.contains("TIMEOUT") {
                skip.push(name.clone());
            }
            rows.push(Row { method: name.clone(), round, secs, files, ok, note });
            let _ = fs::remove_dir_all(&root);
        }
    }

    println!("\nSUMMARY os={os} tree={tree:?} cold={cold} (round 0 kept, excluded from medians)");
    println!("{:<26} {:>9} {:>12} {:>8} {:>6}", "method", "median s", "files/s", "ratio", "fails");
    let base = {
        let mut v: Vec<f64> = rows.iter().filter(|r| r.round > 0 && r.ok && r.method.starts_with("baseline")).map(|r| r.secs).collect();
        median(&mut v)
    };
    for (name, _) in &methods {
        let mut v: Vec<f64> = rows.iter().filter(|r| r.round > 0 && r.ok && &r.method == name).map(|r| r.secs).collect();
        let fails = rows.iter().filter(|r| &r.method == name && !r.ok).count();
        let files = rows.iter().find(|r| &r.method == name).map_or(0, |r| r.files);
        let med = median(&mut v);
        println!(
            "{:<26} {:>9.4} {:>12.0} {:>7.2}x {:>6}",
            name,
            med,
            files as f64 / med,
            base / med,
            fails
        );
    }
    for r in rows.iter().filter(|r| !r.ok) {
        println!("FAIL {} round {}: {}", r.method, r.round, r.note);
    }
    if let Some(out) = std::env::var_os("BENCH_OUT") {
        let _ = fs::write(out, "see log");
    }
    std::process::exit(0);
}

// ------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn small(root: &Path) {
        for d in 0..4 {
            let dir = root.join(format!("d{d}")).join("sub");
            fs::create_dir_all(&dir).unwrap();
            for f in 0..5 {
                fs::write(dir.join(format!("f{f}")), b"x").unwrap();
            }
            fs::write(root.join(format!("d{d}")).join("top"), b"x").unwrap();
        }
        fs::write(root.join("rootfile"), b"x").unwrap();
    }

    fn fresh() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("tree");
        fs::create_dir(&root).unwrap();
        small(&root);
        (t, root)
    }

    #[test]
    fn every_method_removes_the_tree() {
        for threads in [1, 4, 16] {
            let (_t, root) = fresh();
            let (_, ok, note) = time_par(&root, threads);
            assert!(ok, "par {threads}: {note}");
        }
        let (_t, root) = fresh();
        assert!(time_baseline(&root).1);
        let (_t, root) = fresh();
        assert!(time_known(&root, 8).1);
        let (_t, root) = fresh();
        assert!(time_rename(&root).1);
        #[cfg(unix)]
        {
            let (_t, root) = fresh();
            assert!(time_rm(&root).1);
        }
    }

    #[test]
    fn known_names_miss_a_new_file_and_still_finish() {
        let (_t, root) = fresh();
        let known = scan_tree(&root).unwrap();
        fs::write(root.join("d1").join("sub").join("late"), b"x").unwrap();
        fs::create_dir_all(root.join("d2").join("newdir")).unwrap();
        fs::write(root.join("d2").join("newdir").join("x"), b"x").unwrap();
        assert_eq!(par_delete(&root, 4, Some(&known)).unwrap(), 0);
        assert!(gone(&root));
    }

    #[test]
    fn large_flat_folder_uses_chunks() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("flat");
        fs::create_dir(&root).unwrap();
        for i in 0..5000 {
            fs::write(root.join(format!("f{i}")), b"x").unwrap();
        }
        assert_eq!(par_delete(&root, 8, None).unwrap(), 0);
        assert!(gone(&root));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let t = tempfile::tempdir().unwrap();
        let outside = t.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"x").unwrap();
        let root = t.path().join("tree");
        fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("sub").join("link2")).unwrap();
        assert_eq!(par_delete(&root, 4, None).unwrap(), 0);
        assert!(gone(&root));
        assert!(outside.join("keep").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_as_root_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let outside = t.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"x").unwrap();
        let link = t.path().join("link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(par_delete(&link, 4, None).is_err());
        assert!(outside.join("keep").exists());
    }
}
