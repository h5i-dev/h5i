//! The state a native binding reaches, and the rules for reaching it.
//!
//! Everything here is deliberately free of GC-managed values. Boa's host data
//! must be `Trace`, and the only way to hold a `JsObject` correctly is to trace
//! it; rather than do that for event handlers, timer callbacks and promise
//! resolvers, **those all live on the JavaScript side** (see `prelude.js`) and
//! this holds only plain Rust state. That is why every field below can be
//! `unsafe_ignore_trace` honestly: none of them can reach the heap Boa collects.

use std::cell::RefCell;
use std::collections::BTreeMap;

use boa_engine::JsData;
use boa_gc::{Finalize, Trace};

use crate::engine::Dom;
use crate::broker::Broker;

/// One line a page wrote to the console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleLine {
    pub level: String,
    pub text: String,
    /// `"page"` when the page called `console.*`, `"engine"` when this engine
    /// is the one complaining.
    ///
    /// An agent reading a console needs to know which it is looking at: "the
    /// site reported an error" and "the browser could not do something" call
    /// for different responses, and they were indistinguishable.
    pub source: String,
    /// How many times in a row this exact line was said.
    ///
    /// remix.run logged one identical error **1486 times**, which is not
    /// information — it is the same information, at a volume that buries
    /// everything else and blows up the snapshot an agent reads.
    pub repeats: usize,
}

fn page_source() -> String {
    "page".to_string()
}

impl ConsoleLine {
    pub fn page(level: &str, text: String) -> Self {
        Self { level: level.to_string(), text, source: page_source(), repeats: 1 }
    }

    /// A line this engine said about itself, not about the page.
    pub fn engine(level: &str, text: String) -> Self {
        Self { level: level.to_string(), text, source: "engine".to_string(), repeats: 1 }
    }
}

/// Append a line, collapsing an immediate repeat into a count.
///
/// Consecutive rather than global on purpose: a page that alternates two
/// messages is saying something different from one that says the same thing
/// twice, and collapsing across the whole log would lose the order an agent
/// reads for cause and effect.
pub fn push_console(log: &mut Vec<ConsoleLine>, line: ConsoleLine) {
    if let Some(last) = log.last_mut()
        && last.text == line.text && last.level == line.level && last.source == line.source
    {
        last.repeats += 1;
        return;
    }
    log.push(line);
}

/// A Web API the page asked for and this engine does not have.
///
/// Counted rather than logged once, because "it called this forty times" and
/// "it called this once at startup" are different facts about whether the page
/// can work here at all.
#[derive(Debug, Default)]
pub struct Unsupported(pub BTreeMap<String, usize>);

impl Unsupported {
    pub fn record(&mut self, name: &str) {
        *self.0.entry(name.to_string()).or_insert(0) += 1;
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Most-used first: an API called forty times is more likely to be why the
    /// page is broken than one called once.
    pub fn ranked(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = self
            .0
            .iter()
            .map(|(name, count)| (name.clone(), *count))
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        out
    }
}

/// What the native bindings act on.
pub struct Host {
    /// The one real DOM. See [`crate::engine::Dom`] for the borrow rule.
    pub dom: Dom,

    /// The same broker the document loaded through, so a request script makes
    /// is policy-checked and receipted exactly like one the parser made. This
    /// is the whole reason script belongs in *this* engine: nothing a page does
    /// reaches the wire without a record, including the traffic every other
    /// engine's evidence is thinnest about.
    pub broker: std::sync::Arc<dyn Broker>,

    /// The page this document was loaded from, for resolving relative fetches.
    pub base: url::Url,

    /// Who this session says it is, taken from the broker when the realm is
    /// built and never asked for again.
    ///
    /// From the broker, and that is the whole design rather than a detail. The
    /// broker is the half that writes `User-Agent` and `Accept-Language`; if
    /// this side held its own copy — a constant in `prelude.js`, say, which is
    /// exactly where these values used to live — then the page and the wire
    /// would be two independent answers to one question, and a server that
    /// reads both would see two browsers. See [`crate::identity`].
    #[cfg(feature = "identity")]
    pub identity: std::sync::Arc<crate::identity::Identity>,

