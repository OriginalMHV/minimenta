//! Reads a whole directory with `GetFileInformationByHandleEx` and the
//! `FileIdBothDirectoryInfo` class. One call returns the names, sizes,
//! allocation sizes and file IDs of many entries, like `getattrlistbulk` on
//! macOS. `FindFirstFileW` would not return the allocation size.

use std::cell::RefCell;
use std::ffi::OsString;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::ptr;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_NO_MORE_FILES, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_BOTH_DIR_INFO, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdBothDirectoryInfo, GetFileInformationByHandleEx,
    OPEN_EXISTING,
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

    BUF.with_borrow_mut(|buf| {
        loop {
            // SAFETY: the buffer is writable and large enough for at least one record.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    handle.0,
                    FileIdBothDirectoryInfo,
                    buf.as_mut_ptr().cast(),
                    (buf.len() * size_of::<u64>()) as u32,
                )
            };
            if ok == 0 {
                let error = unsafe { GetLastError() };
                if error == ERROR_NO_MORE_FILES {
                    return Ok(());
                }
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            let base = buf.as_ptr().cast::<u8>();
            let mut offset = 0;
            loop {
                // SAFETY: the call filled the buffer with records that are
                // linked by NextEntryOffset, and the last one has offset 0.
                unsafe {
                    let info = base.add(offset).cast::<FILE_ID_BOTH_DIR_INFO>();
                    add_entry(info, dir, subdirs);
                    let next = ptr::addr_of!((*info).NextEntryOffset).read_unaligned();
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
/// `info` must point to a complete record written by the kernel.
unsafe fn add_entry(info: *const FILE_ID_BOTH_DIR_INFO, dir: &mut Dir, subdirs: &mut Vec<SubDir>) {
    unsafe {
        let len = ptr::addr_of!((*info).FileNameLength).read_unaligned() as usize / 2;
        let wide = std::slice::from_raw_parts(ptr::addr_of!((*info).FileName).cast::<u16>(), len);
        if wide == [u16::from(b'.')] || wide == [u16::from(b'.'), u16::from(b'.')] {
            return;
        }
        let name = OsString::from_wide(wide);
        let name = name.as_encoded_bytes();
        let attributes = ptr::addr_of!((*info).FileAttributes).read_unaligned();
        let allocated = ptr::addr_of!((*info).AllocationSize)
            .read_unaligned()
            .max(0) as u64;
        let size = ptr::addr_of!((*info).EndOfFile).read_unaligned().max(0) as u64;
        let id = ptr::addr_of!((*info).FileId).read_unaligned() as u64;

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
}
