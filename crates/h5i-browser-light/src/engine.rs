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

/// A request a form asked for, caught on its way to the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    pub url: Url,
    /// `GET` or `POST`. Anything else never reaches here: Blitz's own
    /// submission algorithm declines to produce it.
    pub method: String,
    /// The encoded body, for `POST`. Empty for `GET`, whose fields are already
    /// in the URL's query by the time it arrives.
    pub body: Vec<u8>,
    pub content_type: Option<String>,
}

/// A [`NavigationProvider`] that catches the request instead of following it.
///
/// Blitz calls this from inside `submit_form`, so the request arrives on the
/// same thread and is picked up immediately afterwards. The `Mutex` is here to
/// satisfy the trait's `Send + Sync` bound rather than to guard a race — the
/// page has exactly one owner (see `stream`'s module docs).
#[derive(Default)]
struct CapturedNavigation {
    slot: Arc<std::sync::Mutex<Option<Submission>>>,
}

impl blitz_traits::navigation::NavigationProvider for CapturedNavigation {
    fn navigate_to(&self, options: blitz_traits::navigation::NavigationOptions) {
        let (body, content_type) = match &options.document_resource {
            blitz_traits::net::Body::Form(form) => {
                let mut encoded = String::new();
                url::form_urlencoded::Serializer::new(&mut encoded).extend_pairs(
                    form.iter().filter_map(|entry| match &entry.value {
                        blitz_traits::net::EntryValue::String(value) => {
                            Some((entry.name.clone(), value.clone()))
                        }
                        // A file upload has no bytes this engine ever had: it
                        // would have to read the box's filesystem to fill one
                        // in, which is a capability a browser should not
                        // quietly acquire. Dropped, and the field is absent
                        // rather than empty so a server can tell.
                        _ => None,
                    }),
                );
                (
                    encoded.into_bytes(),
                    Some("application/x-www-form-urlencoded".to_string()),
                )
            }
            _ => (Vec::new(), options.content_type.clone()),
        };

        if let Ok(mut slot) = self.slot.lock() {
            *slot = Some(Submission {
                url: options.url.clone(),
                method: format!("{:?}", options.method).to_uppercase(),
                body,
                content_type,
            });
        }
    }
}

/// A loaded, resolved document.
pub struct Page {
    doc: BaseDocument,
    url: Url,
    options: PageOptions,
    /// Where [`CapturedNavigation`] leaves whatever the last form asked for.
    pending_navigation: Arc<std::sync::Mutex<Option<Submission>>>,
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

        let captured = CapturedNavigation::default();
        let pending_navigation = captured.slot.clone();

