use super::*;
use lifecycle::{
    Actions, Effect, Report, Slot, BUSY, CANCELLED, FAILED, FINALIZED, RECORDING, SELECTING,
    UNSUPPORTED, WATCHDOG,
};

fn effects(actions: Actions) -> Vec<Effect> {
    actions.into_iter().collect()
}

fn extent(width: u32, height: u32) -> geometry::Extent {
    geometry::Extent { width, height }
}

/// A monotonic clock for the lifecycle, in the same units the driver will pass.
const ZERO: u64 = 0;

#[test]
fn picked_geometry_is_capped_even_and_refused_when_degenerate() {
    // A monitor at or below the cap is passed through unchanged, so a 1:1 pick stays bit-exact.
    assert_eq!(
        geometry::output(extent(1920, 1080)),
        Some(extent(1920, 1080))
    );
    assert_eq!(
        geometry::output(extent(2560, 1440)),
        Some(extent(2560, 1440))
    );
    // A 4K pick shrinks on the wider axis and lands exactly on the cap.
    assert_eq!(
        geometry::output(extent(3840, 2160)),
        Some(extent(2560, 1440))
    );
    // Odd edges round down: H.264 with 4:2:0 subsampling cannot take an odd stride, and rounding
    // down stays inside the content instead of sampling past its edge.
    assert_eq!(
        geometry::output(extent(3839, 2159)),
        Some(extent(2560, 1438))
    );
    // The height is what hits its cap here, so the width keeps the aspect rather than the limit.
    assert_eq!(
        geometry::output(extent(4096, 2160)),
        Some(extent(2560, 1350))
    );
    assert_eq!(geometry::output(extent(3, 3)), Some(extent(2, 2)));
    assert_eq!(geometry::output(extent(100, 200)), Some(extent(100, 200)));
    for item in [
        extent(0, 0),
        extent(1, 1000),
        extent(1000, 1),
        extent(0, 1080),
    ] {
        assert_eq!(geometry::output(item), None, "{item:?}");
    }
}

#[test]
fn a_frame_may_only_claim_content_that_fits_its_surface() {
    let surface = extent(1920, 1080);
    assert_eq!(
        geometry::content(extent(1920, 1080), surface),
        Some(surface)
    );
    // A window that shrank mid-capture reports less than the surface; the rest is undefined data.
    assert_eq!(
        geometry::content(extent(800, 600), surface),
        Some(extent(800, 600))
    );
    for frame in [
        extent(0, 1),
        extent(1, 0),
        extent(1921, 1080),
        extent(1920, 1081),
    ] {
        assert_eq!(geometry::content(frame, surface), None, "{frame:?}");
    }
}

#[test]
fn capture_timestamps_need_no_conversion() {
    // A capture frame carries a WinRT TimeSpan, whose unit is 100 ns — the same unit the muxer's sample
    // times are in — so the pacer and the muxer share one clock and nothing scales it. At the usual 10 MHz
    // QPC a frequency division would leave the value unchanged by luck, and would distort the length of a
    // recording on any other counter.
    assert_eq!(geometry::UNITS_PER_SECOND, 10_000_000);
    // One frame period at the capture rate, and 30 of them still short of a second: the remainder is
    // deliberately not spread, because the sample times carry the real clock and the duration is fixed.
    assert_eq!(geometry::SAMPLE_DURATION, 333_333);
    assert_eq!(
        geometry::SAMPLE_DURATION * i64::from(geometry::FRAME_RATE),
        9_999_990
    );
}

#[test]
fn the_pacer_keeps_one_frame_per_interval_and_the_first_of_a_burst() {
    let interval = geometry::SAMPLE_DURATION;
    assert_eq!(interval, 333_333);
    let mut pacer = geometry::Pacer::default();
    assert!(pacer.accept(0));
    // One unit short of the interval is still the same frame period.
    assert!(!pacer.accept(interval - 1));
    assert!(pacer.accept(interval));
    // A burst that arrives inside one period keeps the first frame: streaming cannot look ahead, and
    // the pool has already checked the later ones back in by the time a decision could be made.
    assert!(!pacer.accept(interval + 1));
    assert!(!pacer.accept(interval + 100_000));
    assert!(pacer.accept(interval * 2));
    // A time that does not advance is refused, because a sample whose time goes backwards fails the
    // whole encoder stream.
    assert!(!pacer.accept(interval * 2));
    assert!(!pacer.accept(interval));
    assert_eq!(pacer.counts(), (3, 5));
}

