//! Deletes a folder with many threads.
//!
//! `remove_dir_all` removes one entry at a time. This module walks the tree on
//! a pool of threads. Each thread lists a folder, removes its files and hands
//! its sub folders to other threads. The folder itself goes last, on the thread
//! that finishes its last child.
//!
//! On Unix every folder is open as a file descriptor. Files go with `unlinkat`
//! relative to that descriptor, so no thread resolves a long path again, and a
//! symlink that replaces a folder in the middle of the walk cannot lead the
//! walk out of the tree. On Windows the walk uses `read_dir`, `remove_file`
//! and `remove_dir`.
//!
//! Evidence for the design is in docs/experiments.md.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rayon::ThreadPool;
use rayon::prelude::*;

/// Names per task in a big folder. A folder with fewer names stays on one thread.
const CHUNK: usize = 1024;

/// The most threads. More threads were slower on Windows and gave nothing on Linux.
const MAX_THREADS: usize = 8;

/// The entries of one folder, split by what the walk does with them.
struct Listing {
    /// Files, symlinks and every other entry that is not a folder to enter.
    files: Vec<sys::Name>,
    dirs: Vec<sys::Name>,
}

/// Deletes the folder `path` and everything in it. A symlink or a file at
/// `path` is refused, and no link is ever followed.
///
/// All entries that can go are removed, also after an error. The first error is
/// returned at the end. Its message starts with the path of the failed entry
/// relative to `path`.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    let pool = pool()?;
    let (parent, name) = sys::open_parent(path)?;
    let (handle, dev) = sys::open_root(&parent, &name)?;
    let shared = Arc::new(Shared::new(dev));
    let root = Arc::new(Node {
        handle,
        name,
        parent: None,
        shared: shared.clone(),
    });
    pool.scope(|scope| run_dir(scope, &root));
    // Every task is done, so the root is the only reference left.
    finish(&parent, &root);
    shared.take_error()
}

/// One pool for the whole program. The scanner may use the global pool, and the
/// worker runs one delete at a time, so a private pool never competes with it.
fn pool() -> io::Result<&'static ThreadPool> {
    static POOL: OnceLock<ThreadPool> = OnceLock::new();
    if let Some(pool) = POOL.get() {
        return Ok(pool);
    }
    sys::raise_open_files();
    let threads = std::thread::available_parallelism().map_or(4, usize::from);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads.min(MAX_THREADS))
        .thread_name(|i| format!("delete-{i}"))
        .build()
        .map_err(io::Error::other)?;
    Ok(POOL.get_or_init(|| pool))
}

struct Shared {
    /// The device of the root folder. A folder on another device is a mount point.
    dev: sys::Dev,
    failed: AtomicBool,
    first_error: Mutex<Option<io::Error>>,
}

impl Shared {
    fn new(dev: sys::Dev) -> Self {
        Self {
            dev,
            failed: AtomicBool::new(false),
            first_error: Mutex::new(None),
        }
    }

    /// Keeps the first error. `path` is relative to the root and is only built
    /// for that first error.
    fn fail(&self, path: impl FnOnce() -> PathBuf, error: io::Error) {
        if self.failed.swap(true, Ordering::Relaxed) {
            return;
        }
        let path = path();
        let error = if path.as_os_str().is_empty() {
            error
        } else {
            io::Error::new(error.kind(), format!("{}: {error}", path.display()))
        };
        *self
            .first_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
    }

    fn take_error(&self) -> io::Result<()> {
        let error = self
            .first_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        error.map_or(Ok(()), Err)
    }
}

/// A folder that the walk has open. The folder removes itself from its parent
/// when the last reference drops, so the bottom-up `rmdir` needs no extra pass.
/// A child holds its parent, so a folder stays open while a child runs. Open
/// folders are the chain from the root to each running thread, not the whole tree.
struct Node {
    handle: sys::Handle,
    name: sys::Name,
    /// None for the root. `remove_tree` finishes the root itself.
    parent: Option<Arc<Node>>,
    shared: Arc<Shared>,
}

impl Node {
    /// The path relative to the root. Empty for the root.
    fn path(&self) -> PathBuf {
        match &self.parent {
            None => PathBuf::new(),
            Some(parent) => parent.path().join(sys::os_name(&self.name)),
        }
    }

