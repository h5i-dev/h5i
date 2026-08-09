//! The script realm: a JavaScript engine wired to the one real DOM.
//!
//! # Why Boa, and why this version
//!
//! Boa is pure Rust, which keeps the build hermetic: this crate already refuses
//! C build dependencies (`system-fonts` is off to avoid libfontconfig), and the
//! cross-check matrix compiles it for windows-msvc, darwin and musl from Linux,
//! where `ring`'s C build is already a known blocker. QuickJS or V8 would add
//! another dependency of exactly that kind. See ROADMAP §12.2.
//!
//! The version is pinned to 0.19 for a reason that is not preference. Boa 0.20
//! and later depend on `icu_normalizer ~2.0`, and `parley` — which Blitz pulls
//! for text — requires `^2.1.1`. Those ranges are disjoint and semver-compatible,
//! so Cargo must pick one and cannot. Boa 0.19 depends on the 1.x line, which is
//! semver-*incompatible* with 2.x and therefore allowed to coexist. Upstream Boa
//! has already moved `main` to `~2.2.0`, which would resolve, but that is
//! unreleased (`1.0.0-dev`). So this pin is a dated workaround with a known exit:
//! when Boa releases past that change, this moves forward and the duplicate ICU
//! line in the lockfile goes away.
//!
//! # The DOM is not here
//!
//! Every JS object that names a node is a wrapper over a `NodeId`, and the Blitz
//! document remains the only tree. A second tree inside the engine would let the
//! snapshot, the paint, the events and the script state drift apart, and nothing
//! downstream could tell which one was right.

mod dom_api;
pub mod host;
pub mod modules;

use std::rc::Rc;

use boa_engine::{js_string, Context, JsValue, Module, Source};

use crate::engine::Dom;
use host::{ConsoleLine, Host, HostHandle};

/// Boa's own prelude is the language; this is the browser.
const PRELUDE: &str = include_str!("prelude.js");

/// How long a settle may take before it is cut off and *reported* as cut off.
const SETTLE_BUDGET_MS: u64 = 2_000;

/// How far the virtual clock advances per settle round.
///
/// Virtual rather than wall-clock, deliberately: an agent driving this engine
/// does not want to wait out a page's `setTimeout(1000)`, and a run whose
/// timing depends on how loaded the machine was is a run nobody can reproduce.
const TICK_MS: u64 = 16;

/// What a settle actually did, so a caller never has to guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    /// Virtual milliseconds elapsed.
    pub elapsed_ms: u64,
    /// Timer callbacks run.
    pub timers_run: usize,
    /// True when the budget ran out with work still pending. A snapshot taken
    /// here describes a page that had not finished.
    pub cut_off: bool,
    /// Timers still queued when it stopped.
    pub pending_timers: usize,
}

impl Settled {
    /// The one-line form that belongs next to a snapshot.
    pub fn render(&self) -> String {
        if self.cut_off {
            format!(
                "still busy after {}ms ({} timers pending) — this page had not finished",
                self.elapsed_ms, self.pending_timers
            )
        } else {
            format!("settled after {}ms", self.elapsed_ms)
        }
    }
}

/// A JavaScript realm bound to one document.
pub struct Script {
    context: Context,
    host: Rc<Host>,
    /// Module evaluations whose outcome is not known yet. See
    /// [`Script::collect_module_failures`].
    pending_modules: Vec<boa_engine::object::builtins::JsPromise>,
}

impl Script {
    /// Build a realm over `dom`, install the primitives and run the prelude.
    pub fn new(
        dom: Dom,
        broker: std::sync::Arc<crate::net::Broker>,
        base: &url::Url,
    ) -> Result<Self, String> {
        let url = base.to_string();
        let host = Rc::new(Host::new(dom, broker, base.clone()));
        // The loader is built before the context because the context owns it,
        // and it needs the host to reach the broker. Nothing else in the realm
        // is allowed to fetch, so this is the only door modules have.
        let loader = Rc::new(modules::BrokerModuleLoader::new(host.clone()));
        let mut context = Context::builder()
            .module_loader(loader)
            .build()
            .map_err(|e| format!("could not build the script realm: {e}"))?;
        context.insert_data(HostHandle(host.clone()));

        dom_api::install(&mut context).map_err(|e| e.to_string())?;
        context
            .register_global_property(
                js_string!("__h5iUrl"),
                js_string!(url.as_str()),
                boa_engine::property::Attribute::empty(),
            )
            .map_err(|e| e.to_string())?;
        context
            .eval(Source::from_bytes(PRELUDE))
            .map_err(|e| format!("the browser prelude failed to load: {e}"))?;

        Ok(Self {
            context,
            host,
            pending_modules: Vec::new(),
        })
    }

