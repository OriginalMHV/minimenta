//! Reads the NTFS master file table (MFT) of the volume that holds the root,
//! in large sequential blocks, and builds the tree from it. On a cold disk this
//! is much faster than listing every directory. It needs an administrator,
//! because it opens the volume itself. In any other case it returns `None`,
//! and the caller scans the normal way.

use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::FileExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;
use std::sync::atomic::Ordering::Relaxed;

use windows_sys::Win32::Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION_TYPE, TOKEN_QUERY, TokenElevationType,
    TokenElevationTypeLimited,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_NO_BUFFERING,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetFileInformationByHandle, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW,
    GetVolumePathNameW, OPEN_EXISTING,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

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
    let (root_record, root_seq) = record_number(root)?;

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
    let Ok(records) = u32::try_from(total) else {
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
    let mut table = Table::new(records);
    for part in parts {
        if !table.merge(part?) {
            return Ok(None);
        }
    }
    Ok(table.build(root_record, root_seq, progress))
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn until_nul(buf: &[u16]) -> &[u16] {
    &buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]
}

/// `root` as the volume functions need it: without the `\\?\` prefix of
/// canonical paths, which they do not accept, and NUL-terminated.
fn volume_query(root: &Path) -> Vec<u16> {
    let plain = root
        .to_str()
        .and_then(|s| s.strip_prefix(r"\\?\"))
        .filter(|s| !s.starts_with("UNC\\"));
    wide(plain.map_or(root, Path::new))
}

/// The mount point of the volume that holds `root`, NUL-terminated, when
/// that volume is NTFS.
fn ntfs_mount_point(root: &Path) -> Option<[u16; 1024]> {
    let root = volume_query(root);
    let mut mount = [0u16; 1024];
    let mut fs_name = [0u16; 64];
    // SAFETY: every buffer is NUL-terminated or has its length passed along.
    let ok = unsafe {
        GetVolumePathNameW(root.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) != 0
            && GetVolumeInformationW(
                mount.as_ptr(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            ) != 0
    };
    (ok && until_nul(&fs_name) == "NTFS".encode_utf16().collect::<Vec<_>>()).then_some(mount)
}

/// Whether `root` is the top folder of an NTFS volume: a drive root such as
/// `C:\`, or a volume that is mounted in a folder. The MFT reader always wins
/// a scan of such a root.
pub(super) fn is_volume_root(root: &Path) -> bool {
    ntfs_mount_point(root)
        .is_some_and(|mount| same_folder(until_nul(&mount), until_nul(&volume_query(root))))
}

/// Whether two Windows paths name the same folder when they differ at most in
/// case and in trailing separators. The mount point from `GetVolumePathNameW`
/// ends in a backslash, and a canonical path does not.
fn same_folder(a: &[u16], b: &[u16]) -> bool {
    fn folded(path: &[u16]) -> Vec<char> {
        let path = path.strip_suffix(&[u16::from(b'\\')]).unwrap_or(path);
        char::decode_utf16(path.iter().copied())
            .flat_map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER).to_lowercase())
            .collect()
    }
    folded(a) == folded(b)
}

/// Whether running minimenta elevated would let it read the MFT of the
/// volume that holds `root`.
pub(super) fn elevation_helps(root: &Path) -> bool {
    token_elevation().is_some_and(|kind| elevation_helps_with(kind, || on_ntfs(root)))
}

/// An elevated run helps only an administrator whose token UAC limited: a
/// standard user cannot elevate, and an elevated process already reads the
/// MFT. Only NTFS has an MFT.
fn elevation_helps_with(kind: TOKEN_ELEVATION_TYPE, ntfs: impl FnOnce() -> bool) -> bool {
    kind == TokenElevationTypeLimited && ntfs()
}

fn on_ntfs(root: &Path) -> bool {
    ntfs_mount_point(root).is_some()
}

/// The elevation type of the token of this process.
fn token_elevation() -> Option<TOKEN_ELEVATION_TYPE> {
    let mut token = ptr::null_mut();
    // SAFETY: the pseudo handle of the current process needs no closing.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return None;
    }
    // SAFETY: the token handle is valid and is closed on drop.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut kind: TOKEN_ELEVATION_TYPE = 0;
    let mut len = 0;
    // SAFETY: `kind` is a valid output buffer of the given size.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenElevationType,
            (&raw mut kind).cast(),
            size_of::<TOKEN_ELEVATION_TYPE>() as u32,
            &raw mut len,
        )
    };
    (ok != 0).then_some(kind)
}

