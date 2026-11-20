//! The Windows driver: every Win32, Direct3D 11, WinRT and Media Foundation call, in one file.
//!
//! Deliberate contents. The decisions live in [`super::lifecycle`], the arithmetic in
//! [`super::geometry`] and [`super::convert`], and both are tested on every host. What is here can only
//! run against a Windows display, so it cannot be tested from this workspace; it is checked by a
//! cross-target `cargo check`, and each block says which projection and which documented behaviour it
//! relies on.
//!
//! **One thread owns the capture.** A request spawns a thread that does everything itself: opens the
//! system picker, builds the device, the frame pool, the session and the muxer, takes frames, packs them,
//! writes them, closes the session, finalises the file, and releases every object. The pool's
//! `FrameArrived` callback — which a free-threaded pool raises on the pool's own worker — does one thing
//! only: it signals the capture thread's existing condition variable. No COM object crosses a thread, so
//! no interface has to be asserted `Send`, and the D3D11 context is never used concurrently. The macOS
//! build has the same shape: all of `capture.m`'s work happens on the main queue.
//!
//! Stage A limits, all of them visible rather than silent:
//!
//! * **video only** — `audio: true` is refused by the lifecycle before the picker opens, because Windows
//!   Graphics Capture carries no audio and the loopback path is a separate stage;
//! * **CPU packing** — the frame is copied to a staging texture, mapped, and packed by
//!   [`super::convert`]. The GPU alternatives (`ID3D11VideoProcessor`, or a Media Foundation DX manager so
//!   the encoder reads the surface directly) are later stages;
//! * **SDR BGRA only** — the pool is created in `DXGI_FORMAT_B8G8R8A8_UNORM`, so content from an HDR
//!   display keeps the sRGB interpretation that format implies rather than being re-tagged;
//! * **one fixed output size**, taken from the item at pick time. A frame whose content is smaller than
//!   its surface is rescaled into that fixed size, and the undefined remainder of the surface is never
//!   sampled. A frame that cannot be packed, or a muxer that refuses it, ends the capture as `FAILED`
//!   rather than silently changing the resolution of a recording already being muxed.

use std::{
    ffi::CStr,
    mem::size_of,
    slice,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, LazyLock, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

use windows::core::{Interface, PCWSTR};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCapturePicker,
    GraphicsCaptureSession,
};
use windows::Graphics::DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat};
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::{
    Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
    IDXGIDevice,
};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFSinkWriter, MFCreateAttributes, MFCreateMediaType, MFCreateMemoryBuffer,
    MFCreateSample, MFCreateSinkWriterFromURL, MFMediaType_Video, MFShutdown, MFStartup,
    MFVideoFormat_H264, MFVideoFormat_NV12, MFSTARTUP_FULL, MF_LOW_LATENCY,
    MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_VERSION,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DispatcherQueueOptions, RoInitialize, DQTAT_COM_NONE,
    DQTYPE_THREAD_CURRENT, RO_INIT_MULTITHREADED,
};
use windows::Win32::UI::Shell::IInitializeWithWindow;

use super::{
    convert, geometry,
    lifecycle::{self, Actions, Effect, Report},
    Event, Parent,
};

/// The one capture slot, and the mailbox of the capture in flight. Both are process-wide because a screen
/// capture is: the macOS build keeps one `currentCapture` for the same reason.
static SLOT: LazyLock<Mutex<lifecycle::Slot>> =
    LazyLock::new(|| Mutex::new(lifecycle::Slot::default()));
static RUNNING: LazyLock<Mutex<Option<Arc<Control>>>> = LazyLock::new(|| Mutex::new(None));
/// The clock the watchdog is measured against, so a deadline cannot move with the calendar.
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// How often the capture thread wakes to advance the watchdog when nothing else happens.
const TICK: Duration = Duration::from_millis(250);

/// MFVideoInterlaceMode_Progressive. The crate does not project that enum and the attribute store takes
/// the integer, so the documented value goes in with its name rather than as a bare literal.
const PROGRESSIVE: u32 = 2;

