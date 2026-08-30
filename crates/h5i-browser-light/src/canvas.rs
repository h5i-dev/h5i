//! Canvas 2D, drawn for real.
//!
//! Both reference engines fake this: Lightpanda ships sixty-one no-op bridge
//! functions, so `fillRect` is callable, returns `undefined` and paints
//! nothing, and Obscura's `DOMSnapshot` invents geometry for the same reason.
//! Neither has a rasteriser, so faking is the only move available to them.
//!
//! This engine has one. `blitz-paint` over `vello_cpu` already turns the page
//! into pixels on the CPU, and `vello_cpu` is a general 2D rasteriser a canvas
//! can use directly. So a canvas here **draws**, composites into the page like
//! any other image, and shows up in a screenshot.
//!
//! # The rule this module is built around
//!
//! roadmap-history.md §B8.4: *missing APIs are named, never stubbed silently.*
//! A page that draws its content on a canvas and comes back blank, with nothing
//! saying why, is indistinguishable from a page that drew nothing. So the
//! surface splits in two:
//!
//! * What is implemented **rasterises**: paths, rectangles, arcs, fills,
//!   strokes, transforms, the state stack, `toDataURL`.
//! * What is not is **reported by name** through the same `unsupported()`
//!   channel as every other missing Web API, and appears in the snapshot's
//!   note. Text, gradients, patterns, shadows, `drawImage`, `clip` and the
//!   `ImageData` operations are on that list today.
//!
//! An agent reading `note: this page used Web APIs this engine does not have
//! (CanvasRenderingContext2D.fillText x12)` knows to route to Chromium. An agent
//! reading a blank canvas knows nothing.
//!
//! # How it reaches the page
//!
//! A canvas keeps an RGBA buffer. On flush it is attached to the `<canvas>`
//! element as raster image data, which `blitz-paint` draws for any element
//! carrying it, not only `<img>`. There is no GPU path and no
//! `custom_paint_source_id`: the buffer *is* the surface.

use std::collections::HashMap;

use vello_cpu::kurbo::{Affine, BezPath, Cap, Join, Point, Rect, Shape, Stroke};
use vello_cpu::peniko::color::{AlphaColor, Srgb};
use vello_cpu::peniko::Fill;
use vello_cpu::{RenderContext, RenderMode, Resources};

/// The largest canvas this engine will allocate, per side.
///
/// A page may ask for `<canvas width="1000000">`, and the buffer for one is
/// four terabytes. Bounded rather than trusted, for the same reason
/// `Max-Age=9223372036854775807` is bounded in the cookie jar: a number off the
/// page must not be able to end the session.
const MAX_SIDE: u32 = 8192;

/// What a canvas is when nobody said otherwise. The HTML default.
const DEFAULT_WIDTH: u32 = 300;
const DEFAULT_HEIGHT: u32 = 150;

/// One entry on the `save()`/`restore()` stack.
#[derive(Debug, Clone)]
struct State {
    transform: Affine,
    fill: AlphaColor<Srgb>,
    stroke: AlphaColor<Srgb>,
    line_width: f64,
    global_alpha: f64,
    line_cap: Cap,
    line_join: Join,
    fill_rule: Fill,
}

impl Default for State {
    fn default() -> Self {
        Self {
            transform: Affine::IDENTITY,
            // The spec's defaults: opaque black for both.
            fill: AlphaColor::from_rgba8(0, 0, 0, 255),
            stroke: AlphaColor::from_rgba8(0, 0, 0, 255),
            line_width: 1.0,
            global_alpha: 1.0,
            line_cap: Cap::Butt,
            line_join: Join::Miter,
            fill_rule: Fill::NonZero,
        }
    }
}

