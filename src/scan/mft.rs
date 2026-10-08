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
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FlushFileBuffers,
    GetFileInformationByHandle, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW,
    GetVolumePathNameW, OPEN_EXISTING,
};

use super::Progress;
use super::ntfs::{Table, apply_fixups, mft_extents, parse_boot, parse_record};
use crate::tree::Dir;

/// Large enough for sequential disk throughput, small enough for memory.
const CHUNK: u64 = 16 << 20;

pub(super) fn scan(root: &Path, progress: &Progress) -> io::Result<Option<Dir>> {
    let Some(volume) = open_volume(root) else {
        return Ok(None);
    };
    let root_record = record_number(root)?;

    // The boot sector is 512 bytes, but reads must cover whole sectors.
    let mut first = vec![0u8; 4096];
    read_at(&volume, &mut first, 0)?;
    let Some(boot) = parse_boot(&first) else {
        return Ok(None);
    };
    // Records must not cross clusters, so a run always holds whole records.
    if !boot.cluster_size.is_multiple_of(boot.record_size as u64)
        || !CHUNK.is_multiple_of(boot.cluster_size)
    {
        return Ok(None);
    }
    let record_bytes = (boot.record_size as u64).next_multiple_of(boot.bytes_per_sector);
    let mut record0 = vec![0u8; record_bytes as usize];
    read_at(&volume, &mut record0, boot.mft_offset)?;
    let record0 = &mut record0[..boot.record_size];
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

    // Temporary timing printout to find the cost of each phase.
    let profile = std::env::var_os("MINIMENTA_PROFILE").is_some();
    let started = std::time::Instant::now();
    let mut read_time = std::time::Duration::ZERO;
    let mut table = Table::with_capacity(capacity);
    let mut buf = vec![0u8; CHUNK as usize];
    let mut index = 0u64;
    for run in runs {
        let run_bytes = run.clusters * boot.cluster_size;
        let Some(lcn) = run.lcn else {
            index += run_bytes / boot.record_size as u64;
            continue;
        };
        let mut done = 0;
        while done < run_bytes && index < total {
            if progress.cancel.load(Relaxed) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "scan cancelled"));
            }
            let len = (run_bytes - done).min(CHUNK) as usize;
            let t = std::time::Instant::now();
            read_at(&volume, &mut buf[..len], lcn * boot.cluster_size + done)?;
            read_time += t.elapsed();
            for record in buf[..len].chunks_exact_mut(boot.record_size) {
                if index >= total {
                    break;
                }
                if apply_fixups(record)
                    && let Some(parsed) = parse_record(record)
                {
                    table.add(index as u32, &parsed);
                }
                index += 1;
            }
            done += len as u64;
            progress.disk.fetch_add(len as u64, Relaxed);
        }
    }
    let parsed = started.elapsed();
    let dir = table.build(root_record, progress);
    if profile {
        eprintln!(
            "mft: {total} records ({} MiB), read {:.0} ms, parse {:.0} ms, build {:.0} ms",
            size >> 20,
            read_time.as_secs_f64() * 1e3,
            (parsed - read_time).as_secs_f64() * 1e3,
            (started.elapsed() - parsed).as_secs_f64() * 1e3
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
    // SAFETY: `device` is NUL-terminated.
    let handle = unsafe {
        CreateFileW(
            device.as_ptr(),
            GENERIC_READ,
            share,
            ptr::null(),
            OPEN_EXISTING,
            0,
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
        let Some(from_mft) = scan(&root, &progress).unwrap() else {
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