/// `Windows.Foundation.AsyncStatus`, whose enum lives in the `windows-future` crate that this workspace
/// does not depend on directly, and whose projection exposes no HRESULT accessor. The completion state is
/// therefore read through the integer the enum wraps, against the documented values: Started 0, Completed
/// 1, Canceled 2, Error 3.
const ASYNC_STARTED: i32 = 0;
const ASYNC_CANCELED: i32 = 2;

fn slot() -> MutexGuard<'static, lifecycle::Slot> {
    SLOT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn running() -> MutexGuard<'static, Option<Arc<Control>>> {
    RUNNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Monotonic milliseconds for the lifecycle's watchdog.
fn clock() -> u64 {
    u64::try_from(EPOCH.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Whether this device can capture screens at all. Microsoft's own answer to that question is
/// `GraphicsCaptureSession::IsSupported`, and it asks the user for nothing — the counterpart of the macOS
/// build's `@available` gate. Cached, because the status command polls it and the answer cannot change
/// while the process runs.
pub(super) fn supported() -> bool {
    static SUPPORTED: LazyLock<bool> = LazyLock::new(|| {
        // WinRT activation needs the calling thread initialised; this runs on a host worker thread, so the
        // initialisation belongs to that thread alone. A thread that already is initialised reports
        // S_FALSE, which the projection returns as success.
        let initialised = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
        initialised && GraphicsCaptureSession::IsSupported().unwrap_or(false)
    });
    *SUPPORTED
}

/// The capture thread's mailbox: who asked for a stop, whether a frame is waiting, whether the item went
/// away underneath us, and whether the session is over.
struct Control {
    token: u64,
    state: Mutex<State>,
    changed: Condvar,
    frames: AtomicBool,
    failed: AtomicBool,
}

#[derive(Default)]
struct State {
    requested: bool,
    settled: bool,
}

impl Control {
    /// The command layer asking to stop. A token that is not current is dropped here as well, so a late
    /// stop cannot disturb the capture that replaced the one it was aimed at.
    fn request(&self, token: u64) {
        if token != self.token {
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            state.requested = true;
            self.changed.notify_one();
        }
    }

    /// The only thing the `FrameArrived` callback does: wake the thread that owns the pool. It touches no
    /// COM object, which is what keeps every interface on one thread.
    fn arrived(&self) {
        self.frames.store(true, Ordering::SeqCst);
        self.changed.notify_one();
    }

    /// The `GraphicsCaptureItem.Closed` notification: the thing being captured is gone, which nobody asked
    /// for, so it is a failure rather than a cancel.
    fn failed(&self) {
        self.failed.store(true, Ordering::SeqCst);
        self.changed.notify_one();
    }

    fn take(&self) -> (bool, bool, bool) {
        let Ok(mut state) = self.state.lock() else {
            return (false, false, false);
        };
        (
            core::mem::take(&mut state.requested),
            self.failed.swap(false, Ordering::SeqCst),
            self.frames.swap(false, Ordering::SeqCst),
        )
    }

    fn settled(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.settled)
    }

    /// Whether a stop is waiting while the picker is still up. The system picker cannot be dismissed by
    /// the app, so a stop during it is answered by abandoning the operation.
    fn waiting_stop(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.requested)
    }

    fn settle(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.settled = true;
            self.changed.notify_one();
        }
    }

    /// Wait for a stop, a frame, a failure, a settlement, or the tick that advances the watchdog.
    fn wait(&self) {
        let Ok(state) = self.state.lock() else {
            return;
        };
        let _ = self.changed.wait_timeout_while(state, TICK, |state| {
            !state.requested
                && !state.settled
                && !self.frames.load(Ordering::SeqCst)
                && !self.failed.load(Ordering::SeqCst)
        });
    }
}

