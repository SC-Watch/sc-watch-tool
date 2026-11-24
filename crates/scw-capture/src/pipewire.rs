//! A PipeWire video stream read into frames: the screen a desktop shares
//! through the screen-cast portal, or (in tests) any video source.
//!
//! libpipewire is loaded when a stream is opened, not linked: the program
//! builds with no PipeWire headers or C compiler, runs where PipeWire is
//! missing (and says so), and uses whichever PipeWire the system has. Only
//! the stream calls of `pipewire/stream.h` are used, with the structures
//! copied from its headers (`spa.rs` builds the parameters).
//!
//! PipeWire runs the stream on its own thread. Its process callback keeps
//! only the newest buffer, and turns it into a BGR frame only when a grab
//! is waiting for one, so a 60 Hz screen costs nothing between bursts.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::fd::{IntoRawFd, OwnedFd};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use libloading::Library;
use scw_vision::Image;

use crate::CaptureError;
use crate::spa::{self, Layout, VideoFormat};

const LIBRARY: &str = "libpipewire-0.3.so.0";

// pipewire/stream.h
const DIRECTION_INPUT: c_int = 0;
const FLAG_AUTOCONNECT: c_int = 1 << 0;
const FLAG_MAP_BUFFERS: c_int = 1 << 2;
const STATE_ERROR: c_int = -1;
const STATE_STREAMING: c_int = 3;
const ID_ANY: u32 = 0xffff_ffff;
const VERSION_STREAM_EVENTS: u32 = 2;
// spa/buffer/buffer.h
const CHUNK_FLAG_CORRUPTED: i32 = 1;

type Opaque = *mut c_void;
type Callback = Option<unsafe extern "C" fn()>;

#[repr(C)]
struct SpaHook {
    link: [Opaque; 2],
    cb: [Opaque; 2],
    removed: Opaque,
    private: Opaque,
}

#[repr(C)]
struct StreamEvents {
    version: u32,
    destroy: Callback,
    state_changed: Option<unsafe extern "C" fn(Opaque, c_int, c_int, *const c_char)>,
    control_info: Callback,
    io_changed: Callback,
    param_changed: Option<unsafe extern "C" fn(Opaque, u32, *const c_void)>,
    add_buffer: Callback,
    remove_buffer: Callback,
    process: Option<unsafe extern "C" fn(Opaque)>,
    drained: Callback,
    command: Callback,
    trigger_done: Callback,
}

#[repr(C)]
struct PwBuffer {
    buffer: *mut SpaBuffer,
    // user_data, size, requested, time follow; not read
}

#[repr(C)]
struct SpaBuffer {
    n_metas: u32,
    n_datas: u32,
    metas: Opaque,
    datas: *mut SpaData,
}

#[repr(C)]
struct SpaData {
    kind: u32,
    flags: u32,
    fd: i64,
    mapoffset: u32,
    maxsize: u32,
    data: Opaque,
    chunk: *mut SpaChunk,
}

#[repr(C)]
struct SpaChunk {
    offset: u32,
    size: u32,
    stride: i32,
    flags: i32,
}

/// The calls used, looked up in the library once.
struct Api {
    _lib: Library,
    init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char),
    loop_new: unsafe extern "C" fn(*const c_char, *const c_void) -> Opaque,
    loop_get: unsafe extern "C" fn(Opaque) -> Opaque,
    loop_start: unsafe extern "C" fn(Opaque) -> c_int,
    loop_stop: unsafe extern "C" fn(Opaque),
    loop_lock: unsafe extern "C" fn(Opaque),
    loop_unlock: unsafe extern "C" fn(Opaque),
    loop_destroy: unsafe extern "C" fn(Opaque),
    context_new: unsafe extern "C" fn(Opaque, Opaque, usize) -> Opaque,
    context_destroy: unsafe extern "C" fn(Opaque),
    connect: unsafe extern "C" fn(Opaque, Opaque, usize) -> Opaque,
    connect_fd: unsafe extern "C" fn(Opaque, c_int, Opaque, usize) -> Opaque,
    core_disconnect: unsafe extern "C" fn(Opaque) -> c_int,
    properties_new: unsafe extern "C" fn(*const c_char, ...) -> Opaque,
    stream_new: unsafe extern "C" fn(Opaque, *const c_char, Opaque) -> Opaque,
    stream_add_listener: unsafe extern "C" fn(Opaque, *mut SpaHook, *const StreamEvents, Opaque),
    stream_connect:
        unsafe extern "C" fn(Opaque, c_int, u32, c_int, *const *const c_void, u32) -> c_int,
    stream_disconnect: unsafe extern "C" fn(Opaque) -> c_int,
    stream_destroy: unsafe extern "C" fn(Opaque),
    stream_update_params: unsafe extern "C" fn(Opaque, *const *const c_void, u32) -> c_int,
    dequeue: unsafe extern "C" fn(Opaque) -> *mut PwBuffer,
    queue: unsafe extern "C" fn(Opaque, *mut PwBuffer) -> c_int,
}

