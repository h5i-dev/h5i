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
use blitz_dom::node::SpecialElementData;
use blitz_dom::{BaseDocument, DocumentConfig, local_name};
use blitz_html::HtmlDocument;
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use h5i_error::H5iError;
use url::Url;

use crate::fonts::FontSetup;
use crate::broker::Broker;
use crate::net::BrokerNet;
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
    /// How long one navigation may take, first byte to last.
    ///
    /// The bound the per-phase budgets could not give. A request timeout bounds
    /// a request, the script-phase budget bounds the script, and a page that
    /// spends thirty seconds on the network *and* twenty in its script is
    /// inside every one of them while taking the better part of a minute.
    ///
    /// This is the cheap half of "make JavaScript stoppable": it does not kill
    /// a single runaway job — Boa's cancellation is checked between jobs, and
    /// only a separate process could interrupt one — but it bounds everything
    /// that *is* interruptible under one number, and it does so without
    /// splitting the engine in half. `Page` holds an `Rc<RefCell<BaseDocument>>`
    /// and is pinned to its thread; moving script into a killable worker means
    /// moving the document with it.
    pub navigation_budget: std::time::Duration,
}

impl Default for PageOptions {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            scale: 1.0,
            max_snapshot_lines: 500,
            script: false,
            // Above what a slow real page takes and far below what a stuck one
            // would, which is the shape every ceiling in this engine has.
            navigation_budget: std::time::Duration::from_secs(45),
        }
    }
}

/// What [`Page::select_option`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectOutcome {
    /// Set. Carries the value the form will submit, which is what a recording
    /// should hold — the text is what the agent read, and the two differ on
    /// most real forms.
    Chosen(String),
    /// It is a `<select>`, and nothing in it matched by value or by text.
    NoSuchOption,
    /// It was never a `<select>`.
    NotASelect,
}

/// A request a form asked for, caught on its way to the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    pub url: Url,
    /// The document the form was in.
    ///
    /// A form's `action` is chosen by the page, not by the agent — the agent
    /// asks for a button to be pressed, and the page decides where that goes.
    /// So the submission is policed as a request *from* this origin, which is
    /// what stops a page on the open web POSTing to the box's dev server the
    /// moment somebody clicks its submit button. Filled in by
    /// [`Page::submit_form`], which is the only thing that knows it.
    pub document: Url,
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
#[derive(Clone)]
struct CapturedNavigation {
    slot: Arc<std::sync::Mutex<Option<Submission>>>,
    /// The document the form lives in, so the captured request carries the
    /// origin it was made from. Filled here rather than left for the caller
    /// because a `Submission` with no origin is one the policy trusts, and a
    /// field that defaults to trusted is a field somebody forgets to set.
    document: Url,
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
                document: self.document.clone(),
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
    /// What this document is written in.
    ///
    /// Carried on the page rather than worked out where it is needed, because
    /// two very different things depend on it — decoding the bytes, and
    /// encoding a URL's query — and they must not be able to disagree.
    encoding: &'static encoding_rs::Encoding,
    options: PageOptions,
    /// Where [`CapturedNavigation`] leaves whatever the last form asked for.
    pending_navigation: Arc<std::sync::Mutex<Option<Submission>>>,
    /// The script realm, when this page has one. `None` when script is off,
    /// which is still the default: `capabilities.javascript` is the gate, and
    /// flipping it is a threat-model decision rather than a feature flag
    /// (ROADMAP §12.5).
    script: Option<crate::script::Script>,
    /// What this page's load cost, against what it was allowed.
    ///
    /// Recorded on the page rather than read from the broker at snapshot time,
    /// because the broker's counters belong to whatever page is loading *now*
    /// and a snapshot of an earlier page would read the wrong ones.
    budget_spent: Option<(crate::budget::Spent, crate::budget::Limits)>,
    /// How long this navigation has left.
    ///
    /// Armed when the page began loading, not when the script phase starts, so
    /// the time already spent on the network counts against it. That is the
    /// point: the per-phase budgets each bound their own step, and a page that
    /// is inside every one of them can still take the better part of a minute.
    deadline: crate::budget::Deadline,
    /// Whether `run_scripts` was called, regardless of whether it built a realm.
    ///
    /// A page with no script elements never gets one, so `script.is_some()`
    /// alone cannot tell "script is off" from "there was nothing to run".
    ran_scripts: bool,
    /// Set when the layout engine panicked while reading this page.
    ///
    /// The outline that follows was produced from whatever state layout reached,
    /// which is worth saying out loud: a short page and a half-laid-out one look
    /// the same to a reader who is not told.
    layout_failure: Option<String>,
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
        broker: Arc<dyn Broker>,
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

            // Decoded as the document says it is written, not as UTF-8. A
            // `euc-jp` page read as UTF-8 is mojibake, and every answer that
            // follows — the outline, the snapshot, a link's query — is then
            // wrong with nothing to say so. Still lossy within that encoding: a
            // page with one bad byte should render.
            let content_type = declared_content_type(&outcome);
            let encoding = crate::encoding::sniff(&outcome.body, content_type.as_deref());
            let html = crate::encoding::decode(&outcome.body, encoding);
            let final_url = outcome.final_url.clone();
            let status = outcome.status.unwrap_or(0);

            // Decided before the page is built, because the marker lives in the
            // markup we already hold and building is the expensive half.
            let refresh = meta_refresh(&html, &final_url);
            if let Some((delay, next)) = &refresh
                && *delay <= META_REFRESH_MAX_DELAY_SECONDS && !visited.contains(next)
            {
                followed.push(final_url.to_string());
                visited.push(next.clone());
                target = next.clone();
                continue;
            }

            let mut page = Self::from_html(&html, &final_url, broker, fonts, options);
            page.encoding = encoding;

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
    /// Build a page from the bytes a server sent, rather than from a string
    /// somebody already decoded.
    ///
    /// The distinction matters because *how* those bytes become a string is a
    /// property of the document: a `euc-jp` page decoded as UTF-8 is mojibake,
    /// and every downstream answer — the outline, the snapshot, a link's query
    /// — is then wrong in a way nothing reports.
    pub fn from_bytes(
        bytes: &[u8],
        content_type: Option<&str>,
        base_url: &Url,
        broker: Arc<dyn Broker>,
        fonts: FontSetup,
        options: PageOptions,
    ) -> Self {
        let encoding = crate::encoding::sniff(bytes, content_type);
        let html = crate::encoding::decode(bytes, encoding);
        let mut page = Self::from_html(&html, base_url, broker, fonts, options);
        page.encoding = encoding;
        page
    }

