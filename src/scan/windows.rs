//! Reads a whole directory with `GetFileInformationByHandleEx` and the
//! `FileFullDirectoryInfo` class. One call returns the names, sizes and
//! allocation sizes of many entries, like `getattrlistbulk` on macOS.
//! `FindFirstFileW` would not return the allocation size.

use std::cell::RefCell;
use std::ffi::OsString;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::ptr;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FULL_DIR_INFO, FILE_ID_BOTH_DIR_INFO, FILE_LIST_DIRECTORY,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileFullDirectoryInfo,
    FileIdBothDirectoryInfo, GetFileInformationByHandleEx, OPEN_EXISTING,
};

use super::{Ctx, NativePath, SubDir};
use crate::tree::{Dir, Kind};

// u64 words keep the buffer 8-byte aligned, as the records need.
const BUF_WORDS: usize = 8 * 1024;

thread_local! {
    static BUF: RefCell<Box<[u64]>> = RefCell::new(vec![0; BUF_WORDS].into_boxed_slice());
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

pub(super) fn read_dir(
    _ctx: &Ctx,
    path: &NativePath,
    dir: &mut Dir,
    subdirs: &mut Vec<SubDir>,
    _expected: u32,
) -> io::Result<()> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 path. Backup semantics are
    // needed to open a directory.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let handle = Handle(handle);

    // The full class leaves out the 8.3 short name, which NTFS may have to
    // look up in the file record of every entry. File systems without the
    // class get the class with file IDs and short names.
    let mut full = true;
    BUF.with_borrow_mut(|buf| {
        loop {
            let class = if full {
                FileFullDirectoryInfo
            } else {
                FileIdBothDirectoryInfo
            };
            // SAFETY: the buffer is writable and large enough for at least one record.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    handle.0,
                    class,
                    buf.as_mut_ptr().cast(),
                    (buf.len() * size_of::<u64>()) as u32,
                )
            };
            if ok == 0 {
                let error = unsafe { GetLastError() };
                if error == ERROR_NO_MORE_FILES {
                    return Ok(());
                }
                if full && error == ERROR_INVALID_PARAMETER && dir.entries.is_empty() {
                    full = false;
                    continue;
                }
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            let base = buf.as_ptr().cast::<u8>();
            let mut offset = 0;
            loop {
                // SAFETY: the call filled the buffer with records of `class`
                // that are linked by NextEntryOffset, and the last one has
                // offset 0.
                unsafe {
                    let next = if full {
                        #[allow(clippy::cast_ptr_alignment, reason = "the u64 buffer and NextEntryOffset keep records 8-byte aligned, and fields are read unaligned")]
                        let info = base.add(offset).cast::<FILE_FULL_DIR_INFO>();
                        let name = record_name(
                            ptr::addr_of!((*info).FileName).cast(),
                            ptr::addr_of!((*info).FileNameLength).read_unaligned(),
                        );
                        add_entry(
                            name,
                            ptr::addr_of!((*info).FileAttributes).read_unaligned(),
                            ptr::addr_of!((*info).AllocationSize).read_unaligned(),
                            ptr::addr_of!((*info).EndOfFile).read_unaligned(),
                            0,
                            dir,
                            subdirs,
                        );
                        ptr::addr_of!((*info).NextEntryOffset).read_unaligned()
                    } else {
                        #[allow(clippy::cast_ptr_alignment, reason = "the u64 buffer and NextEntryOffset keep records 8-byte aligned, and fields are read unaligned")]
                        let info = base.add(offset).cast::<FILE_ID_BOTH_DIR_INFO>();
                        let name = record_name(
                            ptr::addr_of!((*info).FileName).cast(),
                            ptr::addr_of!((*info).FileNameLength).read_unaligned(),
                        );
                        add_entry(
                            name,
                            ptr::addr_of!((*info).FileAttributes).read_unaligned(),
                            ptr::addr_of!((*info).AllocationSize).read_unaligned(),
                            ptr::addr_of!((*info).EndOfFile).read_unaligned(),
                            ptr::addr_of!((*info).FileId).read_unaligned() as u64,
                            dir,
                            subdirs,
                        );
                        ptr::addr_of!((*info).NextEntryOffset).read_unaligned()
                    };
                    if next == 0 {
                        break;
                    }
                    offset += next as usize;
                }
            }
        }
    })
}

/// # Safety
/// `name` must point to `bytes` bytes of UTF-16 inside a record.
unsafe fn record_name<'a>(name: *const u16, bytes: u32) -> &'a [u16] {
    unsafe { std::slice::from_raw_parts(name, bytes as usize / 2) }
}

fn add_entry(
    wide: &[u16],
    attributes: u32,
    allocated: i64,
    size: i64,
    id: u64,
    dir: &mut Dir,
    subdirs: &mut Vec<SubDir>,
) {
    if wide == [u16::from(b'.')] || wide == [u16::from(b'.'), u16::from(b'.')] {
        return;
    }
    let name = OsString::from_wide(wide);
    let name = name.as_encoded_bytes();
    let (allocated, size) = (allocated.max(0) as u64, size.max(0) as u64);

    if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        // Junctions, directory symlinks and mount points are never
        // followed, like symlinks on the other platforms.
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            dir.push(name, Kind::Symlink, 0, 0, 0);
        } else {
            subdirs.push(SubDir {
                index: dir.entries.len(),
                expected: 0,
                ino: id,
            });
            dir.push(name, Kind::Dir, 0, 0, 0);
        }
        return;
    }
    // Files with a reparse tag, such as OneDrive placeholders, count with
    // the space they really take, which is close to 0 when only in the cloud.
    dir.push(name, Kind::File, allocated, size, 0);
}
