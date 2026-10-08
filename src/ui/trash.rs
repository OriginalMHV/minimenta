//! Moves items to the Trash.
//!
//! On macOS this does not use the `trash` crate: it links Foundation and
//! libobjc, so every run paid to load them even when nothing was deleted.
//! Here Finder runs through `osascript`, and Foundation is loaded with
//! `dlopen` only when Finder cannot be used.

use std::path::PathBuf;

#[cfg(not(target_os = "macos"))]
pub fn move_to_trash(paths: &[PathBuf]) -> Result<(), String> {
    trash::delete_all(paths).map_err(|e| e.to_string())
}

/// Uses Finder first, so "Put Back" works. Falls back to the file manager API
/// when Finder cannot be controlled, for example without Automation permission.
#[cfg(target_os = "macos")]
pub fn move_to_trash(paths: &[PathBuf]) -> Result<(), String> {
    // AppleScript text cannot hold a path that is not UTF-8.
    let utf8: Vec<&str> = paths.iter().filter_map(|p| p.to_str()).collect();
    if !utf8.is_empty() && finder::delete(&utf8) && utf8.len() == paths.len() {
        return Ok(());
    }
    let mut first_error = None;
    for path in paths.iter().filter(|p| p.symlink_metadata().is_ok()) {
        if let Err(e) = file_manager::trash(path) {
            first_error.get_or_insert_with(|| format!("{}: {e}", path.display()));
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(target_os = "macos")]
mod finder {
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

    pub fn delete(paths: &[&str]) -> bool {
        let script = format!("{COLLECT}  tell application \"Finder\" to delete theFiles\nend run");
        osascript(&script, paths)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

/// Calls `-[NSFileManager trashItemAtURL:resultingItemURL:error:]` through
/// the Objective-C runtime, which `dlopen` loads on first use.
#[cfg(target_os = "macos")]
mod file_manager {
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::mem::transmute;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
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

    pub fn trash(path: &Path) -> Result<(), String> {
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

    unsafe fn trash_in_pool(rt: &Runtime, path: &CStr) -> Result<(), String> {
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
            let trashed = send_trash(
                manager,
                sel(c"trashItemAtURL:resultingItemURL:error:"),
                url,
                null_mut(),
                &mut error,
            );
            if is_yes(trashed) {
                return Ok(());
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
        assert!(!error.is_empty());
    }

    #[test]
    #[ignore = "moves a file into the Trash of the user who runs it"]
    fn file_manager_moves_a_file_to_the_trash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("minimenta \"fallback\" ø.txt");
        std::fs::write(&path, b"x").unwrap();
        file_manager::trash(&path).unwrap();
        assert!(path.symlink_metadata().is_err());
    }
}