#[test]
fn the_bitrate_stays_inside_what_the_encoder_accepts() {
    assert_eq!(geometry::bitrate(extent(2560, 1440)), 6_635_520);
    assert_eq!(geometry::bitrate(extent(1920, 1080)), 3_732_480);
    // A small pick is lifted to the floor rather than handed to the encoder at a rate it cannot use.
    assert_eq!(geometry::bitrate(extent(640, 480)), 1_000_000);
    assert_eq!(geometry::bitrate(extent(2, 2)), 1_000_000);
}

#[test]
fn packing_is_exact_when_the_output_matches_the_content() {
    // BGRA, one row of four bytes per pixel, no padding.
    let source: [u8; 16] = [
        255, 0, 0, 255, // blue
        0, 0, 255, 255, // red
        0, 255, 0, 255, // green
        255, 255, 255, 255, // white
    ];
    let sampler = convert::Sampler::new(extent(2, 2), extent(2, 2));
    let mut y = vec![0u8; 4];
    let mut uv = vec![0u8; 2];
    assert!(sampler.pack(&source, 8, &mut y, &mut uv));
    // BT.601 limited range, integer form: luma 16..=235, chroma centred on 128.
    assert_eq!(y, vec![41, 82, 144, 235]);
    // The single 2x2 block averages to a mid grey, so the chroma is the neutral 128.
    assert_eq!(uv, vec![128, 128]);
}

#[test]
fn known_colours_pack_to_bt601_limited_range_values() {
    let cases: [([u8; 4], u8, u8, u8); 6] = [
        // B, G, R, alpha -> luma, Cb, Cr
        ([0, 0, 0, 255], 16, 128, 128),        // black
        ([255, 255, 255, 255], 235, 128, 128), // white
        ([0, 0, 255, 255], 82, 90, 240),       // red
        ([0, 255, 0, 255], 144, 54, 34),       // green
        ([255, 0, 0, 255], 41, 240, 110),      // blue
        ([128, 128, 128, 255], 126, 128, 128), // mid grey
    ];
    for (bgra, luma, blue, red) in cases {
        let sampler = convert::Sampler::new(extent(2, 2), extent(2, 2));
        let source = [bgra; 4].concat();
        let mut y = vec![0u8; 4];
        let mut uv = vec![0u8; 2];
        assert!(sampler.pack(&source, 8, &mut y, &mut uv), "{bgra:?}");
        assert_eq!(y, vec![luma; 4], "{bgra:?} luma");
        assert_eq!(uv, vec![blue, red], "{bgra:?} chroma");
    }
}

#[test]
fn a_downscale_averages_luma_and_chroma_over_the_block() {
    // Four 2x2 quadrants: red, green, blue, white. A 4x4 to 2x2 shrink puts each output pixel on one
    // quadrant, so bilinear degenerates to the box filter and the chroma block spans all four.
    let quadrant = |b: u8, g: u8, r: u8| [b, g, r, 255u8];
    let mut source = Vec::new();
    for row in 0..4 {
        for column in 0..4 {
            let pixel = match (row < 2, column < 2) {
                (true, true) => quadrant(0, 0, 255),
                (true, false) => quadrant(0, 255, 0),
                (false, true) => quadrant(255, 0, 0),
                (false, false) => quadrant(255, 255, 255),
            };
            source.extend_from_slice(&pixel);
        }
    }
    let sampler = convert::Sampler::new(extent(4, 4), extent(2, 2));
    let mut y = vec![0u8; 4];
    let mut uv = vec![0u8; 2];
    assert!(sampler.pack(&source, 16, &mut y, &mut uv));
    assert_eq!(y, vec![82, 144, 41, 235]);
    assert_eq!(uv, vec![128, 128]);
}

#[test]
fn an_upscale_reads_no_neighbour_outside_the_mapped_region() {
    // Content smaller than the encoder's fixed size happens when the picked window shrinks
    // mid-recording. The taps at the last column and row have no neighbour to read, and a zero weight
    // must mean the neighbour is not touched at all — not read one pixel past the surface, which on
    // the last row would be outside the mapped region.
    let source = [[255u8, 255, 255, 255]; 4];
    let sampler = convert::Sampler::new(extent(2, 2), extent(4, 4));
    let mut y = vec![0u8; 16];
    let mut uv = vec![0u8; 8];
    assert!(sampler.pack(&source.concat(), 8, &mut y, &mut uv));
    assert_eq!(y, vec![235; 16]);
    assert_eq!(uv, vec![128; 8]);
}

