//! Reads the NTFS master file table (MFT) of the volume that holds the root,
//! in large sequential blocks, and builds the tree from it. On a cold disk this
//! is much faster than listing every directory. It needs an administrator,
//! because it opens the volume itself. In any other case it returns `None`,
//! and the caller scans the normal way.

use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::FileExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use std::ptr;
use std::sync::atomic::Ordering::Relaxed;

use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_NO_BUFFERING,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FlushFileBuffers,
    GetFileInformationByHandle, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW,
    GetVolumePathNameW, OPEN_EXISTING,
};

use super::Progress;
use super::ntfs::{Part, Table, apply_fixups, mft_extents, parse_boot, parse_record};
use crate::tree::Dir;
use rayon::prelude::*;

/// Each task reads and parses this much. Several tasks run at once, so the
/// disk always has several requests in flight.
const CHUNK: u64 = 4 << 20;

/// Unbuffered reads need buffers aligned to the sector size.
#[derive(Clone, Copy)]
#[repr(C, align(4096))]
struct Page([u8; 4096]);

fn pages(bytes: u64) -> Vec<Page> {
    vec![Page([0; 4096]); bytes.div_ceil(4096) as usize]
}

fn as_bytes(pages: &mut [Page]) -> &mut [u8] {
    // SAFETY: a Page is plain bytes, so the pages form one byte slice.
    unsafe { std::slice::from_raw_parts_mut(pages.as_mut_ptr().cast::<u8>(), pages.len() * 4096) }
}

/// One block of the MFT: where it is on disk and its first record number.
struct Task {
    offset: u64,
    len: u64,
    first: u64,
}

pub(super) fn scan(root: &Path, threads: usize, progress: &Progress) -> io::Result<Option<Dir>> {
    let Some(volume) = open_volume(root) else {
        return Ok(None);
    };
    let root_record = record_number(root)?;

    // The boot sector is 512 bytes, but reads must cover whole sectors.
    let mut first = pages(4096);
    read_at(&volume, as_bytes(&mut first), 0)?;
    let Some(boot) = parse_boot(as_bytes(&mut first)) else {
        return Ok(None);
    };
    // Records must not cross clusters, so a run always holds whole records.
    if !boot.cluster_size.is_multiple_of(boot.record_size as u64)
        || !CHUNK.is_multiple_of(boot.cluster_size)
        || !4096u64.is_multiple_of(boot.bytes_per_sector)
    {
        return Ok(None);
    }
    let record_bytes = (boot.record_size as u64).next_multiple_of(boot.bytes_per_sector);
    let mut buf0 = pages(record_bytes);
    read_at(
        &volume,
        &mut as_bytes(&mut buf0)[..record_bytes as usize],
        boot.mft_offset,
    )?;
    let record0 = &mut as_bytes(&mut buf0)[..boot.record_size];
    if !apply_fixups(record0) {
        return Ok(None);
    }
    let Some((runs, size)) = mft_extents(record0) else {
        return Ok(None);
    };
    let total = size / boot.record_size as u64;
    let Ok(capacity) = usize::try_from(total) else {
        return Ok(None);
    };

    let record_size = boot.record_size as u64;
    let mut tasks = Vec::new();
    let mut index = 0u64;
    for run in runs {
        let run_bytes = run.clusters * boot.cluster_size;
        if let Some(lcn) = run.lcn {
            let mut done = 0;
            while done < run_bytes && index + done / record_size < total {
                let len = (run_bytes - done).min(CHUNK);
                let first = index + done / record_size;
                tasks.push(Task {
                    offset: lcn * boot.cluster_size + done,
                    len,
                    first,
                });
                done += len;
            }
        }
        index += run_bytes / record_size;
    }

    // Temporary timing printout to find the cost of each phase.
    let profile = std::env::var_os("MINIMENTA_PROFILE").is_some();
    let started = std::time::Instant::now();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(io::Error::other)?;
    let parts: Vec<io::Result<Part>> = pool.install(|| {
        tasks
            .par_iter()
            .map_init(
                || pages(CHUNK),
                |buf, task| {
                    if progress.cancel.load(Relaxed) {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "scan cancelled"));
                    }
                    let bytes = &mut as_bytes(buf)[..task.len as usize];
                    read_at(&volume, bytes, task.offset)?;
                    progress.disk.fetch_add(task.len, Relaxed);
                    let mut part = Part::default();
                    for (i, record) in bytes.chunks_exact_mut(boot.record_size).enumerate() {
                        let index = task.first + i as u64;
                        if index >= total {
                            break;
                        }
                        if apply_fixups(record)
                            && let Some(parsed) = parse_record(record)
                        {
                            part.add(index as u32, &parsed);
                        }
                    }
                    Ok(part)
                },
            )
            .collect()
    });
    let read = started.elapsed();
    let mut table = Table::with_capacity(capacity);
    for part in parts {
        table.merge(part?);
    }
    let merged = started.elapsed();
    let dir = table.build(root_record, progress);
    if profile {
        eprintln!(
            "mft: {total} records ({} MiB), read and parse {:.0} ms, merge {:.0} ms, build {:.0} ms",
            size >> 20,
            read.as_secs_f64() * 1e3,
            (merged - read).as_secs_f64() * 1e3,
            (started.elapsed() - merged).as_secs_f64() * 1e3
        );
    }
    Ok(Some(dir))
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn until_nul(buf: &[u16]) -> &[u16] {
    &buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]
}

