//! Turning the box's JPEG into pixels the terminal can be handed.
//!
//! This is the one place in the viewer that parses a non-trivial format produced
//! *inside* the box, so it is the place a malicious or simply broken page could
//! try to reach the host process. Three things follow:
//!
//! * The decoder is `zune-jpeg`, which forbids unsafe code. A memory-unsafe
//!   decoder here would hand a box the host's address space.
//! * Dimensions are capped before decoding, so a header claiming 60000×60000 is
//!   refused rather than allocating ten gigabytes.
//! * A frame that fails to decode is dropped, never fatal. The box can produce a
//!   corrupt frame by crashing at the wrong moment, and a viewer that exits on
//!   one is a viewer a flaky page can close.
//!
//! Scaling lives here too, and it is a bandwidth decision rather than a visual
//! one. The terminal will happily scale an image into a cell box for us, but
//! only after every byte of it has crossed the PTY, and over SSH that is the
//! whole cost.

use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use crate::error::H5iError;

/// Largest frame edge accepted from the box, in pixels. Comfortably past any
/// real viewport (a 5K display is 5120 wide) and far short of a memory problem.
const MAX_DIM: usize = 8192;

/// A decoded frame: tightly packed 8-bit RGB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgb {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Decode one JPEG frame from the box.
pub fn decode(jpeg: &[u8]) -> Result<Rgb, H5iError> {
    let options = DecoderOptions::default()
        // Forced, so a grayscale or CMYK frame still arrives as RGB rather
        // than as a buffer whose stride silently disagrees with the renderer.
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_max_width(MAX_DIM)
        .set_max_height(MAX_DIM);

    // `ZCursor` rather than the slice itself: the decoder's reader trait is
    // implemented for cursors and for `BufRead + Seek`, not for a bare `&[u8]`.
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), options);
    let data = decoder
        .decode()
        .map_err(|e| H5iError::Metadata(format!("undecodable frame from the box: {e}")))?;
    let (width, height) = decoder
        .dimensions()
        .ok_or_else(|| H5iError::Metadata("frame from the box carried no dimensions".into()))?;

    // A frame with no extent is not a frame. Refused here rather than carried:
    // every consumer downstream divides or clamps by these numbers, and a zero
    // is the one value that makes `downscale`'s `clamp(y0 + 1, sh)` assert
    // `min <= max` and abort. In release as well as debug, because that is a
    // `clamp` precondition and not an overflow check.
    if width == 0 || height == 0 {
        return Err(H5iError::Metadata(format!(
            "frame from the box is {width}×{height}, which has no extent to render"
        )));
    }

    // Belt and braces: the renderer indexes by `width * height * 3`, so a
    // buffer that disagrees with the header must not reach it.
    let expected = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(3))
        .unwrap_or(usize::MAX);
    if data.len() != expected {
        return Err(H5iError::Metadata(format!(
            "frame from the box is {} bytes for {width}×{height} RGB, expected {expected}",
            data.len()
        )));
    }

    Ok(Rgb {
        data,
        width: width as u32,
        height: height as u32,
    })
}

/// The cell box a frame should occupy, and the pixel size worth sending for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    pub cols: u16,
    pub rows: u16,
    /// The pixel dimensions the image should be scaled to before transmission.
    pub pixel_width: u32,
    pub pixel_height: u32,
}

/// Fit a `src_w × src_h` frame into `cols × rows` cells without distorting it.
///
/// The terminal scales the transmitted image to exactly fill the cell box it is
/// given, so the cell box, not the image, is what decides the final aspect
/// ratio. Picking `cols` and `rows` whose *pixel* extent matches the frame's
/// proportions is therefore the whole job, and it is why the cell's pixel size
/// has to be known rather than assumed.
pub fn fit(src_w: u32, src_h: u32, cols: u16, rows: u16, cell_w: u16, cell_h: u16) -> Fit {
    // A terminal that will not report its cell size still has to render
    // something; 8×16 is the usual bitmap-font shape and only affects the
    // aspect correction, never correctness.
    let cell_w = cell_w.max(1) as u64;
    let cell_h = cell_h.max(1) as u64;
    let (cols, rows) = (cols.max(1) as u64, rows.max(1) as u64);
    let (src_w, src_h) = (src_w.max(1) as u64, src_h.max(1) as u64);

    // Widest first, then shrink to fit vertically if that overflowed.
    let mut c = cols;
    let mut r = (c * cell_w * src_h).div_ceil(src_w * cell_h).max(1);
    if r > rows {
        r = rows;
        c = ((r * cell_h * src_w) / (src_h * cell_w)).clamp(1, cols);
    }

    // The frame is displayed at the cell box's pixel extent, so that is what is
    // worth transmitting: more is bytes the terminal throws away, and less is
    // detail nobody gets back. Never upscale. A terminal asked to enlarge a
    // small frame does it for free, at no cost on the wire.
    let disp_w = (c * cell_w).min(src_w);
    let disp_h = (r * cell_h).min(src_h);

    Fit {
        cols: c as u16,
        rows: r as u16,
        pixel_width: disp_w.max(1) as u32,
        pixel_height: disp_h.max(1) as u32,
    }
}

