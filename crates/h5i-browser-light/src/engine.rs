//! Loading a page and getting something back out of it.
//!
//! One shot, by design: fetch, parse, resolve, then answer questions about the
//! result (a snapshot, a screenshot, the text). There is no event loop and no
//! session here, because Tier 1 has no script to run and nothing that changes
//! the document after load. When Tier 2 adds a live view and Tier 3 adds
//! script, the loop belongs around this, not inside it.

use std::sync::Arc;

use anyrender::ImageRenderer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{BaseDocument, DocumentConfig};
use blitz_html::HtmlDocument;
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use h5i_error::H5iError;
use url::Url;

use crate::fonts::FontSetup;
use crate::net::{Broker, BrokerNet};
use crate::receipt::Initiator;
use crate::snapshot::Snapshot;

/// Viewport and budget for a page.
#[derive(Debug, Clone)]
pub struct PageOptions {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub max_snapshot_lines: usize,
}

impl Default for PageOptions {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            scale: 1.0,
            max_snapshot_lines: 500,
        }
    }
}

/// A loaded, resolved document.
pub struct Page {
    doc: BaseDocument,
    url: Url,
    options: PageOptions,
}

impl Page {
    /// Fetch a URL and load it.
    ///
    /// The navigation itself is policy-checked like any other request: asking
    /// to open a page is not a way around the allowlist, it is the first entry
    /// in the receipt.
    pub fn open(
        url: &Url,
        broker: Arc<Broker>,
        fonts: FontSetup,
        options: PageOptions,
    ) -> Result<Self, H5iError> {
        let outcome = broker.fetch(url, Initiator::Navigation);
        if let Some(error) = outcome.error {
            return Err(H5iError::Metadata(format!("could not open {url}: {error}")));
        }

        // Lossy on purpose: a page with one bad byte should render, and the
        // alternative is refusing a document over an encoding detail nobody
        // asked us to police.
        let html = String::from_utf8_lossy(&outcome.body).into_owned();
        let final_url = outcome.final_url.clone();

        Ok(Self::from_html(&html, &final_url, broker, fonts, options))
    }

    /// Load HTML that is already in hand (a local file, or a test fixture).
    ///
    /// Subresources still go through the broker, so a local file cannot pull
    /// a remote tracker without a policy decision and a receipt line.
    pub fn from_html(
        html: &str,
        base_url: &Url,
        broker: Arc<Broker>,
        fonts: FontSetup,
        options: PageOptions,
    ) -> Self {
        let viewport = Viewport::new(
            options.width,
            options.height,
            options.scale,
            ColorScheme::Light,
        );

        let mut doc: BaseDocument = HtmlDocument::from_html(
            html,
            DocumentConfig {
                viewport: Some(viewport),
                base_url: Some(base_url.to_string()),
                net_provider: Some(Arc::new(BrokerNet::new(broker))),
                font_ctx: Some(fonts.context),
                ..Default::default()
            },
        )
        .into_inner();

        // Twice, deliberately. The broker is synchronous, so subresources have
        // already completed by the time parsing returns, but their results
        // arrive as messages that `resolve` drains at its *start*. The first
        // pass applies the stylesheets; the second lays out with them.
        doc.resolve(0.0);
        doc.resolve(0.0);

        Self {
            doc,
            url: base_url.clone(),
            options,
        }
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The outline an agent reads.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot::capture(&self.doc, self.url.as_str(), self.options.max_snapshot_lines)
    }

    /// Rasterise the viewport and encode it as a PNG.
    pub fn screenshot_png(&mut self) -> Result<Vec<u8>, H5iError> {
        let width = self.options.width;
        let height = self.options.height;
        let scale = self.options.scale as f64;

        let mut renderer = VelloCpuImageRenderer::new(width, height);
        let mut rgba: Vec<u8> = Vec::new();
        let doc = &mut self.doc;
        renderer.render_to_vec(
            |scene| paint_scene(scene, doc, scale, width, height, 0, 0),
            &mut rgba,
        );

        encode_png(&rgba, width, height)
    }