        let mut doc: BaseDocument = HtmlDocument::from_html(
            html,
            DocumentConfig {
                viewport: Some(viewport),
                base_url: Some(base_url.to_string()),
                net_provider: Some(Arc::new(BrokerNet::new(broker))),
                font_ctx: Some(fonts.context),
                // Forms dispatch through this. Without it Blitz's default
                // provider does nothing at all, and a submit would look like a
                // page that simply ignored the button.
                navigation_provider: Some(Arc::new(captured)),
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
            pending_navigation,
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

    /// Put `text` into a text field, replacing whatever was there.
    ///
    /// Replace rather than append, because the verb an agent reaches for is
    /// "this field should say X" and a verb that appended would make retrying
    /// after a failed submit produce `alicealice`.
    ///
    /// Returns `false` when the node is not something that takes text, so the
    /// caller can say which of "no such ref" and "that is a link, not a field"
    /// happened.
    pub fn type_into(&mut self, node_id: usize, text: &str) -> bool {
        let Some(node) = self.doc.get_node(node_id) else {
            return false;
        };
        if node
            .element_data()
            .and_then(|el| el.text_input_data())
            .is_none()
        {
            return false;
        }

        // Focus first: the caret is drawn from it, so a viewer watching sees
        // the field an agent is typing into rather than text appearing in a
        // box nothing is pointing at.
        self.doc.set_focus_to(node_id);
        self.doc.with_text_input(node_id, |mut driver| {
            driver.select_all();
            driver.insert_or_replace_selection(text);
        });
        // Typing changes layout — a longer value can reflow the form — and
        // nothing else in this file re-resolves on the agent's behalf.
        self.doc.resolve(0.0);
        true
    }

    /// What a text field currently holds.
    ///
    /// Read from the editor rather than the `value` attribute, because typing
    /// updates the former and leaves the latter at whatever the HTML said. A
    /// snapshot built from the attribute would show an agent the value it was
    /// served rather than the one it just typed.
    pub fn field_value(&self, node_id: usize) -> Option<String> {
        let node = self.doc.get_node(node_id)?;
        let input = node.element_data()?.text_input_data()?;
        Some(input.editor.text().to_string())
    }

    /// Submit the form that owns `node_id`, and return the request it produced.
    ///
    /// Blitz owns the hard part — the HTML form submission algorithm, which
    /// decides what is in the entry list, how it is encoded, and whether the
    /// method turns it into a query or a body. It dispatches the result to a
    /// [`blitz_traits::navigation::NavigationProvider`], so what this does is
    /// hand it a provider that captures the request instead of performing it,
    /// then return that request for the broker to police like any other.
    ///
    /// The alternative was reimplementing form encoding here, which is a spec
    /// with more corners than it looks and no security benefit: the boundary
    /// that matters is the wire, and the wire is still ours.
    pub fn submit_form(&mut self, node_id: usize) -> Result<Submission, H5iError> {
        // Blitz keeps a control-to-form map but does not expose it, so the
        // owner is found by walking up. That misses the `form=` attribute's
        // remote-owner case, which is rare enough to be a stated limit rather
        // than a reimplementation of the association algorithm.
        let form_id = self
            .enclosing_form(node_id)
            .ok_or_else(|| {
                H5iError::Metadata("that control is not inside a form this page defines".into())
            })?;

        self.doc.submit_form(form_id, node_id);

        self.pending_navigation
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
            .ok_or_else(|| {
                H5iError::Metadata(
                    "the form produced no request — its method or scheme is not one this \
                     engine submits (http and https, GET and POST)"
                        .into(),
                )
            })
    }

    /// Walk up for a `<form>`, for controls Blitz's owner map does not cover.
    fn enclosing_form(&self, node_id: usize) -> Option<usize> {
        let mut current = self.doc.get_node(node_id)?;
        for _ in 0..64 {
            if current
                .element_data()
                .is_some_and(|el| el.name.local.as_ref() == "form")
            {
                return Some(current.id);
            }
            current = self.doc.get_node(current.parent?)?;
        }
        None
    }

    /// How far down the document the viewport currently sits.
    pub fn scroll_offset(&self) -> (f64, f64) {
        let scroll = self.doc.viewport_scroll();
        (scroll.x, scroll.y)
    }

    /// The height of the document including whatever overflows the root box.
    ///
    /// Not `size.height`, and the difference is the whole page on a real site.
    /// A stylesheet that says `html, body { height: 100% }` — which is most of
    /// the web, Wikipedia included — sizes the root box to the viewport and
    /// lets the article overflow it. Reading `size.height` there reports a
    /// 40-screen article as exactly one screen tall, so `scroll_by` clamped
    /// every scroll to zero and the engine could only scroll unstyled pages.
    /// That is what the local test pages were, which is why nothing caught it
    /// until this ran against Wikipedia.
    pub fn content_height(&self) -> f64 {
        let layout = &self.doc.root_element().final_layout;
        layout.size.height.max(layout.content_size.height) as f64
    }

    /// How far this document can scroll: everything below the fold.
    ///
    /// Deliberately not taffy's `Layout::scroll_height`, which was tried and is
    /// the wrong question. That measures overflow *within* an element's own
    /// box, so it reads zero for an unstyled page whose root box simply grew to
    /// 4000px — there is no overflow inside the root, the overflow is past the
    /// viewport. The scrollable range of a document is its height minus the
    /// window, and the only thing that was ever wrong here is what "its height"
    /// meant.
    fn max_scroll_y(&self) -> f64 {
        (self.content_height() - self.options.height as f64).max(0.0)
    }

    /// Scroll, clamped to the document.
    ///
    /// Returns whether anything moved, which is what lets the live view stay
    /// at zero frames per second: a scroll at the bottom of the page is not a
    /// reason to encode and send an identical frame.
    pub fn scroll_by(&mut self, dx: f64, dy: f64) -> bool {
        let (x, y) = self.scroll_offset();
        let max_y = self.max_scroll_y();
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

    /// Load whatever a form asked for, through the same broker as everything
    /// else. A refused submission is an error the agent reads, not a blank page.
    pub fn open_submission(&self, submission: &Submission) -> Result<Page, H5iError> {
        let outcome = self.broker.send(
            &submission.url,
            Initiator::Navigation,
            &submission.method,
            &submission.body,
            submission.content_type.as_deref(),
        );
        if let Some(error) = outcome.error {
            return Err(H5iError::Metadata(format!(
                "could not submit to {}: {error}",
                submission.url
            )));
        }
        let html = String::from_utf8_lossy(&outcome.body).into_owned();
        Ok(Page::from_html(
            &html,
            &outcome.final_url,
            self.broker.clone(),
            self.fonts(),
            self.options.clone(),
        ))
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

/// Flatten the renderer's premultiplied RGBA onto an opaque white canvas.
///
/// Blitz paints no base layer, so a page that declares no `background-color`
/// comes back with every untouched pixel at `(0,0,0,0)`. The default text
/// colour is black, so simply dropping the alpha channel — which JPEG forces,
/// having none — turned the background black and the text with it, and the live
/// view arrived black-on-black. White is the canvas a real browser starts from,
/// so compositing onto it here is what makes an undeclared background look the
/// way the page's author saw it.
///
/// The buffer is premultiplied (verified: a 50%-red fill reads back as
/// `(128,0,0,128)`), so the source needs no scaling and the backdrop
/// contributes `255 - a` per channel. That is also why the PNG goes through
/// here: PNG is specified as *straight* alpha, so writing these bytes out with
/// their alpha attached rendered every translucent pixel too dark.
fn flatten_onto_white(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, H5iError> {
    let expected = width as usize * height as usize * 4;
    if rgba.len() < expected {
        return Err(H5iError::Internal(format!(
            "renderer produced {} bytes for a {width}x{height} frame, expected {expected}",
            rgba.len()
        )));
    }

    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for pixel in rgba[..expected].chunks_exact(4) {
        let backdrop = 255 - pixel[3];
        rgb.extend_from_slice(&[
            pixel[0].saturating_add(backdrop),
            pixel[1].saturating_add(backdrop),
            pixel[2].saturating_add(backdrop),
        ]);
    }
    Ok(rgb)
}

fn encode_jpeg(
    rgba: &[u8],
    width: u32,
    height: u32,
    quality: u8,
) -> Result<Vec<u8>, H5iError> {
    use image::codecs::jpeg::JpegEncoder;

    let rgb = flatten_onto_white(rgba, width, height)?;

    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, quality.clamp(1, 100))
        .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
        .map_err(|e| H5iError::Metadata(format!("failed to encode the frame: {e}")))?;
    Ok(jpeg)
}

/// Encode the screenshot, opaque.
///
/// Opaque rather than alpha-preserving on purpose: a screenshot of a page that
/// declared no background is not a transparency the caller asked for, it is a
/// canvas nobody painted, and handing it over as a hole means the image reads
/// differently against a light and a dark viewer. This is also what Chromium's
/// `captureScreenshot` does unless transparency is requested explicitly.
fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, H5iError> {
    use image::codecs::png::PngEncoder;
    use image::ImageEncoder;

    let rgb = flatten_onto_white(rgba, width, height)?;

    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&rgb, width, height, image::ExtendedColorType::Rgb8)
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

    /// Decode an encoded frame so a test can talk about pixels.
    fn decoded(encoded: &[u8]) -> image::RgbImage {
        image::load_from_memory(encoded)
            .expect("the frame decodes")
            .to_rgb8()
    }

    /// The bottom-right corner: past the end of any content these tests lay
    /// out, so it is the canvas and nothing else. Sampling there is what keeps
    /// these assertions independent of whether the host has fonts.
    const CANVAS: (u32, u32) = (399, 199);

    #[test]
    fn a_page_that_declares_no_background_renders_white_not_black() {
        let sink = Arc::new(MemorySink::new());
        let mut page = page_from("<!doctype html><body><p>hi</p></body>", Policy::new(), sink);

        let png = decoded(&page.screenshot_png().expect("screenshot"));
        assert_eq!(
            png.get_pixel(CANVAS.0, CANVAS.1).0,
            [255, 255, 255],
            "an undeclared background is the canvas, and the canvas is white"
        );

        // The JPEG is the one that was broken: it has no alpha channel to hide
        // an unpainted pixel in, so `(0,0,0,0)` became black and the default
        // black text became invisible on it.
        let jpeg = decoded(&page.screenshot_jpeg(85).expect("frame"));
        let corner = jpeg.get_pixel(CANVAS.0, CANVAS.1).0;
        assert!(
            corner.iter().all(|&c| c > 250),
            "the live view's frame should be white here, got {corner:?}"
        );
    }

    #[test]
    fn black_content_stays_visible_against_the_canvas() {
        // Black-on-black, stated without needing a glyph: the default text
        // colour painted as a box has to survive the flatten as black while
        // the canvas around it stays white.
        let sink = Arc::new(MemorySink::new());
        let mut page = page_from(
            "<!doctype html><style>html,body{margin:0}\
             div{width:100px;height:100px;background:#000}</style><div></div>",
            Policy::new(),
            sink,
        );

        let img = decoded(&page.screenshot_jpeg(90).expect("frame"));
        let inside = img.get_pixel(50, 50).0;
        let outside = img.get_pixel(CANVAS.0, CANVAS.1).0;
        assert!(inside.iter().all(|&c| c < 5), "the black box: {inside:?}");
        assert!(outside.iter().all(|&c| c > 250), "the canvas: {outside:?}");
    }

    #[test]
    fn a_translucent_fill_composites_onto_white_rather_than_darkening() {
        // The renderer's buffer is premultiplied, so a 50%-red fill arrives as
        // (128,0,0,128). Written out as straight alpha that reads as a dark
        // red; composited onto white it is the colour the page actually shows.
        let sink = Arc::new(MemorySink::new());
        let mut page = page_from(
            "<!doctype html><style>html,body{margin:0}\
             div{width:200px;height:100px;background:rgba(255,0,0,0.5)}</style><div></div>",
            Policy::new(),
            sink,
        );

        let img = decoded(&page.screenshot_png().expect("screenshot"));
        let [r, g, b] = img.get_pixel(100, 50).0;
        assert!(r > 250, "the red channel stays full, got {r}");
        assert!(
            (120..=135).contains(&g) && (120..=135).contains(&b),
            "green and blue should be lifted halfway to white, got {g} and {b}"
        );
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

#[cfg(test)]
mod input_tests {
    use super::*;
    use crate::policy::Policy;
    use crate::receipt::MemorySink;

    fn page_with(html: &str) -> Page {
        let broker = Arc::new(
            Broker::new(Policy::new(), Arc::new(MemorySink::new()), None).expect("broker"),
        );
        let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(4));
        let factory = PageFactory::new(broker, fonts.sources.clone(), PageOptions::default());
        factory.from_html(html, &Url::parse("https://site.example/page").unwrap())
    }

    fn ref_node(page: &Page, name: &str) -> usize {
        let snapshot = page.snapshot();
        snapshot
            .refs
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("no ref named {name} in {:?}", snapshot.refs))
            .node_id
    }

    const LOGIN: &str = "<html><body><form method='post' action='/session'>\
        <input type='text' name='user' placeholder='username'>\
        <input type='password' name='password' placeholder='password'>\
        <input type='submit' value='Go'></form></body></html>";

    #[test]
    fn typing_replaces_the_field_rather_than_appending_to_it() {
        // Append semantics would turn a retry after a failed submit into
        // `alicealice`, which is the kind of bug an agent cannot see.
        let mut page = page_with(LOGIN);
        let user = ref_node(&page, "username");

        assert!(page.type_into(user, "alice"));
        assert_eq!(page.field_value(user).as_deref(), Some("alice"));
        assert!(page.type_into(user, "bob"));
        assert_eq!(page.field_value(user).as_deref(), Some("bob"));
    }

    #[test]
    fn the_snapshot_shows_what_was_typed_not_what_was_served() {
        // Read from the editor, not the `value` attribute: an outline built
        // from the attribute would make `type` look like it silently failed.
        let mut page = page_with(LOGIN);
        let user = ref_node(&page, "username");
        page.type_into(user, "alice");

        let rendered = page.snapshot().render();
        assert!(rendered.contains("\"alice\""), "{rendered}");
    }

    #[test]
    fn typing_into_something_that_is_not_a_field_is_refused() {
        let mut page = page_with("<html><body><a href='/x'>a link</a></body></html>");
        let link = ref_node(&page, "a link");
        assert!(!page.type_into(link, "nope"));
    }

    #[test]
    fn a_post_form_becomes_a_post_with_the_typed_values_in_its_body() {
        let mut page = page_with(LOGIN);
        let user = ref_node(&page, "username");
        let password = ref_node(&page, "password");
        page.type_into(user, "alice");
        page.type_into(password, "hunter2");

        let submission = page.submit_form(ref_node(&page, "Go")).expect("submits");
        assert_eq!(submission.method, "POST");
        assert_eq!(submission.url.as_str(), "https://site.example/session");
        let body = String::from_utf8(submission.body).unwrap();
        assert!(body.contains("user=alice"), "{body}");
        assert!(body.contains("password=hunter2"), "{body}");
        assert_eq!(
            submission.content_type.as_deref(),
            Some("application/x-www-form-urlencoded")
        );
    }

    #[test]
    fn a_get_form_puts_its_fields_in_the_query_and_carries_no_body() {
        let mut page = page_with(
            "<html><body><form method='get' action='/search'>\
             <input type='text' name='q' placeholder='query'>\
             <input type='submit' value='Find'></form></body></html>",
        );
        let q = ref_node(&page, "query");
        page.type_into(q, "kelp forests");

        let submission = page.submit_form(ref_node(&page, "Find")).expect("submits");
        assert_eq!(submission.method, "GET");
        assert!(submission.body.is_empty(), "a GET carries no body");
        assert!(
            submission.url.query().unwrap_or_default().contains("q=kelp+forests"),
            "{}",
            submission.url
        );
    }

    #[test]
    fn a_control_outside_any_form_says_so_rather_than_submitting_nothing() {
        let mut page = page_with("<html><body><input type='text' placeholder='loose'></body></html>");
        let loose = ref_node(&page, "loose");
        let error = page.submit_form(loose).expect_err("nothing to submit");
        assert!(format!("{error}").contains("not inside a form"), "{error}");
    }
}
