//! The capture session's own state machine, translated from `native/macos/capture.m`.
//!
//! The shared `state::Machine` above this module owns the product state — phase, token, publication,
//! cleanup. This module owns the *capture*, and it holds to the rules the Objective-C implementation
//! already holds to, because those rules are what keep a recording from ending twice, from publishing
//! an unfinished file, and from confusing the user's cancel with a failure:
//!
//! * one capture per process, and a callback carrying a retired token can never complete the session
//!   that replaced it — the same generation guard as `currentCapture != self`;
//! * the terminal event waits for both halves, the capture and the muxer, to settle in either order,
//!   and is emitted exactly once;
//! * `FINALIZED` is reported only after the muxer closed the file, because that event is what makes
//!   the shared machine hard-link and publish it;
//! * a stop before anything was captured is `CANCELLED`, after it is `FAILED`;
//! * a stop that never settles is declared `FAILED` by a watchdog instead of leaving the reservation
//!   held and the file unpublishable.
//!
//! Everything here is decisions and bookkeeping. The driver in `backend.rs` performs the COM calls and
//! reports what happened, which is what makes these transitions testable on any host.

use super::geometry::{self, Extent};

/// The event codes `state::Machine::event` already understands. They are not this module's invention:
/// 1 and 2 are progress, 3 to 7 are terminal, and 3 is the only one that publishes a file.
pub(super) const SELECTING: i32 = 1;
pub(super) const RECORDING: i32 = 2;
pub(super) const FINALIZED: i32 = 3;
pub(super) const CANCELLED: i32 = 4;
pub(super) const FAILED: i32 = 5;
pub(super) const UNSUPPORTED: i32 = 6;
pub(super) const BUSY: i32 = 7;

/// How long a stop may go unsettled before the capture is declared failed. The macOS side waits 15
/// seconds for ScreenCaptureKit to settle; a hung COM call deserves no more patience than that.
pub(super) const WATCHDOG: u64 = 15_000;

/// What the driver must do next, in the order the effects are listed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Effect {
    /// Open the system picker. Nothing is captured, and nothing is consented to, until a user picks
    /// something in it.
    Present,
    /// Build the frame pool, the capture session and the muxer for this output size, then start
    /// capturing and report [`Report::Started`].
    Capture(Extent),
    /// Close the capture and hand the frames still in the pool to the muxer. Never closes the file.
    StopCapture,
    /// Flush and close the MP4, then report [`Report::Encoded`]. The file is publishable only after
    /// this has returned success.
    FinalizeOutput,
    /// Release every resource. No further report may arrive for this session, and a muxer dropped
    /// without [`Effect::FinalizeOutput`] leaves a file that nothing will ever publish.
    Release,
    /// Report one event to the shared state machine.
    Emit(i32),
}

/// The transitions a driver has to perform, in order. An empty list means "nothing to do".
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Actions(Vec<Effect>);

impl Actions {
    fn one(effect: Effect) -> Self {
        Self(vec![effect])
    }

    fn push(&mut self, effect: Effect) {
        self.0.push(effect);
    }

    /// Whether the session has been retired, which is when the slot drops it.
    fn releases(&self) -> bool {
        self.0.contains(&Effect::Release)
    }

    /// Borrowed view, for a driver that must look at the decision without consuming it.
    pub(super) fn iter(&self) -> std::slice::Iter<'_, Effect> {
        self.0.iter()
    }
}

impl IntoIterator for Actions {
    type Item = Effect;
    type IntoIter = std::vec::IntoIter<Effect>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// What the driver observed, reported back so the lifecycle alone decides what it means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Report {
    /// The picker returned an item; the value is its size in pixels.
    Selected(Extent),
    /// Frames are arriving and the muxer is open.
    Started,
    /// The user dismissed the picker without choosing anything.
    Cancelled,
    /// The picker, the item or the capture failed on its own.
    Failed,
    /// The capture closed. `error` means it ended by itself rather than on request.
    Stopped { error: bool },
    /// The muxer closed. `ok` false means the MP4 is unusable.
    Encoded { ok: bool },
}

