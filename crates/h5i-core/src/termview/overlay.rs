//! Drawing hint labels over the page.
//!
//! The labels are composited into the decoded frame rather than written as
//! terminal text on top of the image, and that is a decision worth stating
//! because the other way looks easier.
//!
//! The graphics protocol does have a z-index, so an image could be placed under
//! the text and the labels written as cells. What it does not have is a
//! consistent answer about *cell backgrounds*: an image below the text is above
//! the cell backgrounds in one reading of the spec and below them in another,
//! and terminals differ. A label whose background sometimes paints and
//! sometimes does not is a label that is sometimes unreadable, over page pixels
//! we do not control. Compositing into the frame has one behaviour everywhere,
//! costs a few thousand pixel writes, and needs nothing of the terminal beyond
//! what it is already doing.
//!
//! It also puts the labels at *screen* resolution. The frame is downscaled to
//! the cell box before transmission, so drawing after that step means a chip is
//! the size it will actually be looked at, rather than a chip drawn at viewport
//! scale and then shrunk into illegibility.

/// One label, positioned in the scaled frame's own pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Chip {
    /// The whole label, including whatever has been typed already.
    pub label: String,
    /// How many leading characters the human has typed.
    ///
    /// Drawn dimmed rather than removed. Removing them would make the remaining
    /// text jump left as it was typed, and a label that moves while it is being
    /// aimed at is a label that gets mistyped.
    pub typed: usize,
    pub x: i32,
    pub y: i32,
}

/// Amber, which is legible over both a white page and a dark one, and is not a
/// colour a page is likely to be using for something else at this size.
const CHIP_BG: [u8; 3] = [255, 200, 40];
const CHIP_FG: [u8; 3] = [20, 20, 20];
/// The part already typed. Still legible, clearly spent.
const CHIP_DIM: [u8; 3] = [150, 120, 30];
const CHIP_EDGE: [u8; 3] = [60, 45, 0];

/// Glyph cell, before scaling.
const GLYPH_W: i32 = 5;
const GLYPH_H: i32 = 7;
/// How much bigger the drawn glyph is than the bitmap.
const SCALE: i32 = 2;
/// Space around the text inside the chip.
const PAD: i32 = 2;
/// Gap between glyphs.
const GAP: i32 = 1;

/// Where the label for a target goes, and how big it will be.
///
/// The rect is in viewport pixels; the answer is in the scaled frame's pixels.
/// The label sits at the target's top-left, which is where every implementation
/// of this idea puts it and where a reader's eye already is when they are
/// deciding what to press.
///
/// Nudged back inside the frame rather than clipped: a chip half off the left
/// edge is a label that cannot be read, and a target at `x = 0` is common.
pub fn place(
    label: &str,
    typed: usize,
    rect: (f64, f64, f64, f64),
    viewport: (u32, u32),
    frame: (u32, u32),
) -> Chip {
    let (vw, vh) = (viewport.0.max(1) as f64, viewport.1.max(1) as f64);
    let (fw, fh) = (frame.0 as f64, frame.1 as f64);
    let x = rect.0 / vw * fw;
    let y = rect.1 / vh * fh;

    let (w, h) = size(label);
    let max_x = (frame.0 as i32 - w).max(0);
    let max_y = (frame.1 as i32 - h).max(0);
    Chip {
        label: label.to_string(),
        typed,
        x: (x.round() as i32).clamp(0, max_x),
        y: (y.round() as i32).clamp(0, max_y),
    }
}

/// The pixel extent of a chip for `label`.
pub fn size(label: &str) -> (i32, i32) {
    let glyphs = label.chars().count().max(1) as i32;
    let text_w = glyphs * GLYPH_W * SCALE + (glyphs - 1) * GAP;
    (text_w + PAD * 2 + 2, GLYPH_H * SCALE + PAD * 2 + 2)
}

/// Composite every chip into a tightly packed RGB frame.
///
/// Bounds are checked per pixel rather than per chip. The chips are positioned
/// from a rect the *page* supplied, by way of a scale this viewer computed, and
/// a page that can make this function index past the end of a host buffer is a
/// page with a memory bug to hand.
pub fn draw(rgb: &mut [u8], width: u32, height: u32, chips: &[Chip]) {
    let mut surface = Surface { rgb, width, height };
    for chip in chips {
        let (w, h) = size(&chip.label);
        surface.fill(chip.x, chip.y, w, h, CHIP_EDGE);
        surface.fill(chip.x + 1, chip.y + 1, w - 2, h - 2, CHIP_BG);

        let mut pen = chip.x + 1 + PAD;
        for (index, ch) in chip.label.chars().enumerate() {
            let colour = if index < chip.typed { CHIP_DIM } else { CHIP_FG };
            surface.glyph(pen, chip.y + 1 + PAD, ch, colour);
            pen += GLYPH_W * SCALE + GAP;
        }
    }
}

/// A frame being drawn into: the pixels and the two numbers that say what
/// counts as inside them.
///
/// A type rather than three arguments threaded through every helper, because
/// the three must never be separated: the whole safety of this file is that no
/// coordinate is trusted until it has been checked against *these* bounds, and
/// a helper taking a buffer and someone else's dimensions is how that stops
/// being true.
struct Surface<'a> {
    rgb: &'a mut [u8],
    width: u32,
    height: u32,
}

