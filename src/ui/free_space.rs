use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::tree::format_size;

/// How long a value stays fresh while deletes are pending.
const MAX_AGE: Duration = Duration::from_secs(2);

/// Reads the free space of the volume that holds a folder.
pub type Reader = Box<dyn Fn(&Path) -> Option<u64>>;

/// The free space of the volume under the folder on screen, with the folder
/// and the time of the read. The browser refreshes it on events, never on
/// every redraw, so a redraw only formats the stored value.
pub struct FreeSpace {
    bytes: Option<u64>,
    path: PathBuf,
    read_at: Option<Instant>,
    reader: Reader,
}

impl FreeSpace {
    pub fn new(reader: Reader) -> Self {
        Self {
            bytes: None,
            path: PathBuf::new(),
            read_at: None,
            reader,
        }
    }

    pub fn real() -> Self {
        Self::new(Box::new(available))
    }

    /// Reads the volume of `path` again.
    pub fn refresh(&mut self, path: &Path, now: Instant) {
        self.set((self.reader)(path), path, now);
    }

    /// Stores a value that was read for `path` at `now`.
    pub fn set(&mut self, bytes: Option<u64>, path: &Path, now: Instant) {
        self.bytes = bytes;
        if self.path != path {
            self.path = path.to_path_buf();
        }
        self.read_at = Some(now);
    }

    /// Whether the value is for another folder than `path`.
    pub fn is_for_other_folder(&self, path: &Path) -> bool {
        self.path != path
    }

    /// Whether the value is older than the refresh time.
    pub fn is_old(&self, now: Instant) -> bool {
        self.read_at
            .is_none_or(|at| now.saturating_duration_since(at) >= MAX_AGE)
    }

    /// For example `182.3 GiB free`. Nothing when the read failed.
    pub fn text(&self) -> Option<String> {
        self.bytes
            .map(|b| format!("{} free", format_size(b).trim_start()))
    }
}

/// The bytes that a normal user can still write on the volume of `path`.
#[cfg(unix)]
pub fn available(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `path` is a valid C string and `stat` has room for the result.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: statvfs returned 0, so it filled the struct.
    let stat = unsafe { stat.assume_init() };
    // The field types differ between platforms.
    #[allow(clippy::unnecessary_cast, clippy::cast_lossless)]
    Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

#[cfg(windows)]
pub fn available(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    // SAFETY: `wide` ends with a nul, and `free` is a valid output. The other
    // two outputs may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &raw mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(not(any(unix, windows)))]
pub fn available(_path: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_call_returns_a_value_for_a_folder_and_nothing_for_a_missing_one() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(available(tmp.path()).is_some_and(|b| b > 0));
        assert_eq!(available(&tmp.path().join("missing")), None);
    }

    #[test]
    fn the_text_uses_the_size_format_and_a_failed_read_shows_nothing() {
        let now = Instant::now();
        let mut free = FreeSpace::new(Box::new(|_| None));
        assert_eq!(free.text(), None);
        free.set(
            Some(1024 * 1024 * 1024 * 1024 + 1024 * 1024 * 300),
            Path::new("/a"),
            now,
        );
        assert_eq!(free.text().as_deref(), Some("1.0 TiB free"));
        free.refresh(Path::new("/a"), now);
        assert_eq!(free.text(), None);
    }

    #[test]
    fn a_value_is_old_after_two_seconds_and_belongs_to_one_folder() {
        let now = Instant::now();
        let mut free = FreeSpace::new(Box::new(|_| Some(1)));
        assert!(free.is_old(now));
        free.refresh(Path::new("/a"), now);
        assert!(!free.is_old(now + Duration::from_millis(1999)));
        assert!(free.is_old(now + Duration::from_secs(2)));
        assert!(!free.is_for_other_folder(Path::new("/a")));
        assert!(free.is_for_other_folder(Path::new("/b")));
    }
}