/// One capture, from the pick to the terminal event.
pub(super) struct Session {
    token: u64,
    /// Monotonic milliseconds of the last transition, injected by the slot so the watchdog is
    /// testable without a clock.
    now: u64,
    /// A stop was asked for; late notifications cannot undo it.
    stopping: bool,
    /// A capture session exists, so a stop has something to close.
    capturing: bool,
    /// Frames are flowing, which is what separates a cancel from a failure.
    started: bool,
    capture_settled: bool,
    output_settled: bool,
    terminal: Option<i32>,
    deadline: Option<u64>,
}

impl Session {
    fn new(token: u64, now: u64) -> Self {
        Self {
            token,
            now,
            stopping: false,
            capturing: false,
            started: false,
            capture_settled: false,
            output_settled: false,
            terminal: None,
            deadline: None,
        }
    }

    /// Announce the reservation, then ask the user. The order is the macOS order: the shared machine
    /// is already in `Selecting` from `begin`, and the picker is what the user sees.
    fn present(&mut self) -> Actions {
        let mut actions = Actions::one(Effect::Emit(SELECTING));
        actions.push(Effect::Present);
        actions
    }

    /// The user picked something. Geometry the encoder cannot take is a failure, not a silent retry:
    /// consent was given, and nothing of it may be recorded afterwards. A second selection is ignored,
    /// as the Objective-C `didUpdateWithFilter` handler ignores every notification that arrives after
    /// a stream exists.
    fn selected(&mut self, item: Extent) -> Actions {
        if self.capturing || self.stopping || self.terminal.is_some() {
            return Actions::default();
        }
        let Some(output) = geometry::output(item) else {
            return self.finish(FAILED);
        };
        self.capturing = true;
        Actions::one(Effect::Capture(output))
    }

    fn started(&mut self) -> Actions {
        if !self.capturing || self.started {
            return Actions::default();
        }
        self.started = true;
        // A stop that arrived while the pipeline was being built wins, exactly as on macOS: the user
        // is never told "recording" for something already on its way out.
        if self.stopping || self.terminal.is_some() {
            return Actions::default();
        }
        Actions::one(Effect::Emit(RECORDING))
    }

    /// The user, or shutdown, asked to stop.
    fn stop(&mut self) -> Actions {
        if self.stopping || self.terminal.is_some() {
            return Actions::default();
        }
        if !self.capturing {
            return self.finish(CANCELLED);
        }
        self.request_stop()
    }

    /// The capture is closed by the driver, which then reports [`Report::Stopped`].
    fn request_stop(&mut self) -> Actions {
        self.stopping = true;
        // The watchdog starts when the stop is issued rather than when the close returns, so a call
        // that never comes back cannot hold the reservation forever. It is armed once: a second stop
        // does not buy more time.
        if self.deadline.is_none() {
            self.deadline = Some(self.now.saturating_add(WATCHDOG));
        }
        Actions::one(Effect::StopCapture)
    }

    /// The capture closed and its frames were handed over.
    fn stopped(&mut self, error: bool) -> Actions {
        if self.capture_settled {
            return Actions::default();
        }
        self.capture_settled = true;
        if self.terminal.is_none() && (error || !self.started) {
            // A session that never carried a frame was not a recording that failed: it never started,
            // so the user's stop is a cancel.
            self.terminal = Some(if self.started { FAILED } else { CANCELLED });
        }
        if self.output_settled {
            return self.complete();
        }
        // The file is closed only now, so a publish can never see a half-written MP4.
        Actions::one(Effect::FinalizeOutput)
    }

    /// The muxer settled. Closing the file successfully is what finalizes the recording; a failure
    /// latched earlier is never overwritten by it.
    fn encoded(&mut self, ok: bool) -> Actions {
        if self.output_settled {
            return Actions::default();
        }
        self.output_settled = true;
        self.finish(if ok { FINALIZED } else { FAILED })
    }