/// Start one capture.
///
/// Nothing is reported from the calling thread: the command layer holds the shared state machine's lock
/// across this call, and that is the same lock the event callback takes, so a synchronous callback here
/// would deadlock the host. The macOS build dispatches to the main queue for the same reason.
pub(super) fn start(path: &CStr, audio: bool, token: u64, event: Event, parent: Parent) {
    // The workspace hands a UTF-8 path. Media Foundation wants UTF-16, and a destination that is not valid
    // UTF-8 is refused rather than written somewhere the user did not ask for.
    let destination = path.to_str().map(str::to_owned).unwrap_or_default();
    let supported = supported();
    let actions = { slot().start(clock(), token, audio, supported, !destination.is_empty()) };
    if !actions
        .iter()
        .any(|effect| matches!(effect, Effect::Present))
    {
        // A refusal: no picker, no thread, no capture. The lifecycle decided which event the shared
        // machine gets, and it is still reported off this thread for the lock reason above.
        refuse(event, token, actions);
        return;
    }
    let control = Arc::new(Control {
        token,
        state: Mutex::new(State::default()),
        changed: Condvar::new(),
        frames: AtomicBool::new(false),
        failed: AtomicBool::new(false),
    });
    *running() = Some(Arc::clone(&control));
    let context = Context {
        token,
        event,
        parent,
        destination,
        control,
    };
    match std::thread::Builder::new()
        .name("uebox-capture".into())
        .spawn(move || run(context, actions))
    {
        Ok(_) => {}
        Err(_) => {
            // No thread, so no capture: retire the reservation exactly as a terminal event would.
            *running() = None;
            let actions = { slot().report(clock(), token, Report::Failed) };
            refuse(event, token, actions);
        }
    }
}

/// Stop the current capture. Idempotent, and never waits for it to settle.
pub(super) fn stop(token: u64) {
    if let Some(control) = running().as_ref().cloned() {
        control.request(token);
    }
}

/// Emit the events a refusal produced, away from the thread that asked for the capture. A refusal carries
/// nothing else: the lifecycle answers a refusal with one `Emit` and no session, so there is no COM to do
/// it alongside.
fn refuse(event: Event, token: u64, actions: Actions) {
    let _ = tauri::async_runtime::spawn_blocking(move || {
        for effect in actions {
            if let Effect::Emit(code) = effect {
                event(token, code);
            }
        }
    });
}

/// What one capture thread carries: plain data only, so nothing has to be asserted `Send`.
struct Context {
    token: u64,
    event: Event,
    parent: Parent,
    destination: String,
    control: Arc<Control>,
}

/// The objects the capture thread owns, in the order it acquires them: the item the user picked, then the
/// pipeline built from it. Both die with the thread.
#[derive(Default)]
struct Holding {
    item: Option<GraphicsCaptureItem>,
    pipeline: Option<Pipeline>,
}

/// The thread's entry point.
fn run(context: Context, actions: Actions) {
    // The picker is documented to need a thread with an active dispatcher queue, and a Win32 app has no
    // XAML dispatcher of its own: `CreateDispatcherQueueController` on this thread is what Microsoft
    // documents for that. The controller is held for the whole thread, because dropping it tears the
    // queue down.
    let queue = unsafe {
        CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_NONE,
        })
    }
    .ok();
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    let _ = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) };
    let mut holding = Holding::default();
    execute(actions, &mut holding, &context);
    // The normal path released everything already; this covers a cancel or a refusal that never built a
    // pipeline, and it runs on the thread that created the objects, which is the only requirement.
    release(&mut holding, &context);
    drop(queue);
}

/// Carry out the lifecycle's decision, in order. `holding` belongs to this thread, so every COM call
/// below happens on the thread that created it.
fn execute(actions: Actions, holding: &mut Holding, context: &Context) {
    for effect in actions {
        match effect {
            Effect::Emit(code) => (context.event)(context.token, code),
            Effect::Present => present(holding, context),
            Effect::Capture(output) => capture(holding, context, output),
            Effect::StopCapture => stop_capture(holding, context),
            Effect::FinalizeOutput => finalize_output(holding, context),
            Effect::Release => release(holding, context),
        }
    }
}

fn report(value: Report, holding: &mut Holding, context: &Context) {
    let actions = { slot().report(clock(), context.token, value) };
    execute(actions, holding, context);
}

