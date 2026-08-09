//! Loading a page and getting something back out of it.
//!
//! One shot, by design: fetch, parse, resolve, then answer questions about the
//! result (a snapshot, a screenshot, the text). There is no event loop and no
//! session here, because Tier 1 has no script to run and nothing that changes
//! the document after load. When Tier 2 adds a live view and Tier 3 adds
//! script, the loop belongs around this, not inside it.

use std::cell::RefCell;
use std::rc::Rc;
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
    /// Run the page's own scripts.
    ///
    /// Off by default and opt-in at every layer above, because turning it on is
    /// a change to what an untrusted page can do inside the box rather than a
    /// rendering preference (ROADMAP §12.5).
    pub script: bool,
}

impl Default for PageOptions {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            scale: 1.0,
            max_snapshot_lines: 500,
            script: false,
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

/// The one real DOM, shared with the script realm.
///
/// `Rc<RefCell<_>>` rather than ownership because JavaScript reaches the same
/// tree: a native binding invoked from a callback needs the document long after
/// the call that registered it returned. `Rc` and not `Arc` because none of this
/// crosses a thread — `Page` is not `Send` (see `stream`'s module docs), which
/// is the constraint the whole session architecture already bends around.
///
/// The borrow discipline that keeps this from panicking: **a binding takes the
/// borrow, mutates, and drops it before returning to JS.** Blitz's mutations
/// never call back into script, so no binding can re-enter while holding one.
pub type Dom = Rc<RefCell<BaseDocument>>;

/// A loaded, resolved document.
pub struct Page {
    doc: Dom,
    url: Url,
    options: PageOptions,
    /// Where [`CapturedNavigation`] leaves whatever the last form asked for.
    pending_navigation: Arc<std::sync::Mutex<Option<Submission>>>,
    /// The script realm, when this page has one. `None` when script is off,
    /// which is still the default: `capabilities.javascript` is the gate, and
    /// flipping it is a threat-model decision rather than a feature flag
    /// (ROADMAP §12.5).
    script: Option<crate::script::Script>,
    /// Whether `run_scripts` was called, regardless of whether it built a realm.
    ///
    /// A page with no script elements never gets one, so `script.is_some()`
    /// alone cannot tell "script is off" from "there was nothing to run".
    ran_scripts: bool,
    /// What the last settle did, for the snapshot to report.
    settled: Option<crate::script::Settled>,
    /// Engine-level facts the next snapshot should carry.
    notes: Vec<String>,
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
        let mut target = url.clone();
        // Where a `<meta refresh>` chain has already been, so a page that
        // refreshes to itself — which news and dashboard pages do on purpose —
        // is loaded once rather than forever.
        let mut visited: Vec<Url> = vec![url.clone()];
        let mut followed: Vec<String> = Vec::new();

