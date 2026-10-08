//! Reads the FSEvents history: which directories changed since a given event.
//! CoreServices is loaded with `dlopen` on first use, so runs that do not use
//! the cache do not pay its start-up cost.

use std::ffi::{CStr, OsStr, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

type CFRef = *const c_void;
type Callback = extern "C" fn(CFRef, *mut c_void, usize, *mut c_void, *const u32, *const u64);

const UTF8: u32 = 0x0800_0100;
const MUST_SCAN_SUBDIRS: u32 = 0x01;
const USER_DROPPED: u32 = 0x02;
const KERNEL_DROPPED: u32 = 0x04;
const IDS_WRAPPED: u32 = 0x08;
const HISTORY_DONE: u32 = 0x10;
const ROOT_CHANGED: u32 = 0x20;
const MOUNT: u32 = 0x40;
const UNMOUNT: u32 = 0x80;
/// Report a rename or removal of the root itself as `ROOT_CHANGED`.
const WATCH_ROOT: u32 = 0x04;

#[repr(C)]
struct StreamContext {
    version: isize,
    info: *const c_void,
    retain: extern "C" fn(*const c_void) -> *const c_void,
    release: extern "C" fn(*const c_void),
    copy_description: *const c_void,
}

#[repr(C)]
struct UuidBytes([u8; 16]);

struct Api {
    string_create: extern "C" fn(CFRef, *const u8, isize, u32, u8) -> CFRef,
    array_create: extern "C" fn(CFRef, *const CFRef, isize, *const c_void) -> CFRef,
    release_cf: extern "C" fn(CFRef),
    array_callbacks: *const c_void,
    uuid_bytes: extern "C" fn(CFRef) -> UuidBytes,
    copy_uuid: extern "C" fn(libc::dev_t) -> CFRef,
    current_id: extern "C" fn() -> u64,
    stream_create:
        extern "C" fn(CFRef, Callback, *const StreamContext, CFRef, u64, f64, u32) -> CFRef,
    set_queue: extern "C" fn(CFRef, *mut c_void),
    start: extern "C" fn(CFRef) -> u8,
    stop: extern "C" fn(CFRef),
    invalidate: extern "C" fn(CFRef),
    release_stream: extern "C" fn(CFRef),
}

// SAFETY: the API holds only function pointers and one pointer to an
// immutable framework constant.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut c_void;
    fn dispatch_release(object: *mut c_void);
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe { load() }).as_ref()
}

unsafe fn load() -> Option<Api> {
    unsafe {
        let path = c"/System/Library/Frameworks/CoreServices.framework/CoreServices";
        let handle = libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
        if handle.is_null() {
            return None;
        }
        Some(Api {
            string_create: sym(handle, c"CFStringCreateWithBytes")?,
            array_create: sym(handle, c"CFArrayCreate")?,
            release_cf: sym(handle, c"CFRelease")?,
            array_callbacks: sym(handle, c"kCFTypeArrayCallBacks")?,
            uuid_bytes: sym(handle, c"CFUUIDGetUUIDBytes")?,
            copy_uuid: sym(handle, c"FSEventsCopyUUIDForDevice")?,
            current_id: sym(handle, c"FSEventsGetCurrentEventId")?,
            stream_create: sym(handle, c"FSEventStreamCreate")?,
            set_queue: sym(handle, c"FSEventStreamSetDispatchQueue")?,
            start: sym(handle, c"FSEventStreamStart")?,
            stop: sym(handle, c"FSEventStreamStop")?,
            invalidate: sym(handle, c"FSEventStreamInvalidate")?,
            release_stream: sym(handle, c"FSEventStreamRelease")?,
        })
    }
}

/// Looks up a symbol as a function pointer or data pointer of type `T`.
///
/// # Safety
/// `T` must match the real signature of the symbol.
unsafe fn sym<T: Copy>(handle: *mut c_void, name: &CStr) -> Option<T> {
    assert_eq!(size_of::<T>(), size_of::<*mut c_void>());
    let ptr = unsafe { libc::dlsym(handle, name.as_ptr()) };
    (!ptr.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, T>(&ptr) })
}

pub fn current_event_id() -> Option<u64> {
    Some((api()?.current_id)())
}

