//! Lists a directory with raw `getdents64(2)` into a large buffer and calls
//! `fstatat(2)` relative to the directory descriptor for each entry. Linux has
//! no bulk attribute call, so one stat per entry is the floor (see
//! bench/syscalls_linux.c).
//!
//! Directories count their own blocks in disk usage but add nothing to the
//! apparent size. This matches `du -s` and `du -s --apparent-size`.

use std::cell::RefCell;
use std::ffi::CStr;
use std::io;
use std::mem::MaybeUninit;

use super::{Ctx, SubDir};
use crate::tree::{Dir, Kind, flag};

// u64 words keep the buffer 8-byte aligned for `linux_dirent64`.
const BUF_WORDS: usize = 8 * 1024;

// Offsets in `struct linux_dirent64`: d_ino u64, d_off i64, d_reclen u16,
// d_type u8, then the NUL-terminated name.
const RECLEN: usize = 16;
const NAME: usize = 19;

thread_local! {
    static BUF: RefCell<Box<[u64]>> = RefCell::new(vec![0; BUF_WORDS].into_boxed_slice());
}

struct Fd(libc::c_int);

impl Drop for Fd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

pub(super) fn read_dir(
    ctx: &Ctx,
    path: &CStr,
    dir: &mut Dir,
    subdirs: &mut Vec<SubDir>,
    _expected: u32,
) -> io::Result<()> {
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = unsafe { libc::open(path.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = Fd(fd);

    BUF.with_borrow_mut(|buf| {
        loop {
            let n = unsafe {
                libc::syscall(
                    libc::SYS_getdents64,
                    fd.0,
                    buf.as_mut_ptr(),
                    buf.len() * size_of::<u64>(),
                )
            };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if n == 0 {
                return Ok(());
            }
            let base = buf.as_ptr().cast::<u8>();
            let mut off = 0;
            while off < n as usize {
                // SAFETY: the kernel wrote `n` bytes of whole records, each
                // with its length and a NUL-terminated name.
                unsafe {
                    let p = base.add(off);
                    off += p.add(RECLEN).cast::<u16>().read_unaligned() as usize;
                    let name = CStr::from_ptr(p.add(NAME).cast());
                    if !matches!(name.to_bytes(), b"." | b"..") {
                        add_entry(ctx, fd.0, name, dir, subdirs);
                    }
                }
            }
        }
    })
}

fn add_entry(ctx: &Ctx, fd: libc::c_int, name: &CStr, dir: &mut Dir, subdirs: &mut Vec<SubDir>) {
    let mut st = MaybeUninit::<libc::stat>::uninit();
    let rc = unsafe {
        libc::fstatat(
            fd,
            name.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    let name = name.to_bytes();
    if rc != 0 {
        dir.push(name, Kind::Other, 0, 0, flag::ERROR);
        return;
    }
    // SAFETY: fstatat succeeded, so it filled `st`.
    let st = unsafe { st.assume_init() };
    let disk = st.st_blocks.max(0) as u64 * 512;
    let kind = match st.st_mode & libc::S_IFMT {
        libc::S_IFDIR => {
            if ctx.one_fs && st.st_dev != ctx.root_dev {
                dir.push(name, Kind::Dir, 0, 0, flag::OTHER_FS);
            } else {
                subdirs.push(SubDir {
                    index: dir.entries.len(),
                    expected: 0,
                    ino: st.st_ino,
                });
                dir.push(name, Kind::Dir, disk, 0, 0);
            }
            return;
        }
        libc::S_IFREG => Kind::File,
        libc::S_IFLNK => Kind::Symlink,
        _ => Kind::Other,
    };
    let mut flags = 0;
    if st.st_nlink > 1 {
        flags |= flag::MULTI_LINK;
        if !ctx.first_link(st.st_dev, st.st_ino) {
            flags |= flag::HARDLINK;
        }
    }
    dir.push(name, kind, disk, st.st_size.max(0) as u64, flags);
}

#[cfg(test)]
mod tests {
    use crate::scan::{Options, Progress, scan};
    use std::fs::{self, File};
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use std::process::Command;

    fn du(path: &Path, apparent: bool) -> u64 {
        let mut cmd = Command::new("du");
        cmd.args(["-s", "-x", "-B1"]);
        if apparent {
            cmd.arg("--apparent-size");
        }
        let out = cmd.arg(path).output().unwrap();
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        text.split_whitespace().next().unwrap().parse().unwrap()
    }

    fn opts(threads: usize) -> Options {
        Options {
            one_fs: true,
            threads,
            cache: false,
            mft: false,
        }
    }

    /// Long names push the listing past one 64 KiB `getdents64` buffer.
    fn build(root: &Path) {
        fs::create_dir_all(root.join("a/b/c")).unwrap();
        fs::create_dir(root.join("wide")).unwrap();
        for i in 0..3000 {
            File::create(root.join(format!("wide/{i:0>60}"))).unwrap();
        }
        File::create(root.join("a/one.bin"))
            .unwrap()
            .write_all(&[1; 5000])
            .unwrap();
        File::create(root.join("a/b/c/deep.bin"))
            .unwrap()
            .write_all(&[2; 70_000])
            .unwrap();
        fs::hard_link(root.join("a/b/c/deep.bin"), root.join("link.bin")).unwrap();
        std::os::unix::fs::symlink("a/one.bin", root.join("alias")).unwrap();
        File::create(root.join("sparse.bin"))
            .unwrap()
            .set_len(1 << 30)
            .unwrap();
    }

    /// Returns the item count after it checks both totals against `du -x`.
    fn assert_matches_du(path: &Path) -> u64 {
        let progress = Progress::default();
        let totals = scan(path, &opts(4), &progress).unwrap().totals();
        assert_eq!(
            progress.errors.load(std::sync::atomic::Ordering::Relaxed),
            0
        );

        assert_eq!(totals.disk, du(path, false), "disk");
        assert_eq!(totals.apparent, du(path, true), "apparent");
        totals.items
    }

    /// The blocks of a new directory in `root`, or `None` on a file system
    /// such as tmpfs that reports none, where the block tests prove nothing.
    fn dir_blocks(root: &Path) -> Option<u64> {
        let probe = root.join("probe");
        fs::create_dir(&probe).unwrap();
        let blocks = fs::symlink_metadata(&probe).unwrap().blocks() * 512;
        fs::remove_dir(&probe).unwrap();
        if blocks == 0 {
            eprintln!("skipped: directories in {} use no blocks", root.display());
            return None;
        }
        Some(blocks)
    }

    #[test]
    fn totals_match_du_including_directory_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        build(tmp.path());

        assert_eq!(assert_matches_du(tmp.path()), 3000 + 9);
    }

    /// Run with `MINIMENTA_DU_TREE=/usr cargo test -- --ignored`. The tree must
    /// be readable and must not change during the test.
    #[test]
    #[ignore]
    fn totals_match_du_on_a_real_tree() {
        let path = std::env::var_os("MINIMENTA_DU_TREE").unwrap_or("/usr".into());
        let items = assert_matches_du(Path::new(&path));
        eprintln!("{items} items match du in {}", Path::new(&path).display());
    }

    #[test]
    fn directories_count_their_own_blocks_but_no_apparent_size() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(blocks) = dir_blocks(tmp.path()) else {
            return;
        };
        fs::create_dir(tmp.path().join("empty")).unwrap();

        let dir = scan(tmp.path(), &opts(1), &Progress::default()).unwrap();

        let meta = fs::symlink_metadata(tmp.path()).unwrap();
        assert_eq!(dir.own_disk, meta.blocks() * 512);
        assert_eq!(dir.entries[0].disk, blocks);
        assert_eq!(dir.entries[0].apparent, 0);
        assert_eq!(dir.totals().disk, dir.own_disk + blocks);
    }

    /// The browser rescans a directory after a delete and attaches the result.
    #[test]
    fn rescanning_a_subdirectory_keeps_its_own_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        if dir_blocks(tmp.path()).is_none() {
            return;
        }
        fs::create_dir_all(tmp.path().join("a/b/c")).unwrap();
        File::create(tmp.path().join("a/b/c/deep.bin"))
            .unwrap()
            .write_all(&[2; 70_000])
            .unwrap();
        let mut dir = scan(tmp.path(), &opts(4), &Progress::default()).unwrap();
        let before = dir.totals();
        let i = dir
            .entries
            .iter()
            .position(|e| dir.name(e) == b"a")
            .unwrap();

        let sub = scan(&tmp.path().join("a"), &opts(4), &Progress::default()).unwrap();
        (dir.entries[i].disk, dir.entries[i].apparent) = (0, 0);
        dir.attach(i, sub, 0);

        assert_eq!(dir.totals(), before);
    }
}
