//! Reads a whole directory with `getattrlistbulk(2)`. One call returns names,
//! types, and sizes for many entries, so there is no `lstat` per file.

use std::cell::RefCell;
use std::ffi::CStr;
use std::io;
use std::mem::{size_of, zeroed};
use std::slice;

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
                    add_entry(ctx, p, dir, subdirs);
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
unsafe fn add_entry(ctx: &Ctx, p: *const u8, dir: &mut Dir, subdirs: &mut Vec<SubDir>) {
    unsafe {
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
            if ctx.one_fs && mount_status & DIR_MNTSTATUS_MNTPOINT != 0 {
                dir.push(name, Kind::Dir, 0, 0, flag::OTHER_FS);
            } else {
                subdirs.push(SubDir {
                    index: dir.entries.len(),
                    expected: entry_count,
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