    /// Parse markup into a document. Separated so it can be attempted twice.
    /// One rule this engine adds to the user-agent stylesheet.
    ///
    /// A `<canvas>` is a replaced element: browsers lay it out at its own
    /// width and height even though its `display` is `inline`. Blitz's default
    /// sheet gives it no such treatment, so an inline canvas measured zero by
    /// zero, and a surface composited onto a zero-sized box paints nothing —
    /// the drawing worked, the pixels existed, and the page was blank.
    ///
    /// `inline-block` is the closest shape Blitz will size correctly, and it
    /// keeps a canvas flowing inline with the text around it, which is where
    /// pages put them. Added as a *user-agent* rule so a page's own stylesheet
    /// still overrides it, which is what makes this a default rather than a
    /// decision taken away from the document.
    const CANVAS_UA_CSS: &'static str = "canvas { display: inline-block; }";

    fn parse(
        html: &str,
        base_url: &Url,
        broker: Arc<dyn Broker>,
        fonts: &FontSetup,
        viewport: Viewport,
        captured: CapturedNavigation,
    ) -> BaseDocument {
        HtmlDocument::from_html(
            html,
            DocumentConfig {
                viewport: Some(viewport),
                base_url: Some(base_url.to_string()),
                net_provider: Some(Arc::new(BrokerNet::new(
                    broker,
                    Some(base_url.clone()),
                ))),
                font_ctx: Some(fonts.context.clone()),
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
        .into_inner()
    }

    /// Apply this engine's own additions to the user-agent stylesheet.
    ///
    /// One rule today ([`Page::CANVAS_UA_CSS`]). Kept as a step of its own so
    /// the next one has an obvious home, and so it runs on every path that
    /// builds a document rather than on whichever one somebody remembered.
    fn apply_ua_stylesheet(doc: &mut BaseDocument) {
        doc.add_user_agent_stylesheet(Self::CANVAS_UA_CSS);
    }

    pub fn from_html(
        html: &str,
        base_url: &Url,
        broker: Arc<dyn Broker>,
        fonts: FontSetup,
        options: PageOptions,
    ) -> Self {
        let viewport = Viewport::new(
            options.width,
            options.height,
            options.scale,
            ColorScheme::Light,
        );

        let captured = CapturedNavigation {
            slot: Arc::default(),
            document: base_url.clone(),
        };
        let pending_navigation = captured.slot.clone();

        // Parsing can abort the process, so it is guarded and retried.
        //
        // blitz resolves an `<img src>` while flushing the parser's eager
        // operations and `expect`s it to succeed, so a page carrying a URL it
        // cannot resolve took the whole engine down *before there was a
        // document at all* — no page, no snapshot, no receipts, and a crash
        // where a browser would show the text beside a broken image.
        //
        // `set_attribute` was hardened against the same `expect` earlier; this
        // is the other path to it, and the first fix should have covered both.
        // The retry strips image sources rather than giving up, because an
        // agent came here for the words: a page with unloadable images is still
        // a page, and losing the pictures beats losing everything.
        let attempt = |markup: &str| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Self::parse(
                    markup,
                    base_url,
                    broker.clone(),
                    &fonts,
                    viewport.clone(),
                    captured.clone(),
                )
            }))
        };
        let (mut doc, unloadable_images) = match attempt(html) {
            Ok(doc) => (doc, false),
            Err(_) => match attempt(&strip_image_sources(html)) {
                Ok(doc) => (doc, true),
                // Nothing left to try. An empty document that says so beats a
                // process that is not there to say anything.
                Err(_) => (
                    attempt("<html><body></body></html>")
                        .unwrap_or_else(|_| unreachable!("empty markup always parses")),
                    true,
                ),
            },
        };

        Self::apply_ua_stylesheet(&mut doc);
        seed_checkbox_state(&mut doc);

        // Twice, deliberately. The broker is synchronous, so subresources have
        // already completed by the time parsing returns, but their results
        // arrive as messages that `resolve` drains at its *start*. The first
        // pass applies the stylesheets; the second lays out with them.
        // A panic in either pass is caught and becomes a note on the reading.
        let mut layout_failure = guard_layout(|| {
            doc.resolve(0.0);
            doc.resolve(0.0);
        })
        .err();

        // Say what was lost. A page rebuilt without its images is a different
        // page from the one the server sent, and a reading that does not
        // mention it is a reading that quietly changed the subject.
        if unloadable_images && layout_failure.is_none() {
            layout_failure = Some(
                "this page's images could not be loaded, so it was read without them"
                    .to_string(),
            );
        }

        Self {
            // Assumed until `from_bytes` says otherwise: a string handed
            // straight to `from_html` has already been decoded by someone.
            encoding: encoding_rs::UTF_8,
            doc: Rc::new(RefCell::new(doc)),
            url: base_url.clone(),
            // Armed here rather than at the script phase, so the fetching and
            // parsing already done count against it.
            deadline: crate::budget::Deadline::new(options.navigation_budget),
            budget_spent: None,
            options,
            pending_navigation,
            script: None,
            ran_scripts: false,
            layout_failure,
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
    /// Build a realm for this page and run its script.
    ///
    /// **A realm is built per navigation, deliberately, and is not reused.**
    /// ROADMAP §B11.5.13 lists reuse as a performance item worth ~20ms a page
    /// (§B8.9), and it should stay unbuilt. A realm carries everything the
    /// previous document's script put in it: globals, patched prototypes,
    /// retained closures. Carrying that into the next document means a page can
    /// set attacker-controlled state, cause a navigation, and have that state
    /// visible to the page it navigated to — which is a same-origin-ish
    /// boundary this engine would be removing to save twenty milliseconds.
    ///
    /// Obscura, a much larger engine in the same space, drops and recreates its
    /// whole JS runtime on every navigation for exactly this reason. The cost
    /// is real and it is the right one to pay.
    pub fn run_scripts(&mut self, broker: Arc<dyn Broker>) -> Result<(), H5iError> {
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
                periodic_timers: 0,
            });
            return Ok(());
        }

        let mut script = crate::script::Script::new(self.dom(), broker.clone(), &self.url)
            .map_err(H5iError::Metadata)?;
        script.set_encoding(self.encoding);

        // `<div id="x">` makes `x` a global, which is legacy and is also how a
        // great deal of test and older page script finds its subject. Installed
        // before the first script rather than after, because the first script is
        // usually the one that reaches for it, and a ReferenceError on line one
        // ends a file before it can report anything at all.
        if let Err(error) = script.eval("__h5iInstallNamedAccess()") {
            script.note_error(&format!("named access could not be installed: {error}"));
        }

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
        // Whichever runs out first. The script phase has its own ceiling, and
        // the navigation has one over the whole load; a page that spent thirty
        // seconds fetching does not then get a fresh twenty to run in.
        let phase_budget = SCRIPT_PHASE_BUDGET.min(self.deadline.remaining());
        let mut skipped = 0usize;
        // The origin every `src` below is fetched on behalf of. Cloned once so
        // the loops do not have to hold a borrow of `self` across the calls
        // that mutate the script realm.
        let document = self.url.clone();

        for (index, (node, source)) in classic.into_iter().enumerate() {
            if phase_started.elapsed() >= phase_budget {
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
                    // With the document's origin: a `src` is chosen by the page,
                    // and the response is *executed* in it. Without one the
                    // policy read it as the agent naming a URL, so a page from
                    // the open web could point a `<script src>` at the box's
                    // dev server and run whatever came back.
                    let outcome = broker.fetch_from(
                        &url,
                        crate::receipt::Initiator::Subresource,
                        Some(&document),
                    );
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

        // One budget for the whole phase, not one per stage. The settle used to
        // arm a fresh deadline of its own, so a page that spent the script
        // budget and then the job budget cost the sum of the two — lit.dev took
        // 46 seconds against a 20-second intent. What is left of the phase is
        // what settling gets.
        let left = phase_budget.saturating_sub(phase_started.elapsed());
        script.set_job_budget(left.max(std::time::Duration::from_secs(1)));

        for (_, source) in modules {
            if phase_started.elapsed() >= phase_budget {
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
                    // Document-scoped for the same reason the classic `src`
                    // above is: the URL is the page's choice and the body is
                    // executed.
                    let outcome = broker.fetch_from(
                        &url,
                        crate::receipt::Initiator::Subresource,
                        Some(&document),
                    );
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

        // The document is now as loaded as it is going to get, so say so.
        // Before settling, because these callbacks start timers and fetches of
        // their own and those are part of loading the page rather than of
        // whatever the agent does next.
        //
        // This is one call and it unblocked an entire test suite: testharness.js
        // gates every result it will ever report on a single `load` listener
        // (`on_event(window, 'load', ...)`), with no `readyState` fallback, so
        // an engine that never fires it scores nothing while looking merely
        // slow. Real pages hid this because `readyState` answered "complete"
        // and their fallback path ran.
        if let Err(error) = script.eval("__h5iFireLifecycle()") {
            script.note_error(&format!("the load lifecycle could not be fired: {error}"));
        }

        let settled = script.settle();
        if script.take_dirty() {
            self.note_layout_failure(guard_layout(|| self.doc.borrow_mut().resolve(0.0)));
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
        // A canvas drawn during the script phase reaches the page here. The
        // realm has to be installed on `self` first, because the surfaces live
        // on its host — so this cannot move above the line before it.
        if self.composite_canvases() {
            self.note_layout_failure(guard_layout(|| self.doc.borrow_mut().resolve(0.0)));
        }
        Ok(())
    }

    /// What this document is written in, as the canonical label.
    pub fn encoding(&self) -> &'static str {
        self.encoding.name()
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
        let painted = self.composite_canvases();
        if dirty || painted {
            self.note_layout_failure(guard_layout(|| self.doc.borrow_mut().resolve(0.0)));
        }
        Some(requests)
    }

    /// Wait until something is on the page, or until nothing can put it there.
    ///
    /// Three answers, and the third is the one worth having. A page that runs
    /// no script cannot grow the thing being waited for, so the honest reply is
    /// *immediately* "not there, and nothing here will change that" rather than
    /// a budget spent proving it. The same holds for a scripted page that has
    /// gone quiet, which is where the settle loop's `Quiescent` end comes from.
    pub fn wait_for(&mut self, target: &WaitTarget) -> crate::script::Waited {
        let dom = self.doc.clone();
        let max_lines = self.options.max_snapshot_lines;
        let url = self.url.to_string();
        let scripted = self.script.is_some();
        let mut ready = move || {
            let doc = dom.borrow();
            match target {
                WaitTarget::Selector(selector) => {
                    matches!(doc.query_selector_all(selector), Ok(found) if !found.is_empty())
                }
                // Read through the snapshot walker rather than the raw tree, so
                // "the text is on the page" means the same thing here as it
                // does in the outline the agent is reading. A match in a
                // `<script>` body is not a match a reader would see.
                WaitTarget::Text(needle) => {
                    crate::snapshot::Snapshot::capture(&doc, &url, max_lines, scripted)
                        .lines
                        .iter()
                        .any(|line| line.text.contains(needle.as_str()))
                }
            }
        };

        let Some(script) = self.script.as_mut() else {
            // No realm: it either matches now or it never will.
            let met = ready();
            return crate::script::Waited {
                met,
                // No realm ran, so nothing can have changed.
                changed: false,
                settled: crate::script::Settled {
                    elapsed_ms: 0,
                    timers_run: 0,
                    cut_off: false,
                    pending_timers: 0,
                    periodic_timers: 0,
                },
                end: if met {
                    crate::script::WaitEnd::Met
                } else {
                    crate::script::WaitEnd::Quiescent
                },
            };
        };

        let mut waited = script.settle_until(&mut ready);
        waited.changed = self.after_script(waited.settled.clone());
        waited
    }

    /// Wait until a page expression is true.
    ///
    /// `None` when this session has no realm, which the caller reports as a
    /// routing answer rather than as a condition that failed.
    pub fn wait_for_script(&mut self, expr: &str) -> Option<crate::script::Waited> {
        let script = self.script.as_mut()?;
        let mut waited = script.settle_until_expr(expr);
        waited.changed = self.after_script(waited.settled.clone());
        Some(waited)
    }

    /// Settle bookkeeping shared by everything that re-enters the realm.
    ///
    /// Factored out of `dispatch_event`, which had it inline: a wait can run a
    /// page's own code, so it owes the same layout re-resolve and the same
    /// `settled` record, and forgetting either would leave the next snapshot
    /// describing a document the engine had not laid out.
    fn after_script(&mut self, settled: crate::script::Settled) -> bool {
        let dirty = self.script.as_mut().map(|s| s.take_dirty()).unwrap_or(false);
        self.settled = Some(settled);
        // Canvas surfaces reach the page here, before layout rather than after:
        // attaching the pixels sets the element's intrinsic size, which the
        // layout pass then has to see.
        let painted = self.composite_canvases();
        if dirty || painted {
            self.note_layout_failure(guard_layout(|| self.doc.borrow_mut().resolve(0.0)));
        }
        dirty || painted
    }

    /// Hand every drawn canvas surface to the document as raster image data.
    ///
    /// `blitz-paint` draws `raster_image_data()` for *any* element, not only
    /// `<img>`, which is what lets a canvas composite into the page with no
    /// changes to the paint path and no GPU surface. Blitz's own
    /// `CanvasData` is the other route and is not usable here: it carries a
    /// `custom_paint_source_id` for an external renderer, and this engine's
    /// canvas *is* a CPU buffer.
    ///
    /// Returns whether anything changed, so a page with no canvas pays nothing
    /// and a page whose canvas has not been drawn on since the last pass pays
    /// nothing either.
    fn composite_canvases(&mut self) -> bool {
        let Some(script) = self.script.as_ref() else {
            return false;
        };
        let host = script.host();
        if host.canvases.borrow().is_empty() {
            return false;
        }

        let mut canvases = host.canvases.borrow_mut();
        let dirty = canvases.dirty();
        if dirty.is_empty() {
            return false;
        }

        let doc = self.doc.clone();
        let mut doc = doc.borrow_mut();
        let mut painted = false;
        for node_id in dirty {
            let Some(canvas) = canvases.get_mut(node_id) else {
                continue;
            };
            let (width, height) = (canvas.width(), canvas.height());
            // The paint path expects straight-alpha RGBA; the surface is
            // premultiplied, which is how the rasteriser writes it. Getting
            // this backwards darkens every semi-transparent pixel — the kind
            // of wrong that looks plausible until it is beside a browser.
            let mut straight = Vec::with_capacity(canvas.pixels().len());
            for pixel in canvas.pixels().as_chunks::<4>().0 {
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

            let Some(node) = doc.get_node_mut(node_id) else {
                continue;
            };
            let Some(element) = node.element_data_mut() else {
                continue;
            };
            element.special_data = blitz_dom::node::SpecialElementData::Image(Box::new(
                blitz_dom::node::ImageData::Raster(blitz_dom::node::RasterImageData::new(
                    width,
                    height,
                    std::sync::Arc::new(straight),
                )),
            ));
            canvas.mark_composited();
            painted = true;
        }
        painted
    }

    /// Fire one key event at a node, if this page has a realm to fire it into.
    ///
    /// Silent when there is none, which is the same shape `dispatch_event`
    /// takes: a page with no script cannot be listening, so there is nothing
    /// the absence could be hiding.
    fn dispatch_key(&mut self, node_id: usize, kind: &str, key: &str) {
        let Some(script) = self.script.as_mut() else {
            return;
        };
        let _ = script.dispatch_key(node_id, kind, key);
        let settled = script.settle();
        let dirty = script.take_dirty();
        let _ = script.take_requests();
        self.settled = Some(settled);
        let painted = self.composite_canvases();
        if dirty || painted {
            self.note_layout_failure(guard_layout(|| self.doc.borrow_mut().resolve(0.0)));
        }
    }

    /// How many sockets this page holds open.
    ///
    /// Surfaced because it is the one thing in this engine that makes a session
    /// non-deterministic. Everything else runs on a virtual clock, so two reads
    /// of one page agree; a live socket delivers on wall-clock time, so the
    /// page can differ between two reads without the agent having done
    /// anything. That is a real capability and a real caveat, and the caveat
    /// should not be silent — the determinism claim is one this engine makes
    /// loudly elsewhere.
    pub fn open_sockets(&mut self) -> usize {
        self.script
            .as_mut()
            .map(|script| script.open_sockets())
            .unwrap_or(0)
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

    /// Remember that layout broke, keeping the first failure.
    ///
    /// The first, because a later pass that happens to survive does not undo
    /// the fact that the document was laid out incompletely — and an outline
    /// read from it is short for a reason the agent should be told.
    fn note_layout_failure(&mut self, outcome: Result<(), String>) {
        if let Err(detail) = outcome
            && self.layout_failure.is_none()
        {
            self.layout_failure = Some(detail);
        }
    }

    /// Re-resolve style and layout after script changed the tree.
    ///
    /// Called once after a settle rather than after each mutation: a script that
    /// appends fifty rows should lay out once, not fifty times.
    pub fn refresh(&mut self) {
        self.note_layout_failure(guard_layout(|| self.doc.borrow_mut().resolve(0.0)));
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
            self.ran_scripts,
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

        if let Some(detail) = &self.layout_failure {
            snapshot.notes.push(format!(
                "this engine's layout stage failed on this page ({detail}). What follows was \
                 read from a partly laid-out document and may be incomplete."
            ));
        }

        // What this page spent, when it spent enough to matter.
        //
        // Said rather than left in the request log, because a page that ran out
        // of allowance is a page whose reading is *incomplete* — the same class
        // of fact as "this page had not finished". An agent that is not told
        // reads a half-loaded page as the whole one.
        if let Some((spent, limits)) = &self.budget_spent {
            if spent.requests > limits.max_requests {
                snapshot.notes.push(format!(
                    "this page asked for more than {} requests in one navigation and the \
                     rest were refused. What follows was read from what it managed to load; \
                     the request log names which were denied.",
                    limits.max_requests
                ));
            } else if spent.wire_bytes > limits.max_wire_bytes
                || spent.decoded_bytes > limits.max_decoded_bytes
            {
                snapshot.notes.push(format!(
                    "this page pulled {} bytes ({} decoded) and passed its budget for one \
                     navigation, so later requests were refused.",
                    spent.wire_bytes, spent.decoded_bytes
                ));
            } else if spent.network_time > limits.max_network_time {
                snapshot.notes.push(format!(
                    "this page spent {}ms waiting on the network, past its budget for one \
                     navigation, so later requests were refused.",
                    spent.network_time.as_millis()
                ));
            }
        }

        // Two different facts, one line. `cut_off` says the reading stopped
        // before the page did. `periodic_timers` says the page finished what it
        // owed and is still running a loop that re-arms itself, which makes two
        // reads of it disagree without the agent having acted — the same caveat
        // `open_sockets` carries, and it belongs in the outline for the same
        // reason. Silence here would leave an agent unable to tell a page that
        // is animating from one that is done.
        if let Some(settled) = &self.settled
            && (settled.cut_off || settled.periodic_timers > 0)
        {
            snapshot.notes.push(settled.render());
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
    /// Set a checkbox or radio to a state, rather than toggling it.
    ///
    /// **The reason this exists beside `click` is replay.** A click on a
    /// checkbox is a *toggle*: run it twice and you are back where you started,
    /// so a recorded session that clicks one reaches a different state
    /// depending on what the page happened to be serving. Setting a state is
    /// idempotent, which is what a script replayed tomorrow needs.
    ///
    /// Returns `None` when the node is not something this can act on, so the
    /// caller can say *which* wrong kind of thing it was rather than reporting
    /// a generic failure.
    pub fn set_checked(&mut self, node_id: usize, checked: bool) -> Option<bool> {
        let was = {
            let doc = self.doc.borrow();
            let element = doc.get_node(node_id)?.element_data()?;
            if element.name.local != local_name!("input") {
                return None;
            }
            let kind = element.attr(local_name!("type")).unwrap_or("text");
            if !(kind.eq_ignore_ascii_case("checkbox") || kind.eq_ignore_ascii_case("radio")) {
                return None;
            }
            matches!(element.special_data, SpecialElementData::CheckboxInput(true))
        };

        if was == checked {
            // Already there. Reported as a no-op rather than dispatched, or a
            // replay would fire a `change` the original run never fired.
            return Some(false);
        }

        {
            let mut doc = self.doc.borrow_mut();
            // A radio turns its group off, which is what makes the group a
            // group. Done here rather than left to the page, because nothing
            // else in this engine implements the exclusivity and a form
            // submitted with two of a group checked is a wrong answer.
            if checked {
                let name = doc
                    .get_node(node_id)
                    .and_then(|node| node.element_data())
                    .filter(|el| {
                        el.attr(local_name!("type"))
                            .is_some_and(|k| k.eq_ignore_ascii_case("radio"))
                    })
                    .and_then(|el| el.attr(local_name!("name")))
                    .map(str::to_string);
                if let Some(name) = name {
                    let siblings: Vec<usize> = doc
                        .tree()
                        .iter()
                        .filter_map(|(id, node)| {
                            if id == node_id {
                                return None;
                            }
                            let el = node.data.downcast_element()?;
                            let is_radio = el
                                .attr(local_name!("type"))
                                .is_some_and(|k| k.eq_ignore_ascii_case("radio"));
                            let same_group =
                                el.attr(local_name!("name")).is_some_and(|n| n == name);
                            (is_radio && same_group).then_some(id)
                        })
                        .collect();
                    for id in siblings {
                        if let Some(el) = doc
                            .get_node_mut(id)
                            .and_then(|node| node.data.downcast_element_mut())
                        {
                            el.special_data = SpecialElementData::CheckboxInput(false);
                        }
                    }
                }
            }

            let element = doc
                .get_node_mut(node_id)
                .and_then(|node| node.data.downcast_element_mut())?;
            element.special_data = SpecialElementData::CheckboxInput(checked);
            // `:checked` is a real selector and the cascade has to see it.
            let _ = guard_layout(|| doc.resolve(0.0));
        }

        // The pair a *user* edit fires, in the order a page expects. A page
        // that enables its submit button on `change` needs this or the button
        // stays disabled through a replay that looks like it worked.
        self.dispatch_event(node_id, "input");
        self.dispatch_event(node_id, "change");
        Some(true)
    }

    /// Choose an option in a `<select>`, by its value or its visible text.
    ///
    /// Both, because an agent reading a snapshot sees the *text* and a
    /// recorded script should carry the *value*: the first is what a model has
    /// in hand and the second is what survives a re-render. Value is tried
    /// first, so a page whose option text happens to match another option's
    /// value behaves predictably.
    ///
    /// The three outcomes are kept apart because they are three different
    /// mistakes: it worked, it is a `<select>` and nothing in it matched
    /// (the caller's to fix from a fresh reading), or it was never a `<select>`
    /// (the caller aimed at the wrong element). One message for all three would
    /// send two of them looking in the wrong place.
    pub fn select_option(&mut self, node_id: usize, wanted: &str) -> SelectOutcome {
        let options: Vec<(usize, String, String)> = {
            let doc = self.doc.borrow();
            let element = doc.get_node(node_id).and_then(|node| node.element_data());
            let Some(element) = element else {
                return SelectOutcome::NotASelect;
            };
            if element.name.local != local_name!("select") {
                return SelectOutcome::NotASelect;
            }
            let mut found = Vec::new();
            let mut stack: Vec<usize> = doc.get_node(node_id).map(|n| n.children.clone()).unwrap_or_default();
            stack.reverse();
            while let Some(id) = stack.pop() {
                let Some(child) = doc.get_node(id) else { continue };
                if child.data.is_element_with_tag_name(&local_name!("option")) {
                    let text = crate::snapshot::collapse(&child.text_content());
                    let value = child
                        .element_data()
                        .and_then(|el| el.attr(local_name!("value")))
                        .map(str::to_string)
                        // An option with no `value` submits its text, which is
                        // the rule a form actually follows.
                        .unwrap_or_else(|| text.clone());
                    found.push((id, value, text));
                }
                let mut kids = child.children.clone();
                kids.reverse();
                stack.extend(kids);
            }
            found
        };

        let chosen = options
            .iter()
            .find(|(_, value, _)| value == wanted)
            .or_else(|| options.iter().find(|(_, _, text)| text == wanted))
            .cloned();
        let Some((chosen_id, value, _)) = chosen else {
            return SelectOutcome::NoSuchOption;
        };

        {
            let mut doc = self.doc.borrow_mut();
            // Exactly one selected, which is what a single `<select>` means
            // and what `snapshot` reads back to name the control.
            for (id, _, _) in &options {
                if let Some(el) = doc
                    .get_node_mut(*id)
                    .and_then(|node| node.data.downcast_element_mut())
                {
                    if *id == chosen_id {
                        el.attrs.push(blitz_dom::node::Attribute {
                            name: blitz_dom::QualName::new(
                                None,
                                blitz_dom::ns!(),
                                local_name!("selected"),
                            ),
                            value: "selected".into(),
                        });
                    } else {
                        el.attrs.retain(|a| a.name.local != local_name!("selected"));
                    }
                }
            }
            let _ = guard_layout(|| doc.resolve(0.0));
        }

        self.dispatch_event(node_id, "input");
        self.dispatch_event(node_id, "change");
        SelectOutcome::Chosen(value)
    }

    /// Send a key to whatever has focus, or to a named element.
    ///
    /// Not typing: this is `Enter` to submit, `Escape` to dismiss, `Tab` to
    /// move on — the keys that *do* something rather than the ones that enter
    /// text. `type` covers the second and this covers the first, and merging
    /// them would mean a verb whose meaning depended on its argument.
    pub fn press(&mut self, node_id: usize, key: &str) -> bool {
        {
            let mut doc = self.doc.borrow_mut();
            if doc.get_node(node_id).is_none() {
                return false;
            }
            doc.set_focus_to(node_id);
        }
        // A real key is three events, and a page may be listening for any of
        // them: `keydown` is where `preventDefault` belongs, `keypress` is
        // where legacy code lives, `keyup` is where a debounce ends.
        for kind in ["keydown", "keypress", "keyup"] {
            self.dispatch_key(node_id, kind, key);
        }
        true
    }

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
        let _ = guard_layout(|| doc.resolve(0.0));
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
                self.note_layout_failure(guard_layout(|| self.doc.borrow_mut().resolve(0.0)));
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

/// What a `wait_for` is waiting for.
#[derive(Debug, Clone)]
pub enum WaitTarget {
    /// A CSS selector that must match at least one element.
    Selector(String),
    /// Text that must appear in the outline a reader would see.
    Text(String),
}

/// Builds pages, so a session can navigate.
///
/// It exists because a `Page` consumes its `FontContext`, and parley's is not
/// cloneable — so following a link needs the ingredients kept aside rather
/// than the previous page's leftovers. Rebuilding registers the same font
/// files again, which costs a few milliseconds per navigation and buys a much
/// simpler ownership story than sharing a collection across documents.
pub struct PageFactory {
    broker: Arc<dyn Broker>,
    font_sources: Vec<std::path::PathBuf>,
    options: PageOptions,
}

impl PageFactory {
    pub fn new(
        broker: Arc<dyn Broker>,
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

    pub fn broker(&self) -> &Arc<dyn Broker> {
        &self.broker
    }

    fn fonts(&self) -> FontSetup {
        crate::fonts::load(&self.font_sources, &[], Some(self.font_sources.len()))
    }

    /// Load whatever a form asked for, through the same broker as everything
    /// else. A refused submission is an error the agent reads, not a blank page.
    pub fn open_submission(&self, submission: &Submission) -> Result<Page, H5iError> {
        self.begin_navigation();
        let outcome = self.broker.send_from(
            &submission.url,
            Initiator::Navigation,
            &submission.method,
            &submission.body,
            submission.content_type.as_deref(),
            // The form's own document. A submission is a navigation the *page*
            // chose the destination of, so it is policed as a request from that
            // origin — a page on the open web does not get to POST to the box's
            // dev server because somebody pressed its button.
            Some(&submission.document),
        );
        if let Some(error) = outcome.error {
            return Err(H5iError::Metadata(format!(
                "could not submit to {}: {error}",
                submission.url
            )));
        }
        // A form's response is a document like any other, so it gets the same
        // encoding treatment: a legacy site that answers a POST in shift_jis
        // must not come back as replacement characters — and the same `finish`,
        // which is where the cookie-origin drop and the script run live. This
        // returned the page directly, so a submission was the one navigation
        // that both kept the previous origin's session and never ran its own
        // scripts.
        self.finish(Page::from_bytes(
            &outcome.body,
            declared_content_type(&outcome).as_deref(),
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
    ///
    /// The cookie-origin drop lives here too, and for the same reason. It used
    /// to sit in `open`, against the URL that was *asked for* — which left two
    /// ways to arrive at an origin with somebody else's session still in the
    /// jar:
    ///
    /// * **A form submission.** `open_submission` reached neither this function
    ///   nor the drop, so posting to another origin navigated there with the
    ///   previous origin's cookies intact. The jar is host-scoped, so the
    ///   arriving page's script could then `fetch` the previous origin *with its
    ///   credentials* — the cross-origin credentialed read that
    ///   `Jar::retain_origin`'s doc says cannot happen, since it "cannot be
    ///   fixed without a process split, so it is bounded instead".
    /// * **A redirect.** `open` dropped against the requested URL and the fetch
    ///   then followed hops, so a page served from B ran with A's cookies.
    ///
    /// Against `page.url()` — the origin actually loaded — both close. And it
    /// must precede `run_scripts`, or the drop happens after the script that
    /// was the reason for it.
    fn finish(&self, mut page: Page) -> Result<Page, H5iError> {
        self.finish_page(&mut page)?;
        Ok(page)
    }

    /// The rule itself, on a borrow, so the two infallible constructors can run
    /// it too rather than keeping their own copy of half of it.
    fn finish_page(&self, page: &mut Page) -> Result<(), H5iError> {
        if self.broker.keep_only_origin(page.url()) {
            page.note(
                "cookies from the previous origin were dropped on navigation: this engine \
                 holds a session only for the origin currently loaded",
            );
        }
        if self.options.script {
            page.run_scripts(self.broker.clone())?;
        }
        // After everything, so subresources and script fetches are counted.
        let allowance = self.broker.budget();
        page.budget_spent = Some((allowance.spent, allowance.limits));
        Ok(())
    }

    /// [`Self::finish`] for the constructors that cannot fail.
    ///
    /// They ran scripts themselves and did not drop the previous origin's
    /// cookies — a third and fourth copy of half the rule, which is how
    /// `open_submission` came to be missing it entirely.
    fn finish_reporting(&self, mut page: Page) -> Page {
        if let Err(error) = self.finish_page(&mut page) {
            eprintln!("h5i-browser-light: the script realm failed to start: {error}");
        }
        page
    }

    /// Whether this factory runs page script, for `capabilities` and for the
    /// engine line the viewers show.
    pub fn runs_script(&self) -> bool {
        self.options.script
    }

    /// A navigation is starting.
    ///
    /// The page's network allowance resets here, at the top of every path that
    /// builds one. A fresh page is a fresh decision by the agent, and the
    /// budget exists to bound untrusted page code rather than the principal
    /// driving the engine (see [`crate::budget`]).
    ///
    /// Before the navigation's own request, deliberately: resetting afterwards
    /// would give the page that just spent its allowance a clean slate for the
    /// subresources it is about to ask for.
    fn begin_navigation(&self) {
        self.broker.reset_budget();
    }

    pub fn open(&self, url: &Url) -> Result<Page, H5iError> {
        self.begin_navigation();
        // Leaving an origin drops its cookies — in `finish`, against the origin
        // actually loaded rather than the one asked for. See
        // `cookies::Jar::retain_origin` for why that bound exists and what it
        // costs, and `finish` for why it moved.
        let page = Page::open(url, self.broker.clone(), self.fonts(), self.options.clone())?;
        self.finish(page)
    }

    /// Load HTML already in hand, running its scripts if the options ask.
    ///
    /// Infallible in the loading, because HTML always parses into something. A
    /// script that failed to *run* is reported through the page's console
    /// rather than here: one broken script does not make a page unreadable, and
    /// the agent needs the half that worked.
    /// The same as [`PageFactory::from_html`], but from bytes whose encoding is
    /// not yet known — so the document gets to say what it is written in.
    pub fn from_bytes(&self, bytes: &[u8], content_type: Option<&str>, base_url: &Url) -> Page {
        self.begin_navigation();
        self.finish_reporting(Page::from_bytes(
            bytes,
            content_type,
            base_url,
            self.broker.clone(),
            self.fonts(),
            self.options.clone(),
        ))
    }

    pub fn from_html(&self, html: &str, base_url: &Url) -> Page {
        self.begin_navigation();
        self.finish_reporting(Page::from_html(
            html,
            base_url,
            self.broker.clone(),
            self.fonts(),
            self.options.clone(),
        ))
    }
}

/// Resolve style and layout, surviving a panic in the layout engine.
///
/// Blitz can panic — the GNU bash manual, a single one-megabyte page, aborts it
/// with `attempt to subtract with overflow` deep in layout construction. A
/// panic is the one outcome an agent cannot act on: not a thin page, not an
/// error it can read, but a dead process and no answer at all.
///
/// So the panic is caught and becomes a fact about the reading. The document is
/// left in whatever state layout reached, which is why the caller records a
/// note: a page that half-laid-out is a page whose outline may be short, and
/// the agent should be told why rather than left to wonder.
///
/// `AssertUnwindSafe` because the document is behind a `RefCell` that a panic
/// may leave mid-update. That is exactly the risk being accepted: a possibly
/// incomplete tree, read and reported, in place of no process.
fn guard_layout(body: impl FnOnce()) -> Result<(), String> {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));

    outcome.map_err(|payload| {
        
        payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "the layout engine panicked".to_string())
    })
}

/// Remove `src` from every `<img>`, for a second attempt at markup that killed
/// the parser.
///
/// Deliberately blunt: this runs only after a panic has already proved the
/// markup is not survivable as written, and picking out *which* URL blitz
/// choked on would mean re-implementing its resolver to find out.
fn strip_image_sources(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let lowered = html.to_ascii_lowercase();
    let mut at = 0;
    while let Some(found) = lowered[at..].find("<img") {
        let start = at + found;
        let end = lowered[start..].find('>').map(|e| start + e + 1).unwrap_or(html.len());
        out.push_str(&html[at..start]);
        // Keep the tag and everything except its source attributes, so `alt`
        // survives — which is the part an agent was going to read anyway.
        for piece in html[start..end].split_ascii_whitespace() {
            let name = piece.split('=').next().unwrap_or("").to_ascii_lowercase();
            if matches!(name.as_str(), "src" | "srcset" | "data-src") {
                continue;
            }
            out.push_str(piece);
            out.push(' ');
        }
        out.push('>');
        at = end;
    }
    out.push_str(&html[at..]);
    out
}

/// The `Content-Type` a response declared, if it declared one.
fn declared_content_type(outcome: &crate::net::FetchOutcome) -> Option<String> {
    outcome
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.clone())
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
const CHALLENGE_MARKERS: [&str; 10] = [
    "enable javascript and cookies to continue",
    "checking your browser before accessing",
    "verifying you are human",
    "cf-browser-verification",
    "please enable cookies",
    "ddos protection by",
    "attention required! | cloudflare",
    // pypi.org's search results, which say the page did not load rather than
    // that you were challenged — but mean the same thing to a reader: what
    // follows is not the page that was asked for.
    "javascript is disabled in your browser",
    "please enable javascript to proceed",
    "a required part of this site couldn't load",
];

fn challenge_marker(html: &str) -> Option<&'static str> {
    // Typographic apostrophes normalised to ASCII: pypi writes "couldn’t" with
    // U+2019, and a matcher that only knows `'` would miss it while looking
    // like it had checked.
    let lowered = html.to_ascii_lowercase().replace(['\u{2019}', '\u{02BC}'], "'");
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
    // `as_chunks` rather than `chunks_exact`: the chunk size is a constant, so
    // each pixel arrives as a `[u8; 4]` whose four indexes the compiler can
    // bounds-check once instead of four times per pixel.
    for pixel in rgba[..expected].as_chunks::<4>().0 {
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

/// Give every checkbox and radio its checked state *before* the first style
/// pass, so `:checked` can match during the cascade.
///
/// blitz decides this in `create_checkbox_input`, which runs during **layout
/// construction**. Selector matching runs before that, in the same `resolve`,
/// so when stylo asks an unclicked page "is this `:checked`?" the element's
/// `special_data` is still `None`, the answer is `unwrap_or(false)`, and every
/// `:checked` rule loses. Resolving twice does not save it: the second pass
/// finds nothing dirty and skips the cascade, so the rule never gets a second
/// chance.
///
/// The consequence is not subtle. The script-free tab and accordion pattern
/// (`input:checked ~ .panel { display: block }`) renders as a tab strip with no
/// panel under it, which is exactly the case where a page deliberately avoided
/// JavaScript so that it would still work for a reader like this one.
///
/// This seeds the same value from the same attribute that blitz would have
/// used, only early enough to be seen. `create_checkbox_input` then finds the
/// state already present and leaves it alone, so style and layout agree.
///
/// HTML boolean-attribute semantics: presence means checked, whatever the
/// value. That matches blitz's own rule, which matters more here than being
/// independently right, because both passes have to reach the same answer.
fn seed_checkbox_state(doc: &mut BaseDocument) {
    let inputs: Vec<(usize, bool)> = doc
        .tree()
        .iter()
        .filter_map(|(id, node)| {
            let element = node.data.downcast_element()?;
            if element.name.local != local_name!("input") {
                return None;
            }
            // Only the two types blitz builds checkbox state for. A `type` it
            // does not know is a text input there, and would be one here too.
            if !matches!(
                element.attr(local_name!("type")),
                Some(t) if t.eq_ignore_ascii_case("checkbox") || t.eq_ignore_ascii_case("radio")
            ) {
                return None;
            }
            // Never overwrite state that already exists: on a re-parse the
            // element may carry a value a click or a script put there.
            if !matches!(element.special_data, SpecialElementData::None) {
                return None;
            }
            Some((id, element.has_attr(local_name!("checked"))))
        })
        .collect();

    for (id, checked) in inputs {
        if let Some(element) = doc
            .get_node_mut(id)
            .and_then(|node| node.data.downcast_element_mut())
        {
            element.special_data = SpecialElementData::CheckboxInput(checked);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Policy;
    use crate::receipt::MemorySink;

    fn page_from(html: &str, policy: Policy, sink: Arc<MemorySink>) -> Page {
        let broker = crate::net::LocalBroker::new(policy, sink, None).expect("broker");
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
        // Named by its `<label>`, which beats the placeholder: that is the
        // order the accessible-name computation specifies, and it is the
        // better handle — "Email" is what a person sees the field called, and
        // the placeholder is example text that a redesign will change.
        assert!(rendered.contains("textbox \"Email\""), "{rendered}");
        // The placeholder is still the fallback where there is no label.
        let bare = page_from(
            r#"<!doctype html><body><input type="email" placeholder="you@example.com"></body>"#,
            Policy::new(),
            Arc::new(MemorySink::new()),
        );
        assert!(
            bare.snapshot().render().contains("textbox \"you@example.com\""),
            "{}",
            bare.snapshot().render()
        );
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
    fn a_checked_input_styles_its_siblings() {
        // The script-free tab pattern: a radio, and a panel that only becomes
        // visible because a `:checked ~` rule says so. blitz decides an input's
        // checked state during *layout construction*, which runs after selector
        // matching, so before `seed_checkbox_state` every `:checked` rule lost
        // and this page painted a white square where the panel should be.
        //
        // Asserted in pixels rather than in text, because text extraction does
        // not honour `display: none` and would report both panels either way.
        let sink = Arc::new(MemorySink::new());
        let mut page = page_from(
            "<!doctype html><style>html,body{margin:0}\
             .panel{display:none;width:400px;height:200px;background:#000}\
             #on:checked ~ .panel{display:block}</style>\
             <input type=\"radio\" name=\"t\" id=\"on\" checked><div class=\"panel\"></div>",
            Policy::new(),
            sink,
        );

        let painted = decoded(&page.screenshot_png().expect("screenshot"));
        assert_eq!(
            painted.get_pixel(CANVAS.0, CANVAS.1).0,
            [0, 0, 0],
            "`:checked ~ .panel` should have made the black panel visible"
        );
    }

    #[test]
    fn an_unchecked_input_does_not_style_its_siblings() {
        // The other half, so the test above cannot pass by making everything
        // match: the same page with the attribute removed must stay blank.
        let sink = Arc::new(MemorySink::new());
        let mut page = page_from(
            "<!doctype html><style>html,body{margin:0}\
             .panel{display:none;width:400px;height:200px;background:#000}\
             #on:checked ~ .panel{display:block}</style>\
             <input type=\"radio\" name=\"t\" id=\"on\"><div class=\"panel\"></div>",
            Policy::new(),
            sink,
        );

        let painted = decoded(&page.screenshot_png().expect("screenshot"));
        assert_eq!(
            painted.get_pixel(CANVAS.0, CANVAS.1).0,
            [255, 255, 255],
            "nothing is checked, so the panel should still be display:none"
        );
    }

    #[test]
    fn a_checkbox_keeps_its_checked_state_through_layout() {
        // `seed_checkbox_state` writes the state blitz would have written
        // later. If the two ever disagreed, the cascade and the layout would be
        // describing different pages; this pins that they agree for a checkbox
        // as well as a radio.
        let sink = Arc::new(MemorySink::new());
        let mut page = page_from(
            "<!doctype html><style>html,body{margin:0}\
             .panel{display:none;width:400px;height:200px;background:#000}\
             input:checked ~ .panel{display:block}</style>\
             <input type=\"checkbox\" checked><div class=\"panel\"></div>",
            Policy::new(),
            sink,
        );

        let painted = decoded(&page.screenshot_png().expect("screenshot"));
        assert_eq!(
            painted.get_pixel(CANVAS.0, CANVAS.1).0,
            [0, 0, 0],
            "a checked checkbox should match `:checked` during the cascade too"
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
mod navigation_origin_tests {
    use super::*;
    use crate::policy::Policy;
    use crate::receipt::MemorySink;

    /// The jar bound `Jar::retain_origin` documents — "at any moment the jar
    /// holds only cookies for the origin currently loaded" — rested on a drop
    /// that `open` performed and `open_submission` did not.
    ///
    /// The jar is host-scoped, so arriving at another origin with the previous
    /// one's cookies still in it means the new page's script can `fetch` the
    /// previous origin *with its credentials*. That is the cross-origin
    /// credentialed read the bound exists to make impossible, reached by
    /// pressing a submit button.
    #[test]
    fn a_form_submission_drops_the_previous_origins_cookies() {
        let broker = crate::net::LocalBroker::new(
                Policy::new().allow("bank.example").allow("evil.example"),
                Arc::new(MemorySink::new()),
                None,
            )
            .expect("broker");
        // A live session on one origin.
        broker.jar().store(
            &Url::parse("https://bank.example/").unwrap(),
            ["sid=secret; HttpOnly"],
        );
        assert_eq!(broker.jar().len(), 1);

        let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(4));
        let factory = PageFactory::new(broker.clone(), fonts.sources.clone(), PageOptions::default());

        // Landing on another origin — however we got there — must drop it.
        let page = Page::from_bytes(
            b"<html><body>hi</body></html>",
            Some("text/html"),
            &Url::parse("https://evil.example/collect").unwrap(),
            broker.clone(),
            factory.fonts(),
            PageOptions::default(),
        );
        let page = factory.finish(page).expect("finish");

        assert!(
            broker.jar().is_empty(),
            "the previous origin's session survived the navigation"
        );
        assert!(
            broker
                .jar()
                .header_for(&Url::parse("https://bank.example/").unwrap())
                .is_none(),
            "and it must not be sendable"
        );
        assert!(
            page.notes.iter().any(|n| n.contains("dropped on navigation")),
            "the drop is stated, not silent: {:?}",
            page.notes
        );
    }

    /// Same-origin navigation keeps the session, which is the whole point of
    /// having one.
    #[test]
    fn staying_on_an_origin_keeps_the_session_through_finish() {
        let broker = crate::net::LocalBroker::new(
                Policy::new().allow("bank.example"),
                Arc::new(MemorySink::new()),
                None,
            )
            .expect("broker");
        broker.jar().store(
            &Url::parse("https://bank.example/").unwrap(),
            ["sid=secret; HttpOnly"],
        );
        let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(4));
        let factory = PageFactory::new(broker.clone(), fonts.sources.clone(), PageOptions::default());

        let page = Page::from_bytes(
            b"<html><body>ok</body></html>",
            Some("text/html"),
            &Url::parse("https://bank.example/account").unwrap(),
            broker.clone(),
            factory.fonts(),
            PageOptions::default(),
        );
        let page = factory.finish(page).expect("finish");
        assert_eq!(broker.jar().len(), 1);
        assert!(!page.notes.iter().any(|n| n.contains("dropped on navigation")));
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;
    use crate::policy::Policy;
    use crate::receipt::MemorySink;

    fn page_with(html: &str) -> Page {
        let broker = crate::net::LocalBroker::new(Policy::new(), Arc::new(MemorySink::new()), None).expect("broker");
        let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(4));
        let factory = PageFactory::new(broker, fonts.sources.clone(), PageOptions::default());
        factory.from_html(html, &Url::parse("https://site.example/page").unwrap())
    }

    /// A wrapper that has swallowed a block of structure used to report one
    /// run-on line — `TitleBody textRead more`, three pieces of the page with
    /// no separator — and then suppress those pieces as prose it claimed to
    /// have already said. It had said them all at once, unreadably, and the
    /// structure an outline exists to show was gone.
    #[test]
    fn a_list_item_does_not_swallow_the_block_inside_it() {
        let page = page_with(
            "<html><body><ul><li>\
               <h3>Widget</h3>\
               <p>A thing that widgets.</p>\
               <a href=\"/buy\">Buy now</a>\
             </li></ul></body></html>",
        );
        let rendered = page.snapshot().render();

        assert!(
            !rendered.contains("WidgetA thing"),
            "the wrapper ran the block together:\n{rendered}"
        );
        for piece in ["Widget", "A thing that widgets.", "Buy now"] {
            assert!(
                rendered.contains(piece),
                "{piece:?} was suppressed as prose the wrapper never said:\n{rendered}"
            );
        }
        // And each piece appears once, not once inside the wrapper's line and
        // again on its own.
        assert_eq!(
            rendered.matches("A thing that widgets.").count(),
            1,
            "duplicated:\n{rendered}"
        );
    }

    /// The other side of the same rule, and the reason it is scoped to *block*
    /// descendants. Prose with a link in it is read well by the existing rule,
    /// and a heading wrapping a single link is a shape where the wrapper's name
    /// is the only thing carrying the heading level. Neither may change.
    #[test]
    fn prose_with_a_link_and_a_heading_around_one_are_unchanged() {
        let page = page_with(
            "<html><body>\
               <p>Please <a href=\"/here\">read this</a> first.</p>\
               <h2><a href=\"/sec\">Section two</a></h2>\
             </body></html>",
        );
        let rendered = page.snapshot().render();
        assert!(
            rendered.contains("Please read this first."),
            "the paragraph lost its own sentence:\n{rendered}"
        );
        assert!(
            rendered.contains("Section two"),
            "the heading lost its name:\n{rendered}"
        );
        assert!(
            rendered.contains("heading2"),
            "the heading level went missing:\n{rendered}"
        );
    }

    /// A table cell is the same shape as a list item and was wrong the same way.
    #[test]
    fn a_table_cell_does_not_swallow_the_block_inside_it() {
        let page = page_with(
            "<html><body><table><tr><td>\
               <p>First</p><p>Second</p>\
             </td></tr></table></body></html>",
        );
        let rendered = page.snapshot().render();
        assert!(
            !rendered.contains("FirstSecond"),
            "the cell ran its paragraphs together:\n{rendered}"
        );
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
