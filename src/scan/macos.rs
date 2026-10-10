//! Reads a whole directory with `getattrlistbulk(2)`. One call returns names,
//! types, and sizes for many entries, so there is no `lstat` per file.

use std::cell::{Cell, RefCell};
use std::ffi::CStr;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::slice;
use std::sync::OnceLock;

use super::{Ctx, SubDir};
use crate::tree::{Dir, Kind, flag};

const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const DIR_MNTSTATUS_MNTPOINT: u32 = 0x1;
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

// u64 words keep the buffer 8-byte aligned, as the kernel expects.
const BUF_WORDS: usize = 32 * 1024;

thread_local! {
    static BUF: RefCell<Box<[u64]>> = RefCell::new(vec![0; BUF_WORDS].into_boxed_slice());
    /// The devices seen last and what their entry counts are good for. A
    /// scan through a firmlink moves between two volumes all the time.
    static COUNTS: Cell<[Option<(libc::dev_t, Counts)>; 4]> = const { Cell::new([None; 4]) };
}

#[derive(Clone, Copy, Default)]
struct Counts {
    /// The file system keeps an exact entry count for every directory, as
    /// APFS and HFS+ do. Others may leave it out or estimate it.
    exact: bool,
    /// A directory with a count of 0 is empty and need not be opened. This
    /// holds on the system volume, a sealed APFS snapshot, except for its
    /// firmlinks. Other volumes can have graft points, such as the cryptexes
    /// in /System/Volumes/Preboot, which report 0 entries and no flag.
    zero_is_empty: bool,
}

fn counts(fd: libc::c_int, dev: libc::dev_t) -> Counts {
    let mut known = COUNTS.get();
    if let Some((_, counts)) = known.iter().flatten().find(|(d, _)| *d == dev) {
        return *counts;
    }
    let mut fs: libc::statfs = unsafe { zeroed() };
    let counts = if unsafe { libc::fstatfs(fd, &raw mut fs) } == 0 {
        // SAFETY: f_fstypename is a NUL-terminated C string written by the kernel.
        let name = unsafe { CStr::from_ptr(fs.f_fstypename.as_ptr()) };
        let exact = matches!(name.to_bytes(), b"apfs" | b"hfs");
        Counts {
            exact,
            zero_is_empty: zero_is_empty(
                exact,
                fs.f_flags & libc::MNT_ROOTFS as u32 != 0,
                crate::cache::firmlinks(),
            ),
        }
    } else {
        Counts::default()
    };
    known.rotate_right(1);
    known[0] = Some((dev, counts));
    COUNTS.set(known);
    counts
}

/// Whether a count of 0 means an empty folder: on the system volume, when
/// the firmlinks are known. Their placeholders report 0 entries as well, so
/// without the table every folder is opened.
fn zero_is_empty(exact: bool, root_fs: bool, firmlinks: &[(PathBuf, PathBuf)]) -> bool {
    exact && root_fs && !firmlinks.is_empty()
}

/// Whether `parent/name` is a firmlink. A firmlink reports the entry count of
/// its empty placeholder, not of the folder it leads to.
fn is_firmlink(parent: &[u8], name: &[u8]) -> bool {
    crate::cache::firmlinks()
        .iter()
        .any(|(link, _)| joins(link, parent, name))
}

/// Whether `path` is `parent/name`.
fn joins(path: &Path, parent: &[u8], name: &[u8]) -> bool {
    let path = path.as_os_str().as_bytes();
    let parent = parent.strip_suffix(b"/").unwrap_or(parent);
    path.len() == parent.len() + 1 + name.len()
        && path.starts_with(parent)
        && path[parent.len()] == b'/'
        && path.ends_with(name)
}

/// The firmlinks that work on this system, as link and data folder: both
/// paths lead to the same folder.
fn active_firmlinks() -> &'static [(PathBuf, PathBuf)] {
    use std::os::unix::fs::MetadataExt;
    static ACTIVE: OnceLock<Vec<(PathBuf, PathBuf)>> = OnceLock::new();
    ACTIVE.get_or_init(|| {
        crate::cache::firmlinks()
            .iter()
            .filter(
                |(link, data)| match (std::fs::metadata(link), std::fs::metadata(data)) {
                    (Ok(l), Ok(d)) => l.is_dir() && (l.dev(), l.ino()) == (d.dev(), d.ino()),
                    _ => false,
                },
            )
            .cloned()
            .collect()
    })
}

