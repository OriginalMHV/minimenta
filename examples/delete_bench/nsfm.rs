//! Copy of `file_manager` in src/ui/trash.rs (inner items only).
#![allow(clippy::all, clippy::pedantic, dead_code)]
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