fn stop_lifecycle(holding: &mut Holding, context: &Context) {
    let actions = { slot().stop(clock(), context.token) };
    execute(actions, holding, context);
}

fn tick(holding: &mut Holding, context: &Context) {
    let actions = { slot().tick(clock()) };
    execute(actions, holding, context);
}

/// Open the system picker and wait for the user. Nothing is captured before it returns an item, and a stop
/// that arrives while it is open is the user's cancel: the operation is abandoned, which is the closest a
/// Win32 app gets to dismissing a picker it does not own.
fn present(holding: &mut Holding, context: &Context) {
    let value = match pick(context) {
        Pick::Chosen(item) => match geometry_of(&item) {
            Some(extent) => {
                holding.item = Some(item);
                Report::Selected(extent)
            }
            None => Report::Failed,
        },
        // The picker completes with E_ABORT, or with a null item, when the user closes it without
        // choosing. That is their cancel, not our failure.
        Pick::Cancelled => Report::Cancelled,
        Pick::Failed => Report::Failed,
    };
    report(value, holding, context);
}

/// Build the pipeline for the picked geometry, then wait for a stop, a frame, a closed item, or the
/// watchdog. Every effect the lifecycle returns afterwards runs from this stack, so the capture thread
/// stays the only owner of the COM objects.
fn capture(holding: &mut Holding, context: &Context, output: geometry::Extent) {
    let Some(item) = holding.item.take() else {
        report(Report::Failed, holding, context);
        return;
    };
    let Some(pipeline) =
        Pipeline::build(item.clone(), output, &context.control, &context.destination)
    else {
        report(Report::Failed, holding, context);
        return;
    };
    holding.item = Some(item);
    holding.pipeline = Some(pipeline);
    // Capturing starts only once the pipeline is on this thread, so a frame can never arrive for a
    // session with nothing to serve it.
    let started = holding
        .pipeline
        .as_ref()
        .is_some_and(|pipeline| pipeline.session.StartCapture().is_ok());
    if !started {
        report(Report::Failed, holding, context);
        return;
    }
    report(Report::Started, holding, context);
    while !context.control.settled() {
        if let Some(pipeline) = holding.pipeline.as_mut() {
            pipeline.drain();
        }
        if context.control.settled() {
            break;
        }
        context.control.wait();
        let (requested, failed, frames) = context.control.take();
        if failed {
            report(Report::Failed, holding, context);
        } else if requested {
            stop_lifecycle(holding, context);
        } else if !frames {
            tick(holding, context);
        }
    }
}

/// Close the capture and hand the frames still in the pool to the muxer. The file stays open: only
/// [`Effect::FinalizeOutput`] may close it, because that is what makes it publishable.
fn stop_capture(holding: &mut Holding, context: &Context) {
    let closed = holding
        .pipeline
        .as_mut()
        .is_some_and(|pipeline| pipeline.close());
    report(Report::Stopped { error: !closed }, holding, context);
}

/// Flush and close the MP4. Success here is the only thing that lets the shared machine publish the file;
/// a failure leaves it in the pending directory for cleanup to remove.
fn finalize_output(holding: &mut Holding, context: &Context) {
    let ok = holding
        .pipeline
        .as_ref()
        .is_some_and(|pipeline| pipeline.finalize());
    report(Report::Encoded { ok }, holding, context);
}

/// Drop every COM object and forget this capture. A muxer that never got `FinalizeOutput` is dropped here,
/// which leaves an unusable file — and nothing publishes a file except a `FINALIZED` event, so that is the
/// intended outcome.
fn release(holding: &mut Holding, context: &Context) {
    drop(holding.pipeline.take());
    drop(holding.item.take());
    if running()
        .as_ref()
        .is_some_and(|control| Arc::ptr_eq(control, &context.control))
    {
        *running() = None;
    }
    let _ = unsafe { MFShutdown() };
    context.control.settle();
}

/// What the system picker did.
enum Pick {
    Chosen(GraphicsCaptureItem),
    /// The user closed it without choosing, or the host asked to stop while it was open.
    Cancelled,
    /// Activation, the host window, or the remote call failed.
    Failed,
}

