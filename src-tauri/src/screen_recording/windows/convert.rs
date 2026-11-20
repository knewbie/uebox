//! Turning one mapped capture surface into the encoder's NV12 planes.
//!
//! Stage A does this on the CPU: the capture hands back a BGRA texture, it is copied to a staging
//! texture, mapped once, and packed here. The GPU alternatives — `ID3D11VideoProcessor` for the scale
//! and colour conversion, or a Media Foundation DX manager so the encoder reads the surface directly —
//! are later stages. What is here is the part that can be checked without a display or a driver:
//! integer bilinear sampling, then the BT.601 limited-range conversion an H.264 encoder expects.
//!
//! The sampling coordinates are computed once per capture, not per frame: the item size and the
//! encoder size are fixed at pick time, so a per-pixel divide would be millions of pointless
//! divisions a frame.

use super::geometry::Extent;

/// Source origin and interpolation weight for one output column or row.
type Tap = (usize, u32);

/// The fixed mapping from an encoder pixel to the capture surface, plus the packer that uses it.
pub(super) struct Sampler {
    content: Extent,
    output: Extent,
    columns: Vec<Tap>,
    rows: Vec<Tap>,
}

impl Sampler {
    pub(super) fn new(content: Extent, output: Extent) -> Self {
        Self {
            content,
            output,
            columns: (0..output.width)
                .map(|index| axis(index, content.width, output.width))
                .collect(),
            rows: (0..output.height)
                .map(|index| axis(index, content.height, output.height))
                .collect(),
        }
    }

    /// Packs `source` — `content` pixels wide, `pitch` bytes per row, BGRA — into the Y plane and the
    /// interleaved UV plane of an NV12 frame.
    ///
    /// Returns false when the buffers or the geometry disagree, which is a bug rather than a capture
    /// condition: the caller must drop the frame and never write a partial one, because a short frame
    /// corrupts everything the encoder produces after it.
    pub(super) fn pack(&self, source: &[u8], pitch: usize, y: &mut [u8], uv: &mut [u8]) -> bool {
        let width = self.output.width as usize;
        let height = self.output.height as usize;
        let row_bytes = self.content.width as usize * 4;
        let rows = self.content.height as usize;
        if !width.is_multiple_of(2)
            || !height.is_multiple_of(2)
            || rows == 0
            || y.len() < width * height
            || uv.len() < width * height / 2
            || pitch < row_bytes
            || source.len() < row_bytes + (rows - 1) * pitch
        {
            return false;
        }
        for block in 0..height / 2 {
            for pair in 0..width / 2 {
                let mut sums = [0u32; 3];
                for dy in 0..2 {
                    let row = block * 2 + dy;
                    for dx in 0..2 {
                        let column = pair * 2 + dx;
                        let rgb = self.sample(source, pitch, column, row);
                        let pixel = row * width + column;
                        y[pixel] = luma(rgb[0], rgb[1], rgb[2]);
                        for channel in 0..3 {
                            sums[channel] += rgb[channel];
                        }
                    }
                }
                // Chroma sits on a 2x2 grid in NV12, so it is averaged over the block instead of
                // sampled at one of its corners: that is what keeps a downscale from ringing.
                let average = [sums[0] / 4, sums[1] / 4, sums[2] / 4];
                let chroma = block * width + pair * 2;
                uv[chroma] = blue_chroma(average[0], average[1], average[2]);
                uv[chroma + 1] = red_chroma(average[0], average[1], average[2]);
            }
        }
        true
    }

    /// Bilinear sample at an encoder coordinate, as red, green, blue. `x` and `y` are output
    /// coordinates.
    fn sample(&self, source: &[u8], pitch: usize, x: usize, y: usize) -> [u32; 3] {
        let (column, horizontal) = self.columns[x];
        let (row, vertical) = self.rows[y];
        let origin = row * pitch + column * 4;
        // A zero weight means the neighbour is unused, so it is not read at all: on the last column or
        // row the neighbour would sit outside the mapped region.
        let east = if horizontal == 0 { 0 } else { 4 };
        let south = if vertical == 0 { 0 } else { pitch };
        let mut bgr = [0u32; 3];
        for channel in 0..3 {
            // The surface is BGRA, so blue is the first byte and red the third.
            let near = source[origin + channel] as u32;
            let right = source[origin + east + channel] as u32;
            let below = source[origin + south + channel] as u32;
            let corner = source[origin + south + east + channel] as u32;
            let top = blend(near, right, horizontal);
            let bottom = blend(below, corner, horizontal);
            bgr[channel] = blend(top, bottom, vertical);
        }
        [bgr[2], bgr[1], bgr[0]]
    }
}

/// One output axis position in source coordinates, as an origin and a 0..=255 weight.
///
/// The centre of the output pixel is mapped into the content, so a 1:1 pick lands exactly on source
/// pixels with a zero weight rather than half a pixel off — the identity case has to be bit-exact,
/// because that is what a monitor at or below 2560x1440 does every frame.
fn axis(index: u32, source: u32, target: u32) -> Tap {
    if source == 0 || target == 0 {
        return (0, 0);
    }
    if source == target {
        return (index as usize, 0);
    }
    // Sixteenths of a sixteenth of a source pixel, in i64: (2 * index + 1) * source * 128 reaches
    // 4.3e9 at 4096 columns, which does not fit an i32.
    let position = ((2 * index as i64 + 1) * source as i64 * 128 / target as i64 - 128).max(0);
    let last = source as usize - 1;
    let origin = (position >> 8) as usize;
    if origin >= last {
        (last, 0)
    } else {
        (origin, (position & 0xff) as u32)
    }
}

fn blend(left: u32, right: u32, weight: u32) -> u32 {
    (left * (256 - weight) + right * weight) >> 8
}

/// BT.601 limited-range luma. The result is provably 16..=235, so no clamp is needed.
fn luma(red: u32, green: u32, blue: u32) -> u8 {
    (((66 * red + 129 * green + 25 * blue + 128) >> 8) + 16) as u8
}

/// BT.601 limited-range Cb. The result is 90..=240 for any input; the clamp is for the arithmetic
/// shift's floor behaviour, not for reachability.
fn blue_chroma(red: u32, green: u32, blue: u32) -> u8 {
    (((-38 * red as i32 - 74 * green as i32 + 112 * blue as i32 + 128) >> 8) + 128).clamp(0, 255)
        as u8
}

fn red_chroma(red: u32, green: u32, blue: u32) -> u8 {
    (((112 * red as i32 - 94 * green as i32 - 18 * blue as i32 + 128) >> 8) + 128).clamp(0, 255)
        as u8
}
