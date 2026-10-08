//! Moves items to the Trash.
//!
//! On macOS this does not use the `trash` crate: it links Foundation and
//! libobjc, so every run paid to load them even when nothing was deleted.
//! Here Finder runs through `osascript`, and Foundation is loaded with
//! `dlopen` only when Finder cannot be used.

use std::path::{Path, PathBuf};

/// An item in the Trash and the place it came from, so `restore` can put it
/// back.
pub struct Trashed {
    pub original: PathBuf,
    #[cfg(target_os = "macos")]
    location: PathBuf,
    #[cfg(not(target_os = "macos"))]
    item: trash::TrashItem,
}

/// Moves `paths` to the Trash. Returns the items that can be put back, which
/// can be fewer than `paths` when a move failed, and the first error.
#[cfg(not(target_os = "macos"))]
pub fn move_to_trash(paths: &[PathBuf]) -> (Vec<Trashed>, Result<(), String>) {
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    // The Trash keeps the resolved folder, for example the long form of an
    // 8.3 name on Windows, so compare resolved folders and names.
    let resolve = |folder: &Path| std::fs::canonicalize(folder).ok();
    let wanted: Vec<_> = paths
        .iter()
        .map(|path| Some((path.parent().and_then(resolve)?, path.file_name()?)))
        .collect();
    let result = trash::delete_all(paths).map_err(|e| e.to_string());
    // Find the items moved just now, the newest first for a path that was
    // moved before too.
    let mut listed = trash::os_limited::list().unwrap_or_default();
    listed.retain(|item| item.time_deleted >= started - 1);
    listed.sort_by_key(|item| std::cmp::Reverse(item.time_deleted));
    let trashed = paths
        .iter()
        .zip(wanted)
        .filter(|(path, _)| path.symlink_metadata().is_err())
        .filter_map(|(path, wanted)| {
            let (folder, name) = wanted?;
            // Each entry belongs to one item, even when two names look alike.
            let at = listed.iter().position(|item| {
                same_name(item, name) && resolve(&item.original_parent).as_ref() == Some(&folder)
            })?;
            // Restore by the real name, not the display name.
            let mut item = listed.remove(at);
            item.name = name.to_os_string();
            Some(Trashed {
                original: path.clone(),
                item,
            })
        })
        .collect();
    (trashed, result)
}

/// Whether the Trash entry `item` is the item called `name`. Windows lists
/// the display name, which leaves out a known extension while Explorer hides
/// extensions, but the entry's id (its `$R` file) keeps the extension.
#[cfg(not(target_os = "macos"))]
fn same_name(item: &trash::TrashItem, name: &std::ffi::OsStr) -> bool {
    if item.name == name {
        return true;
    }
    let name = Path::new(name);
    cfg!(windows)
        && name.file_stem() == Some(item.name.as_os_str())
        && Path::new(&item.id).extension() == name.extension()
}

/// Puts items back where they came from. Returns how many came back.
#[cfg(not(target_os = "macos"))]
pub fn restore(items: &[Trashed]) -> Result<usize, String> {
    trash::os_limited::restore_all(items.iter().map(|t| t.item.clone()))
        .map_err(|e| e.to_string())?;
    Ok(items.len())
}

/// Uses Finder first, so "Put Back" works. Falls back to the file manager API
/// when Finder cannot be controlled, for example without Automation permission.
#[cfg(target_os = "macos")]
pub fn move_to_trash(paths: &[PathBuf]) -> (Vec<Trashed>, Result<(), String>) {
    // AppleScript text cannot hold a path that is not UTF-8.
    let utf8: Vec<&str> = paths.iter().filter_map(|p| p.to_str()).collect();
    if !utf8.is_empty()
        && utf8.len() == paths.len()
        && let Some(locations) = finder::delete(&utf8)
    {
        // Finder returns the new places in the order of the request.
        let trashed = if locations.len() == paths.len() {
            paths
                .iter()
                .zip(locations)
                .map(|(original, location)| Trashed {
                    original: original.clone(),
                    location,
                })
                .collect()
        } else {
            Vec::new()
        };
        return (trashed, Ok(()));
    }
    let mut trashed = Vec::new();
    let mut first_error = None;
    for path in paths.iter().filter(|p| p.symlink_metadata().is_ok()) {
        match file_manager::trash(path) {
            Ok(Some(location)) => trashed.push(Trashed {
                original: path.clone(),
                location,
            }),
            Ok(None) => {}
            Err(e) => {
                first_error.get_or_insert_with(|| format!("{}: {e}", path.display()));
            }
        }
    }
    (trashed, first_error.map_or(Ok(()), Err))
}

