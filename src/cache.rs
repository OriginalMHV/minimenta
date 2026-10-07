//! Stores a scanned tree on disk, so the next run can load it and re-list only
//! the directories that FSEvents reports as changed.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread::JoinHandle;

use crate::tree::{Dir, Entry, Kind, Sort, flag};

const MAGIC: &[u8; 8] = b"MMCACHE\0";
const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub root: PathBuf,
    pub one_fs: bool,
    pub volume: [u8; 16],
    /// FSEvents position taken before the scan started, so changes made
    /// during the scan are replayed next time.
    pub event_id: u64,
    pub scan_secs: f64,
    /// Seconds since the Unix epoch.
    pub saved_at: u64,
}

/// `~/Library/Caches/minimenta/<hash of the root path>.bin`
pub fn file_for(root: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let home = std::env::var_os("HOME")?;
    let hash = root.as_os_str().as_bytes().iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    Some(PathBuf::from(home).join(format!("Library/Caches/minimenta/{hash:016x}.bin")))
}

pub fn encode(header: &Header, dir: &Dir) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut out = Vec::with_capacity(64 + dir.names.len() + dir.entries.len() * 40);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    let root = header.root.as_os_str().as_bytes();
    out.extend_from_slice(&(root.len() as u32).to_le_bytes());
    out.extend_from_slice(root);
    out.push(u8::from(header.one_fs));
    out.extend_from_slice(&header.volume);
    out.extend_from_slice(&header.event_id.to_le_bytes());
    out.extend_from_slice(&header.scan_secs.to_le_bytes());
    out.extend_from_slice(&header.saved_at.to_le_bytes());
    encode_dir(&mut out, dir);
    out
}

fn encode_dir(out: &mut Vec<u8>, dir: &Dir) {
    out.extend_from_slice(&(dir.names.len() as u32).to_le_bytes());
    out.extend_from_slice(&dir.names);
    out.extend_from_slice(&(dir.entries.len() as u32).to_le_bytes());
    for e in &dir.entries {
        out.extend_from_slice(&e.name_start.to_le_bytes());
        out.extend_from_slice(&e.name_len.to_le_bytes());
        out.extend_from_slice(&e.disk.to_le_bytes());
        out.extend_from_slice(&e.apparent.to_le_bytes());
        out.extend_from_slice(&e.items.to_le_bytes());
        out.push(e.kind as u8);
        out.push(e.flags & flag::PERSISTENT);
        out.push(u8::from(e.dir.is_some()));
        if let Some(sub) = &e.dir {
            encode_dir(out, sub);
        }
    }
}

pub fn decode(bytes: &[u8]) -> Option<(Header, Dir)> {
    use std::os::unix::ffi::OsStrExt;
    let mut r = Reader { bytes, pos: 0 };
    if r.take(8)? != MAGIC || r.u32()? != VERSION {
        return None;
    }
    let root_len = r.u32()? as usize;
    let root = PathBuf::from(std::ffi::OsStr::from_bytes(r.take(root_len)?));
    let header = Header {
        root,
        one_fs: r.u8()? != 0,
        volume: r.take(16)?.try_into().ok()?,
        event_id: r.u64()?,
        scan_secs: f64::from_le_bytes(r.take(8)?.try_into().ok()?),
        saved_at: r.u64()?,
    };
    let dir = decode_dir(&mut r)?;
    (r.pos == bytes.len()).then_some((header, dir))
}

fn decode_dir(r: &mut Reader) -> Option<Dir> {
    let names_len = r.u32()? as usize;
    let names = r.take(names_len)?.to_vec();
    let count = r.u32()? as usize;
    let mut entries = Vec::with_capacity(count.min(r.remaining() / 35));
    for _ in 0..count {
        let name_start = r.u32()?;
        let name_len = r.u32()?;
        if name_start as usize + name_len as usize > names.len() {
            return None;
        }
        let disk = r.u64()?;
        let apparent = r.u64()?;
        let items = r.u64()?;
        let kind = match r.u8()? {
            0 => Kind::File,
            1 => Kind::Dir,
            2 => Kind::Symlink,
            3 => Kind::Other,
            _ => return None,
        };
        let flags = r.u8()? & flag::PERSISTENT;
        let dir = if r.u8()? != 0 { Some(Box::new(decode_dir(r)?)) } else { None };
        entries.push(Entry { name_start, name_len, disk, apparent, items, kind, flags, dir });
    }
    // The cache is written right after a scan or update, in the default order.
    Some(Dir { names, entries, sort: Some(Sort::default()) })
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let slice = self.bytes.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

pub fn load(root: &Path) -> Option<(Header, Dir)> {
    decode(&fs::read(file_for(root)?).ok()?)
}

static PENDING: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

/// Writes the cache on a background thread. Call [`flush`] before the process
/// exits. The file is replaced atomically, so a crash never leaves half a file.
pub fn save_in_background(header: Header, dir: &Dir) {
    let Some(path) = file_for(&header.root) else { return };
    let bytes = encode(&header, dir);
    let handle = std::thread::spawn(move || {
        let _ = write_atomically(&path, &bytes);
    });
    if let Some(previous) = PENDING.lock().unwrap().replace(handle) {
        let _ = previous.join();
    }
}

pub fn flush() {
    if let Some(handle) = PENDING.lock().unwrap().take() {
        let _ = handle.join();
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("cache path has no parent"))?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp", std::process::id()));
    let mut file = fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    drop(file);
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Header, Dir) {
        let mut sub = Dir::default();
        sub.push(b"file.bin", Kind::File, 4096, 1000, 0);
        sub.push(b"link", Kind::Symlink, 0, 12, flag::HARDLINK);
        let mut root = Dir::default();
        root.push(b"sub", Kind::Dir, 0, 0, 0);
        root.push(b"top.txt", Kind::File, 8192, 5000, flag::SELECTED);
        root.attach(0, sub, 0);
        root.sort(Sort::default());
        let header = Header {
            root: PathBuf::from("/Users/me/code"),
            one_fs: true,
            volume: [7; 16],
            event_id: 42,
            scan_secs: 1.5,
            saved_at: 1_700_000_000,
        };
        (header, root)
    }

    fn describe(dir: &Dir) -> Vec<String> {
        let mut out = Vec::new();
        for e in &dir.entries {
            let name = String::from_utf8_lossy(dir.name(e));
            out.push(format!("{name} {} {} {} {:?} {}", e.disk, e.apparent, e.items, e.kind, e.flags));
            if let Some(sub) = &e.dir {
                out.extend(describe(sub).into_iter().map(|line| format!("{name}/{line}")));
            }
        }
        out
    }

    #[test]
    fn round_trips_the_tree_and_header_without_transient_flags() {
        let (header, dir) = sample();
        let (h2, d2) = decode(&encode(&header, &dir)).unwrap();
        assert_eq!(h2, header);
        let expected: Vec<String> = describe(&dir).into_iter().map(|l| l.replace(" 16", " 0")).collect();
        assert_eq!(describe(&d2), expected);
    }

    #[test]
    fn rejects_truncated_or_foreign_files() {
        let (header, dir) = sample();
        let bytes = encode(&header, &dir);
        for cut in [0, 7, 20, bytes.len() / 2, bytes.len() - 1] {
            assert!(decode(&bytes[..cut]).is_none(), "accepted a file cut at {cut}");
        }
        let mut foreign = bytes.clone();
        foreign[0] = b'X';
        assert!(decode(&foreign).is_none());
    }

    #[test]
    fn cache_files_differ_per_root() {
        assert_ne!(file_for(Path::new("/a")), file_for(Path::new("/b")));
    }
}