    /// Rasterise the viewport and encode it as a JPEG.
    ///
    /// The live view wants JPEG rather than PNG: the viewers expect it, and a
    /// photographic-quality frame every scroll costs far less on the wire than
    /// a lossless one nobody is diffing.
    pub fn screenshot_jpeg(&mut self, quality: u8) -> Result<Vec<u8>, H5iError> {
        let width = self.options.width;
        let height = self.options.height;
        let scale = self.options.scale as f64;

        let mut renderer = VelloCpuImageRenderer::new(width, height);
        let mut rgba: Vec<u8> = Vec::new();
        let doc = &mut self.doc;
        renderer.render_to_vec(
            |scene| paint_scene(scene, doc, scale, width, height, 0, 0),
            &mut rgba,
        );

        encode_jpeg(&rgba, width, height, quality)
    }

    /// How far down the document the viewport currently sits.
    pub fn scroll_offset(&self) -> (f64, f64) {
        let scroll = self.doc.viewport_scroll();
        (scroll.x, scroll.y)
    }

    /// The laid-out height of the document, used to clamp scrolling.
    pub fn content_height(&self) -> f64 {
        self.doc.root_element().final_layout.size.height as f64
    }

    /// Scroll, clamped to the document.
    ///
    /// Returns whether anything moved, which is what lets the live view stay
    /// at zero frames per second: a scroll at the bottom of the page is not a
    /// reason to encode and send an identical frame.
    pub fn scroll_by(&mut self, dx: f64, dy: f64) -> bool {
        let (x, y) = self.scroll_offset();
        let max_y = (self.content_height() - self.options.height as f64).max(0.0);
        let next_x = (x + dx).max(0.0);
        let next_y = (y + dy).clamp(0.0, max_y);

        if (next_x - x).abs() < f64::EPSILON && (next_y - y).abs() < f64::EPSILON {
            return false;
        }
        self.doc.set_viewport_scroll(blitz_dom::Point {
            x: next_x,
            y: next_y,
        });
        true
    }

    /// The link at a viewport coordinate, resolved against the page's base.
    ///
    /// Hit-testing takes the scroll offset into account because the viewer
    /// reports where the human clicked on screen, not where that is in the
    /// document.
    pub fn link_at(&self, x: f32, y: f32) -> Option<Url> {
        let (scroll_x, scroll_y) = self.scroll_offset();
        let hit = self
            .doc
            .hit(x + scroll_x as f32, y + scroll_y as f32)?;

        // The hit lands on whatever box is topmost — often a text run inside
        // the anchor rather than the anchor itself — so walk up for the href.
        let mut node_id = hit.node_id;
        for _ in 0..16 {
            let node = self.doc.get_node(node_id)?;
            if let Some(href) = node
                .attrs()
                .and_then(|attrs| {
                    attrs
                        .iter()
                        .find(|attr| attr.name.local.as_ref() == "href")
                })
                .map(|attr| attr.value.as_str())
            {
                return self.url.join(href).ok();
            }
            node_id = node.parent?;
        }
        None
    }