impl Api {
    fn load() -> Result<Api, String> {
        // SAFETY: loading the system's PipeWire runs its constructors, which
        // is what any PipeWire program does.
        let lib = unsafe { Library::new(LIBRARY) }.map_err(|e| {
            format!(
                "PipeWire's library ({LIBRARY}) could not be loaded - is PipeWire installed? ({e})"
            )
        })?;
        Ok(Api {
            init: sym(&lib, "pw_init")?,
            loop_new: sym(&lib, "pw_thread_loop_new")?,
            loop_get: sym(&lib, "pw_thread_loop_get_loop")?,
            loop_start: sym(&lib, "pw_thread_loop_start")?,
            loop_stop: sym(&lib, "pw_thread_loop_stop")?,
            loop_lock: sym(&lib, "pw_thread_loop_lock")?,
            loop_unlock: sym(&lib, "pw_thread_loop_unlock")?,
            loop_destroy: sym(&lib, "pw_thread_loop_destroy")?,
            context_new: sym(&lib, "pw_context_new")?,
            context_destroy: sym(&lib, "pw_context_destroy")?,
            connect: sym(&lib, "pw_context_connect")?,
            connect_fd: sym(&lib, "pw_context_connect_fd")?,
            core_disconnect: sym(&lib, "pw_core_disconnect")?,
            properties_new: sym(&lib, "pw_properties_new")?,
            stream_new: sym(&lib, "pw_stream_new")?,
            stream_add_listener: sym(&lib, "pw_stream_add_listener")?,
            stream_connect: sym(&lib, "pw_stream_connect")?,
            stream_disconnect: sym(&lib, "pw_stream_disconnect")?,
            stream_destroy: sym(&lib, "pw_stream_destroy")?,
            stream_update_params: sym(&lib, "pw_stream_update_params")?,
            dequeue: sym(&lib, "pw_stream_dequeue_buffer")?,
            queue: sym(&lib, "pw_stream_queue_buffer")?,
            _lib: lib,
        })
    }
}

/// A function in the library, as the type its header gives it. The pointer
/// stays valid while `lib` is loaded, which [`Api`] keeps it.
fn sym<T: Copy>(lib: &Library, name: &str) -> Result<T, String> {
    // SAFETY: each caller names the function's type from PipeWire's header.
    unsafe { lib.get::<T>(name) }
        .map(|s| *s)
        .map_err(|e| format!("PipeWire has no {name} ({e}) - too old?"))
}

/// Which node to read.
#[derive(Clone, Debug)]
pub enum Target {
    /// A node id, as the portal hands out.
    Id(u32),
    /// A node's name or serial (PipeWire's `target.object`).
    Object(String),
}

#[derive(Default)]
struct Slot {
    format: Option<VideoFormat>,
    state: c_int,
    error: Option<String>,
    /// A grab is waiting for the next buffer.
    want: bool,
    frame: Option<Image<u8>>,
    /// Why the last buffer could not be read, once.
    unreadable: Option<String>,
    frames: u64,
}