#[test]
fn a_padded_pitch_reads_each_row_at_its_own_stride() {
    // A mapped surface carries row padding; the second row must be found at the pitch, not at the
    // content width.
    let red = [0u8, 0, 255, 255];
    let green = [0u8, 255, 0, 255];
    let blue = [255u8, 0, 0, 255];
    let mut source = Vec::new();
    source.extend_from_slice(&red);
    source.extend_from_slice(&red);
    source.extend_from_slice(&[0u8; 8]);
    source.extend_from_slice(&green);
    source.extend_from_slice(&blue);
    let sampler = convert::Sampler::new(extent(2, 2), extent(2, 2));
    let mut y = vec![0u8; 4];
    let mut uv = vec![0u8; 2];
    assert!(sampler.pack(&source, 16, &mut y, &mut uv));
    assert_eq!(y, vec![82, 82, 144, 41]);
    // Chroma over red, red, green, blue: 127, 63, 63 in red, green and blue.
    assert_eq!(uv, vec![119, 156]);
}

#[test]
fn packing_refuses_buffers_geometry_or_pitch_that_disagree() {
    let sampler = convert::Sampler::new(extent(2, 2), extent(2, 2));
    let source = [0u8; 16];
    let mut y = vec![0u8; 4];
    let mut uv = vec![0u8; 2];
    // A pitch shorter than a row is a contradiction, not a capture condition.
    assert!(!sampler.pack(&source, 7, &mut y, &mut uv));
    // A mapped region that does not cover the last row.
    assert!(!sampler.pack(&source[..11], 8, &mut y, &mut uv));
    // Planes too small for the frame.
    assert!(!sampler.pack(&source, 8, &mut y[..3], &mut uv));
    assert!(!sampler.pack(&source, 8, &mut y, &mut uv[..1]));
    // An odd output stride can never be produced by geometry::output, so it is refused outright.
    let odd = convert::Sampler::new(extent(3, 3), extent(3, 3));
    let mut y = vec![0u8; 9];
    let mut uv = vec![0u8; 5];
    assert!(!odd.pack(&[0u8; 36], 12, &mut y, &mut uv));
}

#[test]
fn every_refusal_is_decided_before_the_picker_opens() {
    // The order is the macOS order: busy first, then capability, then the destination, and only then
    // the thing this stage cannot do at all.
    let mut slot = Slot::default();
    assert_eq!(
        effects(slot.start(ZERO, 1, false, false, true)),
        vec![Effect::Emit(UNSUPPORTED)]
    );
    assert!(!slot.busy());
    assert_eq!(
        effects(slot.start(ZERO, 2, false, true, false)),
        vec![Effect::Emit(FAILED)]
    );
    assert_eq!(
        effects(slot.start(ZERO, 3, true, true, true)),
        vec![Effect::Emit(UNSUPPORTED)]
    );
    assert!(!slot.busy());
    assert_eq!(
        effects(slot.start(ZERO, 4, false, true, true)),
        vec![Effect::Emit(SELECTING), Effect::Present]
    );
    assert!(slot.busy());
    assert_eq!(
        effects(slot.start(ZERO, 5, false, true, true)),
        vec![Effect::Emit(BUSY)]
    );
}

#[test]
fn system_audio_is_refused_in_this_stage() {
    // Windows Graphics Capture carries no audio. The checkbox must not be answered with a silent
    // video-only file, and the user must not be asked to consent to something that cannot be recorded.
    let mut slot = Slot::default();
    assert_eq!(
        effects(slot.start(ZERO, 1, true, true, true)),
        vec![Effect::Emit(UNSUPPORTED)]
    );
    assert!(
        !slot.busy(),
        "a refused request must not reserve a capture slot"
    );
    assert_eq!(
        effects(slot.start(ZERO, 2, false, true, true)),
        vec![Effect::Emit(SELECTING), Effect::Present]
    );
}

#[test]
fn a_stop_before_the_pick_is_a_cancel_and_settles_once() {
    let mut slot = Slot::default();
    slot.start(ZERO, 1, false, true, true);
    assert_eq!(
        effects(slot.stop(1_000, 1)),
        vec![Effect::Release, Effect::Emit(CANCELLED)]
    );
    assert!(!slot.busy());
    // A late stop, or the user dismissing the picker afterwards, cannot report a second terminal.
    assert_eq!(effects(slot.stop(2_000, 1)), Vec::new());
    assert_eq!(
        effects(slot.report(3_000, 1, Report::Cancelled)),
        Vec::new()
    );
    assert_eq!(
        effects(slot.report(4_000, 1, Report::Selected(extent(1920, 1080)))),
        Vec::new()
    );
}