    /// The document's visible text, for the case where the caller wants prose
    /// rather than structure.
    pub fn text(&self) -> String {
        self.snapshot()
            .lines
            .iter()
            .filter(|line| !line.text.is_empty())
            .map(|line| line.text.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Builds pages, so a session can navigate.
///
/// It exists because a `Page` consumes its `FontContext`, and parley's is not
/// cloneable — so following a link needs the ingredients kept aside rather
/// than the previous page's leftovers. Rebuilding registers the same font
/// files again, which costs a few milliseconds per navigation and buys a much
/// simpler ownership story than sharing a collection across documents.
pub struct PageFactory {
    broker: Arc<Broker>,
    font_sources: Vec<std::path::PathBuf>,
    options: PageOptions,
}

impl PageFactory {
    pub fn new(
        broker: Arc<Broker>,
        font_sources: Vec<std::path::PathBuf>,
        options: PageOptions,
    ) -> Self {
        Self {
            broker,
            font_sources,
            options,
        }
    }

    pub fn options(&self) -> &PageOptions {
        &self.options
    }

    pub fn broker(&self) -> &Arc<Broker> {
        &self.broker
    }

    fn fonts(&self) -> FontSetup {
        crate::fonts::load(&self.font_sources, &[], Some(self.font_sources.len()))
    }

    pub fn open(&self, url: &Url) -> Result<Page, H5iError> {
        Page::open(
            url,
            self.broker.clone(),
            self.fonts(),
            self.options.clone(),
        )
    }

    pub fn from_html(&self, html: &str, base_url: &Url) -> Page {
        Page::from_html(
            html,
            base_url,
            self.broker.clone(),
            self.fonts(),
            self.options.clone(),
        )
    }
}

fn encode_jpeg(
    rgba: &[u8],
    width: u32,
    height: u32,
    quality: u8,
) -> Result<Vec<u8>, H5iError> {
    use image::codecs::jpeg::JpegEncoder;

    let expected = width as usize * height as usize * 4;
    if rgba.len() < expected {
        return Err(H5iError::Internal(format!(
            "renderer produced {} bytes for a {width}x{height} frame, expected {expected}",
            rgba.len()
        )));
    }

    // JPEG has no alpha, and the frame is opaque anyway: drop the channel
    // rather than let the encoder guess.
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for pixel in rgba[..expected].chunks_exact(4) {
        rgb.extend_from_slice(&pixel[..3]);
    }

    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, quality.clamp(1, 100))
        .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
        .map_err(|e| H5iError::Metadata(format!("failed to encode the frame: {e}")))?;
    Ok(jpeg)
}

fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, H5iError> {
    use image::codecs::png::PngEncoder;
    use image::ImageEncoder;

    let expected = width as usize * height as usize * 4;
    if rgba.len() < expected {
        return Err(H5iError::Internal(format!(
            "renderer produced {} bytes for a {width}x{height} frame, expected {expected}",
            rgba.len()
        )));
    }

    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(
            &rgba[..expected],
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| H5iError::Metadata(format!("failed to encode the screenshot: {e}")))?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Policy;
    use crate::receipt::MemorySink;

    fn page_from(html: &str, policy: Policy, sink: Arc<MemorySink>) -> Page {
        let broker = Arc::new(Broker::new(policy, sink, None).expect("broker"));
        let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(4));
        Page::from_html(
            html,
            &Url::parse("https://example.com/").unwrap(),
            broker,
            fonts,
            PageOptions {
                width: 400,
                height: 200,
                ..Default::default()
            },
        )
    }

    #[test]
    fn a_document_becomes_an_outline_with_refs_on_the_actionable_parts() {
        let sink = Arc::new(MemorySink::new());
        let page = page_from(
            r#"<!doctype html><html><head><title>Docs</title></head><body>
                <h1>Getting started</h1>
                <p>Install it first.</p>
                <div><span><a href="/guide">Read the guide</a></span></div>
                <button>Run</button>
            </body></html>"#,
            Policy::new(),
            sink,
        );

        let snapshot = page.snapshot();
        let rendered = snapshot.render();

        assert_eq!(snapshot.title, "Docs");
        assert!(rendered.contains("heading1 \"Getting started\""), "{rendered}");
        assert!(rendered.contains("paragraph \"Install it first.\""), "{rendered}");
        // The link is wrapped in div>span, but the outline should not make an
        // agent walk through anonymous containers to find it.
        assert!(rendered.contains("link \"Read the guide\""), "{rendered}");
        assert_eq!(snapshot.refs.len(), 2, "link and button take refs");
        assert_eq!(snapshot.refs[0].role, "link");
        assert_eq!(snapshot.refs[1].role, "button");
    }

    #[test]
    fn actionable_elements_inside_prose_still_get_refs() {
        // The bug this pins: treating `p` and `label` as leaves and not
        // recursing lost the link inside the sentence and the input inside
        // the label — the two things an agent is most likely to want.
        let sink = Arc::new(MemorySink::new());
        let page = page_from(
            r#"<!doctype html><body>
                 <p>See the <a href="/guide">guide</a> for more.</p>
                 <label>Email <input type="email" placeholder="you@example.com"></label>
                 <label><input type="checkbox"> Subscribe</label>
                 <input type="hidden" name="csrf" value="secret-token">
               </body>"#,
            Policy::new(),
            sink,
        );

        let snapshot = page.snapshot();
        let roles: Vec<&str> = snapshot.refs.iter().map(|r| r.role.as_str()).collect();
        assert_eq!(roles, vec!["link", "textbox", "checkbox"], "{roles:?}");

        let rendered = snapshot.render();
        // The paragraph keeps its whole sentence...
        assert!(rendered.contains("paragraph \"See the guide for more.\""), "{rendered}");
        // ...and the link is addressable underneath it rather than lost in it.
        assert!(rendered.contains("link \"guide\" [ref=e1]"), "{rendered}");
        // An empty input is named by its placeholder, not left anonymous.
        assert!(rendered.contains("textbox \"you@example.com\""), "{rendered}");
        // The hidden CSRF field is not something to act on, and its value is
        // not something to put in front of a model.
        assert!(!rendered.contains("secret-token"), "{rendered}");
    }

    #[test]
    fn an_image_is_named_by_its_alt_text() {
        let sink = Arc::new(MemorySink::new());
        let page = page_from(
            r#"<!doctype html><body><img src="/d.png" alt="Architecture diagram"></body>"#,
            Policy::new().allow("example.com"),
            sink,
        );
        let snapshot = page.snapshot();
        assert_eq!(snapshot.refs.len(), 1);
        assert_eq!(snapshot.refs[0].name, "Architecture diagram");
    }

    #[test]
    fn script_and_style_never_reach_the_snapshot() {
        let sink = Arc::new(MemorySink::new());
        let page = page_from(
            r#"<!doctype html><html><head>
                 <style>.a { color: red }</style>
               </head><body>
                 <script>var secret = "do not exfiltrate";</script>
                 <p>Visible</p>
               </body></html>"#,
            Policy::new(),
            sink,
        );

        let rendered = page.snapshot().render();
        assert!(rendered.contains("Visible"));
        assert!(!rendered.contains("do not exfiltrate"), "{rendered}");
        assert!(!rendered.contains("color: red"), "{rendered}");
    }

    #[test]
    fn a_third_party_subresource_is_denied_and_the_page_still_renders() {
        // This is the whole product in one test: the tracker never loads, the
        // decision is recorded, and the page is not collateral damage.
        let sink = Arc::new(MemorySink::new());
        let mut page = page_from(
            r#"<!doctype html><html><body>
                 <link rel="stylesheet" href="https://cdn.tracker.test/s.css">
                 <img src="https://cdn.tracker.test/pixel.gif">
                 <h1>Still here</h1>
               </body></html>"#,
            Policy::new().allow("example.com"),
            sink.clone(),
        );

        let denied = sink.denied_urls();
        assert!(
            denied.iter().any(|u| u.contains("s.css")),
            "the stylesheet should be refused: {denied:?}"
        );
        assert!(
            denied.iter().any(|u| u.contains("pixel.gif")),
            "the tracking pixel should be refused: {denied:?}"
        );
        assert!(sink.fetched_urls().is_empty(), "nothing should reach the wire");

        assert!(page.snapshot().render().contains("Still here"));

        // And the screenshot is a real frame, not the blank one you get when a
        // denied resource is left pending forever (see `net`'s module docs).
        let png = page.screenshot_png().expect("screenshot encodes");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']), "png magic");
        assert!(png.len() > 100, "a blank-refusal render would be tiny");
    }

    #[test]
    fn screenshot_dimensions_follow_the_viewport() {
        let sink = Arc::new(MemorySink::new());
        let mut page = page_from("<!doctype html><p>hi</p>", Policy::new(), sink);
        let png = page.screenshot_png().expect("screenshot");

        // PNG IHDR carries width/height big-endian at a fixed offset.
        let width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
        let height = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
        assert_eq!((width, height), (400, 200));
    }

    #[test]
    fn text_extraction_drops_the_structure_and_keeps_the_prose() {
        let sink = Arc::new(MemorySink::new());
        let page = page_from(
            "<!doctype html><body><h1>Title</h1><div><p>Body copy.</p></div></body>",
            Policy::new(),
            sink,
        );
        let text = page.text();
        assert!(text.contains("Title"));
        assert!(text.contains("Body copy."));
    }
}