/// One canvas element's drawing surface.
pub struct Canvas {
    width: u32,
    height: u32,
    /// Everything drawn so far, RGBA8, premultiplied the way `vello_cpu`
    /// writes it.
    pixels: Vec<u8>,
    state: State,
    saved: Vec<State>,
    /// The path being built by `moveTo`/`lineTo`/`arc`/…
    path: BezPath,
    /// Where the current subpath began, for `closePath` and for the implicit
    /// close a fill performs.
    start: Option<Point>,
    /// Where the pen is, in user space.
    pen: Option<Point>,
    /// Set when anything has been drawn since the last flush, so a page that
    /// creates a canvas and never touches it costs no rasterisation.
    dirty: bool,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Self {
        let width = width.clamp(1, MAX_SIDE);
        let height = height.clamp(1, MAX_SIDE);
        Self {
            width,
            height,
            // Transparent black, which is what a fresh canvas is.
            pixels: vec![0; (width as usize) * (height as usize) * 4],
            state: State::default(),
            saved: Vec::new(),
            path: BezPath::new(),
            start: None,
            pen: None,
            dirty: false,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Whether anything has been drawn since this was created or last
    /// composited into the page.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Note that the page now holds this surface.
    ///
    /// Cleared after compositing rather than after drawing, so a page whose
    /// canvas has not moved since the last pass costs nothing — and a page that
    /// draws in a loop composites once per settle rather than once per call.
    pub fn mark_composited(&mut self) {
        self.dirty = false;
    }

    /// The surface, RGBA8, for compositing into the page.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Resizing clears, which is the spec's rule and is also how pages use it:
    /// `canvas.width = canvas.width` is the idiomatic erase.
    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.clamp(1, MAX_SIDE);
        let height = height.clamp(1, MAX_SIDE);
        *self = Canvas::new(width, height);
    }

    // ── state ────────────────────────────────────────────────────────────

    pub fn save(&mut self) {
        self.saved.push(self.state.clone());
    }

    /// `restore` on an empty stack is a no-op, not an error. The spec says so,
    /// and pages rely on it.
    pub fn restore(&mut self) {
        if let Some(state) = self.saved.pop() {
            self.state = state;
        }
    }

    pub fn set_fill_style(&mut self, colour: &str) -> bool {
        match parse_colour(colour) {
            Some(parsed) => {
                self.state.fill = parsed;
                true
            }
            // An unparseable colour leaves the previous one in place, which is
            // the spec's rule. Reported to the caller so it can name the value
            // rather than silently painting in the wrong colour.
            None => false,
        }
    }

    pub fn set_stroke_style(&mut self, colour: &str) -> bool {
        match parse_colour(colour) {
            Some(parsed) => {
                self.state.stroke = parsed;
                true
            }
            None => false,
        }
    }

    pub fn set_line_width(&mut self, width: f64) {
        // Zero, negative and non-finite are ignored per the spec.
        if width.is_finite() && width > 0.0 {
            self.state.line_width = width;
        }
    }

    pub fn set_global_alpha(&mut self, alpha: f64) {
        if alpha.is_finite() && (0.0..=1.0).contains(&alpha) {
            self.state.global_alpha = alpha;
        }
    }

    pub fn set_line_cap(&mut self, cap: &str) {
        self.state.line_cap = match cap {
            "round" => Cap::Round,
            "square" => Cap::Square,
            _ => Cap::Butt,
        };
    }

    pub fn set_line_join(&mut self, join: &str) {
        self.state.line_join = match join {
            "round" => Join::Round,
            "bevel" => Join::Bevel,
            _ => Join::Miter,
        };
    }

    // ── transforms ───────────────────────────────────────────────────────

    pub fn translate(&mut self, x: f64, y: f64) {
        self.state.transform *= Affine::translate((x, y));
    }

    pub fn scale(&mut self, x: f64, y: f64) {
        self.state.transform *= Affine::scale_non_uniform(x, y);
    }

    pub fn rotate(&mut self, radians: f64) {
        self.state.transform *= Affine::rotate(radians);
    }

    pub fn transform(&mut self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) {
        self.state.transform *= Affine::new([a, b, c, d, e, f]);
    }

    pub fn set_transform(&mut self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) {
        self.state.transform = Affine::new([a, b, c, d, e, f]);
    }

    pub fn reset_transform(&mut self) {
        self.state.transform = Affine::IDENTITY;
    }

    // ── paths ────────────────────────────────────────────────────────────

    pub fn begin_path(&mut self) {
        self.path = BezPath::new();
        self.start = None;
        self.pen = None;
    }

    pub fn move_to(&mut self, x: f64, y: f64) {
        let point = Point::new(x, y);
        self.path.move_to(point);
        self.start = Some(point);
        self.pen = Some(point);
    }

    pub fn line_to(&mut self, x: f64, y: f64) {
        let point = Point::new(x, y);
        // A `lineTo` with no current point starts the subpath instead, which is
        // what the spec says and what careless pages depend on.
        if self.pen.is_none() {
            self.move_to(x, y);
            return;
        }
        self.path.line_to(point);
        self.pen = Some(point);
    }

    pub fn quad_to(&mut self, cx: f64, cy: f64, x: f64, y: f64) {
        if self.pen.is_none() {
            self.move_to(cx, cy);
        }
        self.path.quad_to(Point::new(cx, cy), Point::new(x, y));
        self.pen = Some(Point::new(x, y));
    }

    pub fn curve_to(&mut self, c1x: f64, c1y: f64, c2x: f64, c2y: f64, x: f64, y: f64) {
        if self.pen.is_none() {
            self.move_to(c1x, c1y);
        }
        self.path.curve_to(
            Point::new(c1x, c1y),
            Point::new(c2x, c2y),
            Point::new(x, y),
        );
        self.pen = Some(Point::new(x, y));
    }

    pub fn close_path(&mut self) {
        if self.pen.is_some() {
            self.path.close_path();
            self.pen = self.start;
        }
    }

    pub fn rect(&mut self, x: f64, y: f64, w: f64, h: f64) {
        let rect = Rect::new(x, y, x + w, y + h);
        self.path.extend(rect.path_elements(0.1));
        // A `rect` leaves no current point for a following `lineTo`, per spec.
        self.pen = None;
        self.start = None;
    }

    /// An arc, appended to the current path.
    ///
    /// Built from `kurbo`'s own arc rather than by hand, so the flattening is
    /// the rasteriser's and a circle looks like one at every radius.
    pub fn arc(&mut self, x: f64, y: f64, radius: f64, start: f64, end: f64, ccw: bool) {
        if !radius.is_finite() || radius < 0.0 {
            return;
        }
        let sweep = arc_sweep(start, end, ccw);
        let arc = vello_cpu::kurbo::Arc::new(Point::new(x, y), (radius, radius), start, sweep, 0.0);
        let mut elements = arc.path_elements(0.1);
        // The first element of an arc is a `MoveTo`; a path already in progress
        // wants a line to the arc's start instead, which is the spec's rule.
        if let Some(first) = elements.next() {
            match (self.pen, first) {
                (Some(_), vello_cpu::kurbo::PathEl::MoveTo(to)) => self.path.line_to(to),
                (None, element) => {
                    self.path.push(element);
                    if let vello_cpu::kurbo::PathEl::MoveTo(to) = element {
                        self.start = Some(to);
                    }
                }
                (_, element) => self.path.push(element),
            }
        }
        for element in elements {
            self.path.push(element);
        }
        self.pen = Some(Point::new(
            x + radius * (start + sweep).cos(),
            y + radius * (start + sweep).sin(),
        ));
    }

    pub fn set_fill_rule(&mut self, rule: &str) {
        self.state.fill_rule = if rule == "evenodd" {
            Fill::EvenOdd
        } else {
            Fill::NonZero
        };
    }

    // ── drawing ──────────────────────────────────────────────────────────

    pub fn fill(&mut self) {
        let path = self.path.clone();
        self.paint_path(&path, true);
    }

    pub fn stroke(&mut self) {
        let path = self.path.clone();
        self.paint_path(&path, false);
    }

    pub fn fill_rect(&mut self, x: f64, y: f64, w: f64, h: f64) {
        if w == 0.0 || h == 0.0 {
            return;
        }
        let rect = Rect::new(x, y, x + w, y + h);
        let path: BezPath = rect.path_elements(0.1).collect();
        self.paint_path(&path, true);
    }

    pub fn stroke_rect(&mut self, x: f64, y: f64, w: f64, h: f64) {
        let rect = Rect::new(x, y, x + w, y + h);
        let path: BezPath = rect.path_elements(0.1).collect();
        self.paint_path(&path, false);
    }

    /// Erase a rectangle back to transparent.
    ///
    /// Done against the buffer rather than through the rasteriser: `vello_cpu`
    /// composites, and there is no blend mode here that turns opaque pixels
    /// transparent. Writing zeroes is what "clear" means.
    pub fn clear_rect(&mut self, x: f64, y: f64, w: f64, h: f64) {
        if w == 0.0 || h == 0.0 {
            return;
        }
        // Through the current transform, because `clearRect` is transformed
        // like everything else. Only the axis-aligned bounding box is cleared,
        // which is exact for the transforms pages actually use on it
        // (translate and scale) and conservative for a rotation.
        let rect = Rect::new(x, y, x + w, y + h);
        let bounds = self.state.transform.transform_rect_bbox(rect);
        let x0 = bounds.x0.floor().max(0.0) as usize;
        let y0 = bounds.y0.floor().max(0.0) as usize;
        let x1 = (bounds.x1.ceil().max(0.0) as usize).min(self.width as usize);
        let y1 = (bounds.y1.ceil().max(0.0) as usize).min(self.height as usize);
        for row in y0..y1 {
            let from = (row * self.width as usize + x0) * 4;
            let to = (row * self.width as usize + x1) * 4;
            if to <= self.pixels.len() && from < to {
                self.pixels[from..to].fill(0);
            }
        }
        self.dirty = true;
    }

    /// Rasterise one path onto the surface, filled or stroked.
    ///
    /// Each call is its own `RenderContext` rendered over the existing buffer.
    /// A canvas is drawn on incrementally by pages that call it in a loop, so
    /// the surface has to survive between calls; keeping one long-lived scene
    /// and replaying it would make every draw cost the whole history.
    fn paint_path(&mut self, path: &BezPath, fill: bool) {
        if path.elements().is_empty() {
            return;
        }
        let width = self.width as u16;
        let height = self.height as u16;

        let mut context = RenderContext::new(width, height);
        context.set_transform(self.state.transform);

        let colour = if fill {
            self.state.fill
        } else {
            self.state.stroke
        };
        let colour = colour.multiply_alpha(self.state.global_alpha as f32);
        context.set_paint(colour);

        if fill {
            context.set_fill_rule(self.state.fill_rule);
            context.fill_path(path);
        } else {
            context.set_stroke(Stroke {
                width: self.state.line_width,
                start_cap: self.state.line_cap,
                end_cap: self.state.line_cap,
                join: self.state.line_join,
                ..Default::default()
            });
            context.stroke_path(path);
        }
        context.flush();

        let mut layer = vec![0u8; self.pixels.len()];
        let mut resources = Resources::new();
        context.render_to_buffer(
            &mut resources,
            &mut layer,
            width,
            height,
            RenderMode::OptimizeQuality,
        );
        composite_over(&mut self.pixels, &layer);
        self.dirty = true;
    }

    /// The surface as a PNG, for `toDataURL`.
    pub fn to_png(&self) -> Option<Vec<u8>> {
        // Un-premultiply on the way out: `vello_cpu` writes premultiplied
        // alpha and PNG expects straight, so a half-transparent red written
        // premultiplied reads back as a darker red without this.
        let mut straight = Vec::with_capacity(self.pixels.len());
        for pixel in self.pixels.as_chunks::<4>().0 {
            let alpha = pixel[3];
            if alpha == 0 {
                straight.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let un = |channel: u8| -> u8 {
                ((channel as u32 * 255 + alpha as u32 / 2) / alpha as u32).min(255) as u8
            };
            straight.extend_from_slice(&[un(pixel[0]), un(pixel[1]), un(pixel[2]), alpha]);
        }

        let mut out = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut out);
        image::ImageEncoder::write_image(
            encoder,
            &straight,
            self.width,
            self.height,
            image::ExtendedColorType::Rgba8,
        )
        .ok()?;
        Some(out)
    }
}

/// Source-over compositing of one rasterised layer onto the surface.
///
/// Both sides are premultiplied, which is what makes this the plain Porter-Duff
/// form rather than something with a division in it.
fn composite_over(surface: &mut [u8], layer: &[u8]) {
    for (under, over) in surface
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(layer.as_chunks::<4>().0)
    {
        let alpha = over[3] as u32;
        if alpha == 0 {
            continue;
        }
        if alpha == 255 {
            under.copy_from_slice(over);
            continue;
        }
        let inverse = 255 - alpha;
        for channel in 0..4 {
            let blended = over[channel] as u32 + (under[channel] as u32 * inverse + 127) / 255;
            under[channel] = blended.min(255) as u8;
        }
    }
}

/// How far an arc sweeps, given the direction it was asked for.
///
/// The fiddly part of `arc()`, and the one every implementation gets wrong
/// once: a full circle is `0` to `2π`, and normalising that to a range would
/// turn it into a sweep of zero and draw nothing.
fn arc_sweep(start: f64, end: f64, ccw: bool) -> f64 {
    let full = std::f64::consts::TAU;
    let raw = end - start;
    if ccw {
        if raw <= -full {
            return -full;
        }
        let mut sweep = raw % full;
        if sweep > 0.0 {
            sweep -= full;
        }
        sweep
    } else {
        if raw >= full {
            return full;
        }
        let mut sweep = raw % full;
        if sweep < 0.0 {
            sweep += full;
        }
        sweep
    }
}

/// Parse the colour forms a canvas actually receives.
///
/// `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb()`, `rgba()`, and the named colours.
/// Deliberately not a full CSS colour parser: what is not understood is
/// *reported* to the caller, which reports it as an unsupported value rather
/// than painting in the wrong colour and looking as though it worked.
fn parse_colour(input: &str) -> Option<AlphaColor<Srgb>> {
    let text = input.trim();
    if let Some(hex) = text.strip_prefix('#') {
        return parse_hex(hex);
    }
    let lowered = text.to_ascii_lowercase();
    if let Some(rest) = lowered
        .strip_prefix("rgba(")
        .or_else(|| lowered.strip_prefix("rgb("))
    {
        let body = rest.strip_suffix(')')?;
        let parts: Vec<&str> = body
            .split([',', '/', ' '])
            .filter(|piece| !piece.trim().is_empty())
            .collect();
        if parts.len() < 3 {
            return None;
        }
        let channel = |text: &str| -> Option<u8> {
            let text = text.trim();
            if let Some(percent) = text.strip_suffix('%') {
                let value: f64 = percent.trim().parse().ok()?;
                Some((value / 100.0 * 255.0).clamp(0.0, 255.0) as u8)
            } else {
                let value: f64 = text.parse().ok()?;
                Some(value.clamp(0.0, 255.0) as u8)
            }
        };
        let r = channel(parts[0])?;
        let g = channel(parts[1])?;
        let b = channel(parts[2])?;
        let a = match parts.get(3) {
            Some(text) => {
                let text = text.trim();
                let value: f64 = match text.strip_suffix('%') {
                    Some(percent) => percent.trim().parse::<f64>().ok()? / 100.0,
                    None => text.parse().ok()?,
                };
                (value.clamp(0.0, 1.0) * 255.0).round() as u8
            }
            None => 255,
        };
        return Some(AlphaColor::from_rgba8(r, g, b, a));
    }
    named_colour(&lowered)
}

fn parse_hex(hex: &str) -> Option<AlphaColor<Srgb>> {
    let digit = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let bytes = hex.as_bytes();
    match bytes.len() {
        3 | 4 => {
            let mut channels = [255u8; 4];
            for (at, byte) in bytes.iter().enumerate() {
                let value = digit(*byte)?;
                channels[at] = value * 16 + value;
            }
            Some(AlphaColor::from_rgba8(
                channels[0],
                channels[1],
                channels[2],
                channels[3],
            ))
        }
        6 | 8 => {
            let mut channels = [255u8; 4];
            for at in 0..bytes.len() / 2 {
                let high = digit(bytes[at * 2])?;
                let low = digit(bytes[at * 2 + 1])?;
                channels[at] = high * 16 + low;
            }
            Some(AlphaColor::from_rgba8(
                channels[0],
                channels[1],
                channels[2],
                channels[3],
            ))
        }
        _ => None,
    }
}

/// The named colours a canvas is actually given.
///
/// Not the full CSS list: the rest fall through to being reported as
/// unsupported, which is the honest answer and a fixable one, rather than
/// silently black.
fn named_colour(name: &str) -> Option<AlphaColor<Srgb>> {
    let rgb = match name {
        "transparent" => return Some(AlphaColor::from_rgba8(0, 0, 0, 0)),
        "black" => (0, 0, 0),
        "silver" => (192, 192, 192),
        "gray" | "grey" => (128, 128, 128),
        "white" => (255, 255, 255),
        "maroon" => (128, 0, 0),
        "red" => (255, 0, 0),
        "purple" => (128, 0, 128),
        "fuchsia" | "magenta" => (255, 0, 255),
        "green" => (0, 128, 0),
        "lime" => (0, 255, 0),
        "olive" => (128, 128, 0),
        "yellow" => (255, 255, 0),
        "navy" => (0, 0, 128),
        "blue" => (0, 0, 255),
        "teal" => (0, 128, 128),
        "aqua" | "cyan" => (0, 255, 255),
        "orange" => (255, 165, 0),
        "pink" => (255, 192, 203),
        "brown" => (165, 42, 42),
        "gold" => (255, 215, 0),
        "indigo" => (75, 0, 130),
        "violet" => (238, 130, 238),
        "darkgray" | "darkgrey" => (169, 169, 169),
        "lightgray" | "lightgrey" => (211, 211, 211),
        _ => return None,
    };
    Some(AlphaColor::from_rgba8(rgb.0, rgb.1, rgb.2, 255))
}

/// Every canvas in one document, by node id.
#[derive(Default)]
pub struct Canvases {
    by_node: HashMap<usize, Canvas>,
}

impl Canvases {
    pub fn new() -> Self {
        Self::default()
    }

