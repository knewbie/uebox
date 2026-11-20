//! Windows screen capture: Windows Graphics Capture into an H.264 MP4 through Media Foundation.
//!
//! This module is the Windows counterpart of [`super::native`], and it keeps the same shape on
//! purpose: it compiles on every host, refuses everywhere it cannot run, and the shared state machine
//! in [`super::state`] is unchanged. Three rules hold throughout.
//!
//! * **The user picks, always.** The only route to a capture is the system picker, which is the
//!   Windows equivalent of the ScreenCaptureKit content-sharing picker the macOS build presents. The
//!   shortcuts that skip it — `GraphicsCaptureItem::CreateForWindow`, `CreateForMonitor`, and the
//!   borderless access request — are not used anywhere here, so there is no path that records
//!   something nobody chose.
//! * **The system says it is recording.** Windows draws the yellow border around a captured item by
//!   itself. `IsBorderRequired` is never set to false, because turning it off is exactly the automatic
//!   consent this product refuses to ask for.
//! * **Nothing is captured before a refusal has been decided.** Capability, destination, and the
//!   audio request are judged in [`lifecycle`] before the picker opens, so a user is never asked to
//!   consent to a recording the build cannot produce.
//!
//! Stage A records video only. Windows Graphics Capture carries no audio at all, so the system-audio
//! path is a separate stage (WASAPI loopback, AAC, a second muxer stream) and `audio: true` is refused
//! rather than silently answered with a mute file.
//!
//! What is where: [`geometry`] and [`convert`] are integer arithmetic with no COM in sight and are
//! tested on every host; [`lifecycle`] is the capture state machine translated from
//! `native/macos/capture.m`, also tested on every host; `backend` (Windows only) is the driver that
//! performs the Win32, Direct3D 11, WinRT and Media Foundation calls and reports what happened.

mod convert;
mod geometry;
mod lifecycle;

#[cfg(test)]
mod tests;

#[cfg(windows)]
mod backend;

use std::ffi::{c_int, CStr};

pub(super) type Event = extern "C" fn(u64, c_int);

/// The window the system picker is presented against. Only a Windows build reads the value, and it is
/// never interpreted: it goes to `InitializeWithWindow` and nowhere else.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Parent(usize);

impl Parent {
    pub(super) const NONE: Self = Self(0);

    #[cfg(windows)]
    pub(super) const fn from_raw(handle: usize) -> Self {
        Self(handle)
    }
}

/// Only a capability check, never a permission request — the counterpart of the macOS build's
/// `@available` gate. Microsoft's own judgement for "can this device capture screens" is
/// `GraphicsCaptureSession::IsSupported`, which asks the user for nothing; it is cached because the
/// status command polls it and the answer cannot change while the process runs.
pub(super) fn available() -> bool {
    #[cfg(windows)]
    {
        backend::supported()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Start one capture.
///
/// Never calls back synchronously: the caller holds the shared state machine's lock across this call,
/// and that is the same lock the event callback takes. Everything happens on the capture's own thread,
/// as it does on the macOS host where the work is dispatched to the main queue.
pub(super) fn start(_path: &CStr, _audio: bool, _token: u64, _event: Event, _parent: Parent) {
    #[cfg(windows)]
    backend::start(_path, _audio, _token, _event, _parent);
}

/// Ask the current capture to stop. Idempotent, and a token that is no longer current does nothing.
pub(super) fn stop(_token: u64) {
    #[cfg(windows)]
    backend::stop(_token);
}
