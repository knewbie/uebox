//! The size, pacing and timing rules of a Windows capture, away from every COM call.
//!
//! These are the numbers `native/macos/capture.m` already applies at pick time: refuse degenerate
//! content, cap at 2560x1440, round down to even edges, never below 2, 30 fps, three buffers. Screen
//! CaptureKit is told the interval and the queue depth and the OS honours them; Windows Graphics
//! Capture has no equivalent below Windows 11 23H2, so the cap and the accounting live here. Keeping
//! them in plain integer arithmetic is what makes the Windows pipeline testable without a display, a
//! GPU, or a capture session.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Extent {
    pub(super) width: u32,
    pub(super) height: u32,
}

pub(super) const MIN_EDGE: u32 = 2;
pub(super) const MAX_WIDTH: u32 = 2560;
pub(super) const MAX_HEIGHT: u32 = 1440;
pub(super) const FRAME_RATE: u32 = 30;
/// The macOS side sets `queueDepth = 3`. Three buffers absorb one slow encode without letting a
/// stalled encoder pile up GPU memory that the capture has to wait on.
pub(super) const BUFFERS: i32 = 3;
/// Media Foundation timestamps are 100 ns units; this is how many of them make a second.
pub(super) const UNITS_PER_SECOND: i64 = 10_000_000;
/// One frame at the capture rate, in the same units. 30 fps does not divide a second evenly, and the
/// remainder is deliberately not spread: the sample times carry the real QPC, the duration is fixed.
pub(super) const SAMPLE_DURATION: i64 = UNITS_PER_SECOND / FRAME_RATE as i64;

/// The encoder's output size for an item the user picked.
///
/// The same rule as the macOS side, in the same order: refuse anything below two pixels, shrink to
/// fit the cap, round *down* to even edges (H.264 subsampling needs it), then floor at two. Rounding
/// down rather than to nearest keeps the output inside the content instead of sampling past its edge.
pub(super) fn output(item: Extent) -> Option<Extent> {
    if item.width < MIN_EDGE || item.height < MIN_EDGE {
        return None;
    }
    // The shrink factor is `min(1, 2560/w, 1440/h)` applied to both edges, so each edge is bounded by
    // its own cap and by what the other axis's cap implies for it. Working that out in integers rather
    // than in floating point is what keeps the limiting edge exactly on the cap: multiplying a pick by
    // `2560.0 / w` comes back one ulp below 2560 often enough to cost two columns, and an output size
    // that depends on the rounding of a divide is one the encoder cannot rely on.
    let fit = |edge: u32, other: u32, own: u32, other_limit: u32| {
        let across = u64::from(edge) * u64::from(other_limit) / u64::from(other);
        let capped = u64::min(across, u64::from(own)) as u32;
        u32::min(edge, capped)
    };
    let even = |edge: u32| (edge & !1).max(MIN_EDGE);
    Some(Extent {
        width: even(fit(item.width, item.height, MAX_WIDTH, MAX_HEIGHT)),
        height: even(fit(item.height, item.width, MAX_HEIGHT, MAX_WIDTH)),
    })
}

/// The valid rectangle inside a frame's surface.
///
/// The surface is always the size the pool was created with; when the content is smaller the rest of
/// the surface is undefined data, and when it is larger the content is clipped. So a frame is only
/// usable if it reports a rectangle that fits inside the surface it was handed with. A frame that
/// lies about its own content is dropped, never fed to the encoder.
pub(super) fn content(frame: Extent, surface: Extent) -> Option<Extent> {
    if frame.width == 0
        || frame.height == 0
        || frame.width > surface.width
        || frame.height > surface.height
    {
        return None;
    }
    Some(frame)
}

/// Target bitrate for the H.264 stream, in bits per second.
///
/// Screen content is mostly flat colour, so 0.06 bits per pixel per frame is comfortably above what a
/// window or a document needs and well below what the same resolution would want for film. The clamp
/// keeps the value inside what the encoder accepts on a low-resolution pick.
pub(super) fn bitrate(output: Extent) -> u32 {
    const BITS_PER_PIXEL_FRAME: f64 = 0.06;
    let estimate =
        output.width as f64 * output.height as f64 * FRAME_RATE as f64 * BITS_PER_PIXEL_FRAME;
    estimate.clamp(1_000_000.0, 12_000_000.0) as u32
}

/// One frame per interval, keeping the first of a burst.
///
/// Streaming cannot look ahead, so "newest of a burst" is not reachable without buffering frames that
/// the pool has already checked back in. Out-of-order times are dropped rather than passed on: an
/// `IMFSample` whose time goes backwards makes the encoder fail the whole stream, and the failure
/// would land on a recording the user believes is fine.
pub(super) struct Pacer {
    next: i64,
    last: Option<i64>,
    kept: u64,
    dropped: u64,
}

impl Default for Pacer {
    fn default() -> Self {
        Self {
            next: i64::MIN,
            last: None,
            kept: 0,
            dropped: 0,
        }
    }
}

impl Pacer {
    /// `time` is a Media Foundation timestamp: 100 ns units, which is what both a capture frame's
    /// `SystemRelativeTime` and the muxer's sample times are already expressed in. The return says
    /// whether this frame goes to the encoder.
    pub(super) fn accept(&mut self, time: i64) -> bool {
        if self.last.is_some_and(|last| time <= last) || time < self.next {
            self.dropped += 1;
            return false;
        }
        self.last = Some(time);
        self.next = time.saturating_add(SAMPLE_DURATION);
        self.kept += 1;
        true
    }

    /// (encoded, dropped) frames so far. A capture that dropped every frame after the pick is a
    /// recording of nothing, and this is where that becomes visible.
    pub(super) fn counts(&self) -> (u64, u64) {
        (self.kept, self.dropped)
    }
}