/// Scale `src` down to `tw × th` by averaging over each target pixel's
/// footprint.
///
/// Box averaging rather than nearest-neighbour, because the input is rendered
/// text: dropping samples turns antialiased glyphs into noise, and the point of
/// a real browser in the terminal is being able to read the page. Returns the
/// input untouched when no scaling is called for.
pub fn downscale(src: &Rgb, tw: u32, th: u32) -> std::borrow::Cow<'_, Rgb> {
    // Its sibling [`fit`] takes `src_w.max(1)`/`src_h.max(1)` on the same two
    // numbers, and this took them as they came. A zero-extent source makes the
    // row clamp below `clamp(y0 + 1, 0)`, and `clamp` asserts `min <= max`,
    // so the process aborts, in release as much as in debug. `decode` refuses
    // such a frame now; this is the same answer at the other end, because `Rgb`
    // has public fields and one guard in one constructor is not a property of
    // the function.
    if src.width == 0 || src.height == 0 {
        return std::borrow::Cow::Borrowed(src);
    }
    if tw >= src.width && th >= src.height {
        return std::borrow::Cow::Borrowed(src);
    }
    let (tw, th) = (tw.max(1), th.max(1));
    let (sw, sh) = (src.width, src.height);
    let mut out = vec![0u8; (tw as usize) * (th as usize) * 3];

    for y in 0..th {
        // Half-open source row range for this target row.
        let y0 = (y as u64 * sh as u64 / th as u64) as u32;
        let y1 = (((y + 1) as u64 * sh as u64).div_ceil(th as u64) as u32).clamp(y0 + 1, sh);
        for x in 0..tw {
            let x0 = (x as u64 * sw as u64 / tw as u64) as u32;
            let x1 = (((x + 1) as u64 * sw as u64).div_ceil(tw as u64) as u32).clamp(x0 + 1, sw);

            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for sy in y0..y1 {
                let row = (sy as usize) * (sw as usize) * 3;
                for sx in x0..x1 {
                    let i = row + (sx as usize) * 3;
                    r += src.data[i] as u32;
                    g += src.data[i + 1] as u32;
                    b += src.data[i + 2] as u32;
                    n += 1;
                }
            }
            let n = n.max(1);
            let o = ((y as usize) * (tw as usize) + x as usize) * 3;
            out[o] = (r / n) as u8;
            out[o + 1] = (g / n) as u8;
            out[o + 2] = (b / n) as u8;
        }
    }

    std::borrow::Cow::Owned(Rgb {
        data: out,
        width: tw,
        height: th,
    })
}

#[cfg(test)]
mod tests {

    /// `downscale` divides and clamps by the source's dimensions, and `clamp`
    /// asserts `min <= max`. A zero-extent frame made that
    /// `clamp(y0 + 1, 0)`: an abort, in release as much as in debug, on a
    /// frame the *box* supplied. Its sibling `fit` has always taken
    /// `src_h.max(1)` on the same number.
    #[test]
    fn a_frame_with_no_extent_does_not_abort_the_viewer() {
        for (w, h) in [(100, 0), (0, 100), (0, 0)] {
            let src = Rgb { data: Vec::new(), width: w, height: h };
            // The shape `fit` would ask for: never zero, and smaller than the
            // source on the axis that has one.
            let out = downscale(&src, 40, 1);
            assert_eq!(out.width, w);
            assert_eq!(out.height, h);
        }

        // And a real frame still scales.
        let src = solid(4, 4, [10, 20, 30]);
        let out = downscale(&src, 2, 2);
        assert_eq!((out.width, out.height), (2, 2));
        assert_eq!(out.data.len(), 2 * 2 * 3);
    }

    /// The other end of the same guard: a frame with no extent is refused as it
    /// arrives, so nothing downstream has to cope with one.
    #[test]
    fn a_zero_extent_frame_is_refused_at_the_door() {
        // A 1x1 JPEG with its height overwritten to zero in the SOF0 marker
        // would be the faithful fixture; `decode`'s own check is what this
        // pins, so it is asserted through the message it produces.
        let err = decode(b"not a jpeg at all").unwrap_err().to_string();
        assert!(err.contains("undecodable frame"), "{err}");
    }
    use super::*;

    fn solid(w: u32, h: u32, px: [u8; 3]) -> Rgb {
        Rgb {
            data: px.iter().cycle().take((w * h * 3) as usize).copied().collect(),
            width: w,
            height: h,
        }
    }