    /// The page's `<script type="importmap">`, if it declared one.
    ///
    /// Held here rather than on the loader because it is a property of the
    /// *document*: the engine reads it out of the parsed tree before any script
    /// runs, which is when the specification says it is locked in. A map that
    /// arrived after the first import would change what earlier imports meant.
    ///
    /// `None` is "the page declared no map", which is the case the bare
    /// specifier refusal was written for and still describes exactly. See
    /// [`crate::script::import_map`].
    pub import_map: RefCell<Option<crate::script::import_map::ImportMap>>,

    /// Set whenever script changed the tree, so the engine knows to re-resolve
    /// style and layout once rather than after every mutation.
    /// What the document is written in.
    ///
    /// Here as well as on the `Page` because the URL query encoder needs it and
    /// lives on this side. Set once, when the realm is built for a page that
    /// knows its own encoding; UTF-8 until then, which is what a string handed
    /// straight to the parser already is.
    pub encoding: RefCell<&'static encoding_rs::Encoding>,
    pub dirty: RefCell<bool>,
    /// Whether the cascade has been recomputed since the tree last changed.
    ///
    /// Separate from `dirty`, which the settle loop consumes to decide whether
    /// to lay out. `getComputedStyle` needs a *style* recalc rather than a
    /// layout, and it needs one on demand: a page that builds its DOM in script
    /// and asks for a computed value gets nothing otherwise, because Stylo has
    /// not seen the new nodes. Two flags because one consumer clearing the
    /// other's would skip a pass someone was relying on.
    pub styles_stale: RefCell<bool>,

    pub console: RefCell<Vec<ConsoleLine>>,

    pub unsupported: RefCell<Unsupported>,

    /// Every `<canvas>` this document has drawn on.
    ///
    /// Kept here rather than on the element because a canvas surface is *not*
    /// part of the DOM: it survives reflow, it is not serialised by
    /// `outerHTML`, and it is exactly the kind of thing a second tree would let
    /// drift from the first. Keyed by node id, so a canvas removed from the
    /// document keeps its pixels for as long as script holds a reference to it,
    /// which is what a page doing off-screen composition depends on.
    pub canvases: RefCell<crate::canvas::Canvases>,

    /// URLs script asked for, in order, so a caller can say which action caused
    /// which request. The receipt remains the record; this is only the link,
    /// stamped by the one component that knows the causal fact.
    pub requests: RefCell<Vec<RequestLink>>,

    /// Comment text, keyed by node id.
    ///
    /// `NodeData::Comment` carries no payload in this version of blitz, and a
    /// page that writes a comment marker and reads it back should get what it
    /// wrote — so the text lives here rather than being quietly lost.
    pub comments: RefCell<std::collections::HashMap<usize, String>>,

    /// Requests script asked for that have not been answered yet.
    ///
    /// Ordered, so a page that issues ten requests starts them in the order it
    /// wrote them rather than in whatever order a hash landed.
    pub pending_fetches: RefCell<std::collections::BTreeMap<u64, FetchSlot>>,

    /// Ticket numbers for those. Never reused within a realm, so a late reply
    /// cannot resolve a promise that belongs to a later request.
    pub next_fetch: std::cell::Cell<u64>,

    /// Scripts the policy refused, so a `ReferenceError` for a global one of
    /// them would have defined can be attributed to the refusal instead of
    /// being reported as an engine that lacks jQuery.
    pub refused_scripts: RefCell<Vec<String>>,