#[test]
fn the_happy_path_finalizes_only_after_the_muxer_closed() {
    let mut slot = Slot::default();
    slot.start(ZERO, 7, false, true, true);
    assert_eq!(
        effects(slot.report(1, 7, Report::Selected(extent(3840, 2160)))),
        vec![Effect::Capture(extent(2560, 1440))]
    );
    assert_eq!(
        effects(slot.report(2, 7, Report::Started)),
        vec![Effect::Emit(RECORDING)]
    );
    assert_eq!(effects(slot.report(3, 7, Report::Started)), Vec::new());
    assert_eq!(effects(slot.stop(4, 7)), vec![Effect::StopCapture]);
    // The reservation is still held: the file is not closed yet, so nothing may publish it.
    assert!(slot.busy());
    assert_eq!(
        effects(slot.stop(5, 7)),
        Vec::new(),
        "a stop does not buy a second one"
    );
    assert_eq!(
        effects(slot.report(6, 7, Report::Stopped { error: false })),
        vec![Effect::FinalizeOutput]
    );
    assert_eq!(
        effects(slot.report(7, 7, Report::Encoded { ok: true })),
        vec![Effect::Release, Effect::Emit(FINALIZED)]
    );
    assert!(!slot.busy());
    assert_eq!(
        effects(slot.report(8, 7, Report::Encoded { ok: true })),
        Vec::new()
    );
}

#[test]
fn muxer_and_capture_may_settle_in_either_order() {
    // ScreenCaptureKit can deliver the two completions in either order, and so can a closed session
    // racing a muxer error. The terminal event must wait for both, whichever reports first.
    let mut slot = Slot::default();
    slot.start(ZERO, 1, false, true, true);
    slot.report(1, 1, Report::Selected(extent(1920, 1080)));
    slot.report(2, 1, Report::Started);
    // The muxer fails while frames are still flowing: the capture is closed, then the file settles.
    assert_eq!(
        effects(slot.report(3, 1, Report::Encoded { ok: false })),
        vec![Effect::StopCapture]
    );
    assert_eq!(
        effects(slot.report(4, 1, Report::Stopped { error: false })),
        vec![Effect::Release, Effect::Emit(FAILED)]
    );
    assert!(!slot.busy());

    let mut slot = Slot::default();
    slot.start(ZERO, 2, false, true, true);
    slot.report(1, 2, Report::Selected(extent(1920, 1080)));
    slot.report(2, 2, Report::Started);
    // The item was closed by the system, then the file closes cleanly. A clean close does not turn a
    // failed capture into a finished recording.
    assert_eq!(
        effects(slot.report(3, 2, Report::Stopped { error: true })),
        vec![Effect::FinalizeOutput]
    );
    assert_eq!(
        effects(slot.report(4, 2, Report::Encoded { ok: true })),
        vec![Effect::Release, Effect::Emit(FAILED)]
    );
}

#[test]
fn a_session_that_never_carried_a_frame_is_cancelled_not_failed() {
    let mut slot = Slot::default();
    slot.start(ZERO, 1, false, true, true);
    slot.report(1, 1, Report::Selected(extent(1920, 1080)));
    // The pipeline was built but no frame arrived, and the user stopped it.
    assert_eq!(effects(slot.stop(2, 1)), vec![Effect::StopCapture]);
    assert_eq!(
        effects(slot.report(3, 1, Report::Stopped { error: false })),
        vec![Effect::FinalizeOutput]
    );
    assert_eq!(
        effects(slot.report(4, 1, Report::Encoded { ok: true })),
        vec![Effect::Release, Effect::Emit(CANCELLED)]
    );
}