/// What the PipeWire thread and the grabbing thread share.
struct Shared {
    api: Arc<Api>,
    stream: AtomicPtr<c_void>,
    slot: Mutex<Slot>,
    arrived: Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The buffer, as a frame for a waiting grab.
    ///
    /// # Safety
    /// `b` is a buffer just dequeued from the stream, not yet queued back.
    unsafe fn read(&self, b: *mut PwBuffer) {
        let mut slot = self.lock();
        if !slot.want {
            return;
        }
        let Some(f) = slot.format else {
            return;
        };
        // SAFETY: a dequeued buffer's spa_buffer, data and chunk stay valid
        // until it is queued back, which the caller does after this.
        let read = unsafe { frame_of(b, f) };
        match read {
            Ok(img) => {
                slot.frame = Some(img);
                slot.want = false;
                slot.frames += 1;
                self.arrived.notify_all();
            }
            Err(why) => {
                if slot.unreadable.is_none() {
                    slot.unreadable = Some(why);
                    self.arrived.notify_all();
                }
            }
        }
    }
}

/// # Safety
/// As [`Shared::read`].
unsafe fn frame_of(b: *mut PwBuffer, f: VideoFormat) -> Result<Image<u8>, String> {
    let layout =
        Layout::from_id(f.format).ok_or(format!("pixel format {} is not readable", f.format))?;
    // SAFETY: see the caller.
    let data = unsafe {
        let buf = (*b).buffer;
        if buf.is_null() || (*buf).n_datas == 0 || (*buf).datas.is_null() {
            return Err("an empty buffer".into());
        }
        &*(*buf).datas
    };
    if data.data.is_null() || data.chunk.is_null() {
        return Err(if data.kind == spa::DATA_DMA_BUF {
            "the screen came as GPU memory (DMA-BUF), which this version cannot read".into()
        } else {
            format!("a buffer of kind {} that is not mapped", data.kind)
        });
    }
    // SAFETY: a buffer's chunk is valid with it.
    let chunk = unsafe { &*data.chunk };
    if chunk.flags & CHUNK_FLAG_CORRUPTED != 0 {
        return Err("a corrupted buffer".into());
    }
    let max = data.maxsize as usize;
    let offset = (chunk.offset as usize).min(max);
    let size = (chunk.size as usize).min(max - offset);
    let (w, h) = (f.width as usize, f.height as usize);
    let stride = if chunk.stride > 0 {
        chunk.stride as usize
    } else {
        w * 4
    };
    // SAFETY: the mapped memory holds maxsize bytes from data.
    let bytes = unsafe { std::slice::from_raw_parts((data.data as *const u8).add(offset), size) };
    spa::to_bgr(bytes, w, h, stride, layout)
        .ok_or_else(|| format!("{size} bytes is too few for {w}x{h}, {stride} a row"))
}

unsafe extern "C" fn on_state(data: Opaque, _old: c_int, state: c_int, error: *const c_char) {
    // SAFETY: data is the Shared given to add_listener, alive while the
    // stream is.
    let shared = unsafe { &*(data as *const Shared) };
    let mut slot = shared.lock();
    slot.state = state;
    if state == STATE_ERROR {
        slot.error = Some(if error.is_null() {
            "the stream failed".into()
        } else {
            // SAFETY: PipeWire passes a C string, or null.
            unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned()
        });
    }
    shared.arrived.notify_all();
}

unsafe extern "C" fn on_param(data: Opaque, id: u32, param: *const c_void) {
    if id != spa::PARAM_FORMAT || param.is_null() {
        return;
    }
    // SAFETY: as on_state; a pod is a size, a type, then size bytes.
    let shared = unsafe { &*(data as *const Shared) };
    let bytes = unsafe {
        let size = *(param as *const u32) as usize;
        std::slice::from_raw_parts(param as *const u8, 8 + size)
    };
    let format = spa::parse_format(bytes);
    shared.lock().format = format;
    let stream = shared.stream.load(Ordering::Acquire);
    if format.is_some() && !stream.is_null() {
        let pod = spa::aligned(&spa::buffers());
        let params = [pod.as_ptr() as *const c_void];
        // SAFETY: called on the stream's own thread, as PipeWire's examples
        // do; the pod is copied before this returns.
        unsafe { (shared.api.stream_update_params)(stream, params.as_ptr(), 1) };
    }
    shared.arrived.notify_all();
}