    fn child_path(&self, name: &sys::Name) -> PathBuf {
        self.path().join(sys::os_name(name))
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Some(parent) = &self.parent {
            finish(&parent.handle, self);
        }
    }
}

/// Removes the empty folder `node` from `parent`.
fn finish(parent: &sys::Handle, node: &Node) {
    match sys::rmdir(parent, &node.name) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) if sys::not_empty(&e) => {
            // Something created an entry after the listing. Sweep, retry once.
            sweep(&node.handle, &node.path(), &node.shared);
            if let Err(e) = sys::rmdir(parent, &node.name)
                && e.kind() != io::ErrorKind::NotFound
            {
                node.shared.fail(|| node.path(), e);
            }
        }
        Err(e) => node.shared.fail(|| node.path(), e),
    }
}

/// Removes what is left in a folder on one thread.
fn sweep(handle: &sys::Handle, base: &Path, shared: &Shared) {
    let listing = match sys::list(handle) {
        Ok(listing) => listing,
        Err(e) => {
            shared.fail(|| base.to_path_buf(), e);
            return;
        }
    };
    for name in &listing.files {
        unlink(handle, name, || base.join(sys::os_name(name)), shared);
    }
    for name in &listing.dirs {
        let path = base.join(sys::os_name(name));
        match sys::open_child(handle, name, shared.dev) {
            Ok(child) => {
                sweep(&child, &path, shared);
                if let Err(e) = sys::rmdir(handle, name)
                    && e.kind() != io::ErrorKind::NotFound
                {
                    shared.fail(|| path, e);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => shared.fail(|| path, e),
        }
    }
}

fn unlink(handle: &sys::Handle, name: &sys::Name, path: impl FnOnce() -> PathBuf, shared: &Shared) {
    if let Err(e) = sys::unlink(handle, name)
        && e.kind() != io::ErrorKind::NotFound
    {
        shared.fail(path, e);
    }
}

fn run_dir(scope: &rayon::Scope<'_>, node: &Arc<Node>) {
    let listing = match sys::list(&node.handle) {
        Ok(listing) => listing,
        Err(e) => {
            node.shared.fail(|| node.path(), e);
            return;
        }
    };
    for name in listing.dirs {
        let parent = node.clone();
        // The child opens its folder when it runs, so a folder with many sub
        // folders does not hold one descriptor for each of them.
        scope.spawn(move |scope| open_and_run(scope, parent, name));
    }
    let remove = |chunk: &[sys::Name]| {
        for name in chunk {
            unlink(&node.handle, name, || node.child_path(name), &node.shared);
        }
    };
    if listing.files.len() > CHUNK {
        listing.files.par_chunks(CHUNK).for_each(remove);
    } else {
        remove(&listing.files);
    }
}

fn open_and_run(scope: &rayon::Scope<'_>, parent: Arc<Node>, name: sys::Name) {
    match sys::open_child(&parent.handle, &name, parent.shared.dev) {
        Ok(handle) => {
            let shared = parent.shared.clone();
            let child = Arc::new(Node {
                handle,
                name,
                parent: Some(parent),
                shared,
            });
            run_dir(scope, &child);
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => parent.shared.fail(|| parent.child_path(&name), e),
    }
}

#[cfg(unix)]
mod sys {
    use std::ffi::{CStr, CString, OsString};
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::Path;
    use std::sync::Once;

    use super::Listing;

    pub type Name = CString;
    pub type Dev = libc::dev_t;

    pub struct Handle {
        fd: OwnedFd,
    }

    fn check(rc: libc::c_int) -> io::Result<()> {
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn fstat_dev(fd: RawFd) -> io::Result<Dev> {
        // SAFETY: `stat` is plain data, and `fstat` fills it.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        check(unsafe { libc::fstat(fd, &raw mut st) })?;
        Ok(st.st_dev)
    }

    fn from_fd(fd: libc::c_int) -> io::Result<(Handle, Dev)> {
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a new descriptor that nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let dev = fstat_dev(fd.as_raw_fd())?;
        Ok((Handle { fd }, dev))
    }

    /// Opens the parent folder of `path` and returns it with the last name.
    pub fn open_parent(path: &Path) -> io::Result<(Handle, Name)> {
        let invalid = |what| io::Error::new(io::ErrorKind::InvalidInput, what);
        let name = path
            .file_name()
            .ok_or_else(|| invalid("the path has no final name"))?;
        let name = CString::new(name.as_bytes()).map_err(|_| invalid("the name has a NUL byte"))?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = CString::new(parent.as_os_str().as_bytes())
            .map_err(|_| invalid("the path has a NUL byte"))?;
        let fd = unsafe {
            libc::open(
                parent.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        Ok((from_fd(fd)?.0, name))
    }

    fn open_at(parent: &Handle, name: &Name) -> io::Result<(Handle, Dev)> {
        // O_NOFOLLOW makes a symlink fail here, so the walk never enters one.
        let fd = unsafe {
            libc::openat(
                parent.fd.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        from_fd(fd)
    }

    pub fn open_root(parent: &Handle, name: &Name) -> io::Result<(Handle, Dev)> {
        open_at(parent, name)
    }

    /// Opens a child folder. A folder on another device is a mount point and is
    /// refused. `remove_dir_all` would enter it. This walk stops with an error
    /// instead, because the user chose to delete one tree and not another disk.
    pub fn open_child(parent: &Handle, name: &Name, dev: Dev) -> io::Result<Handle> {
        let (handle, child_dev) = open_at(parent, name)?;
        if child_dev == dev {
            Ok(handle)
        } else {
            Err(io::Error::other("the folder is on another file system"))
        }
    }

    /// Lists a folder without following links.
    pub fn list(handle: &Handle) -> io::Result<Listing> {
        // `closedir` closes the descriptor, so the stream gets a copy.
        let copy = unsafe { libc::dup(handle.fd.as_raw_fd()) };
        if copy < 0 {
            return Err(io::Error::last_os_error());
        }
        let dir = unsafe { libc::fdopendir(copy) };
        if dir.is_null() {
            let error = io::Error::last_os_error();
            unsafe { libc::close(copy) };
            return Err(error);
        }
        // The copy shares its offset with the folder descriptor.
        unsafe { libc::rewinddir(dir) };
        let mut listing = Listing {
            files: Vec::new(),
            dirs: Vec::new(),
        };
        loop {
            let entry = unsafe { libc::readdir(dir) };
            if entry.is_null() {
                break;
            }
            // SAFETY: `readdir` returned a valid entry that lives until the next call.
            let entry = unsafe { &*entry };
            let name = unsafe { CStr::from_ptr(entry.d_name.as_ptr()) };
            if matches!(name.to_bytes(), b"." | b"..") {
                continue;
            }
            let is_dir = match entry.d_type {
                libc::DT_DIR => true,
                libc::DT_UNKNOWN => is_dir_at(handle, name),
                _ => false,
            };
            let name = name.to_owned();
            if is_dir {
                listing.dirs.push(name);
            } else {
                listing.files.push(name);
            }
        }
        unsafe { libc::closedir(dir) };
        Ok(listing)
    }

    /// For file systems without `d_type`. A symlink is not a folder here.
    fn is_dir_at(handle: &Handle, name: &CStr) -> bool {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::fstatat(
                handle.fd.as_raw_fd(),
                name.as_ptr(),
                &raw mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        rc == 0 && (st.st_mode & libc::S_IFMT) == libc::S_IFDIR
    }

    pub fn unlink(handle: &Handle, name: &Name) -> io::Result<()> {
        check(unsafe { libc::unlinkat(handle.fd.as_raw_fd(), name.as_ptr(), 0) })
    }

    pub fn rmdir(parent: &Handle, name: &Name) -> io::Result<()> {
        check(unsafe { libc::unlinkat(parent.fd.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) })
    }

    pub fn not_empty(error: &io::Error) -> bool {
        matches!(error.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST))
    }

    pub fn os_name(name: &Name) -> OsString {
        OsString::from_vec(name.as_bytes().to_vec())
    }

    /// Raises the soft limit of open files once, toward the hard limit. The walk
    /// holds one descriptor for each folder on the way down. macOS refuses a
    /// value above `OPEN_MAX`, even when the hard limit is higher. The limit is
    /// never lowered.
    pub fn raise_open_files() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
                return;
            }
            #[cfg(target_os = "macos")]
            let cap = 10_240;
            #[cfg(not(target_os = "macos"))]
            let cap = 65_536;
            let want = limit.rlim_max.min(cap);
            if want > limit.rlim_cur {
                limit.rlim_cur = want;
                unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) };
            }
        });
    }
}

#[cfg(windows)]
mod sys {
    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};

    use super::Listing;

    pub type Name = OsString;
    pub type Dev = u64;

    /// A folder is its path. Windows has no descriptor to open it relative to.
    pub struct Handle {
        path: PathBuf,
    }

    pub fn open_parent(path: &Path) -> io::Result<(Handle, Name)> {
        let name = path.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "the path has no final name")
        })?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Ok((
            Handle {
                path: parent.to_path_buf(),
            },
            name.to_os_string(),
        ))
    }

    /// Refuses a symlink, a junction and a file.
    pub fn open_root(parent: &Handle, name: &Name) -> io::Result<(Handle, Dev)> {
        let path = parent.path.join(name);
        let file_type = fs::symlink_metadata(&path)?.file_type();
        if file_type.is_symlink() || !file_type.is_dir() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a folder"));
        }
        Ok((Handle { path }, 0))
    }

    #[allow(clippy::unnecessary_wraps)]
    pub fn open_child(parent: &Handle, name: &Name, _dev: Dev) -> io::Result<Handle> {
        Ok(Handle {
            path: parent.path.join(name),
        })
    }

    /// A symlink or a junction is listed as a file, so the walk removes the
    /// link and never enters it. `FileType::is_symlink` is true for every
    /// reparse point that names another place (a symlink or a mount point).
    /// Other reparse points, such as cloud placeholders, hold their own files
    /// and are entered like a folder, as `remove_dir_all` does.
    pub fn list(handle: &Handle) -> io::Result<Listing> {
        let mut listing = Listing {
            files: Vec::new(),
            dirs: Vec::new(),
        };
        for entry in fs::read_dir(&handle.path)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() && !file_type.is_symlink() {
                listing.dirs.push(entry.file_name());
            } else {
                listing.files.push(entry.file_name());
            }
        }
        Ok(listing)
    }

    /// Removes a file, a file symlink, or a link to a folder. Like std, a
    /// read-only item loses the attribute and is tried again.
    pub fn unlink(handle: &Handle, name: &Name) -> io::Result<()> {
        let path = handle.path.join(name);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                if let Ok(meta) = fs::symlink_metadata(&path) {
                    let mut permissions = meta.permissions();
                    // Windows only: the attribute, not a Unix mode.
                    #[allow(clippy::permissions_set_readonly_false)]
                    permissions.set_readonly(false);
                    let _ = fs::set_permissions(&path, permissions);
                }
                fs::remove_file(&path)
                    .or_else(|_| fs::remove_dir(&path))
                    .map_err(|_| e)
            }
            Err(e) => fs::remove_dir(&path).map_err(|_| e),
        }
    }

    pub fn rmdir(parent: &Handle, name: &Name) -> io::Result<()> {
        fs::remove_dir(parent.path.join(name))
    }

    pub fn not_empty(error: &io::Error) -> bool {
        // ERROR_DIR_NOT_EMPTY
        error.raw_os_error() == Some(145)
    }

    pub fn os_name(name: &Name) -> OsString {
        name.clone()
    }

    pub fn raise_open_files() {}
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn tree_of(files_per_dir: usize, width: usize, depth: usize, root: &Path) {
        fs::create_dir_all(root).unwrap();
        for f in 0..files_per_dir {
            fs::write(root.join(format!("f{f}")), b"x").unwrap();
        }
        if depth > 0 {
            for d in 0..width {
                tree_of(files_per_dir, width, depth - 1, &root.join(format!("d{d}")));
            }
        }
    }

    #[test]
    fn removes_a_large_mixed_tree_like_remove_dir_all() {
        let tmp = tempfile::tempdir().unwrap();
        let (ours, theirs) = (tmp.path().join("ours"), tmp.path().join("theirs"));
        for root in [&ours, &theirs] {
            tree_of(7, 4, 4, root);
            fs::create_dir(root.join("empty")).unwrap();
            fs::write(root.join("d1").join("big"), vec![b'y'; 1 << 20]).unwrap();
        }
        remove_tree(&ours).unwrap();
        fs::remove_dir_all(&theirs).unwrap();
        assert!(!ours.exists());
        assert!(!theirs.exists());
    }

    #[test]
    fn removes_a_flat_folder_in_chunks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("flat");
        fs::create_dir(&root).unwrap();
        for i in 0..5000 {
            fs::write(root.join(format!("f{i}")), b"x").unwrap();
        }
        remove_tree(&root).unwrap();
        assert!(!root.exists());
    }

    #[test]
    fn a_deep_tree_does_not_run_out_of_descriptors() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("deep");
        let mut dir = root.clone();
        for _ in 0..300 {
            dir.push("d");
        }
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("leaf"), b"x").unwrap();
        remove_tree(&root).unwrap();
        assert!(!root.exists());
    }

    #[test]
    fn a_missing_path_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let error = remove_tree(&tmp.path().join("missing")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn a_file_is_refused_and_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("file");
        fs::write(&file, b"x").unwrap();
        assert!(remove_tree(&file).is_err());
        assert!(file.exists());
    }

    #[test]
    fn a_path_without_a_final_name_is_refused() {
        let error = remove_tree(Path::new("/")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_folder_is_removed_and_its_target_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir_all(outside.join("inner")).unwrap();
        fs::write(outside.join("keep"), b"x").unwrap();
        fs::write(outside.join("inner").join("keep"), b"x").unwrap();
        let root = tmp.path().join("tree");
        fs::create_dir_all(root.join("sub")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("sub").join("link2")).unwrap();
        std::os::unix::fs::symlink(outside.join("keep"), root.join("filelink")).unwrap();
        remove_tree(&root).unwrap();
        assert!(!root.exists());
        assert!(outside.join("keep").exists());
        assert!(outside.join("inner").join("keep").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_as_the_root_is_refused_and_its_target_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"x").unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(remove_tree(&link).is_err());
        assert!(outside.join("keep").exists());
        assert!(link.symlink_metadata().is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn a_symlink_to_a_folder_is_removed_and_its_target_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"x").unwrap();
        let root = tmp.path().join("tree");
        fs::create_dir(&root).unwrap();
        if std::os::windows::fs::symlink_dir(&outside, root.join("link")).is_err() {
            eprintln!("skipped: this account may not create symlinks");
            return;
        }
        remove_tree(&root).unwrap();
        assert!(!root.exists());
        assert!(outside.join("keep").exists());
    }

    #[cfg(windows)]
    #[test]
    fn a_read_only_file_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("tree");
        fs::create_dir(&root).unwrap();
        let file = root.join("locked");
        fs::write(&file, b"x").unwrap();
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file, permissions).unwrap();
        remove_tree(&root).unwrap();
        assert!(!root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_refuses_unlinks_gives_an_error_and_the_rest_goes() {
        use std::os::unix::fs::PermissionsExt;

        // root ignores the permission bits, so the folder would not refuse.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("tree");
        tree_of(3, 3, 2, &root);
        let locked = root.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("stuck"), b"x").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();

        let error = remove_tree(&root).unwrap_err();
        let still_there = locked.join("stuck").exists();
        let siblings: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(error.to_string().starts_with("locked"), "{error}");
        assert!(still_there);
        assert_eq!(siblings, ["locked"]);
    }

    #[test]
    fn a_folder_that_gained_an_entry_after_the_listing_is_swept_and_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("tree");
        fs::create_dir_all(root.join("late").join("deeper")).unwrap();
        fs::write(root.join("late").join("deeper").join("x"), b"x").unwrap();
        fs::write(root.join("late").join("y"), b"x").unwrap();

        let (parent, name) = sys::open_parent(&root).unwrap();
        let (handle, dev) = sys::open_root(&parent, &name).unwrap();
        let shared = Arc::new(Shared::new(dev));
        let node = Node {
            handle,
            name,
            parent: None,
            shared: shared.clone(),
        };
        // Nobody listed the folder, as if every entry arrived after the listing.
        finish(&parent, &node);
        shared.take_error().unwrap();
        assert!(!root.exists());
    }

    #[test]
    fn many_deletes_at_once_share_the_pool() {
        let tmp = tempfile::tempdir().unwrap();
        let roots: Vec<_> = (0..6).map(|i| tmp.path().join(format!("t{i}"))).collect();
        for root in &roots {
            tree_of(5, 3, 3, root);
        }
        std::thread::scope(|scope| {
            for root in &roots {
                scope.spawn(move || remove_tree(root).unwrap());
            }
        });
        assert!(roots.iter().all(|r| !r.exists()));
    }
}