/// Opens the NTFS volume that holds `root` for reading, or returns `None`
/// when that is not possible, for example without administrator rights.
fn open_volume(root: &Path) -> Option<File> {
    let mount = ntfs_mount_point(root)?;
    let mut volume = [0u16; 64];
    // SAFETY: `mount` is NUL-terminated and the length of `volume` is passed.
    if unsafe {
        GetVolumeNameForVolumeMountPointW(mount.as_ptr(), volume.as_mut_ptr(), volume.len() as u32)
    } == 0
    {
        return None;
    }
    // `\\?\Volume{...}\` names the root folder. Without the last backslash it
    // names the volume itself.
    let mut device = until_nul(&volume).to_vec();
    if device.last() == Some(&u16::from(b'\\')) {
        device.pop();
    }
    device.push(0);
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

/// The MFT record number of a directory, the low 48 bits of its file index,
/// and the sequence number of the record, the high 16 bits.
fn record_number(path: &Path) -> io::Result<(u32, u16)> {
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
    if unsafe { GetFileInformationByHandle(dir.as_raw_handle(), &raw mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let index = u64::from(info.nFileIndexHigh) << 32 | u64::from(info.nFileIndexLow);
    let record = u32::try_from(index & 0xFFFF_FFFF_FFFF).map_err(io::Error::other)?;
    Ok((record, (index >> 48) as u16))
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
    use windows_sys::Win32::Security::{TokenElevationTypeDefault, TokenElevationTypeFull};

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

    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn mount_points_match_paths_in_case_and_trailing_separator() {
        let same = |a: &str, b: &str| same_folder(&utf16(a), &utf16(b));
        assert!(same(r"C:\", "C:"));
        assert!(same(r"C:\", r"c:\"));
        assert!(same(r"C:\Mnt\Data\", r"c:\mnt\data"));
        assert!(same(r"D:\Mnt\Å\", r"d:\mnt\å"));
        assert!(!same(r"C:\", r"C:\Windows"));
        assert!(!same(r"C:\Mnt\Data\", r"C:\Mnt"));
        assert!(!same(r"C:\", r"D:\"));
    }

    #[test]
    fn a_drive_root_is_a_volume_root_and_a_folder_is_not() {
        let system = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let drive = system.ancestors().last().unwrap();
        assert!(on_ntfs(drive));
        assert!(is_volume_root(drive), "{drive:?}");
        let drive = fs::canonicalize(drive).unwrap();
        assert!(is_volume_root(&drive), "{drive:?}");
        let windows = fs::canonicalize(&system).unwrap();
        assert!(!is_volume_root(&windows), "{windows:?}");
        let tmp = tempfile::tempdir().unwrap();
        let folder = fs::canonicalize(tmp.path()).unwrap();
        assert!(!is_volume_root(&folder), "{folder:?}");
    }

    #[test]
    fn only_a_limited_administrator_on_ntfs_gets_the_hint() {
        assert!(elevation_helps_with(TokenElevationTypeLimited, || true));
        assert!(!elevation_helps_with(TokenElevationTypeLimited, || false));
        assert!(!elevation_helps_with(TokenElevationTypeFull, || true));
        assert!(!elevation_helps_with(TokenElevationTypeDefault, || true));
    }

    /// Windows needs NTFS for its system volume. A process that can open a
    /// volume has a full token, so it never gets the hint.
    #[test]
    fn a_process_that_reads_the_volume_never_gets_the_hint() {
        let system = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        assert!(on_ntfs(&system));
        assert!(token_elevation().is_some());
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        if open_volume(&root).is_some() {
            assert!(!elevation_helps(&root));
        }
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