/// Identifies the event history of a volume. It changes when the history is
/// reset, which makes old event IDs meaningless.
pub fn volume_uuid(dev: u64) -> Option<[u8; 16]> {
    let api = api()?;
    let uuid = (api.copy_uuid)(dev as libc::dev_t);
    if uuid.is_null() {
        return None;
    }
    let bytes = (api.uuid_bytes)(uuid).0;
    (api.release_cf)(uuid);
    Some(bytes)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    /// The whole subtree must be scanned again, not only the directory itself.
    pub recursive: bool,
}

#[derive(Default)]
struct State {
    changes: Vec<Change>,
    done: bool,
    unusable: bool,
}

#[derive(Default)]
struct Collector {
    state: Mutex<State>,
    wake: Condvar,
}

extern "C" fn retain(info: *const c_void) -> *const c_void {
    unsafe { Arc::increment_strong_count(info.cast::<Collector>()) };
    info
}

extern "C" fn release(info: *const c_void) {
    unsafe { Arc::decrement_strong_count(info.cast::<Collector>()) };
}

extern "C" fn on_events(
    _: CFRef,
    info: *mut c_void,
    n: usize,
    paths: *mut c_void,
    flags: *const u32,
    _: *const u64,
) {
    let collector = unsafe { &*info.cast::<Collector>() };
    let paths = paths.cast::<*const c_char>();
    let mut state = collector.state.lock().unwrap();
    for i in 0..n {
        let flags = unsafe { *flags.add(i) };
        if flags & HISTORY_DONE != 0 {
            state.done = true;
        } else if flags & (USER_DROPPED | KERNEL_DROPPED | IDS_WRAPPED | ROOT_CHANGED) != 0 {
            state.unusable = true;
        } else {
            let path = unsafe { CStr::from_ptr(*paths.add(i)) };
            state.changes.push(Change {
                path: PathBuf::from(OsStr::from_bytes(path.to_bytes())),
                recursive: flags & (MUST_SCAN_SUBDIRS | MOUNT | UNMOUNT) != 0,
            });
        }
    }
    collector.wake.notify_all();
}

/// Returns the directories below `root` that changed after event `since`, or
/// `None` when the history cannot answer within `budget` or `cancel` is set.
/// Then the caller must scan everything.
pub fn changes_since(
    root: &Path,
    since: u64,
    budget: Duration,
    cancel: &AtomicBool,
) -> Option<Vec<Change>> {
    let api = api()?;
    let collector = Arc::new(Collector::default());
    let root = root.as_os_str().as_bytes();
    let path = (api.string_create)(
        std::ptr::null(),
        root.as_ptr(),
        root.len() as isize,
        UTF8,
        0,
    );
    if path.is_null() {
        return None;
    }
    let paths = (api.array_create)(std::ptr::null(), &path, 1, api.array_callbacks);
    (api.release_cf)(path);
    let context = StreamContext {
        version: 0,
        info: Arc::as_ptr(&collector).cast(),
        retain,
        release,
        copy_description: std::ptr::null(),
    };
    let stream = (api.stream_create)(
        std::ptr::null(),
        on_events,
        &context,
        paths,
        since,
        0.0,
        WATCH_ROOT,
    );
    (api.release_cf)(paths);
    if stream.is_null() {
        return None;
    }
    let queue = unsafe { dispatch_queue_create(c"minimenta.fsevents".as_ptr(), std::ptr::null()) };
    (api.set_queue)(stream, queue);

    let mut result = None;
    if (api.start)(stream) != 0 {
        let deadline = Instant::now() + budget;
        let mut state = collector.state.lock().unwrap();
        // Short waits, so Esc on the progress screen takes effect quickly.
        while !state.done && !state.unusable && !cancel.load(Relaxed) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let slice = left.min(Duration::from_millis(100));
            state = collector.wake.wait_timeout(state, slice).unwrap().0;
        }
        if state.done && !state.unusable && !cancel.load(Relaxed) {
            result = Some(std::mem::take(&mut state.changes));
        }
    }
    (api.stop)(stream);
    (api.invalidate)(stream);
    (api.release_stream)(stream);
    unsafe { dispatch_release(queue) };
    result
}