    /// Run one script from the page. An error is returned, not swallowed: a
    /// page whose script threw is a fact the agent needs.
    pub fn eval(&mut self, source: &str) -> Result<(), String> {
        self.context
            .eval(Source::from_bytes(source))
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Run one module from the page.
    ///
    /// Modules are deferred by definition: they parse now and evaluate when
    /// their whole import graph has loaded, which is why this returns nothing
    /// and the result arrives during [`Self::settle`]. A failure to *parse* is
    /// returned here; a failure to load an import surfaces as a rejected
    /// promise, which `settle` reports through the console.
    pub fn eval_module(&mut self, source: &str, path: &str) -> Result<(), String> {
        let source = Source::from_reader(source.as_bytes(), Some(std::path::Path::new(path)));
        let module = Module::parse(source, None, &mut self.context)
            .map_err(|error| format!("module did not parse: {error}"))?;

        // Kept rather than attached to: the outcome is read in `settle`, after
        // the jobs that decide it have run. A module whose import failed
        // otherwise rejects into nothing and the page looks merely empty.
        let promise = module.load_link_evaluate(&mut self.context);
        self.pending_modules.push(promise);
        Ok(())
    }

    /// Report any module that failed to load or threw, once the jobs that would
    /// settle it have run.
    fn collect_module_failures(&mut self) {
        let pending = std::mem::take(&mut self.pending_modules);
        let mut still_pending = Vec::new();

        for promise in pending {
            match promise.state() {
                boa_engine::builtins::promise::PromiseState::Pending => still_pending.push(promise),
                boa_engine::builtins::promise::PromiseState::Fulfilled(_) => {}
                boa_engine::builtins::promise::PromiseState::Rejected(reason) => {
                    let text = reason
                        .to_string(&mut self.context)
                        .map(|s| s.to_std_string_escaped())
                        .unwrap_or_else(|_| "<unrenderable>".to_string());
                    self.host.console.borrow_mut().push(ConsoleLine {
                        level: "error".to_string(),
                        text: format!("module failed: {text}"),
                    });
                }
            }
        }

        // A module still pending when the page settled is one whose imports
        // never arrived. Said plainly, because an agent reading a thin outline
        // would otherwise blame the page.
        for _ in &still_pending {
            self.host.console.borrow_mut().push(ConsoleLine {
                level: "error".to_string(),
                text: "a module was still loading when the page settled: its imports did not \
                       finish arriving"
                    .to_string(),
            });
        }
    }

    /// Evaluate and return the completion value, for tests and for a future
    /// `session eval`.
    pub fn eval_value(&mut self, source: &str) -> Result<String, String> {
        match self.context.eval(Source::from_bytes(source)) {
            Ok(value) => Ok(value
                .to_string(&mut self.context)
                .map(|s| s.to_std_string_escaped())
                .unwrap_or_else(|_| "<unrenderable>".to_string())),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Run everything the page still owes, and say what happened.
    ///
    /// "Run until settled" is a subsystem rather than a phrase (ROADMAP §12.4).
    /// The loop drains promise jobs, then any timer now due on the virtual
    /// clock, then repeats — because a timer can queue a promise and a promise
    /// can set a timer. It stops when a round does nothing, or when the budget
    /// is spent, and the difference is reported rather than hidden. A snapshot
    /// that quietly returned early is a wrong answer that looks like a right one.
    pub fn settle(&mut self) -> Settled {
        let mut clock = 0u64;
        let mut timers_run = 0usize;

        loop {
            self.context.run_jobs();

            // Layout observers are driven from here rather than from a frame
            // clock, because this engine has no frames at rest: an observer
            // that waited for a repaint would never fire at all.
            self.run_layout_observers();

            let ran = self.run_due_timers(clock);
            timers_run += ran;

            if ran == 0 {
                // Nothing was due now. If something is still queued, advance
                // the virtual clock toward it rather than sleeping.
                if self.pending_timers() == 0 {
                    self.collect_module_failures();
                    return Settled {
                        elapsed_ms: clock,
                        timers_run,
                        cut_off: false,
                        pending_timers: 0,
                    };
                }
                clock += TICK_MS;
            }

            if clock >= SETTLE_BUDGET_MS {
                self.context.run_jobs();
                self.collect_module_failures();
                return Settled {
                    elapsed_ms: clock,
                    timers_run,
                    cut_off: true,
                    pending_timers: self.pending_timers(),
                };
            }
        }
    }

    fn run_layout_observers(&mut self) {
        let _ = self
            .context
            .eval(Source::from_bytes("__h5iRunLayoutObservers()"));
    }

    fn run_due_timers(&mut self, clock: u64) -> usize {
        let source = format!("__h5iRunTimers({clock})");
        match self.context.eval(Source::from_bytes(&source)) {
            Ok(JsValue::Integer(n)) => n.max(0) as usize,
            Ok(JsValue::Rational(n)) => n.max(0.0) as usize,
            _ => 0,
        }
    }

    fn pending_timers(&mut self) -> usize {
        match self
            .context
            .eval(Source::from_bytes("__h5iPendingTimers()"))
        {
            Ok(JsValue::Integer(n)) => n.max(0) as usize,
            Ok(JsValue::Rational(n)) => n.max(0.0) as usize,
            _ => 0,
        }
    }

    /// Fire an event at a node, the way a real click would.
    pub fn dispatch(&mut self, node_id: usize, event_type: &str) -> Result<(), String> {
        // Constructed by kind rather than always as a bare `Event`, because a
        // handler reading `event.key` or `event.clientX` off a click gets
        // `undefined` otherwise and takes a branch it should not.
        let constructor = match event_type {
            "click" | "mousedown" | "mouseup" => "MouseEvent",
            "keydown" | "keyup" | "keypress" => "KeyboardEvent",
            "input" => "InputEvent",
            _ => "Event",
        };
        let source = format!(
            "(() => {{ const target = __h5iWrapById({node_id}); \
             if (target) target.dispatchEvent(new {constructor}({event_type:?}, \
               {{ bubbles: true, cancelable: true }})); }})()"
        );
        self.eval(&source)
    }

    /// Did script change the tree since this was last asked?
    pub fn take_dirty(&self) -> bool {
        self.host.take_dirty()
    }

    /// Record an error the host saw, so it lands with the page's own console
    /// output rather than in a stream nobody reads.
    pub fn note_error(&self, text: &str) {
        self.name_missing_global(text);
        self.host.console.borrow_mut().push(ConsoleLine {
            level: "error".to_string(),
            text: text.to_string(),
        });
    }

    /// Record the identifier behind a `ReferenceError` as an API we lack.
    ///
    /// The prelude can trap an unknown property on an object it owns, but it
    /// cannot trap a name that was never declared: `Sentry.init(...)` throws
    /// before any object is consulted. The thrown message is the only evidence
    /// there is, and it happens to carry exactly the missing name. Reading it
    /// back turns six anonymous console lines into six named gaps.
    fn name_missing_global(&self, text: &str) {
        let Some((_, rest)) = text.split_once("ReferenceError: ") else {
            return;
        };
        let Some((name, _)) = rest.split_once(" is not defined") else {
            return;
        };
        // Only accept something shaped like an identifier, so a page that puts
        // this phrasing in a thrown string cannot write arbitrary text into the
        // list an agent reads.
        let name = name.trim();
        let identifier = !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
        if identifier {
            self.host.unsupported.borrow_mut().record(name);
        }
    }

    /// Point `document.currentScript` at the element whose code is about to run.
    ///
    /// A page reads it to find its own tag and the `data-` attributes
    /// configuring it. Returning null unconditionally is right for a module and
    /// wrong for an inline classic script, and the wrong one reads as "this page
    /// has no configuration" rather than as a gap.
    pub fn set_current_script(&mut self, node: Option<usize>) {
        let code = match node {
            Some(id) => format!("globalThis.__h5iCurrentScript = {id};"),
            None => "globalThis.__h5iCurrentScript = null;".to_string(),
        };
        let _ = self.context.eval(Source::from_bytes(&code));
    }

    pub fn console(&self) -> Vec<ConsoleLine> {
        self.host.console.borrow().clone()
    }

    /// Web APIs the page asked for and this engine does not have, most-used
    /// first. Surfaced in the snapshot rather than logged, so an agent finds out
    /// where it is reading.
    pub fn unsupported(&self) -> Vec<(String, usize)> {
        self.host.unsupported.borrow().ranked()
    }

    /// URLs script asked for since the last time this was taken.
    ///
    /// Drained rather than accumulated, so a caller can attribute requests to
    /// the action it just performed instead of to the whole session.
    pub fn take_requests(&self) -> Vec<String> {
        std::mem::take(&mut *self.host.requests.borrow_mut())
    }
}

#[cfg(test)]
mod tests;
