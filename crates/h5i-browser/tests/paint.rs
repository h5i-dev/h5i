//! What the rasteriser gets wrong, pinned.
//!
//! These are not tests of our code. They render a page through the real paint
//! path and assert on pixels, so that a dependency bump either fixes the defect
//! or is refused. Each one names the upstream mechanism it is holding.

use std::sync::Arc;

use h5i_browser::engine::{PageFactory, PageOptions};
use h5i_browser::net::LocalBroker;
use h5i_browser::policy::Policy;
use h5i_browser::receipt::MemorySink;

const WIDTH: u32 = 800;
const HEIGHT: u32 = 200;

/// One wide tile in `vello_common`'s coarse rasteriser. The defect below is
/// aligned to it, which is how it was identified.
const WIDE_TILE: u32 = 256;

fn render(html: &str) -> Vec<u8> {
    let broker =
        LocalBroker::new(Policy::new(), Arc::new(MemorySink::new()), None).expect("broker");
    let fonts =
        h5i_browser::fonts::load(&[], &h5i_browser::fonts::default_font_dirs(), Some(2));
    let options = PageOptions {
        width: WIDTH,
        height: HEIGHT,
        ..Default::default()
    };
    let factory = PageFactory::new(broker, fonts.sources.clone(), options);
    let base = url::Url::parse("https://fixture.example/").unwrap();
    let mut page = factory.from_html(html, &base);
    let png = page.screenshot_png().expect("screenshot");
    decode_png_rgba(&png)
}

/// The longest run of pure white pixels on one row, and where it starts.
///
/// A run, not a count: text is white too, and a glyph never covers forty
/// columns in a row. A filled rectangle does.
fn widest_white_run(rgba: &[u8], y: u32) -> (u32, u32) {
    let row = (y * WIDTH * 4) as usize;
    let (mut best, mut best_at, mut run, mut at) = (0u32, 0u32, 0u32, 0u32);
    for x in 0..WIDTH {
        let p = row + (x * 4) as usize;
        let white = rgba[p] >= 250 && rgba[p + 1] >= 250 && rgba[p + 2] >= 250;
        if white {
            if run == 0 {
                at = x;
            }
            run += 1;
            if run > best {
                best = run;
                best_at = at;
            }
        } else {
            run = 0;
        }
    }
    (best, best_at)
}

fn decode_png_rgba(png: &[u8]) -> Vec<u8> {
    image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .expect("png decodes")
        .to_rgba8()
        .into_raw()
}

/// An element with `box-shadow: inset` that fully covers a wide tile paints
/// that tile white.
///
/// `blitz-paint` draws an inset shadow by filling the padding box with the
/// shadow colour, then punching the border box out of it with a `Color::WHITE`
/// mask inside a `Compose::DestOut` layer. In `vello_common` 0.0.9 the coarse
/// rasteriser's overdraw elimination (`WideTile::fill`, `can_override`) fires
/// on that opaque full-width mask and promotes it to the tile's background,
/// discarding the fill underneath. The mask, which should never be visible,
/// becomes 256 columns of white.
///
/// Every shipped CSS in this repository avoids `inset` for that reason. When a
/// dependency bump fixes this, delete the workaround and this test together.
#[test]
fn an_inset_box_shadow_over_a_whole_wide_tile_paints_it_white() {
    let rgba = render(
        r#"<!doctype html><meta charset="utf-8">
        <style>body{background:#0a0a0c;margin:0}
        .r{position:absolute;left:200px;top:0;width:400px;height:80px;
           background:#1d1d22;box-shadow:inset 2px 0 0 #ff6258}</style>
        <div class="r"></div>"#,
    );
    let (run, at) = widest_white_run(&rgba, 40);
    assert!(
        run >= WIDE_TILE,
        "the defect stopped reproducing: no white run of a whole wide tile \
         (widest was {run} at x={at}). If a dependency bump fixed it, remove \
         this test and the `border-left` workarounds it documents."
    );
    assert_eq!(
        at % WIDE_TILE,
        0,
        "the white run should start on a wide-tile boundary; it started at x={at}"
    );
}

/// The same element drawn with `border-left` instead. This is the workaround,
/// and it has to keep working.
#[test]
fn a_left_border_draws_the_same_bar_without_the_defect() {
    let rgba = render(
        r#"<!doctype html><meta charset="utf-8">
        <style>body{background:#0a0a0c;margin:0}
        .r{position:absolute;left:200px;top:0;width:400px;height:80px;
           background:#1d1d22;border-left:2px solid #ff6258}</style>
        <div class="r"></div>"#,
    );
    let (run, at) = widest_white_run(&rgba, 40);
    assert!(
        run < 40,
        "a bordered row painted {run} white columns at x={at}; it should paint none"
    );
}

/// An outset shadow was never affected, which is what localised the defect to
/// the inset path rather than to shadows in general.
#[test]
fn an_outset_box_shadow_is_unaffected() {
    let rgba = render(
        r#"<!doctype html><meta charset="utf-8">
        <style>body{background:#0a0a0c;margin:0}
        .r{position:absolute;left:200px;top:0;width:400px;height:80px;
           background:#1d1d22;box-shadow:2px 0 0 #ff6258}</style>
        <div class="r"></div>"#,
    );
    let (run, at) = widest_white_run(&rgba, 40);
    assert!(run < 40, "an outset shadow painted {run} white columns at x={at}");
}