impl Surface<'_> {
    fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, colour: [u8; 3]) {
        for row in y..y + h {
            for col in x..x + w {
                self.put(col, row, colour);
            }
        }
    }

    fn glyph(&mut self, x: i32, y: i32, ch: char, colour: [u8; 3]) {
        let Some(bitmap) = bitmap(ch) else {
            return;
        };
        for (row, bits) in bitmap.iter().enumerate() {
            for col in 0..GLYPH_W {
                // Bit 4 is the leftmost column.
                if bits & (1 << (GLYPH_W - 1 - col)) == 0 {
                    continue;
                }
                for dy in 0..SCALE {
                    for dx in 0..SCALE {
                        self.put(x + col * SCALE + dx, y + row as i32 * SCALE + dy, colour);
                    }
                }
            }
        }
    }

    fn put(&mut self, x: i32, y: i32, colour: [u8; 3]) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let index = (y as usize * self.width as usize + x as usize) * 3;
        if index + 2 >= self.rgb.len() {
            return;
        }
        self.rgb[index] = colour[0];
        self.rgb[index + 1] = colour[1];
        self.rgb[index + 2] = colour[2];
    }
}

/// A 5×7 uppercase face, one byte per row.
///
/// Uppercase for a lower-case alphabet on purpose: at five pixels wide the
/// letters with descenders and the ones without are hard to tell apart, and
/// every implementation of hint labels shows them capitalised for that reason.
/// The matching is still case-insensitive, so what is shown and what is typed
/// stay the same name.
fn bitmap(ch: char) -> Option<[u8; GLYPH_H as usize]> {
    let rows = match ch.to_ascii_uppercase() {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        _ => return None,
    };
    Some(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank(w: u32, h: u32) -> Vec<u8> {
        vec![0u8; (w * h * 3) as usize]
    }

    /// Every character the hint alphabet can produce has a face. A label drawn
    /// as a blank chip is a label nobody can type.
    #[test]
    fn every_letter_a_label_can_contain_has_a_glyph() {
        for ch in b'a'..=b'z' {
            assert!(bitmap(ch as char).is_some(), "no glyph for `{}`", ch as char);
        }
    }

    #[test]
    fn a_chip_lands_where_the_target_is_after_the_frame_is_scaled() {
        // A viewport twice the frame's size: everything halves.
        let chip = place("sd", 0, (400.0, 200.0, 50.0, 20.0), (800, 400), (400, 200));
        assert_eq!((chip.x, chip.y), (200, 100));
    }

    /// A target at the very edge still gets a readable label, which means the
    /// chip is nudged inside rather than drawn half off the frame.
    #[test]
    fn a_chip_at_the_edge_is_nudged_inside_the_frame() {
        let (w, h) = size("sd");
        let frame = (100u32, 60u32);
        let chip = place("sd", 0, (99.0, 59.0, 1.0, 1.0), (100, 60), frame);
        assert!(chip.x + w <= frame.0 as i32, "{chip:?} runs off the right");
        assert!(chip.y + h <= frame.1 as i32, "{chip:?} runs off the bottom");
        assert!(chip.x >= 0 && chip.y >= 0, "{chip:?}");
    }

    /// The page supplies the rects these are derived from, so a chip positioned
    /// past the buffer must be dropped pixel by pixel rather than indexed.
    #[test]
    fn a_chip_positioned_outside_the_buffer_writes_nothing_and_does_not_panic() {
        let (w, h) = (40u32, 30u32);
        let mut rgb = blank(w, h);
        draw(
            &mut rgb,
            w,
            h,
            &[
                Chip { label: "sd".into(), typed: 0, x: 5_000, y: 5_000 },
                Chip { label: "sd".into(), typed: 0, x: -5_000, y: -5_000 },
            ],
        );
        assert!(rgb.iter().all(|&b| b == 0), "something was drawn off-frame");
    }

    #[test]
    fn a_chip_inside_the_frame_actually_marks_it() {
        let (w, h) = (80u32, 40u32);
        let mut rgb = blank(w, h);
        draw(&mut rgb, w, h, &[Chip { label: "sd".into(), typed: 0, x: 4, y: 4 }]);
        assert!(rgb.iter().any(|&b| b != 0), "nothing was drawn");

        // And nothing outside the chip's own box was touched.
        let (cw, ch) = size("sd");
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let inside = x >= 4 && x < 4 + cw && y >= 4 && y < 4 + ch;
                if inside {
                    continue;
                }
                let index = (y as usize * w as usize + x as usize) * 3;
                assert_eq!(&rgb[index..index + 3], &[0, 0, 0], "painted outside at {x},{y}");
            }
        }
    }

    /// What has been typed is dimmed rather than dropped, so the label does not
    /// shift left under the fingers aiming at it.
    #[test]
    fn typing_a_prefix_dims_it_without_moving_the_rest() {
        let (w, h) = (80u32, 40u32);
        let mut fresh = blank(w, h);
        let mut partly = blank(w, h);
        draw(&mut fresh, w, h, &[Chip { label: "sd".into(), typed: 0, x: 2, y: 2 }]);
        draw(&mut partly, w, h, &[Chip { label: "sd".into(), typed: 1, x: 2, y: 2 }]);

        assert_ne!(fresh, partly, "the typed prefix was drawn identically");
        assert_eq!(
            size("sd"),
            size("sd"),
            "a chip's extent must not depend on what has been typed"
        );
        // The second glyph is untouched: only the spent half changed.
        let second_glyph_x = 2 + 1 + PAD + GLYPH_W * SCALE + GAP;
        for y in 0..h as i32 {
            for x in second_glyph_x..w as i32 {
                let index = (y as usize * w as usize + x as usize) * 3;
                assert_eq!(
                    &fresh[index..index + 3],
                    &partly[index..index + 3],
                    "the untyped half moved at {x},{y}"
                );
            }
        }
    }
}