unsafe extern "C" fn on_process(data: Opaque) {
    // SAFETY: as on_state.
    let shared = unsafe { &*(data as *const Shared) };
    let stream = shared.stream.load(Ordering::Acquire);
    if stream.is_null() {
        return;
    }
    let api = &shared.api;
    // SAFETY: dequeue and queue on the stream's own thread; only the newest
    // buffer is read, the rest go straight back.
    unsafe {
        let mut newest: *mut PwBuffer = null_mut();
        loop {
            let b = (api.dequeue)(stream);
            if b.is_null() {
                break;
            }
            if !newest.is_null() {
                (api.queue)(stream, newest);
            }
            newest = b;
        }
        if newest.is_null() {
            return;
        }
        shared.read(newest);
        (api.queue)(stream, newest);
    }
}

/// A video stream being read.
pub struct PwCapture {
    shared: Arc<Shared>,
    tloop: Opaque,
    context: Opaque,
    core: Opaque,
    stream: Opaque,
    _hook: Box<SpaHook>,
    _events: Box<StreamEvents>,
}

// SAFETY: the PipeWire objects are only touched with the thread loop's lock
// held (open, drop) or from its own thread (the callbacks).
unsafe impl Send for PwCapture {}

impl PwCapture {
    /// Connects to PipeWire - through `fd` (the portal's remote) or the
    /// user's own daemon - and starts reading `target`.
    pub fn open(fd: Option<OwnedFd>, target: Target, name: &str) -> Result<PwCapture, String> {
        let api = Arc::new(Api::load()?);
        let shared = Arc::new(Shared {
            api: api.clone(),
            stream: AtomicPtr::new(null_mut()),
            slot: Mutex::new(Slot::default()),
            arrived: Condvar::new(),
        });
        let hook = Box::new(SpaHook {
            link: [null_mut(); 2],
            cb: [null_mut(); 2],
            removed: null_mut(),
            private: null_mut(),
        });
        let events = Box::new(StreamEvents {
            version: VERSION_STREAM_EVENTS,
            destroy: None,
            state_changed: Some(on_state),
            control_info: None,
            io_changed: None,
            param_changed: Some(on_param),
            add_buffer: None,
            remove_buffer: None,
            process: Some(on_process),
            drained: None,
            command: None,
            trigger_done: None,
        });
        let cname = CString::new(name).map_err(|e| e.to_string())?;
        let object = match &target {
            Target::Object(o) => Some(CString::new(o.as_str()).map_err(|e| e.to_string())?),
            Target::Id(_) => None,
        };
        let target_id = match target {
            Target::Id(id) => id,
            Target::Object(_) => ID_ANY,
        };
        let c = |s: &'static CStr| s.as_ptr();
        // SAFETY: PipeWire's calls in the order its examples make them; every
        // pointer passed is alive for the call (the pods and strings), or for
        // the stream's life (hook, events, shared - kept in the result).
        unsafe {
            (api.init)(null_mut(), null_mut());
            let tloop = (api.loop_new)(cname.as_ptr(), null());
            if tloop.is_null() {
                return Err("PipeWire would not start a thread".into());
            }
            let context = (api.context_new)((api.loop_get)(tloop), null_mut(), 0);
            if context.is_null() {
                (api.loop_destroy)(tloop);
                return Err("PipeWire would not make a context".into());
            }
            if (api.loop_start)(tloop) < 0 {
                (api.context_destroy)(context);
                (api.loop_destroy)(tloop);
                return Err("PipeWire's thread would not start".into());
            }
            (api.loop_lock)(tloop);
            let core = match fd {
                Some(fd) => (api.connect_fd)(context, fd.into_raw_fd(), null_mut(), 0),
                None => (api.connect)(context, null_mut(), 0),
            };
            let mut me = PwCapture {
                shared: shared.clone(),
                tloop,
                context,
                core,
                stream: null_mut(),
                _hook: hook,
                _events: events,
            };
            if core.is_null() {
                (api.loop_unlock)(tloop);
                return Err(format!(
                    "could not connect to PipeWire ({})",
                    std::io::Error::last_os_error()
                ));
            }
            let props = match &object {
                Some(o) => (api.properties_new)(
                    c(c"media.type"),
                    c(c"Video"),
                    c(c"media.category"),
                    c(c"Capture"),
                    c(c"media.role"),
                    c(c"Screen"),
                    c(c"target.object"),
                    o.as_ptr(),
                    null::<c_char>(),
                ),
                None => (api.properties_new)(
                    c(c"media.type"),
                    c(c"Video"),
                    c(c"media.category"),
                    c(c"Capture"),
                    c(c"media.role"),
                    c(c"Screen"),
                    null::<c_char>(),
                ),
            };
            let stream = (api.stream_new)(core, cname.as_ptr(), props);
            if stream.is_null() {
                (api.loop_unlock)(tloop);
                return Err("PipeWire would not make a stream".into());
            }
            me.stream = stream;
            shared.stream.store(stream, Ordering::Release);
            (api.stream_add_listener)(
                stream,
                &mut *me._hook,
                &*me._events,
                Arc::as_ptr(&shared) as Opaque,
            );
            let pod = spa::aligned(&spa::enum_format());
            let params = [pod.as_ptr() as *const c_void];
            let r = (api.stream_connect)(
                stream,
                DIRECTION_INPUT,
                target_id,
                FLAG_AUTOCONNECT | FLAG_MAP_BUFFERS,
                params.as_ptr(),
                1,
            );
            (api.loop_unlock)(tloop);
            if r < 0 {
                return Err(format!(
                    "the stream would not connect ({})",
                    std::io::Error::from_raw_os_error(-r)
                ));
            }
            Ok(me)
        }
    }