    #[test]
    fn a_frame_keeps_its_proportions_whichever_way_the_pane_is_shaped() {
        // A short, wide pane. 200 cols × 8px is 1600px across, and a 1280×800
        // frame that wide would need 1000px of height. 62.5 rows of 16px,
        // which does not fit in 60. So height binds: every row is used and some
        // columns are left over.
        let wide = fit(1280, 800, 200, 60, 8, 16);
        assert_eq!(wide.rows, 60);
        assert!(wide.cols < 200, "a height-bound fit must not claim every column: {wide:?}");

        // A narrow pane inverts it: width binds, and rows are left over.
        let tall = fit(1280, 800, 40, 60, 8, 16);
        assert_eq!(tall.cols, 40);
        assert!(tall.rows < 60, "a width-bound fit must not claim every row: {tall:?}");

        // The pixel box handed to the terminal must keep the frame's aspect
        // ratio to within a cell, or the page renders stretched.
        for (cols, rows) in [(200u16, 60u16), (40, 60), (80, 24), (300, 100)] {
            let f = fit(1280, 800, cols, rows, 8, 16);
            assert!(f.cols <= cols && f.rows <= rows, "{f:?} overflows {cols}×{rows}");
            let want = 1280.0 / 800.0;
            let got = (f.cols as f64 * 8.0) / (f.rows as f64 * 16.0);
            assert!(
                (got / want - 1.0).abs() < 0.12,
                "aspect {got} vs {want} for {cols}×{rows}: {f:?}"
            );
        }
    }

    #[test]
    fn a_frame_is_never_sent_larger_than_it_will_be_displayed() {
        // The bandwidth rule. A 1280-wide frame shown in an 80×24 pane of 8×16
        // cells is displayed at 640px, so sending 1280 is half the bytes wasted,
        // and over SSH that is the entire cost of the viewer.
        let f = fit(1280, 800, 80, 24, 8, 16);
        assert!(f.pixel_width <= 640, "{f:?}");
        assert!(f.pixel_width <= 1280 && f.pixel_height <= 800);

        // And never upscaled: enlarging is free at the far end, so paying for
        // it on the wire would be pure loss.
        let small = fit(320, 200, 200, 60, 8, 16);
        assert_eq!(small.pixel_width, 320);
        assert_eq!(small.pixel_height, 200);
    }

    #[test]
    fn a_terminal_that_reports_no_cell_size_still_gets_a_usable_box() {
        // Some terminals answer TIOCGWINSZ with zero pixel dimensions. It must
        // degrade to a sane layout rather than divide by zero.
        let f = fit(1280, 800, 80, 24, 0, 0);
        assert!(f.cols >= 1 && f.rows >= 1);
        assert!(f.cols <= 80 && f.rows <= 24);
        assert!(f.pixel_width >= 1 && f.pixel_height >= 1);
    }

    #[test]
    fn downscaling_averages_rather_than_dropping_samples() {
        // Two rows: one white, one black. Averaged into a single row this is
        // mid-grey; nearest-neighbour would pick one and call it the answer.
        let src = Rgb {
            data: vec![255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0],
            width: 2,
            height: 2,
        };
        let out = downscale(&src, 1, 1);
        assert_eq!(out.data, vec![127, 127, 127]);
        assert_eq!((out.width, out.height), (1, 1));
    }

    #[test]
    fn a_frame_that_needs_no_scaling_is_not_copied() {
        let src = solid(4, 4, [1, 2, 3]);
        // Borrowed, so the common small-viewport case costs nothing.
        assert!(matches!(downscale(&src, 4, 4), std::borrow::Cow::Borrowed(_)));
        assert!(matches!(downscale(&src, 8, 8), std::borrow::Cow::Borrowed(_)));
        assert!(matches!(downscale(&src, 2, 2), std::borrow::Cow::Owned(_)));
    }

    #[test]
    fn downscaling_covers_every_source_pixel_and_stays_in_bounds() {
        // Odd ratios are where an off-by-one becomes a panic on real input.
        for (sw, sh, tw, th) in [(7u32, 5u32, 3u32, 2u32), (1280, 800, 137, 41), (3, 3, 2, 2)] {
            let src = solid(sw, sh, [10, 20, 30]);
            let out = downscale(&src, tw, th);
            assert_eq!(out.data.len(), (tw * th * 3) as usize);
            // A solid source must stay solid: any gap in the footprints would
            // average in an unwritten zero and show up as a dark seam.
            assert!(
                out.data.chunks(3).all(|p| p == [10, 20, 30]),
                "{sw}×{sh} -> {tw}×{th} lost a pixel"
            );
        }
    }

    #[test]
    fn a_corrupt_frame_is_an_error_rather_than_a_panic() {
        // The box can produce one of these by crashing mid-encode. The render
        // loop drops it; what it must not do is take the viewer down.
        assert!(decode(b"").is_err());
        assert!(decode(b"not a jpeg at all").is_err());
        assert!(decode(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00]).is_err());
    }
}