        for _ in 0..=MAX_META_REFRESH_HOPS {
            let outcome = broker.fetch(&target, Initiator::Navigation);
            if let Some(error) = outcome.error {
                return Err(H5iError::Metadata(format!("could not open {target}: {error}")));
            }

            // Lossy on purpose: a page with one bad byte should render, and the
            // alternative is refusing a document over an encoding detail nobody
            // asked us to police.
            let html = String::from_utf8_lossy(&outcome.body).into_owned();
            let final_url = outcome.final_url.clone();
            let status = outcome.status.unwrap_or(0);

            // Decided before the page is built, because the marker lives in the
            // markup we already hold and building is the expensive half.
            let refresh = meta_refresh(&html, &final_url);
            if let Some((delay, next)) = &refresh {
                if *delay <= META_REFRESH_MAX_DELAY_SECONDS && !visited.contains(next) {
                    followed.push(final_url.to_string());
                    visited.push(next.clone());
                    target = next.clone();
                    continue;
                }
            }

            let mut page = Self::from_html(&html, &final_url, broker, fonts, options);

            // An HTTP error still has a body, and rendering it silently is how
            // an agent ends up reading a 404 page as though it were the page it
            // asked for. Found by the corpus: crates.io answered 404, the
            // outline came back empty, and nothing anywhere said why.
            if !(200..300).contains(&status) {
                page.note(&format!(
                    "the server answered {status} for this URL; what follows is whatever it \
                     returned with that status, not the page that was asked for"
                ));
            }

            // Frames are not loaded, and `contentDocument` answers null for
            // them exactly as a browser does for a frame it will not let you
            // into. Null is the right answer to give *script*; it is the wrong
            // thing to leave an agent to infer, because the missing content
            // looks like content the page never had.
            let frames = {
                let doc = page.doc.borrow();
                doc.query_selector_all("iframe, frame")
                    .map(|ids| ids.len())
                    .unwrap_or(0)
            };
            if frames > 0 {
                page.note(&format!(
                    "this page has {frames} frame(s), whose content this engine does not load: \
                     anything inside them is absent from the outline below, and script reading \
                     `contentDocument` gets null"
                ));
            }

            // A challenge is not the page, and an outline of one reads as a
            // page that is simply empty. Naming it is the difference between an
            // agent concluding "there is nothing here" and "I was blocked".
            if let Some(marker) = challenge_marker(&html) {
                page.note(&format!(
                    "this looks like a bot challenge rather than the page that was asked for \
                     (it says \"{marker}\"). The content below is the challenge. This engine \
                     runs script but solves no proof-of-work and has no browser fingerprint to \
                     offer, so this site is not readable from here."
                ));
            }

            for from in &followed {
                page.note(&format!(
                    "{from} asked for a <meta refresh> and this engine followed it; what \
                     follows is the page it named"
                ));
            }

            // Present, but not followed. Saying which is the point: a page that
            // refreshes in ten minutes is a page that intends to update itself,
            // not one that redirected.
            if let Some((delay, next)) = refresh {
                if delay > META_REFRESH_MAX_DELAY_SECONDS {
                    page.note(&format!(
                        "this page asks to reload itself as {next} after {delay}s; that is a \
                         page updating itself rather than a redirect, so it was not followed"
                    ));
                } else if visited.iter().filter(|v| **v == next).count() > 0 && followed.is_empty()
                {
                    page.note(&format!(
                        "this page's <meta refresh> points at {next}, which is where we already \
                         are; it was not followed"
                    ));
                }
            }

            return Ok(page);
        }

