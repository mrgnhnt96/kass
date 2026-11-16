//! Running work on the main thread, where AppKit, WebKit and GPUI live.

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::OnceLock;

use gpui_kit::{App, AsyncApp};
use objc2::MainThreadMarker;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

#[repr(C)]
struct DispatchQueue {
    _private: [u8; 0],
}

extern "C" {
    static _dispatch_main_q: DispatchQueue;
    fn dispatch_async_f(
        queue: *const DispatchQueue,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
    fn dispatch_sync_f(
        queue: *const DispatchQueue,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
}

fn main_queue() -> *const DispatchQueue {
    // SAFETY: libdispatch's main queue is a static that lives for the process.
    unsafe { &_dispatch_main_q }
}

type Job = Box<dyn FnOnce() + Send>;

extern "C" fn run_job(context: *mut c_void) {
    // SAFETY: `context` is the `Box<Job>` leaked by `main_async`.
    let job = unsafe { Box::from_raw(context as *mut Job) };
    if catch_unwind(AssertUnwindSafe(job)).is_err() {
        eprintln!("A main-thread job panicked");
    }
}

/// Run `f` on the main thread later, in order with other posted work.
pub fn main_async<F: FnOnce() + Send + 'static>(f: F) {
    let job: Box<Job> = Box::new(Box::new(f));
    // SAFETY: `run_job` takes the box back exactly once.
    unsafe { dispatch_async_f(main_queue(), Box::into_raw(job) as *mut c_void, run_job) };
}

/// Run `f` on the main thread and wait for its result. On the main thread
/// it runs inline.
pub fn main_sync<R: Send, F: FnOnce() -> R + Send>(f: F) -> R {
    if MainThreadMarker::new().is_some() {
        return f();
    }
    struct Slot<F, R> {
        f: Option<F>,
        result: Option<std::thread::Result<R>>,
    }
    extern "C" fn run<F: FnOnce() -> R, R>(context: *mut c_void) {
        // SAFETY: `context` is the `Slot` on the waiting thread's stack,
        // which stays alive until `dispatch_sync_f` returns.
        let slot = unsafe { &mut *(context as *mut Slot<F, R>) };
        if let Some(f) = slot.f.take() {
            slot.result = Some(catch_unwind(AssertUnwindSafe(f)));
        }
    }
    let mut slot = Slot {
        f: Some(f),
        result: None,
    };
    // SAFETY: dispatch_sync_f returns only after `run` finished with `slot`.
    unsafe {
        dispatch_sync_f(
            main_queue(),
            &mut slot as *mut Slot<F, R> as *mut c_void,
            run::<F, R>,
        )
    };
    match slot.result {
        Some(Ok(value)) => value,
        Some(Err(panic)) => std::panic::resume_unwind(panic),
        None => unreachable!("dispatch_sync_f ran the job"),
    }
}

type AppJob = Box<dyn FnOnce(&mut App) + Send>;

static APP_JOBS: OnceLock<UnboundedSender<AppJob>> = OnceLock::new();

/// Start running `with_app` jobs. Called once, when the app starts.
pub(crate) fn install(cx: &mut App) {
    let (tx, mut rx) = unbounded_channel::<AppJob>();
    if APP_JOBS.set(tx).is_err() {
        return;
    }
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Some(job) = rx.recv().await {
            cx.update(|cx| job(cx));
        }
    })
    .detach();
}

/// Run `f` with GPUI's `App` on the main thread, between GPUI's own updates.
pub fn with_app<F: FnOnce(&mut App) + Send + 'static>(f: F) {
    match APP_JOBS.get() {
        Some(tx) => {
            let _ = tx.send(Box::new(f));
        }
        None => eprintln!("with_app before the app started"),
    }
}