    /// Waits up to `wait` for the stream to settle on a format; its size.
    pub fn wait_format(&self, wait: Duration) -> Result<VideoFormat, String> {
        let end = Instant::now() + wait;
        let mut slot = self.shared.lock();
        loop {
            if let Some(e) = &slot.error {
                return Err(e.clone());
            }
            if let Some(f) = slot.format {
                return Ok(f);
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("no picture arrived from the shared screen".into());
            }
            slot = self
                .shared
                .arrived
                .wait_timeout(slot, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// The next frame to arrive within `wait`: None when none did (a screen
    /// that has not changed sends nothing). Only frames that arrive while a
    /// grab waits are read, so none is ever older than the grab.
    pub fn next_frame(&self, wait: Duration) -> Result<Option<Image<u8>>, CaptureError> {
        let end = Instant::now() + wait;
        let mut slot = self.shared.lock();
        slot.frame = None;
        slot.want = true;
        let got = self.wait_frame(slot, end);
        self.shared.lock().want = false;
        got
    }

    fn wait_frame(
        &self,
        mut slot: std::sync::MutexGuard<'_, Slot>,
        end: Instant,
    ) -> Result<Option<Image<u8>>, CaptureError> {
        loop {
            if let Some(e) = &slot.error {
                return Err(CaptureError::System(format!(
                    "the screen stream failed: {e}"
                )));
            }
            if let Some(f) = slot.frame.take() {
                return Ok(Some(f));
            }
            if let Some(why) = slot.unreadable.take() {
                return Err(CaptureError::System(why));
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            slot = self
                .shared
                .arrived
                .wait_timeout(slot, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// Whether the stream is running, and how many frames it has given.
    pub fn streaming(&self) -> (bool, u64) {
        let slot = self.shared.lock();
        (slot.state == STATE_STREAMING, slot.frames)
    }

    pub fn format(&self) -> Option<VideoFormat> {
        self.shared.lock().format
    }
}

impl Drop for PwCapture {
    fn drop(&mut self) {
        let api = &self.shared.api;
        // SAFETY: teardown in the reverse order, the stream's under the lock;
        // the thread is stopped before the context goes.
        unsafe {
            (api.loop_lock)(self.tloop);
            self.shared.stream.store(null_mut(), Ordering::Release);
            if !self.stream.is_null() {
                (api.stream_disconnect)(self.stream);
                (api.stream_destroy)(self.stream);
            }
            if !self.core.is_null() {
                (api.core_disconnect)(self.core);
            }
            (api.loop_unlock)(self.tloop);
            (api.loop_stop)(self.tloop);
            (api.context_destroy)(self.context);
            (api.loop_destroy)(self.tloop);
        }
    }
}