    /// Sockets this page has open, by the id the prelude holds.
    ///
    /// [`crate::broker::Channel`] rather than the socket itself: the connection
    /// belongs to the broker, and what the page holds is the right to send on
    /// it, take what has arrived, and stop. Closing a `WebSocket` removes it
    /// from here, and dropping the last handle shuts the connection down.
    pub sockets: RefCell<std::collections::BTreeMap<u64, std::sync::Arc<dyn crate::broker::Channel>>>,
    /// Event streams, in a second map for the same reason they were a second
    /// type: one can be sent to and the other cannot, and merging them would
    /// mean a `send` that is meaningful for half the values it accepts.
    pub streams: RefCell<std::collections::BTreeMap<u64, std::sync::Arc<dyn crate::broker::Channel>>>,
    /// Shared, so an id names exactly one connection of either kind.
    pub next_socket: std::cell::Cell<u64>,
}

/// One request a page made, and the receipt it turned into.
///
/// The URL is known when the page asks; the sequence number only when the
/// broker records it, which is why the two are filled in at different moments.
/// Both are needed: the URL is what a human reads, and the sequence number is
/// what joins this action to a row in the request log.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RequestLink {
    /// The realm's own ticket, used to fill in `seq` when the answer arrives.
    #[serde(skip)]
    pub ticket: u64,
    pub url: String,
    /// Absent when the request never got as far as a receipt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

/// How many requests this engine will have on the wire at once.
///
/// Six, which is what browsers settled on per host for HTTP/1.1 — enough that a
/// page fanning out its data loads in parallel instead of in a queue, few
/// enough that a page with two hundred images cannot spawn two hundred threads
/// inside a box with a memory ceiling.
pub const MAX_INFLIGHT_FETCHES: usize = 6;

/// A request script made, waiting its turn or waiting for the wire.
pub enum FetchSlot {
    /// Accepted, not yet started: the in-flight limit was already reached.
    Queued {
        url: url::Url,
        method: String,
        body: Vec<u8>,
        content_type: Option<String>,
        /// The headers the page set, which decide whether this request is
        /// simple enough to send without a preflight.
        headers: Vec<(String, String)>,
        /// How this request treats the origin boundary, and whether it carries
        /// the session across one. Both come from the page's own `fetch` init
        /// rather than being assumed, because assuming either is how a
        /// cross-origin read gets made with credentials nobody asked to send.
        mode: crate::cors::Mode,
        credentials: crate::cors::Credentials,
    },
    /// On the wire, on its own thread, with the answer coming back here.
    InFlight(std::sync::mpsc::Receiver<crate::net::FetchOutcome>),
}

/// How the [`Host`] reaches a native binding.
///
/// Boa's host data must be `Trace`, and `Rc<Host>` is not. The handle exists to
/// carry the `unsafe_ignore_trace` in one place with one justification: `Host`
/// holds no GC-managed value and cannot reach one, because every JS-side thing
/// with a lifetime — listeners, timer callbacks, promise resolvers — lives in
/// `prelude.js` where Boa's own collector already owns it.
#[derive(Clone, Trace, Finalize, JsData)]
pub struct HostHandle(#[unsafe_ignore_trace] pub std::rc::Rc<Host>);

impl std::ops::Deref for HostHandle {
    type Target = Host;
    fn deref(&self) -> &Host {
        &self.0
    }
}

impl Host {
    pub fn new(dom: Dom, broker: std::sync::Arc<dyn Broker>, base: url::Url) -> Self {
        // Asked before the broker is stored, and asked once. Across the process
        // split this is a message, so a binding that asked per property read
        // would put a round trip behind `navigator.platform` — and the client
        // caches the answer, because an identity cannot change while a session
        // runs. One `Arc` clone per realm, and no strings copied.
        #[cfg(feature = "identity")]
        let identity = broker.identity();
        Self {
            dom,
            broker,
            base,
            #[cfg(feature = "identity")]
            identity,
            import_map: RefCell::new(None),
            encoding: RefCell::new(encoding_rs::UTF_8),
            dirty: RefCell::new(false),
            styles_stale: RefCell::new(false),
            console: RefCell::new(Vec::new()),
            unsupported: RefCell::new(Unsupported::default()),
            canvases: RefCell::new(crate::canvas::Canvases::new()),
            requests: RefCell::new(Vec::new()),
            comments: RefCell::new(std::collections::HashMap::new()),
            pending_fetches: RefCell::new(std::collections::BTreeMap::new()),
            next_fetch: std::cell::Cell::new(1),
            refused_scripts: RefCell::new(Vec::new()),
            sockets: RefCell::new(std::collections::BTreeMap::new()),
            streams: RefCell::new(std::collections::BTreeMap::new()),
            next_socket: std::cell::Cell::new(1),
        }
    }

    pub fn mark_dirty(&self) {
        *self.dirty.borrow_mut() = true;
        *self.styles_stale.borrow_mut() = true;
    }

    pub fn take_dirty(&self) -> bool {
        let was = *self.dirty.borrow();
        *self.dirty.borrow_mut() = false;
        was
    }
}