    /// The canvas for a node, created at the size the element asks for.
    pub fn get_or_create(&mut self, node_id: usize, width: u32, height: u32) -> &mut Canvas {
        self.by_node
            .entry(node_id)
            .or_insert_with(|| Canvas::new(width, height))
    }

    pub fn get(&self, node_id: usize) -> Option<&Canvas> {
        self.by_node.get(&node_id)
    }

    pub fn get_mut(&mut self, node_id: usize) -> Option<&mut Canvas> {
        self.by_node.get_mut(&node_id)
    }

    /// Every canvas that has been drawn on since the last flush.
    pub fn dirty(&self) -> Vec<usize> {
        self.by_node
            .iter()
            .filter(|(_, canvas)| canvas.is_dirty())
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.by_node.is_empty()
    }
}

/// The default size of a `<canvas>` with no attributes.
pub fn default_size() -> (u32, u32) {
    (DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pixel at (x, y), un-premultiplied, for assertions.
    fn pixel_at(canvas: &Canvas, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let at = ((y * canvas.width() + x) * 4) as usize;
        let p = &canvas.pixels()[at..at + 4];
        (p[0], p[1], p[2], p[3])
    }

    #[test]
    fn a_filled_rectangle_actually_puts_pixels_on_the_surface() {
        // The whole point of the module. A no-op stub passes every API-shape
        // test ever written and fails this one.
        let mut canvas = Canvas::new(100, 100);
        assert!(canvas.set_fill_style("#ff0000"));
        canvas.fill_rect(10.0, 10.0, 30.0, 30.0);

        let (r, g, b, a) = pixel_at(&canvas, 20, 20);
        assert_eq!((r, g, b, a), (255, 0, 0, 255), "inside the rectangle");
        assert_eq!(pixel_at(&canvas, 80, 80).3, 0, "outside it is untouched");
        assert!(canvas.is_dirty());
    }

    #[test]
    fn a_fresh_canvas_is_transparent_and_not_dirty() {
        let canvas = Canvas::new(10, 10);
        assert_eq!(pixel_at(&canvas, 5, 5), (0, 0, 0, 0));
        assert!(
            !canvas.is_dirty(),
            "a canvas nobody drew on must cost no rasterisation"
        );
    }

    #[test]
    fn transforms_move_what_is_drawn() {
        let mut canvas = Canvas::new(100, 100);
        canvas.set_fill_style("blue");
        canvas.translate(50.0, 50.0);
        canvas.fill_rect(0.0, 0.0, 20.0, 20.0);

        assert_eq!(pixel_at(&canvas, 60, 60).2, 255, "moved to the new origin");
        assert_eq!(pixel_at(&canvas, 10, 10).3, 0, "and not at the old one");
    }

    #[test]
    fn save_and_restore_put_the_state_back() {
        let mut canvas = Canvas::new(60, 60);
        canvas.set_fill_style("red");
        canvas.save();
        canvas.set_fill_style("blue");
        canvas.translate(30.0, 0.0);
        canvas.restore();
        canvas.fill_rect(0.0, 0.0, 10.0, 10.0);

        let (r, _, b, _) = pixel_at(&canvas, 5, 5);
        assert_eq!((r, b), (255, 0), "the colour came back");

        // And `restore` on an empty stack is a no-op rather than an error.
        canvas.restore();
        canvas.restore();
    }

    #[test]
    fn clear_rect_erases_back_to_transparent() {
        let mut canvas = Canvas::new(50, 50);
        canvas.set_fill_style("black");
        canvas.fill_rect(0.0, 0.0, 50.0, 50.0);
        assert_eq!(pixel_at(&canvas, 25, 25).3, 255);

        canvas.clear_rect(10.0, 10.0, 20.0, 20.0);
        assert_eq!(pixel_at(&canvas, 20, 20).3, 0, "erased");
        assert_eq!(pixel_at(&canvas, 45, 45).3, 255, "and only where asked");
    }

    #[test]
    fn a_stroked_path_draws_a_line() {
        let mut canvas = Canvas::new(100, 100);
        canvas.set_stroke_style("#00ff00");
        canvas.set_line_width(4.0);
        canvas.begin_path();
        canvas.move_to(10.0, 50.0);
        canvas.line_to(90.0, 50.0);
        canvas.stroke();

        assert!(pixel_at(&canvas, 50, 50).1 > 200, "on the line");
        assert_eq!(pixel_at(&canvas, 50, 10).3, 0, "away from it");
    }

    #[test]
    fn a_full_circle_is_not_a_sweep_of_zero() {
        // The arc bug every implementation ships once: `0` to `2π` normalised
        // into a range is a sweep of nothing, and the circle disappears.
        let mut canvas = Canvas::new(100, 100);
        canvas.set_fill_style("red");
        canvas.begin_path();
        canvas.arc(50.0, 50.0, 30.0, 0.0, std::f64::consts::TAU, false);
        canvas.fill();

        assert_eq!(pixel_at(&canvas, 50, 50).0, 255, "the middle is filled");
        assert_eq!(pixel_at(&canvas, 5, 5).3, 0, "the corner is not");
    }

    #[test]
    fn global_alpha_blends_rather_than_replacing() {
        let mut canvas = Canvas::new(40, 40);
        canvas.set_fill_style("black");
        canvas.fill_rect(0.0, 0.0, 40.0, 40.0);
        canvas.set_fill_style("white");
        canvas.set_global_alpha(0.5);
        canvas.fill_rect(0.0, 0.0, 40.0, 40.0);

        let (r, _, _, a) = pixel_at(&canvas, 20, 20);
        assert_eq!(a, 255, "still opaque");
        assert!(
            (100..=160).contains(&r),
            "half of white over black should be mid-grey, got {r}"
        );
    }

    #[test]
    fn resizing_clears_which_is_how_pages_erase() {
        let mut canvas = Canvas::new(50, 50);
        canvas.set_fill_style("red");
        canvas.fill_rect(0.0, 0.0, 50.0, 50.0);
        canvas.resize(50, 50);
        assert_eq!(pixel_at(&canvas, 25, 25).3, 0, "`canvas.width = w` erases");
    }

    #[test]
    fn an_absurd_size_is_clamped_rather_than_allocated() {
        // A number off the page must not be able to ask for four terabytes.
        let canvas = Canvas::new(10_000_000, 10_000_000);
        assert_eq!(canvas.width(), MAX_SIDE);
        assert_eq!(canvas.height(), MAX_SIDE);
    }

    #[test]
    fn colours_parse_in_the_forms_a_canvas_receives() {
        for form in ["#f00", "#ff0000", "#ff0000ff", "rgb(255,0,0)", "rgba(255,0,0,1)", "red"] {
            let parsed = parse_colour(form).unwrap_or_else(|| panic!("{form} should parse"));
            let [r, g, b, a] = parsed.to_rgba8().to_u8_array();
            assert_eq!((r, g, b, a), (255, 0, 0, 255), "{form}");
        }
        assert_eq!(parse_colour("transparent").unwrap().to_rgba8().to_u8_array()[3], 0);
    }

    /// A value this engine cannot parse is reported rather than painted wrong.
    /// The setter answering `false` is what the script layer turns into an
    /// `unsupported()` entry, which is what puts it in the snapshot's note.
    #[test]
    fn an_unparseable_colour_is_refused_rather_than_guessed() {
        let mut canvas = Canvas::new(10, 10);
        assert!(canvas.set_fill_style("red"));
        assert!(
            !canvas.set_fill_style("linear-gradient(red, blue)"),
            "a gradient is not a colour and must not be read as one"
        );
        // And the previous colour stands, per the spec.
        canvas.fill_rect(0.0, 0.0, 10.0, 10.0);
        assert_eq!(pixel_at(&canvas, 5, 5).0, 255);
    }

    #[test]
    fn to_png_round_trips_through_a_decoder() {
        let mut canvas = Canvas::new(20, 20);
        canvas.set_fill_style("#0000ff");
        canvas.fill_rect(0.0, 0.0, 20.0, 20.0);

        let png = canvas.to_png().expect("encodes");
        assert_eq!(&png[1..4], b"PNG", "it is really a PNG");
        let decoded = image::load_from_memory(&png).expect("decodes").to_rgba8();
        assert_eq!(decoded.dimensions(), (20, 20));
        let pixel = decoded.get_pixel(10, 10).0;
        assert_eq!((pixel[0], pixel[1], pixel[2], pixel[3]), (0, 0, 255, 255));
    }

    #[test]
    fn a_half_transparent_fill_survives_the_png_round_trip() {
        // Premultiplied in the buffer, straight in the file. Getting this
        // backwards darkens every semi-transparent pixel, which is the kind of
        // wrong that looks plausible.
        let mut canvas = Canvas::new(10, 10);
        canvas.set_fill_style("rgba(255, 0, 0, 0.5)");
        canvas.fill_rect(0.0, 0.0, 10.0, 10.0);

        let png = canvas.to_png().expect("encodes");
        let decoded = image::load_from_memory(&png).expect("decodes").to_rgba8();
        let pixel = decoded.get_pixel(5, 5).0;
        assert!(pixel[0] > 240, "red should still be full-strength: {pixel:?}");
        assert!((120..=136).contains(&pixel[3]), "alpha should be ~half: {pixel:?}");
    }
}
