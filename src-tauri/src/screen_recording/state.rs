//! Synthetic, platform-independent recording state machine.
use super::files::Workspace;
pub(super) use super::files::{PUBLISH_FAILED, UNAVAILABLE};

pub(super) const FORBIDDEN: &str = "screen_recording_forbidden";
pub(super) const OPT_IN_REQUIRED: &str = "screen_recording_opt_in_required";
pub(super) const UNSUPPORTED: &str = "screen_recording_unsupported";
pub(super) const BUSY: &str = "screen_recording_busy";
pub(super) const INVALID_TOKEN: &str = "screen_recording_invalid_token";
const FAILED: &str = "screen_recording_failed";
const CLEANUP_FAILED: &str = "screen_recording_cleanup_failed";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Phase {
    #[default]
    Idle,
    Selecting,
    Recording,
    Stopping,
    Completed,
    Cancelled,
    Failed,
    Unsupported,
    Busy,
}

impl Phase {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Selecting => "selecting",
            Self::Recording => "recording",
            Self::Stopping => "stopping",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Unsupported => "unsupported",
            Self::Busy => "busy",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Snapshot {
    pub token: Option<u64>,
    pub phase: Phase,
    pub audio: bool,
    pub file_name: Option<String>,
    pub error: Option<&'static str>,
    pub cleanup_pending: bool,
}

#[derive(Default)]
pub(super) struct Machine {
    last_token: u64,
    active: Option<Workspace>,
    status: Snapshot,
}

impl Machine {
    pub(super) fn busy(&self) -> bool {
        self.active.is_some()
    }

    pub(super) fn snapshot(&self) -> Snapshot {
        self.status.clone()
    }

    pub(super) fn begin(&mut self, work: Workspace, audio: bool) -> Result<u64, &'static str> {
        if self.busy() {
            return Err(BUSY);
        }
        // Never wrap or reuse a token, including after completed/cancelled tasks.
        let token = self.last_token.checked_add(1).ok_or(UNAVAILABLE)?;
        self.last_token = token;
        self.active = Some(work);
        self.status = Snapshot {
            token: Some(token),
            phase: Phase::Selecting,
            audio,
            ..Snapshot::default()
        };
        Ok(token)
    }

    /// True means dispatch stop once. The active reservation survives this call.
    pub(super) fn stop(&mut self, token: u64) -> Result<bool, &'static str> {
        if token == 0 || self.status.token != Some(token) {
            return Err(INVALID_TOKEN);
        }
        if !self.busy() || self.status.phase == Phase::Stopping {
            return Ok(false);
        }
        self.status.phase = Phase::Stopping;
        Ok(true)
    }

    pub(super) fn event(&mut self, token: u64, event: i32) {
        if self.status.token != Some(token) || !self.busy() {
            return;
        }
        match event {
            1 => {} // Selecting never reverses Recording or Stopping.
            2 if self.status.phase == Phase::Selecting => self.status.phase = Phase::Recording,
            3..=7 => self.finish(event),
            _ => {} // Unknown or late start notifications retain the reservation.
        }
    }

    fn finish(&mut self, event: i32) {
        let Some(work) = self.active.take() else {
            return;
        };
        let (phase, error, file_name) = match event {
            3 => match work.publish() {
                Ok(()) => (Phase::Completed, None, Some(work.final_name().to_owned())),
                Err(_) => (Phase::Failed, Some(PUBLISH_FAILED), None),
            },
            4 => (Phase::Cancelled, None, None),
            5 => (Phase::Failed, Some(FAILED), None),
            6 => (Phase::Unsupported, Some(UNSUPPORTED), None),
            7 => (Phase::Busy, Some(BUSY), None),
            _ => unreachable!("terminal events are filtered before finish"),
        };
        let cleanup_pending = !work.cleanup();
        self.status = Snapshot {
            phase,
            error: error.or(if cleanup_pending {
                Some(CLEANUP_FAILED)
            } else {
                None
            }),
            file_name,
            cleanup_pending,
            ..self.status.clone()
        };
    }
}

pub(super) fn authorize(label: &str) -> Result<(), &'static str> {
    if label == "main" {
        Ok(())
    } else {
        Err(FORBIDDEN)
    }
}

pub(super) fn check_start(
    label: &str,
    opt_in: bool,
    supported: bool,
    busy: bool,
) -> Result<(), &'static str> {
    authorize(label)?;
    if !opt_in {
        return Err(OPT_IN_REQUIRED);
    }
    if !supported {
        return Err(UNSUPPORTED);
    }
    if busy {
        return Err(BUSY);
    }
    Ok(())
}

pub(super) fn parse_token(value: &str) -> Result<u64, &'static str> {
    if value.is_empty() || value.len() > 20 || value.starts_with('0') {
        return Err(INVALID_TOKEN);
    }
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(INVALID_TOKEN);
    }
    value.parse().map_err(|_| INVALID_TOKEN)
}