/// Puts items back where they came from. Returns how many came back.
#[cfg(target_os = "macos")]
pub fn restore(items: &[Trashed]) -> Result<usize, String> {
    let mut restored = 0;
    let mut first_error = None;
    for item in items {
        let result = if item.original.symlink_metadata().is_ok() {
            Err(format!("{} exists again", item.original.display()))
        } else {
            std::fs::rename(&item.location, &item.original)
                .map_err(|e| format!("{}: {e}", item.original.display()))
        };
        match result {
            Ok(()) => restored += 1,
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    first_error.map_or(Ok(restored), Err)
}

/// The folder an item came from, for refreshing the view after a restore.
pub fn parent_of(item: &Trashed) -> Option<&Path> {
    item.original.parent()
}

#[cfg(target_os = "macos")]
mod finder {
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    /// The paths go to the script as arguments, so no path is ever quoted
    /// into AppleScript source.
    pub(super) const COLLECT: &str = "on run argv
  set theFiles to {}
  repeat with p in argv
    set end of theFiles to POSIX file (contents of p)
  end repeat
";

    pub(super) fn osascript(script: &str, paths: &[&str]) -> Command {
        let mut command = Command::new("osascript");
        command.arg("-e").arg(script).args(paths);
        command
    }

    /// Returns the new places of the items in the Trash, or `None` when
    /// Finder could not be used.
    pub fn delete(paths: &[&str]) -> Option<Vec<PathBuf>> {
        // Finder returns references to the moved items. Their paths are
        // joined with NUL, which no file name contains.
        let script = format!(
            "{COLLECT}  tell application \"Finder\" to set trashed to delete theFiles
  if class of trashed is not list then set trashed to {{trashed}}
  set out to \"\"
  repeat with t in trashed
    set out to out & POSIX path of (t as alias) & (character id 0)
  end repeat
  return out
end run"
        );
        let output = osascript(&script, paths)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
        Some(
            text.split(|&b| b == 0)
                .filter(|part| !part.is_empty())
                .map(|part| PathBuf::from(std::ffi::OsStr::from_bytes(part)))
                .collect(),
        )
    }
}

/// Calls `-[NSFileManager trashItemAtURL:resultingItemURL:error:]` through
/// the Objective-C runtime, which `dlopen` loads on first use.
#[cfg(target_os = "macos")]
mod file_manager {
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::mem::transmute;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::ptr::null_mut;
    use std::sync::OnceLock;

    type Id = *mut c_void;
    type Sel = *const c_void;
    // BOOL is `bool` on Apple silicon and `signed char` on Intel.
    #[cfg(target_arch = "aarch64")]
    type Bool = bool;
    #[cfg(not(target_arch = "aarch64"))]
    type Bool = i8;

    struct Runtime {
        get_class: unsafe extern "C" fn(*const c_char) -> Id,
        register_sel: unsafe extern "C" fn(*const c_char) -> Sel,
        msg_send: unsafe extern "C" fn(),
        pool_push: unsafe extern "C" fn() -> *mut c_void,
        pool_pop: unsafe extern "C" fn(*mut c_void),
    }

    fn runtime() -> Result<&'static Runtime, String> {
        static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
        RUNTIME.get_or_init(load).as_ref().map_err(Clone::clone)
    }

    fn load() -> Result<Runtime, String> {
        let foundation = c"/System/Library/Frameworks/Foundation.framework/Foundation";
        // SAFETY: the handle stays open for the life of the process, and each
        // symbol is cast to its documented C signature.
        unsafe {
            let handle = libc::dlopen(foundation.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
            if handle.is_null() {
                return Err(format!("cannot load Foundation: {}", last_dl_error()));
            }
            let symbol = |name: &CStr| {
                let ptr = libc::dlsym(handle, name.as_ptr());
                if ptr.is_null() {
                    Err(format!("cannot find {}", name.to_string_lossy()))
                } else {
                    Ok(ptr)
                }
            };
            Ok(Runtime {
                get_class: transmute::<*mut c_void, unsafe extern "C" fn(*const c_char) -> Id>(
                    symbol(c"objc_getClass")?,
                ),
                register_sel: transmute::<*mut c_void, unsafe extern "C" fn(*const c_char) -> Sel>(
                    symbol(c"sel_registerName")?,
                ),
                msg_send: transmute::<*mut c_void, unsafe extern "C" fn()>(symbol(
                    c"objc_msgSend",
                )?),
                pool_push: transmute::<*mut c_void, unsafe extern "C" fn() -> *mut c_void>(symbol(
                    c"objc_autoreleasePoolPush",
                )?),
                pool_pop: transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void)>(symbol(
                    c"objc_autoreleasePoolPop",
                )?),
            })
        }
    }

    unsafe fn last_dl_error() -> String {
        // SAFETY: dlerror returns null or a C string owned by the loader.
        let message = unsafe { libc::dlerror() };
        if message.is_null() {
            "unknown error".into()
        } else {
            // SAFETY: checked for null above.
            unsafe { CStr::from_ptr(message) }
                .to_string_lossy()
                .into_owned()
        }
    }

    /// Returns the new place of the item in the Trash, when the file manager
    /// reports it.
    pub fn trash(path: &Path) -> Result<Option<PathBuf>, String> {
        let rt = runtime()?;
        let path = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        // SAFETY: every message matches the signature of its Foundation
        // method, and the pool releases the autoreleased objects.
        unsafe {
            let pool = (rt.pool_push)();
            let result = trash_in_pool(rt, &path);
            (rt.pool_pop)(pool);
            result
        }
    }

    unsafe fn trash_in_pool(rt: &Runtime, path: &CStr) -> Result<Option<PathBuf>, String> {
        // SAFETY: the caller guarantees that each cast matches the method.
        unsafe {
            let class = |name: &CStr| (rt.get_class)(name.as_ptr());
            let sel = |name: &CStr| (rt.register_sel)(name.as_ptr());
            let send = transmute::<unsafe extern "C" fn(), unsafe extern "C" fn(Id, Sel) -> Id>(
                rt.msg_send,
            );
            let send_bytes = transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(Id, Sel, *const c_char, usize) -> Id,
            >(rt.msg_send);
            let send_id = transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(Id, Sel, Id) -> Id,
            >(rt.msg_send);
            let send_trash = transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(Id, Sel, Id, *mut Id, *mut Id) -> Bool,
            >(rt.msg_send);

            let manager = send(class(c"NSFileManager"), sel(c"defaultManager"));
            // Keeps the exact bytes, also for a name that is not UTF-8.
            let string = send_bytes(
                manager,
                sel(c"stringWithFileSystemRepresentation:length:"),
                path.as_ptr(),
                path.to_bytes().len(),
            );
            if string.is_null() {
                return Err("the name cannot be converted".into());
            }
            let url = send_id(class(c"NSURL"), sel(c"fileURLWithPath:"), string);
            let mut error: Id = null_mut();
            let mut resulting: Id = null_mut();
            let trashed = send_trash(
                manager,
                sel(c"trashItemAtURL:resultingItemURL:error:"),
                url,
                &raw mut resulting,
                &raw mut error,
            );
            if is_yes(trashed) {
                if resulting.is_null() {
                    return Ok(None);
                }
                // The URL is autoreleased, so read its path inside the pool.
                let path = send(resulting, sel(c"path"));
                let bytes = transmute::<
                    unsafe extern "C" fn(),
                    unsafe extern "C" fn(Id, Sel) -> *const c_char,
                >(rt.msg_send)(path, sel(c"fileSystemRepresentation"));
                if bytes.is_null() {
                    return Ok(None);
                }
                let bytes = CStr::from_ptr(bytes).to_bytes();
                return Ok(Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes))));
            }
            if error.is_null() {
                return Err("the file manager refused".into());
            }
            let description = send(error, sel(c"localizedDescription"));
            let utf8 = transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(Id, Sel) -> *const c_char,
            >(rt.msg_send)(description, sel(c"UTF8String"));
            if utf8.is_null() {
                Err("the file manager refused".into())
            } else {
                Err(CStr::from_ptr(utf8).to_string_lossy().into_owned())
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    fn is_yes(b: Bool) -> bool {
        b
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn is_yes(b: Bool) -> bool {
        b != 0
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::path::Path;

    use super::file_manager;
    use super::finder::{COLLECT, osascript};

    /// Runs the same argument handling as the Finder script, but returns the
    /// path that AppleScript would hand to Finder instead of deleting it.
    fn round_trip(path: &str) -> String {
        let script = format!("{COLLECT}  return POSIX path of item 2 of theFiles\nend run");
        let out = osascript(&script, &["/first", path]).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut text = String::from_utf8(out.stdout).unwrap();
        assert_eq!(text.pop(), Some('\n'));
        text
    }

    #[test]
    fn paths_reach_finder_unchanged() {
        for path in [
            "/tmp/plain",
            "/tmp/with space/and 'single' quotes",
            r#"/tmp/double "quoted" name"#,
            r"/tmp/back\slash\",
            "/tmp/colon: in a name",
            "/tmp/new\nline",
            "/tmp/ø 日本 🗑",
            "/tmp/e\u{301} decomposed",
        ] {
            assert_eq!(round_trip(path), path);
        }
    }

    /// APFS and HFS+ look up names without regard to normalization, so the
    /// decomposed form names the same file.
    #[test]
    fn composed_names_reach_finder_decomposed() {
        assert_eq!(round_trip("/tmp/\u{e5}"), "/tmp/a\u{30a}");
    }

    #[test]
    fn a_path_is_never_run_as_applescript() {
        let path = r#"/tmp/x" & (do shell script "exit 3") & "y"#;
        assert_eq!(round_trip(path), path);
    }

    #[test]
    fn file_manager_reports_why_it_failed() {
        let error = file_manager::trash(Path::new("/nonexistent/minimenta")).unwrap_err();
        assert_ne!(error, "");
    }

    #[test]
    #[ignore = "moves a file into the Trash of the user who runs it"]
    fn file_manager_moves_a_file_to_the_trash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("minimenta \"fallback\" ø.txt");
        std::fs::write(&path, b"x").unwrap();
        let location = file_manager::trash(&path)
            .unwrap()
            .expect("the new place in the Trash");
        assert!(path.symlink_metadata().is_err());
        std::fs::rename(&location, &path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"x");
    }
}

#[cfg(test)]
mod restore_tests {
    use super::*;
    use std::fs;

    fn round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        // Explorer hides known extensions, so on Windows both files are
        // listed in the Recycle Bin as "minimenta undo test".
        let file = tmp.path().join("minimenta undo test.txt");
        let twin = tmp.path().join("minimenta undo test.log");
        let folder = tmp.path().join("minimenta undo folder");
        fs::write(&file, b"keep me").unwrap();
        fs::write(&twin, b"and me").unwrap();
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("inside.bin"), vec![7u8; 4096]).unwrap();

        let paths = [file.clone(), twin.clone(), folder.clone()];
        let (trashed, result) = move_to_trash(&paths);
        result.unwrap();
        assert_eq!(trashed.len(), 3, "every item can be put back");
        assert!(paths.iter().all(|p| !p.exists()));

        assert_eq!(restore(&trashed), Ok(3));
        assert_eq!(fs::read(&file).unwrap(), b"keep me");
        assert_eq!(fs::read(&twin).unwrap(), b"and me");
        assert_eq!(fs::read(folder.join("inside.bin")).unwrap().len(), 4096);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn trashed_items_come_back() {
        round_trip();
    }

    /// Asks Finder, which can wait for an Automation permission dialog that
    /// nobody answers on a CI runner, so run it by hand.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "uses Finder and the Trash of the user who runs it"]
    fn trashed_items_come_back_through_finder() {
        round_trip();
    }
}