/// Open the system picker against `parent` and wait for it.
///
/// `PickSingleItemAsync` is documented to require a thread with an active dispatcher queue — which `run`
/// created — and it is a remote async: the picker UI belongs to the system. A Win32 app must also tell it
/// which window hosts the call, or activation fails.
fn pick(context: &Context) -> Pick {
    let Ok(picker) = GraphicsCapturePicker::new() else {
        return Pick::Failed;
    };
    if context.parent.0 != 0 {
        // `cast` is a QueryInterface, which the projection makes safe; what needs care is the handle
        // below, which the system UI copies before returning.
        let Ok(initialize) = picker.cast::<IInitializeWithWindow>() else {
            return Pick::Failed;
        };
        let window = HWND(context.parent.0 as *mut core::ffi::c_void);
        if unsafe { initialize.Initialize(window) }.is_err() {
            return Pick::Failed;
        }
    }
    let Ok(operation) = picker.PickSingleItemAsync() else {
        return Pick::Failed;
    };
    loop {
        let Ok(status) = operation.Status() else {
            return Pick::Failed;
        };
        // The user closed the picker without choosing: `IAsyncInfo` reports Canceled, which is their
        // decision, not a failure of ours. Anything past Started that yields no results is a failure.
        if status.0 == ASYNC_CANCELED {
            return Pick::Cancelled;
        }
        if status.0 > ASYNC_STARTED {
            return match operation.GetResults() {
                Ok(item) => Pick::Chosen(item),
                Err(_) => Pick::Failed,
            };
        }
        if context.control.waiting_stop() {
            return Pick::Cancelled;
        }
        std::thread::sleep(TICK / 4);
    }
}

fn geometry_of(item: &GraphicsCaptureItem) -> Option<geometry::Extent> {
    let SizeInt32 { Width, Height } = item.Size().ok()?;
    // A negative or zero dimension from the compositor is not a geometry to encode.
    Some(geometry::Extent {
        width: u32::try_from(Width).ok()?,
        height: u32::try_from(Height).ok()?,
    })
}

fn abi(extent: geometry::Extent) -> Option<SizeInt32> {
    Some(SizeInt32 {
        Width: i32::try_from(extent.width).ok()?,
        Height: i32::try_from(extent.height).ok()?,
    })
}

/// The COM objects one capture owns, all of them created and destroyed on the capture thread.
struct Pipeline {
    item: GraphicsCaptureItem,
    context: ID3D11DeviceContext,
    staging: ID3D11Texture2D,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    writer: IMFSinkWriter,
    stream: u32,
    /// The surface size the pool was created with, the encoder's fixed output size, and the content of the
    /// last packed frame. The sampler is rebuilt only when the content changes, because the mapping from
    /// encoder pixels to surface pixels is otherwise the same arithmetic every frame.
    surface: geometry::Extent,
    output: geometry::Extent,
    content: geometry::Extent,
    sampler: convert::Sampler,
    frame: i64,
    closed: i64,
    pacer: geometry::Pacer,
    y: Vec<u8>,
    uv: Vec<u8>,
    /// A frame that could not be packed or submitted. It is not handed to the muxer, because a half-written
    /// stream is worse than a shorter one — but the close is told, so the recording fails instead of
    /// publishing something that pretends to be complete.
    broken: bool,
}