        Err(H5iError::Metadata(format!(
            "could not open {url}: it redirected through more than {MAX_META_REFRESH_HOPS} \
             <meta refresh> hops without arriving anywhere"
        )))
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
                // Without this Blitz uses `DummyHtmlParserProvider` and
                // `set_inner_html` silently does nothing: the old children are
                // dropped and no new ones are parsed, so `el.innerHTML = x`
                // empties the element. Supplying the real parser is what makes
                // innerHTML, insertAdjacentHTML and template content work.
                html_parser_provider: Some(Arc::new(blitz_html::HtmlProvider)),
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
            doc: Rc::new(RefCell::new(doc)),
            url: base_url.clone(),
            options,
            pending_navigation,
            script: None,
            ran_scripts: false,
            settled: None,
            notes: Vec::new(),
        }
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Run the page's own scripts, then settle.
    ///
    /// Separate from loading because it is a policy decision, not a parsing
    /// step: a caller that has not opted into script gets a page whose
    /// `<script>` elements are inert, which is exactly what tiers 1 and 2 were.
    pub fn run_scripts(&mut self, broker: Arc<Broker>) -> Result<(), H5iError> {
        // In document order, inline and external together, because execution
        // order is semantics: a bundle that defines a global in one script and
        // uses it in the next breaks if they are reordered.
        enum Source {
            Inline(String),
            External(String),
            /// `type="module"`, inline or external. Deferred by definition and
            /// therefore kept apart: modules evaluate after every classic
            /// script, in their own document order.
            ModuleInline(String),
            ModuleExternal(String),
        }

        /// A script and the element it came from, so `document.currentScript`
        /// can name that element while the code runs.
        type Pending = (usize, Source);

        let sources: Vec<Pending> = {
            let doc = self.doc.borrow();
            doc.query_selector_all("script")
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| {
                            let node = doc.get_node(*id)?;
                            let attr = |name: &str| {
                                node.attrs().and_then(|attrs| {
                                    attrs
                                        .iter()
                                        .find(|a| a.name.local.as_ref() == name)
                                        .map(|a| a.value.to_string())
                                })
                            };
                            // What the `type` attribute means, which is not
                            // "run it anyway". A page embeds data in script
                            // elements — `application/json` for state,
                            // `text/template` for markup — and those are data
                            // blocks the spec says never execute. Running them
                            // parses JSON as JavaScript and fills the console
                            // with syntax errors that blame the page for
                            // something it never asked us to do. Found by
                            // pointing this at github.com.
                            let kind = attr("type").unwrap_or_default();
                            let kind = kind.trim().to_ascii_lowercase();
                            let is_module = kind == "module";
                            let is_classic = kind.is_empty()
                                || matches!(
                                    kind.as_str(),
                                    "text/javascript"
                                        | "application/javascript"
                                        | "text/ecmascript"
                                        | "application/ecmascript"
                                        | "module"
                                );
                            if !is_classic {
                                return None;
                            }

                            let source = match (attr("src"), is_module) {
                                (Some(src), true) => Source::ModuleExternal(src),
                                (Some(src), false) => Source::External(src),
                                (None, is_module) => {
                                    let text = node.text_content();
                                    if text.trim().is_empty() {
                                        return None;
                                    } else if is_module {
                                        Source::ModuleInline(text)
                                    } else {
                                        Source::Inline(text)
                                    }
                                }
                            };
                            Some((*id, source))
                        })
                        .collect()
                })
                .unwrap_or_default()
        };

        // Nothing to run means nothing to build. Starting the realm costs about
        // 15ms — the prelude is 113 KiB of JavaScript, parsed and evaluated from
        // scratch — and a page with no script elements was paying all of it for
        // a realm that would never be asked a question.
        if sources.is_empty() {
            self.ran_scripts = true;
            // Trivially settled, and said so rather than left null: a page with
            // no script has finished by definition, and "we do not know" is a
            // different answer that an agent would have to act on differently.
            self.settled = Some(crate::script::Settled {
                elapsed_ms: 0,
                timers_run: 0,
                cut_off: false,
                pending_timers: 0,
            });
            return Ok(());
        }

        let mut script = crate::script::Script::new(self.dom(), broker.clone(), &self.url)
            .map_err(H5iError::Metadata)?;

        // Classic scripts first, in document order, then modules in document
        // order. That is the deferred semantics `type="module"` carries: a
        // module never runs before a classic script that follows it in the
        // markup, and a page that relies on that ordering breaks if we run them
        // as they appear.
        let (classic, modules): (Vec<Pending>, Vec<Pending>) =
            sources.into_iter().partition(|(_, source)| {
                matches!(source, Source::Inline(_) | Source::External(_))
            });

        let phase_started = std::time::Instant::now();
        let mut skipped = 0usize;

        for (index, (node, source)) in classic.into_iter().enumerate() {
            if phase_started.elapsed() >= SCRIPT_PHASE_BUDGET {
                skipped += 1;
                continue;
            }
            // Which script this was. Boa 0.19 reports neither a line number nor
            // a stack, so the element is the only locus available — and a bare
            // "TypeError: cannot convert null" with no locus at all is the
            // hardest kind of error for an agent to act on.
            let where_from = match &source {
                Source::External(src) => src.clone(),
                _ => format!("inline script #{}", index + 1),
            };
            let code = match source {
                Source::Inline(text) => text,
                Source::External(src) => {
                    // Fetched through the broker like every other subresource,
                    // so a script file is policy-checked and receipted before it
                    // is ever executed. A refusal is reported and the page runs
                    // without it, which is what the agent needs to know.
                    let Ok(url) = self.url.join(&src) else {
                        script.note_error(&format!("script src `{src}` is not a URL"));
                        continue;
                    };
                    let outcome = broker.fetch(&url, crate::receipt::Initiator::Subresource);
                    if let Some(error) = outcome.error {
                        script.note_refused_script(url.as_str());
                        script.note_error(&format!("could not load {url}: {error}"));
                        continue;
                    }
                    // Same rule as modules: an error page is not a script, and
                    // running one produces a syntax error that blames the page.
                    let status = outcome.status.unwrap_or(0);
                    if !(200..300).contains(&status) {
                        script.note_refused_script(url.as_str());
                        script.note_error(&format!(
                            "could not load {url}: the server answered {status}"
                        ));
                        continue;
                    }
                    String::from_utf8_lossy(&outcome.body).into_owned()
                }
                _ => unreachable!("partitioned above"),
            };

            script.set_current_script(Some(node));
            if let Err(error) = script.eval_named(&code, &where_from) {
                // Reported, not fatal: a page with one broken script is still a
                // page, and the agent needs to know which half it is reading.
                //
                // Recorded as not-run too: a bundle that threw halfway leaves
                // its globals undefined exactly as a refused one does, and the
                // ReferenceError that follows should blame this, not the engine.
                script.note_refused_script(&where_from);
                script.note_error(&format!("{where_from}: {error}"));
            }
        }
        // Null again once the classic scripts are done, because that is what a
        // module and a later callback are supposed to see.
        script.set_current_script(None);

        for (_, source) in modules {
            if phase_started.elapsed() >= SCRIPT_PHASE_BUDGET {
                skipped += 1;
                continue;
            }
            let (code, path) = match source {
                Source::ModuleInline(text) => (text, self.url.to_string()),
                Source::ModuleExternal(src) => {
                    // Fetched here rather than by the loader because this is the
                    // entry point rather than an import, but through the same
                    // broker and with the same origin, so it is receipted and
                    // policed identically.
                    let Ok(url) = self.url.join(&src) else {
                        script.note_error(&format!("module src `{src}` is not a URL"));
                        continue;
                    };
                    let outcome = broker.fetch(&url, crate::receipt::Initiator::Subresource);
                    if let Some(error) = outcome.error {
                        script.note_error(&format!("could not load {url}: {error}"));
                        continue;
                    }
                    let status = outcome.status.unwrap_or(0);
                    if !(200..300).contains(&status) {
                        script.note_error(&format!(
                            "could not load {url}: the server answered {status}"
                        ));
                        continue;
                    }
                    let text = String::from_utf8_lossy(&outcome.body).into_owned();
                    (text, outcome.final_url.to_string())
                }
                _ => unreachable!("partitioned above"),
            };

            if let Err(error) = script.eval_module(&code, &path) {
                script.note_error(&error);
            }
        }

        let settled = script.settle();
        if script.take_dirty() {
            self.doc.borrow_mut().resolve(0.0);
        }
        // Drained and discarded: these are the page *loading* — its module
        // graph and any fetch its startup made. Leaving them queued would
        // attribute them to whatever the agent did first, and "this click
        // caused these requests" is the one claim here that has to be exact.
        if skipped > 0 {
            script.note_error(&format!(
                "this page's scripts took longer than {}s, so {skipped} of them were not run. \
                 What follows was rendered by the ones that finished.",
                SCRIPT_PHASE_BUDGET.as_secs()
            ));
        }

        let _ = script.take_requests();
        self.settled = Some(settled);
        self.script = Some(script);
        self.ran_scripts = true;
        Ok(())
    }

    /// Fire a real event at a node and let the page respond.
    pub fn dispatch_event(
        &mut self,
        node_id: usize,
        kind: &str,
    ) -> Option<Vec<crate::script::host::RequestLink>> {
        let script = self.script.as_mut()?;
        let _ = script.dispatch(node_id, kind);
        let settled = script.settle();
        let dirty = script.take_dirty();
        let requests = script.take_requests();
        self.settled = Some(settled);
        if dirty {
            self.doc.borrow_mut().resolve(0.0);
        }
        Some(requests)
    }

    /// Record an engine-level fact for the next snapshot to carry.
    pub fn note(&mut self, text: &str) {
        self.notes.push(text.to_string());
    }

    /// What the last settle did, if script ran.
    pub fn settled(&self) -> Option<&crate::script::Settled> {
        self.settled.as_ref()
    }

    /// Web APIs the page asked for and this engine does not have.
    pub fn unsupported(&self) -> Vec<(String, usize)> {
        self.script
            .as_ref()
            .map(|s| s.unsupported())
            .unwrap_or_default()
    }

    /// What the page logged, for the console pane and the receipt.
    pub fn console(&self) -> Vec<crate::script::host::ConsoleLine> {
        self.script.as_ref().map(|s| s.console()).unwrap_or_default()
    }

    pub fn has_script(&self) -> bool {
        // Whether this page ran script, not whether a realm exists: a page with
        // no script elements never builds one, and reporting that as "script is
        // off" would describe the session wrongly.
        self.ran_scripts
    }

    /// A handle to the document, for the script realm.
    ///
    /// Handing out the `Rc` rather than a reference is the point: the script
    /// realm outlives any single call into it, and both sides must see one tree.
    pub fn dom(&self) -> Dom {
        self.doc.clone()
    }

    /// Re-resolve style and layout after script changed the tree.
    ///
    /// Called once after a settle rather than after each mutation: a script that
    /// appends fifty rows should lay out once, not fifty times.
    pub fn refresh(&mut self) {
        self.doc.borrow_mut().resolve(0.0);
    }

    /// The outline an agent reads.
    ///
    /// Carries the engine's own notes alongside it: whether the page had
    /// finished settling, and which Web APIs it asked for that this engine does
    /// not have. Both are outside the fence because both are facts about the
    /// reading rather than about the page, and both exist so an agent can tell
    /// "this page is empty" from "this page needed something I lack".
    pub fn snapshot(&self) -> Snapshot {
        let mut snapshot = Snapshot::capture(
            &self.doc.borrow(),
            self.url.as_str(),
            self.options.max_snapshot_lines,
        );

        snapshot.notes.extend(self.notes.iter().cloned());

        // Silence is the one answer an agent cannot act on. A page with nothing
        // in it is either genuinely empty, blocked, or built by script this
        // engine could not run — and which of those it is belongs in the
        // outline rather than in the agent's imagination.
        if snapshot.lines.is_empty() {
            let scripts = self.doc.borrow().query_selector_all("script").map(|s| s.len()).unwrap_or(0);
            snapshot.notes.push(format!(
                "this page produced no readable content. It has {scripts} script element(s) \
                 and this engine {}. If it needs JavaScript beyond what is listed above, the \
                 chromium engine has more of it.",
                match (self.script.is_some(), self.ran_scripts, scripts) {
                    (true, _, _) => "ran them",
                    // No realm because there was nothing to run, which is a
                    // different fact from script being switched off.
                    (false, true, 0) => "had none to run",
                    (false, true, _) => "ran them",
                    (false, false, _) => "did not run them (script is off)",
                }
            ));
        }

        if let Some(settled) = &self.settled {
            if settled.cut_off {
                snapshot.notes.push(settled.render());
            }
        }

        let unsupported = self.unsupported();
        if !unsupported.is_empty() {
            let listed = unsupported
                .iter()
                .take(6)
                .map(|(name, count)| format!("{name} x{count}"))
                .collect::<Vec<_>>()
                .join(", ");
            snapshot.notes.push(format!(
                "this page used Web APIs this engine does not have ({listed}). \
                 What depends on them did not run; the chromium engine has them."
            ));
        }

        snapshot
    }

    /// Rasterise the viewport and encode it as a PNG.
    pub fn screenshot_png(&mut self) -> Result<Vec<u8>, H5iError> {
        let width = self.options.width;
        let height = self.options.height;
        let scale = self.options.scale as f64;

        let mut renderer = VelloCpuImageRenderer::new(width, height);
        let mut rgba: Vec<u8> = Vec::new();
        let mut doc = self.doc.borrow_mut();
        renderer.render_to_vec(
            |scene| paint_scene(scene, &mut doc, scale, width, height, 0, 0),
            &mut rgba,
        );
        drop(doc);

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
        let mut doc = self.doc.borrow_mut();
        renderer.render_to_vec(
            |scene| paint_scene(scene, &mut doc, scale, width, height, 0, 0),
            &mut rgba,
        );
        drop(doc);

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
        let mut doc = self.doc.borrow_mut();
        let takes_text = doc
            .get_node(node_id)
            .and_then(|node| node.element_data())
            .and_then(|el| el.text_input_data())
            .is_some();
        if !takes_text {
            return false;
        }

        // Focus first: the caret is drawn from it, so a viewer watching sees
        // the field an agent is typing into rather than text appearing in a
        // box nothing is pointing at.
        doc.set_focus_to(node_id);
        doc.with_text_input(node_id, |mut driver| {
            driver.select_all();
            driver.insert_or_replace_selection(text);
        });
        // Typing changes layout — a longer value can reflow the form — and
        // nothing else in this file re-resolves on the agent's behalf.
        doc.resolve(0.0);
        drop(doc);

        // A *user* edit fires input then change, in that order. Script setting
        // `.value` does not, and must not, or a framework that re-renders on
        // its own write would loop. This is the user path, so it fires.
        if let Some(script) = self.script.as_mut() {
            let _ = script.dispatch(node_id, "input");
            let _ = script.dispatch(node_id, "change");
            let settled = script.settle();
            let dirty = script.take_dirty();
            self.settled = Some(settled);
            if dirty {
                self.doc.borrow_mut().resolve(0.0);
            }
        }
        true
    }

    /// What a text field currently holds.
    ///
    /// Read from the editor rather than the `value` attribute, because typing
    /// updates the former and leaves the latter at whatever the HTML said. A
    /// snapshot built from the attribute would show an agent the value it was
    /// served rather than the one it just typed.
    pub fn field_value(&self, node_id: usize) -> Option<String> {
        let doc = self.doc.borrow();
        let node = doc.get_node(node_id)?;
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

        self.doc.borrow_mut().submit_form(form_id, node_id);

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
        let doc = self.doc.borrow();
        let mut current = doc.get_node(node_id)?;
        for _ in 0..64 {
            if current
                .element_data()
                .is_some_and(|el| el.name.local.as_ref() == "form")
            {
                return Some(current.id);
            }
            current = doc.get_node(current.parent?)?;
        }
        None
    }

    /// How far down the document the viewport currently sits.
    pub fn scroll_offset(&self) -> (f64, f64) {
        let scroll = self.doc.borrow().viewport_scroll();
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
        let doc = self.doc.borrow();
        let layout = &doc.root_element().final_layout;
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
        self.doc.borrow_mut().set_viewport_scroll(blitz_dom::Point {
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
        let doc = self.doc.borrow();
        let hit = doc.hit(x + scroll_x as f32, y + scroll_y as f32)?;

        // The hit lands on whatever box is topmost — often a text run inside
        // the anchor rather than the anchor itself — so walk up for the href.
        let mut node_id = hit.node_id;
        for _ in 0..16 {
            let node = doc.get_node(node_id)?;
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

    /// Load a page and, when the options ask for it, run its scripts.
    ///
    /// One place rather than at each call site, so no path can load a page with
    /// script configured on and quietly not run it.
    fn finish(&self, mut page: Page) -> Result<Page, H5iError> {
        if self.options.script {
            page.run_scripts(self.broker.clone())?;
        }
        Ok(page)
    }

    /// Whether this factory runs page script, for `capabilities` and for the
    /// engine line the viewers show.
    pub fn runs_script(&self) -> bool {
        self.options.script
    }

    pub fn open(&self, url: &Url) -> Result<Page, H5iError> {
        // Leaving an origin drops its cookies. See `cookies::Jar::retain_origin`
        // for why that bound exists and what it costs.
        let dropped = self.broker.jar().retain_origin(url);
        let page = Page::open(url, self.broker.clone(), self.fonts(), self.options.clone())?;
        let mut page = self.finish(page)?;
        if dropped {
            page.note(
                "cookies from the previous origin were dropped on navigation: this engine \
                 holds a session only for the origin currently loaded",
            );
        }
        Ok(page)
    }

    /// Load HTML already in hand, running its scripts if the options ask.
    ///
    /// Infallible in the loading, because HTML always parses into something. A
    /// script that failed to *run* is reported through the page's console
    /// rather than here: one broken script does not make a page unreadable, and
    /// the agent needs the half that worked.
    pub fn from_html(&self, html: &str, base_url: &Url) -> Page {
        let mut page = Page::from_html(
            html,
            base_url,
            self.broker.clone(),
            self.fonts(),
            self.options.clone(),
        );
        if self.options.script {
            if let Err(error) = page.run_scripts(self.broker.clone()) {
                eprintln!("h5i-browser-light: the script realm failed to start: {error}");
            }
        }
        page
    }
}

/// How many `<meta refresh>` hops to follow before giving up.
///
/// Low on purpose. A refresh chain longer than this is not a site pointing at
/// its real address, it is a loop or a tracker bounce.
const MAX_META_REFRESH_HOPS: usize = 3;

/// How long the whole script phase may take before this engine stops starting
/// more of it.
///
/// Boa exposes no wall-clock interrupt, so a single `eval` cannot be cut short —
/// but a page is rarely one script, and this bounds a page that is slow because
/// it has *many*. After the budget the remaining scripts are skipped and the
/// snapshot says how many, so a thin outline is explained rather than
/// mysterious.
///
/// Worth being exact about what it does not cover, because it was written
/// hoping it would: a **module graph evaluates inside `run_jobs`**, which is one
/// call that returns when it returns. lit.dev spends minutes there and this
/// budget never gets a turn. Bounding that needs an interrupt Boa does not
/// have, and a caller who cannot wait must still impose its own timeout.
///
/// Generous, because the alternative to a slow page is a wrong one: a page that
/// merely needs four seconds should get them.
const SCRIPT_PHASE_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

/// The line between "this page redirected" and "this page updates itself".
///
/// json.org serves a one-line document whose only content is a `<meta refresh>`
/// to `json-en.html`; the corpus recorded one line and no error, which reads as
/// a site with nothing on it. A refresh further out than this is a dashboard or
/// a scoreboard intending to reload later, and following it would be wrong.
const META_REFRESH_MAX_DELAY_SECONDS: u64 = 15;

/// Phrases that mean "you are being challenged", not "here is the page".
///
/// Matched against the raw markup because a challenge page renders to almost
/// nothing — the whole problem is that its *outline* is indistinguishable from
/// an empty page. Deliberately specific: a false positive would tell an agent
/// it was blocked by a site that simply had little to say.
const CHALLENGE_MARKERS: [&str; 7] = [
    "enable javascript and cookies to continue",
    "checking your browser before accessing",
    "verifying you are human",
    "cf-browser-verification",
    "please enable cookies",
    "ddos protection by",
    "attention required! | cloudflare",
];

fn challenge_marker(html: &str) -> Option<&'static str> {
    let lowered = html.to_ascii_lowercase();
    CHALLENGE_MARKERS
        .into_iter()
        .find(|marker| lowered.contains(marker))
}

/// The `<meta http-equiv="refresh">` target, if the document names one.
///
/// Parsed from the markup rather than the tree because this decides whether the
/// tree is worth building at all. The content attribute is `delay` optionally
/// followed by `; url=...`, with the quoting and spacing of twenty-five years of
/// hand-written HTML, so it is parsed leniently on purpose.
fn meta_refresh(html: &str, base: &Url) -> Option<(u64, Url)> {
    let lowered = html.to_ascii_lowercase();
    let mut from = 0usize;

    while let Some(found) = lowered[from..].find("<meta") {
        let start = from + found;
        let end = lowered[start..].find('>').map(|e| start + e)?;
        let tag = &html[start..end];
        from = end;

        let lowered_tag = tag.to_ascii_lowercase();
        if !lowered_tag.contains("http-equiv") || !lowered_tag.contains("refresh") {
            continue;
        }
        let Some(content) = attribute_value(tag, "content") else {
            continue;
        };

        let mut parts = content.splitn(2, ';');
        let delay = parts
            .next()
            .map(|d| d.trim().parse::<f64>().unwrap_or(0.0).max(0.0) as u64)
            .unwrap_or(0);
        let Some(rest) = parts.next() else { continue };
        let target = rest
            .trim()
            .trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .trim_start()
            .trim_start_matches('=')
            .trim()
            .trim_matches(|c| c == '\'' || c == '"');
        if target.is_empty() {
            continue;
        }
        if let Ok(url) = base.join(target) {
            return Some((delay, url));
        }
    }
    None
}

/// One attribute out of a raw tag, single or double quoted or bare.
fn attribute_value(tag: &str, name: &str) -> Option<String> {
    let lowered = tag.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(found) = lowered[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        // Must be a whole attribute name, not the tail of another one.
        if at > 0 && !lowered.as_bytes()[at - 1].is_ascii_whitespace() {
            continue;
        }
        let after = tag[from..].trim_start();
        let Some(after) = after.strip_prefix('=') else {
            continue;
        };
        let after = after.trim_start();
        let value = match after.chars().next() {
            Some(quote @ ('"' | '\'')) => after[1..].split(quote).next().unwrap_or(""),
            // Unquoted: ends at whitespace or at the end of the tag. Splitting
            // on whitespace alone kept the `>` and turned `content=ab>` into
            // the value "ab>".
            _ => after
                .split(|c: char| c.is_ascii_whitespace() || c == '>')
                .next()
                .unwrap_or(""),
        };
        return Some(value.to_string());
    }
    None
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

#[cfg(test)]
mod network_tests {
    use super::*;

    #[test]
    fn a_meta_refresh_is_parsed_the_way_hand_written_html_writes_it() {
        let base = Url::parse("https://site.example/dir/page.html").unwrap();

        // Twenty-five years of hand-written variants, all of which appear in
        // the wild and all of which mean the same thing.
        for markup in [
            r#"<meta http-equiv="refresh" content="0; url=next.html">"#,
            r#"<meta http-equiv="Refresh" content="0;URL=next.html">"#,
            r#"<meta http-equiv=refresh content='0; url=next.html'>"#,
            r#"<meta content="0; url=next.html" http-equiv="refresh">"#,
            r#"<META HTTP-EQUIV="REFRESH" CONTENT="0; URL='next.html'">"#,
        ] {
            let found = meta_refresh(markup, &base);
            assert_eq!(
                found.map(|(delay, url)| (delay, url.to_string())),
                Some((0, "https://site.example/dir/next.html".to_string())),
                "failed on {markup}"
            );
        }

        // The delay is read, not assumed.
        assert_eq!(
            meta_refresh(
                r#"<meta http-equiv="refresh" content="600; url=/live">"#,
                &base
            )
            .map(|(d, _)| d),
            Some(600)
        );

        // A refresh with no URL reloads this page; that is not a redirect.
        assert!(meta_refresh(r#"<meta http-equiv="refresh" content="30">"#, &base).is_none());
        // And an unrelated meta is not one.
        assert!(meta_refresh(r#"<meta name="description" content="0; url=x">"#, &base).is_none());
        // `http-equiv="refresh-policy"` must not match on a substring.
        assert!(meta_refresh("<p>content=\"0; url=x\"</p>", &base).is_none());
    }

    #[test]
    fn a_challenge_page_is_recognised_and_an_ordinary_short_page_is_not() {
        assert_eq!(
            challenge_marker("<html><body>Enable JavaScript and cookies to continue</body></html>"),
            Some("enable javascript and cookies to continue")
        );
        assert_eq!(
            challenge_marker("<title>Attention Required! | Cloudflare</title>"),
            Some("attention required! | cloudflare")
        );
        // A short page is not a challenge. Saying otherwise would tell an agent
        // it was blocked by a site that simply had little to say.
        assert_eq!(challenge_marker("<html><body><p>Not found.</p></body></html>"), None);
        assert_eq!(
            challenge_marker("<p>This article explains how to enable JavaScript.</p>"),
            None
        );
    }

    #[test]
    fn an_attribute_is_read_whichever_way_it_was_quoted() {
        assert_eq!(attribute_value(r#"<meta content="a b">"#, "content"), Some("a b".into()));
        assert_eq!(attribute_value(r#"<meta content='a b'>"#, "content"), Some("a b".into()));
        assert_eq!(attribute_value(r#"<meta content=ab>"#, "content"), Some("ab".into()));
        // Not the tail of another attribute: `data-content` is not `content`.
        assert_eq!(attribute_value(r#"<meta data-content="x">"#, "content"), None);
        assert_eq!(attribute_value(r#"<meta charset="utf-8">"#, "content"), None);
    }
}