    fn finish(&mut self, status: i32) -> Actions {
        if self.terminal.is_none() {
            self.terminal = Some(status);
        }
        if !self.capturing || self.capture_settled {
            self.complete()
        } else {
            self.request_stop()
        }
    }

    fn complete(&mut self) -> Actions {
        self.deadline = None;
        // Release first, then report: the macOS side clears `currentCapture` before invoking the
        // callback, so nothing can reach a session whose file is already being published. An
        // un-latched completion is a failure, never a success.
        let status = self.terminal.unwrap_or(FAILED);
        let mut actions = Actions::one(Effect::Release);
        actions.push(Effect::Emit(status));
        actions
    }

    /// Drives the watchdog. A stop that never settled is a failure: the file is not published, because
    /// only `FINALIZED` publishes it, and the workspace cleanup removes whatever the muxer left.
    fn tick(&mut self, now: u64) -> Actions {
        self.now = now;
        if self.terminal.is_some() || !self.deadline.is_some_and(|deadline| now >= deadline) {
            return Actions::default();
        }
        self.terminal = Some(FAILED);
        self.complete()
    }
}

/// The single capture slot a process gets, mirroring `static NSObject *currentCapture`.
#[derive(Default)]
pub(super) struct Slot {
    active: Option<Session>,
}

impl Slot {
    pub(super) fn busy(&self) -> bool {
        self.active.is_some()
    }

    /// The Windows counterpart of `uebox_capture_start`. Every refusal is decided before a picker is
    /// opened, so the user is never asked to consent to a recording this build cannot produce.
    ///
    /// `now` is a monotonic millisecond clock, injected so this is testable without one.
    pub(super) fn start(
        &mut self,
        now: u64,
        token: u64,
        audio: bool,
        supported: bool,
        path: bool,
    ) -> Actions {
        if self.busy() {
            return Actions::one(Effect::Emit(BUSY));
        }
        if !supported {
            return Actions::one(Effect::Emit(UNSUPPORTED));
        }
        if !path {
            return Actions::one(Effect::Emit(FAILED));
        }
        if audio {
            // Stage A records video only. Windows Graphics Capture carries no audio at all and the
            // loopback path that would supply it is a separate stage, so refusing here is what keeps
            // the checkbox honest instead of silently delivering a mute file.
            return Actions::one(Effect::Emit(UNSUPPORTED));
        }
        let mut session = Session::new(token, now);
        let actions = session.present();
        self.active = Some(session);
        actions
    }

    pub(super) fn stop(&mut self, now: u64, token: u64) -> Actions {
        let Some(session) = self
            .active
            .as_mut()
            .filter(|session| session.token == token)
        else {
            return Actions::default();
        };
        session.now = now;
        let actions = session.stop();
        self.retire(&actions);
        actions
    }

    /// A driver report. A token that is not the current session's is dropped: that is the generation
    /// guard which stops a replaced capture from completing the new one.
    pub(super) fn report(&mut self, now: u64, token: u64, report: Report) -> Actions {
        let Some(session) = self
            .active
            .as_mut()
            .filter(|session| session.token == token)
        else {
            return Actions::default();
        };
        session.now = now;
        let actions = match report {
            Report::Selected(item) => session.selected(item),
            Report::Started => session.started(),
            Report::Cancelled => session.finish(CANCELLED),
            Report::Failed => session.finish(FAILED),
            Report::Stopped { error } => session.stopped(error),
            Report::Encoded { ok } => session.encoded(ok),
        };
        self.retire(&actions);
        actions
    }

    /// Advances the watchdog. Called from the driver's own wait rather than from a timer, so a session
    /// with no armed stop is a no-op.
    pub(super) fn tick(&mut self, now: u64) -> Actions {
        let Some(session) = self.active.as_mut() else {
            return Actions::default();
        };
        let actions = session.tick(now);
        self.retire(&actions);
        actions
    }

    fn retire(&mut self, actions: &Actions) {
        if actions.releases() {
            self.active = None;
        }
    }
}