impl Pipeline {
    /// Create the device, the frame pool, the session and the muxer for a fixed output size. Nothing has
    /// been started when this returns, so a failure here cannot leave a half-open capture behind.
    fn build(
        item: GraphicsCaptureItem,
        output: geometry::Extent,
        control: &Arc<Control>,
        destination: &str,
    ) -> Option<Self> {
        let surface = geometry_of(&item)?;
        // The item may have changed between the pick and here. A different geometry now would mean an
        // encoder whose size no longer matches the frames, so it is refused rather than adjusted.
        (geometry::output(surface)? == output).then_some(())?;
        let (device, context) = device()?;
        let dxgi: IDXGIDevice = device.cast().ok()?;
        // The frame pool wants the WinRT device wrapper. The `windows` crate does not project the
        // `Windows.Graphics.Direct3D11` activation class, so the documented DXGI bridge is used instead:
        // `CreateDirect3D11DeviceFromDXGIDevice`, then a query for `IDirect3DDevice`.
        let wrapper = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }.ok()?;
        let winrt: IDirect3DDevice = wrapper.cast().ok()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            geometry::BUFFERS,
            abi(surface)?,
        )
        .ok()?;
        let session = pool.CreateCaptureSession(&item).ok()?;
        let staging = staging(&device, surface)?;
        let (writer, stream) = muxer(destination, output)?;
        let frames = Arc::clone(control);
        // A free-threaded pool raises `FrameArrived` on its own worker thread. That thread does not touch
        // the pool, the device or the muxer: it only wakes the thread that owns them, which is what keeps
        // every interface single-threaded.
        let frame = pool
            .FrameArrived(TypedEventHandler::new(move |_pool, _args| {
                frames.arrived();
                Ok(())
            }))
            .ok()?;
        let closed = {
            let control = Arc::clone(control);
            item.Closed(TypedEventHandler::new(move |_item, _args| {
                control.failed();
                Ok(())
            }))
            .ok()?
        };
        let plane = output.width as usize * output.height as usize;
        Some(Self {
            item,
            context,
            staging,
            pool,
            session,
            writer,
            stream,
            surface,
            output,
            content: surface,
            sampler: convert::Sampler::new(surface, output),
            frame,
            closed,
            pacer: geometry::Pacer::default(),
            y: vec![0; plane],
            uv: vec![0; plane / 2],
            broken: false,
        })
    }

    /// Take every frame the pool holds and pack the ones the rate cap keeps. Frames the compositor queued
    /// before the close still belong in the file, so this runs on the way out as well as on a wake-up.
    fn drain(&mut self) {
        loop {
            // `TryGetNextFrame` reports "nothing available" as an error: the WinRT method returns S_OK with
            // a null out parameter, and the projection turns a null interface into one.
            let Ok(frame) = self.pool.TryGetNextFrame() else {
                return;
            };
            self.encode(&frame);
        }
    }

    /// Copy one frame out of the capture surface and hand it to the muxer. Anything that cannot be proven
    /// about a frame makes it skipped, never partially written: a short sample would corrupt every frame
    /// after it.
    fn encode(&mut self, frame: &Direct3D11CaptureFrame) {
        let Some(content) = frame
            .ContentSize()
            .ok()
            .and_then(|SizeInt32 { Width, Height }| {
                Some(geometry::Extent {
                    width: u32::try_from(Width).ok()?,
                    height: u32::try_from(Height).ok()?,
                })
            })
            .and_then(|content| geometry::content(content, self.surface))
        else {
            return;
        };
        // `SystemRelativeTime` is a WinRT TimeSpan, whose unit is already 100 ns — the same unit Media
        // Foundation timestamps are in — so it goes to the muxer unchanged. Scaling it by a QPC frequency
        // would stretch or shrink the recording's length instead.
        let Some(time) = frame.SystemRelativeTime().ok().map(|span| span.Duration) else {
            return;
        };
        if !self.pacer.accept(time) {
            return;
        }
        // The surface is always the size the pool was created with; the content can be smaller while a
        // window is being resized, and the rest of the surface is undefined data.
        if content != self.content {
            self.content = content;
            self.sampler = convert::Sampler::new(content, self.output);
        }
        let access = frame
            .Surface()
            .and_then(|surface| surface.cast::<IDirect3DDxgiInterfaceAccess>());
        let Ok(access) = access else {
            return;
        };
        // SAFETY: `GetInterface` asks the live wrapper for the DXGI texture it wraps, which is the
        // documented way to reach a capture frame's pixels, and the texture stays valid while the frame is
        // checked out — that is, while `frame` is alive below.
        let texture: ID3D11Texture2D = match unsafe { access.GetInterface() } {
            Ok(texture) => texture,
            Err(_) => return,
        };
        // SAFETY: both textures belong to this device, and a staging texture of the surface size cannot be
        // smaller than the captured one.
        unsafe { self.context.CopyResource(&self.staging, &texture) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the texture belongs to this context, and the subresource filled in below is writable here
        // until the Unmap.
        let mapping = unsafe {
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        };
        if mapping.is_err() {
            return;
        }
        let pitch = mapped.RowPitch as usize;
        let bytes = content.width as usize * 4 + (content.height as usize - 1) * pitch;
        let packed = {
            // SAFETY: the mapped region is readable until the Unmap below, and `bytes` covers exactly the
            // content rows at the stride D3D11 reported — the last row's pixels, without its padding.
            let source = unsafe { slice::from_raw_parts(mapped.pData.cast::<u8>(), bytes) };
            self.sampler.pack(source, pitch, &mut self.y, &mut self.uv)
        };
        // SAFETY: the texture is this context's and was mapped by this thread above.
        unsafe { self.context.Unmap(&self.staging, 0) };
        if !packed || !self.submit(time) {
            self.broken = true;
        }
    }

    /// Hand one packed frame to the muxer with the time the compositor gave it.
    fn submit(&self, time: i64) -> bool {
        let length = self.y.len() + self.uv.len();
        let Ok(sample) = (unsafe { MFCreateSample() }) else {
            return false;
        };
        let Ok(buffer) = (unsafe { MFCreateMemoryBuffer(length as u32) }) else {
            return false;
        };
        let mut pointer: *mut u8 = core::ptr::null_mut();
        let mut capacity = 0u32;
        // SAFETY: Lock writes the pointer and the length of the region it reserved, both writable here, and
        // that region stays valid until the Unlock below.
        if unsafe { buffer.Lock(&mut pointer, Some(&mut capacity), None) }.is_err() {
            return false;
        }
        let filled = capacity as usize >= length;
        if filled {
            // SAFETY: `pointer` owns `capacity` writable bytes and `length` is no more than that. NV12 is
            // the Y plane followed by interleaved UV, one row of UV per two rows of luma, which is exactly
            // how the packer filled the two buffers.
            unsafe {
                let destination = slice::from_raw_parts_mut(pointer, length);
                destination[..self.y.len()].copy_from_slice(&self.y);
                destination[self.y.len()..].copy_from_slice(&self.uv);
            }
        }
        // SAFETY: the buffer is locked by this thread and is not used again until it is unlocked.
        let _ = unsafe { buffer.Unlock() };
        if !filled {
            return false;
        }
        // SAFETY: length is within the reserved capacity, and the buffer is no longer locked.
        unsafe { buffer.SetCurrentLength(length as u32) }.is_ok()
            // SAFETY: sample, buffer and writer are live objects of this capture, and the writer was started
            // with `BeginWriting` when it was built.
            && unsafe { sample.AddBuffer(&buffer) }.is_ok()
            && unsafe { sample.SetSampleTime(time) }.is_ok()
            && unsafe { sample.SetSampleDuration(geometry::SAMPLE_DURATION) }.is_ok()
            && unsafe { self.writer.WriteSample(self.stream, &sample) }.is_ok()
    }

    /// Unsubscribe and close the session, then take the frames the compositor queued before the close.
    fn close(&mut self) -> bool {
        let frames = self.pool.RemoveFrameArrived(self.frame).is_ok();
        let item = self.item.RemoveClosed(self.closed).is_ok();
        let session = self.session.Close().is_ok();
        self.drain();
        frames && item && session && !self.broken
    }

    /// Close the MP4. Only after this returns true is the file complete enough to publish.
    fn finalize(&self) -> bool {
        unsafe { self.writer.Finalize() }.is_ok()
    }
}