/// Whether `parent/name` is the data folder of a firmlink inside `root`, such
/// as /System/Volumes/Data/Users for /Users. A scan of `root` counts it at the
/// firmlink, so it must not count it again.
fn counted_at_firmlink(root: &Path, parent: &[u8], name: &[u8]) -> bool {
    active_firmlinks()
        .iter()
        .any(|(link, data)| link.starts_with(root) && joins(data, parent, name))
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
    expected: u32,
) -> io::Result<()> {
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = unsafe { libc::open(path.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = Fd(fd);

    let mut attrs: libc::attrlist = unsafe { zeroed() };
    attrs.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    attrs.commonattr = libc::ATTR_CMN_RETURNED_ATTRS
        | ATTR_CMN_ERROR
        | libc::ATTR_CMN_NAME
        | libc::ATTR_CMN_DEVID
        | libc::ATTR_CMN_OBJTYPE
        | libc::ATTR_CMN_FILEID;
    attrs.dirattr = libc::ATTR_DIR_ENTRYCOUNT | libc::ATTR_DIR_MOUNTSTATUS;
    attrs.fileattr =
        libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE | libc::ATTR_FILE_DATALENGTH;

    // Only folders on the data volume can be the data folder of a firmlink.
    let data_side = !ctx.one_fs && path.to_bytes().starts_with(b"/System/Volumes/Data");
    let mut seen = 0;
    BUF.with_borrow_mut(|buf| {
        loop {
            let n = unsafe {
                libc::getattrlistbulk(
                    fd.0,
                    (&raw mut attrs).cast(),
                    buf.as_mut_ptr().cast(),
                    buf.len() * size_of::<u64>(),
                    u64::from(libc::FSOPT_PACK_INVAL_ATTRS),
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
            seen += n as u32;
            let mut p = buf.as_ptr().cast::<u8>();
            for _ in 0..n {
                // SAFETY: the kernel wrote `n` packed entries, each starting with its length.
                unsafe {
                    let len: u32 = read(p, 0);
                    add_entry(ctx, fd.0, path.to_bytes(), data_side, p, dir, subdirs);
                    p = p.add(len as usize);
                }
            }
            // The parent listing told us how many entries to expect. Once we
            // have them all, skip the extra call that would only return 0.
            if expected > 0 && seen >= expected {
                return Ok(());
            }
        }
    })
}

unsafe fn read<T: Copy>(p: *const u8, offset: usize) -> T {
    unsafe { p.add(offset).cast::<T>().read_unaligned() }
}

/// Layout of one entry: length, returned set, then the common attributes in
/// bit order (the error code first). Directories then carry the directory
/// attributes and everything else carries the file attributes, never both.
/// `FSOPT_PACK_INVAL_ATTRS` keeps every slot inside a group, so the offsets
/// are fixed.
unsafe fn add_entry(
    ctx: &Ctx,
    fd: libc::c_int,
    parent: &[u8],
    data_side: bool,
    p: *const u8,
    dir: &mut Dir,
    subdirs: &mut Vec<SubDir>,
) {
    unsafe {
        let returned: libc::attribute_set_t = read(p, size_of::<u32>());
        let mut o = size_of::<u32>() + size_of::<libc::attribute_set_t>();
        let error: u32 = read(p, o);
        o += 4;
        let name_ref: libc::attrreference_t = read(p, o);
        let name_ptr = p.add(o).offset(name_ref.attr_dataoffset as isize);
        let name =
            slice::from_raw_parts(name_ptr, (name_ref.attr_length as usize).saturating_sub(1));
        o += size_of::<libc::attrreference_t>();
        if error != 0 {
            dir.push(name, Kind::Other, 0, 0, flag::ERROR);
            return;
        }
        let dev: libc::dev_t = read(p, o);
        o += 4;
        let objtype: u32 = read(p, o);
        o += 4;
        let ino: u64 = read(p, o);
        o += 8;

        if objtype == VDIR {
            let entry_count: u32 = read(p, o);
            let mount_status: u32 = read(p, o + 4);
            // A mount point reports the count of the directory it covers.
            let counts = if returned.dirattr & libc::ATTR_DIR_ENTRYCOUNT != 0 && mount_status == 0 {
                counts(fd, dev)
            } else {
                Counts::default()
            };
            let index = dir.entries.len();
            let firmlink = counts.zero_is_empty && entry_count == 0 && is_firmlink(parent, name);
            // With -x, a firmlink counts as a mount point: it leads to the
            // data volume. Without -x, each folder of the data volume is
            // counted once, at its firmlink if the scan has one.
            if (ctx.one_fs && (mount_status & DIR_MNTSTATUS_MNTPOINT != 0 || firmlink))
                || (data_side && counted_at_firmlink(ctx.root, parent, name))
            {
                dir.push(name, Kind::Dir, 0, 0, flag::OTHER_FS);
            } else if counts.zero_is_empty && entry_count == 0 && !firmlink {
                // An empty directory needs no open, listing, and close.
                dir.push(name, Kind::Dir, 0, 0, 0);
                dir.attach(
                    index,
                    Dir {
                        id: ino,
                        ..Dir::default()
                    },
                    0,
                );
            } else {
                subdirs.push(SubDir {
                    index,
                    expected: if counts.exact { entry_count } else { 0 },
                    ino,
                });
                dir.push(name, Kind::Dir, 0, 0, 0);
            }
            return;
        }

        let nlink: u32 = read(p, o);
        let alloc: i64 = read(p, o + 4);
        let data: i64 = read(p, o + 12);
        let kind = match objtype {
            VREG => Kind::File,
            VLNK => Kind::Symlink,
            _ => Kind::Other,
        };
        let mut flags = 0;
        if nlink > 1 {
            flags |= flag::MULTI_LINK;
            if !ctx.first_link(dev as u64, ino) {
                flags |= flag::HARDLINK;
            }
        }
        dir.push(name, kind, alloc.max(0) as u64, data.max(0) as u64, flags);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Options, Progress, scan};
    use crate::tree::{Dir, Entry};
    use std::collections::HashSet;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn opts() -> Options {
        Options {
            one_fs: false,
            threads: 4,
            cache: false,
            mft: false,
            spread: true,
        }
    }

    fn entry<'a>(dir: &'a Dir, name: &str) -> &'a Entry {
        &dir.entries[dir.find(name.as_bytes()).unwrap()]
    }

    fn subtree<'a>(dir: &'a Dir, name: &str) -> &'a Dir {
        entry(dir, name).dir.as_deref().unwrap()
    }

    /// Detaches the disk image even when an assertion fails.
    struct Mounted(PathBuf);

    impl Drop for Mounted {
        fn drop(&mut self) {
            let _ = Command::new("hdiutil")
                .args(["detach", "-quiet", "-force"])
                .arg(&self.0)
                .status();
        }
    }

    /// Attaches a new APFS disk image at `mount`, or returns `None` when
    /// hdiutil cannot.
    fn attach(image: &Path, mount: &Path) -> Option<Mounted> {
        let created = Command::new("hdiutil")
            .args(["create", "-quiet", "-size", "16m", "-fs", "APFS"])
            .args(["-volname", "mmtest"])
            .arg(image)
            .status()
            .ok()?;
        if !created.success() {
            return None;
        }
        let mounted = Mounted(mount.to_path_buf());
        let attached = Command::new("hdiutil")
            .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
            .arg(mount)
            .arg(image)
            .status()
            .ok()?;
        attached.success().then_some(mounted)
    }

    #[test]
    fn paths_are_matched_below_the_root_and_deeper() {
        let (users, cups) = (Path::new("/Users"), Path::new("/usr/libexec/cups"));
        assert!(super::joins(users, b"/", b"Users"));
        assert!(super::joins(cups, b"/usr/libexec", b"cups"));
        assert!(super::joins(cups, b"/usr/libexec/", b"cups"));
        assert!(!super::joins(cups, b"/usr", b"cups"));
        assert!(!super::joins(cups, b"/usr/libexec", b"cup"));
        assert!(!super::joins(users, b"/", b"Use"));
    }

    /// A scan of / counts the users folder at /Users, so not again below
    /// /System/Volumes/Data. A scan of the data volume counts it there.
    #[test]
    fn a_firmlinked_data_folder_counts_once() {
        if !super::active_firmlinks()
            .iter()
            .any(|(link, _)| link == Path::new("/Users"))
        {
            eprintln!("skipped: /Users is not a firmlink here");
            return;
        }
        let data = b"/System/Volumes/Data";
        assert!(super::counted_at_firmlink(Path::new("/"), data, b"Users"));
        assert!(super::counted_at_firmlink(
            Path::new("/"),
            b"/System/Volumes/Data/",
            b"Users"
        ));
        assert!(!super::counted_at_firmlink(
            Path::new("/System/Volumes/Data"),
            data,
            b"Users"
        ));
        assert!(!super::counted_at_firmlink(Path::new("/"), data, b"Other"));
    }

    /// With -x, a firmlink leads to another volume, like a mount point.
    #[test]
    fn with_one_file_system_a_firmlink_is_not_followed() {
        let root = Path::new("/usr/libexec");
        if !super::active_firmlinks()
            .iter()
            .any(|(link, _)| link == Path::new("/usr/libexec/cups"))
        {
            eprintln!("skipped: /usr/libexec/cups is not a firmlink here");
            return;
        }
        let opts = Options {
            one_fs: true,
            ..opts()
        };
        let dir = scan(root, &opts, &Progress::default()).unwrap();
        let cups = entry(&dir, "cups");
        assert!(cups.has(crate::tree::flag::OTHER_FS));
        assert!(cups.dir.is_none());
    }

    /// A firmlink placeholder such as /Users reports 0 entries. Without the
    /// table, for example in a sandbox that cannot read it, it would look
    /// empty and the home folders would be missing.
    #[test]
    fn without_the_firmlink_table_zero_counts_are_not_trusted() {
        let table = [(
            PathBuf::from("/Users"),
            PathBuf::from("/System/Volumes/Data/Users"),
        )];
        assert!(super::zero_is_empty(true, true, &table));
        assert!(!super::zero_is_empty(true, true, &[]));
        assert!(!super::zero_is_empty(true, false, &table));
        assert!(!super::zero_is_empty(false, true, &table));
    }

    /// Items and bytes of `path` found with plain `std::fs` calls, counting
    /// each hard-linked file once.
    fn walk(path: &Path, seen: &mut HashSet<(u64, u64)>) -> (u64, u64) {
        let Ok(list) = fs::read_dir(path) else {
            return (0, 0);
        };
        let (mut items, mut bytes) = (0, 0);
        for e in list.flatten() {
            let meta = e.path().symlink_metadata().unwrap();
            items += 1;
            if meta.is_dir() {
                let (i, b) = walk(&e.path(), seen);
                (items, bytes) = (items + i, bytes + b);
            } else if meta.nlink() < 2 || seen.insert((meta.dev(), meta.ino())) {
                bytes += meta.len();
            }
        }
        (items, bytes)
    }

    /// On the system volume, empty folders are not opened. /usr/libexec has
    /// empty folders and the firmlink cups, which leads to the data volume.
    #[test]
    fn a_system_folder_counts_the_same_as_a_plain_walk() {
        let root = Path::new("/usr/libexec");
        if !root.is_dir() {
            eprintln!("skipped: no /usr/libexec");
            return;
        }
        let dir = scan(root, &opts(), &Progress::default()).unwrap();
        let (items, bytes) = walk(root, &mut HashSet::new());
        let totals = dir.totals();
        assert_eq!((totals.items, totals.apparent), (items, bytes));
    }

    /// A mount point reports the entry count of the folder it covers.
    #[test]
    fn a_volume_mounted_on_an_empty_folder_is_scanned() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let mount = root.join("mnt");
        fs::create_dir_all(&mount).unwrap();
        let Some(_mounted) = attach(&tmp.path().join("disk.dmg"), &mount) else {
            eprintln!("skipped: hdiutil could not attach a disk image");
            return;
        };
        fs::write(mount.join("on-the-image"), vec![1u8; 50_000]).unwrap();

        let dir = scan(&root, &opts(), &Progress::default()).unwrap();

        assert!(subtree(&dir, "mnt").find(b"on-the-image").is_some());
        assert!(dir.totals().apparent >= 50_000);
    }

    /// More files than one listing call returns, on a volume mounted over a
    /// folder with one entry. The count of the covered folder must not end
    /// the listing early.
    #[test]
    fn a_volume_mounted_on_a_folder_with_entries_is_listed_completely() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let mount = root.join("mnt");
        fs::create_dir_all(&mount).unwrap();
        fs::write(mount.join("covered"), b"x").unwrap();
        let Some(_mounted) = attach(&tmp.path().join("disk.dmg"), &mount) else {
            eprintln!("skipped: hdiutil could not attach a disk image");
            return;
        };
        for i in 0..4000 {
            fs::File::create(mount.join(format!("f{i:04}"))).unwrap();
        }

        let dir = scan(&root, &opts(), &Progress::default()).unwrap();

        let mnt = subtree(&dir, "mnt");
        let files = mnt
            .entries
            .iter()
            .filter(|e| mnt.name(e).starts_with(b"f"))
            .count();
        assert_eq!(files, 4000);
        assert!(mnt.find(b"covered").is_none());
    }
}
