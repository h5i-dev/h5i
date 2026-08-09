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
use crate::net::Broker;

/// One line a page wrote to the console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleLine {
    pub level: String,
    pub text: String,
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
    pub broker: std::sync::Arc<Broker>,

    /// The page this document was loaded from, for resolving relative fetches.
    pub base: url::Url,

    /// Set whenever script changed the tree, so the engine knows to re-resolve
    /// style and layout once rather than after every mutation.
    pub dirty: RefCell<bool>,

    pub console: RefCell<Vec<ConsoleLine>>,

    pub unsupported: RefCell<Unsupported>,

    /// URLs script asked for, in order, so a caller can say which action caused
    /// which request. The receipt remains the record; this is only the link,
    /// stamped by the one component that knows the causal fact.
    pub requests: RefCell<Vec<String>>,

    /// Comment text, keyed by node id.
    ///
    /// `NodeData::Comment` carries no payload in this version of blitz, and a
    /// page that writes a comment marker and reads it back should get what it
    /// wrote — so the text lives here rather than being quietly lost.
    pub comments: RefCell<std::collections::HashMap<usize, String>>,

    /// Scripts the policy refused, so a `ReferenceError` for a global one of
    /// them would have defined can be attributed to the refusal instead of
    /// being reported as an engine that lacks jQuery.
    pub refused_scripts: RefCell<Vec<String>>,
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
    pub fn new(dom: Dom, broker: std::sync::Arc<Broker>, base: url::Url) -> Self {
        Self {
            dom,
            broker,
            base,
            dirty: RefCell::new(false),
            console: RefCell::new(Vec::new()),
            unsupported: RefCell::new(Unsupported::default()),
            requests: RefCell::new(Vec::new()),
            comments: RefCell::new(std::collections::HashMap::new()),
            refused_scripts: RefCell::new(Vec::new()),
        }
    }

    pub fn mark_dirty(&self) {
        *self.dirty.borrow_mut() = true;
    }

    pub fn take_dirty(&self) -> bool {
        let was = *self.dirty.borrow();
        *self.dirty.borrow_mut() = false;
        was
    }
}