/// A D3D11 device with BGRA support at feature level 11.0 or better.
///
/// Windows Graphics Capture hands frames to a 11.0 device; a lower feature level, or a software device,
/// would produce no frames at all, so it is refused here rather than discovered as an empty recording.
fn device() -> Option<(ID3D11Device, ID3D11DeviceContext)> {
    let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
    let mut created: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let mut level = D3D_FEATURE_LEVEL::default();
    // SAFETY: every output pointer is writable for the duration of the call, and the device and context it
    // fills in are owned by the returned values from that point on.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut created),
            Some(&mut level),
            Some(&mut context),
        )
    }
    .ok()?;
    if level.0 < D3D_FEATURE_LEVEL_11_0.0 {
        return None;
    }
    Some((created?, context?))
}

/// The CPU-readable texture a frame is copied into before it is mapped.
fn staging(device: &ID3D11Device, surface: geometry::Extent) -> Option<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: surface.width,
        Height: surface.height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut texture: Option<ID3D11Texture2D> = None;
    // SAFETY: the description is read for the duration of the call and the texture it produces is owned by
    // the returned value.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }.ok()?;
    texture
}

/// The H.264 MP4 muxer, configured for one video stream at the picked output size.
///
/// The input type is stated as NV12 rather than queried: the Microsoft H.264 encoder MFT takes NV12, and
/// `IMFSinkWriter::GetStreamMediaType` is not projected by the `windows` crate, so there is nothing to ask
/// it. Added streams are selected by default, which is why there is no `SetStreamSelection` call either —
/// that method is not part of the projected interface. A muxer that refuses NV12 fails here, and the
/// lifecycle reports that as `FAILED`.
fn muxer(destination: &str, output: geometry::Extent) -> Option<(IMFSinkWriter, u32)> {
    let wide: Vec<u16> = destination
        .encode_utf16()
        .chain(core::iter::once(0))
        .collect();
    let mut attributes: Option<IMFAttributes> = None;
    // SAFETY: the attribute store is filled in by the call and owned by the returned value, and the URL is a
    // NUL-terminated wide string that outlives every call below — the only use Media Foundation makes of it.
    unsafe { MFCreateAttributes(&mut attributes, 1) }.ok()?;
    let attributes = attributes?;
    unsafe { attributes.SetUINT32(&MF_LOW_LATENCY, 1) }.ok()?;
    let writer =
        unsafe { MFCreateSinkWriterFromURL(PCWSTR(wide.as_ptr()), None, Some(&attributes)) }
            .ok()?;
    let size = packed(output.width, output.height);
    let rate = packed(geometry::FRAME_RATE, 1);
    let target = unsafe { MFCreateMediaType() }.ok()?;
    // SAFETY: every key and value is a live GUID or an integer the attribute store takes by value, and the
    // media type is owned by this scope until the writer is configured.
    unsafe {
        target
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .and(target.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264))
            .and(target.SetUINT64(&MF_MT_FRAME_SIZE, size))
            .and(target.SetUINT64(&MF_MT_FRAME_RATE, rate))
            .and(target.SetUINT32(&MF_MT_AVG_BITRATE, geometry::bitrate(output)))
            .and(target.SetUINT32(&MF_MT_INTERLACE_MODE, PROGRESSIVE))
            .and(target.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1))
    }
    .ok()?;
    let stream = unsafe { writer.AddStream(&target) }.ok()?;
    let input = unsafe { MFCreateMediaType() }.ok()?;
    unsafe {
        input
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .and(input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12))
            .and(input.SetUINT64(&MF_MT_FRAME_SIZE, size))
            .and(input.SetUINT64(&MF_MT_FRAME_RATE, rate))
            // The packer writes one byte per pixel with no padding, and a positive stride means the rows run
            // top-down, which is how a capture surface is mapped.
            .and(input.SetUINT32(&MF_MT_DEFAULT_STRIDE, output.width))
            .and(input.SetUINT32(&MF_MT_INTERLACE_MODE, PROGRESSIVE))
            .and(input.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1))
            .and(writer.SetInputMediaType(stream, &input, None::<&IMFAttributes>))
            .and(writer.BeginWriting())
    }
    .ok()?;
    Some((writer, stream))
}

/// A size or a frame rate as the two u32 halves Media Foundation packs into one u64.
fn packed(width: u32, height: u32) -> u64 {
    (u64::from(width) << 32) | u64::from(height)
}