/// Opens the NTFS volume that holds `root` for reading, or returns `None`
/// when that is not possible, for example without administrator rights.
fn open_volume(root: &Path) -> Option<File> {
    // The volume functions do not accept the `\\?\` prefix of canonical paths.
    let plain = root
        .to_str()
        .and_then(|s| s.strip_prefix(r"\\?\"))
        .filter(|s| !s.starts_with("UNC\\"));
    let root = wide(plain.map_or(root, Path::new));
    let mut mount = [0u16; 1024];
    let mut fs_name = [0u16; 64];
    let mut volume = [0u16; 64];
    // SAFETY: every buffer is NUL-terminated or has its length passed along.
    unsafe {
        if GetVolumePathNameW(root.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) == 0
            || GetVolumeInformationW(
                mount.as_ptr(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            ) == 0
            || until_nul(&fs_name) != "NTFS".encode_utf16().collect::<Vec<_>>()
            || GetVolumeNameForVolumeMountPointW(
                mount.as_ptr(),
                volume.as_mut_ptr(),
                volume.len() as u32,
            ) == 0
        {
            return None;
        }
    }
    // `\\?\Volume{...}\` names the root folder. Without the last backslash it
    // names the volume itself.
    let mut device = until_nul(&volume).to_vec();
    if device.last() == Some(&u16::from(b'\\')) {
        device.pop();
    }
    device.push(0);
    flush(&device);
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE;
    // Unbuffered: the table goes straight into our aligned buffers, and does
    // not fill the file cache with data that is read once.
    // SAFETY: `device` is NUL-terminated.
    let handle = unsafe {
        CreateFileW(
            device.as_ptr(),
            GENERIC_READ,
            share,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_NO_BUFFERING,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    // SAFETY: the handle is valid and the File takes ownership of it.
    Some(unsafe { File::from_raw_handle(handle) })
}

/// Recently changed metadata may still be in memory. Flushing the volume
/// writes it out, so the table on disk is current. It needs write access to
/// the volume handle, but nothing is ever written through it.
fn flush(device: &[u16]) {
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE;
    // SAFETY: `device` is NUL-terminated.
    let handle = unsafe {
        CreateFileW(
            device.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            share,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if handle != INVALID_HANDLE_VALUE {
        // SAFETY: the handle is valid and owned by the File, which closes it.
        let volume = unsafe { File::from_raw_handle(handle) };
        unsafe { FlushFileBuffers(volume.as_raw_handle()) };
    }
}

/// The MFT record number of a directory: the low 48 bits of its file index.
fn record_number(path: &Path) -> io::Result<u32> {
    let path = wide(path);
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    // SAFETY: `path` is NUL-terminated. Backup semantics open a directory.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_READ_ATTRIBUTES,
            share,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the handle is valid and the File closes it.
    let dir = unsafe { File::from_raw_handle(handle) };
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid output buffer.
    if unsafe { GetFileInformationByHandle(dir.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let index =
        (u64::from(info.nFileIndexHigh) << 32 | u64::from(info.nFileIndexLow)) & 0xFFFF_FFFF_FFFF;
    u32::try_from(index).map_err(io::Error::other)
}

fn read_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Options;
    use crate::tree::Kind;
    use std::fs;

    /// One line per entry with its path, kind and sizes, sorted. Directories
    /// show no sizes and flags are left out: the directory listing has no link
    /// count, so only the MFT counts a hard-linked file once.
    fn describe(dir: &Dir) -> Vec<String> {
        fn walk(dir: &Dir, prefix: &str, out: &mut Vec<String>) {
            for e in &dir.entries {
                let name = format!("{prefix}{}", String::from_utf8_lossy(dir.name(e)));
                if e.kind == Kind::Dir {
                    out.push(format!("{name} Dir"));
                } else {
                    out.push(format!("{name} {:?} {} {}", e.kind, e.disk, e.apparent));
                }
                if let Some(sub) = &e.dir {
                    walk(sub, &format!("{name}/"), out);
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, "", &mut out);
        out.sort();
        out
    }

    #[test]
    fn the_master_file_table_matches_a_directory_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let write = |rel: &str, len: usize| {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![7u8; len]).unwrap();
        };
        write("empty.bin", 0);
        write("tiny.txt", 100);
        write("a/medium.bin", 5000);
        write("a/b/c/big.bin", 1 << 20);
        write("å 日本/name with spaces.txt", 3000);
        fs::hard_link(root.join("a/medium.bin"), root.join("a/b/link.bin")).unwrap();
        let symlink = std::os::windows::fs::symlink_dir(root.join("a"), root.join("alias")).is_ok();

        let progress = Progress::default();
        let Some(from_mft) = scan(&root, 4, &progress).unwrap() else {
            eprintln!("skipped: reading the master file table needs an administrator and NTFS");
            return;
        };
        let opts = Options {
            one_fs: false,
            threads: 4,
            cache: false,
            mft: false,
        };
        let listed = crate::scan::scan(&root, &opts, &Progress::default()).unwrap();

        assert_eq!(describe(&from_mft), describe(&listed));
        if symlink {
            let alias = from_mft
                .entries
                .iter()
                .find(|e| from_mft.name(e) == b"alias")
                .unwrap();
            assert_eq!(
                alias.kind,
                Kind::Symlink,
                "a directory symlink is not followed"
            );
        }
        let link_flags: Vec<u8> = describe_flags(&from_mft);
        assert!(
            link_flags.contains(&(crate::tree::flag::MULTI_LINK | crate::tree::flag::HARDLINK))
        );
    }

    fn describe_flags(dir: &Dir) -> Vec<u8> {
        dir.entries
            .iter()
            .flat_map(|e| {
                std::iter::once(e.flags)
                    .chain(e.dir.as_deref().map(describe_flags).unwrap_or_default())
            })
            .collect()
    }
}