#[test]
fn a_stop_that_never_settles_is_failed_by_the_watchdog() {
    let mut slot = Slot::default();
    slot.start(ZERO, 1, false, true, true);
    slot.report(1, 1, Report::Selected(extent(1920, 1080)));
    slot.report(2, 1, Report::Started);
    assert_eq!(effects(slot.stop(3, 1)), vec![Effect::StopCapture]);
    // A hung COM call: nothing reports back, and the watchdog must not fire early.
    assert_eq!(effects(slot.tick(3 + WATCHDOG - 1)), Vec::new());
    assert!(slot.busy());
    assert_eq!(
        effects(slot.tick(3 + WATCHDOG)),
        vec![Effect::Release, Effect::Emit(FAILED)]
    );
    assert!(!slot.busy());
    assert_eq!(effects(slot.tick(3 + WATCHDOG * 2)), Vec::new());

    // A stop that does settle clears its own watchdog.
    let mut slot = Slot::default();
    slot.start(ZERO, 2, false, true, true);
    slot.report(1, 2, Report::Selected(extent(1920, 1080)));
    slot.report(2, 2, Report::Started);
    slot.stop(3, 2);
    slot.report(4, 2, Report::Stopped { error: false });
    slot.report(5, 2, Report::Encoded { ok: true });
    assert!(!slot.busy());
}

#[test]
fn a_retired_token_can_never_complete_the_current_capture() {
    let mut slot = Slot::default();
    slot.start(ZERO, 1, false, true, true);
    assert_eq!(
        effects(slot.stop(1, 1)),
        vec![Effect::Release, Effect::Emit(CANCELLED)]
    );
    slot.start(2, 2, false, true, true);
    // Every late report for the first capture is dropped, including one that would publish a file.
    for report in [
        Report::Selected(extent(1920, 1080)),
        Report::Started,
        Report::Encoded { ok: true },
        Report::Stopped { error: false },
        Report::Failed,
        Report::Cancelled,
    ] {
        assert_eq!(effects(slot.report(3, 1, report)), Vec::new(), "{report:?}");
    }
    assert_eq!(effects(slot.stop(4, 1)), Vec::new());
    assert!(
        slot.busy(),
        "the current capture is untouched by a stale token"
    );
    assert_eq!(
        effects(slot.stop(5, 2)),
        vec![Effect::Release, Effect::Emit(CANCELLED)]
    );
}

#[test]
fn a_second_selection_and_a_late_start_do_not_reopen_a_stop() {
    let mut slot = Slot::default();
    slot.start(ZERO, 1, false, true, true);
    assert_eq!(
        effects(slot.report(1, 1, Report::Selected(extent(1920, 1080)))),
        vec![Effect::Capture(extent(1920, 1080))]
    );
    // The picker is gone by now, but a duplicated notification must not build a second pipeline.
    assert_eq!(
        effects(slot.report(2, 1, Report::Selected(extent(2560, 1440)))),
        Vec::new()
    );
    // Geometry the encoder cannot take is a failure after consent was given, not a retry.
    let mut slot = Slot::default();
    slot.start(ZERO, 2, false, true, true);
    assert_eq!(
        effects(slot.report(1, 2, Report::Selected(extent(1, 1)))),
        vec![Effect::Release, Effect::Emit(FAILED)]
    );
    assert!(!slot.busy());

    // A start notification that arrives after the user stopped must not announce "recording".
    let mut slot = Slot::default();
    slot.start(ZERO, 3, false, true, true);
    slot.report(1, 3, Report::Selected(extent(1920, 1080)));
    assert_eq!(effects(slot.stop(2, 3)), vec![Effect::StopCapture]);
    assert_eq!(effects(slot.report(3, 3, Report::Started)), Vec::new());
    assert_eq!(
        effects(slot.report(4, 3, Report::Stopped { error: false })),
        vec![Effect::FinalizeOutput]
    );
    assert_eq!(
        effects(slot.report(5, 3, Report::Encoded { ok: true })),
        vec![Effect::Release, Effect::Emit(FINALIZED)]
    );
}

#[test]
fn a_replaced_capture_starts_only_after_the_previous_one_settles() {
    let mut slot = Slot::default();
    slot.start(ZERO, 1, false, true, true);
    slot.report(1, 1, Report::Selected(extent(1920, 1080)));
    slot.report(2, 1, Report::Started);
    assert_eq!(effects(slot.stop(3, 1)), vec![Effect::StopCapture]);
    // Still settling: a new request is refused rather than queued behind it.
    assert_eq!(
        effects(slot.start(4, 2, false, true, true)),
        vec![Effect::Emit(BUSY)]
    );
    slot.report(5, 1, Report::Stopped { error: false });
    slot.report(6, 1, Report::Encoded { ok: true });
    assert_eq!(
        effects(slot.start(7, 2, false, true, true)),
        vec![Effect::Emit(SELECTING), Effect::Present]
    );
}
