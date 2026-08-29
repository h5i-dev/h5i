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

pub(crate) mod dom_api;
pub mod host;
pub mod import_map;
pub mod modules;

use std::rc::Rc;

use std::time::Duration;

use boa_engine::{js_string, Context, Module, Source};

use crate::engine::Dom;
use host::{ConsoleLine, Host, HostHandle};

/// Boa's own prelude is the language; this is the browser.
const PRELUDE: &str = include_str!("prelude.js");

/// The pieces of the prelude a page has to *ask* for.
///
/// **What this is worth changed, and it is worth less than it was.** Parse and
/// compile were 67 ms of the 83 a realm cost and were paid by every page; they
/// are now paid once per thread (see [`compiled_prelude`]) and a later realm
/// spends under a microsecond there. What a tier still saves per page is the
/// *running* of its code — building its classes and prototypes — which is
/// 12.4 ms for the whole 273 KiB core, so roughly 45 µs per KiB rather than the
/// ~150 µs per KiB it was when the parse was in the number too.
///
/// It still saves the compile for the first page a renderer serves, and that is
/// the page a person is waiting on.
///
/// Code, not bytes: blanking all 164 KiB of comments in a 443 KiB prelude
/// changed parse time by nothing measurable, so the documentation is free and
/// only what the parser tokenises is not. So a surface most pages never touch
/// lives in its own file, is not given to the parser with the core, and arrives
/// when something reads the name it defines — see `lazyGlobals` in the core
/// prelude, which installs the getter that calls back into [`load_tier`].
///
/// The bar for moving something here is that a page which *does* use it pays
/// slightly more (one extra parse of a small file, on every realm that asks —
/// tiers are not shared the way the core is) and a page which does not pays
/// nothing. That trade only holds while the tier is genuinely optional:
/// anything the first paint of an ordinary page touches belongs in the core.
const TIERS: &[(&str, &str)] = &[
    ("conformance", include_str!("prelude/conformance.js")),
    ("sockets", include_str!("prelude/sockets.js")),
    ("has", include_str!("prelude/has.js")),
];

/// Evaluate one tier into this realm, by name.
///
/// Reached from JavaScript as `__h5iTier("name")`. Evaluating into the same
/// realm rather than a fresh one is the whole point: a tier finishes building
/// the object model the core started, so it needs the core's interfaces, and it
/// reaches them through `__h5iInternals` rather than through a shared closure —
/// a separately parsed source has no way into the core's scope.
fn load_tier(
    _this: &boa_engine::JsValue,
    args: &[boa_engine::JsValue],
    context: &mut Context,
) -> boa_engine::JsResult<boa_engine::JsValue> {
    use boa_engine::JsArgs;
    let name = args
        .get_or_undefined(0)
        .to_string(context)?
        .to_std_string_escaped();
    let Some((_, source)) = TIERS.iter().find(|(known, _)| *known == name) else {
        return Err(boa_engine::JsError::from_opaque(
            js_string!(format!("no such prelude tier: {name}")).into(),
        ));
    };
    // Named like the core prelude's own frames, and for the same reason: a
    // stack frame from here is a bug report against this engine, not against
    // the page.
    let path = format!("<h5i browser prelude: {name}>");
    context.eval(Source::from_reader(
        source.as_bytes(),
        Some(std::path::Path::new(&path)),
    ))?;
    Ok(boa_engine::JsValue::undefined())
}

thread_local! {
    /// The prelude, parsed and compiled once for this thread.
    ///
    /// A renderer serves every navigation of a session on one thread, so this
    /// is once per process in the product and once per test in the suite.
    ///
    /// The context it was compiled against is kept with it, and is deliberately
    /// a plain one: no host, no document, no broker. It exists to own the realm
    /// the compiler resolved names against and nothing else, so holding it for
    /// the life of the thread cannot pin a page's DOM — which storing the
    /// *first page's* context here would have done.
    /// **Deliberately holds nothing that will be dropped.** See
    /// [`PreludeTemplate`].
    static PRELUDE_TEMPLATE:
        std::cell::RefCell<Option<std::mem::ManuallyDrop<PreludeTemplate>>> =
        const { std::cell::RefCell::new(None) };
}

/// The compiled prelude and the realm it was compiled against.
///
/// # Why this is never dropped
///
/// It holds `Gc` handles, and freeing those needs Boa's garbage-collected heap
/// — which lives in a thread-local of its own (`BOA_GC`). Rust does not specify
/// the order thread-local destructors run in, so a thread ending could destroy
/// that heap first and leave this dropping handles into freed memory. It does
/// exactly that, reproducibly, and the symptom is not a panic but
/// `tcache_thread_shutdown(): unaligned tcache chunk detected` and `SIGABRT` as
/// the thread exits, with the work already finished and correct.
///
/// It depended on the order the two thread-locals were *first touched*, which is
/// why it appeared only once the compile moved into the navigation. Building a
/// realm touches Boa's heap before it touches this, so its destructor was
/// registered first and ran last, and everything was fine. Warming touches this
/// one first, inverting the order — and warming before a realm exists is the
/// whole point of the overlap (§B15.12a).
///
/// Wrapping it in `ManuallyDrop` means this thread-local has no drop glue at
/// all, so no destructor is registered for it and the order cannot matter. What
/// is left behind is the `Context` and `Script` structs; the GC heap they point
/// into is freed by `BOA_GC`'s own teardown, so this leaks a bounded amount per
/// thread that compiled a prelude rather than a growing one.
///
/// `the_compile_survives_a_thread_that_warmed_before_it_had_a_realm` is the
/// guard, and it fails as an abort rather than an assertion, so it has to run
/// in a thread that then exits.
struct PreludeTemplate {
    /// Held only to keep the compilation realm alive; never run.
    _context: Context,
    script: boa_engine::Script,
}

/// The compiled prelude for this thread, compiling it if this is the first ask.
///
/// The returned handle shares the code block rather than copying it: what comes
/// back is cheap, and what it costs to *make* is paid once.
fn compiled_prelude() -> Result<boa_engine::Script, String> {
    PRELUDE_TEMPLATE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            let mut context = Context::default();
            let script = boa_engine::Script::parse(
                Source::from_reader(PRELUDE.as_bytes(), Some(std::path::Path::new(PRELUDE_PATH))),
                None,
                &mut context,
            )
            .map_err(|e| format!("the browser prelude did not parse: {e}"))?;
            // Compiled here rather than left to the first `bind_to_realm`, so
            // the cost lands on this function and the phase it is measured in.
            script
                .codeblock(&mut context)
                .map_err(|e| format!("the browser prelude did not compile: {e}"))?;
            *slot = Some(std::mem::ManuallyDrop::new(PreludeTemplate {
                _context: context,
                script,
            }));
        }
        Ok(slot
            .as_ref()
            .expect("the template was just built")
            .script
            .clone())
    })
}

/// Compile the prelude now, so the next realm on this thread does not have to.
///
/// Speculative by nature: this is called while a navigation is in flight, before
/// anyone knows whether the page has any script at all, because that is the only
/// window in which the compile overlaps anything (§B15.12a). A page that turns
/// out to have no script has thrown the work away — which is why the caller is
/// careful about *when* it speculates, and why the compile is only ever paid
/// once per thread however many times this is called.
///
/// Errors are dropped rather than returned. A prelude that will not compile is a
/// broken build, it will fail again at [`Script::with_options`] a moment later
/// with the realm's own error, and reporting it here would attach it to the
/// navigation instead of to the realm it actually stops.
pub fn warm_prelude() {
    let _ = compiled_prelude();
}

/// What building one realm cost, by phase.
///
/// Kept because the realm is most of what a small page costs (§B8.9) and the
/// phases move independently: the prelude's *compile* is now paid once per
/// thread while its *run* is paid per page, and a total alone could not tell
/// those apart or show the sharing still working.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealmCost {
    /// Building the Boa context: the language's own globals and intrinsics.
    pub context: Duration,
    /// Installing this engine's native primitives, and the globals the prelude
    /// reads on its way up.
    pub primitives: Duration,
    /// Parsing and compiling the prelude. Near zero after the first realm on a
    /// thread; that it is near zero is the whole point.
    pub prelude_compile: Duration,
    /// Running the prelude: building the object model this realm hands the page.
    pub prelude_run: Duration,
}

impl RealmCost {
    /// What the realm cost in total.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.context + self.primitives + self.prelude_compile + self.prelude_run
    }
}

/// What a stack frame inside this engine's own prelude is called.
///
/// The prelude was the one source evaluated without a path, so any frame in it
/// read `unknown at :1598:24` — and a bug in *our* DOM implementation looked
/// exactly like a bug in the page's bundle. It took three sites failing
/// identically to notice. A frame that names this file is a bug report against
/// this engine, and it should say so.
const PRELUDE_PATH: &str = "<h5i browser prelude>";

/// How much *virtual* time a settle may cover before it is cut off and reported
/// as cut off.
///
/// Raised from two seconds once there was a real bound to lean on. Virtual time
/// is free — advancing it costs only the work the page actually does — so this
/// number was never protecting against a slow page, it was standing in for a
/// wall-clock guard that did not exist. [`JOB_QUEUE_BUDGET`] is that guard now.
///
/// Two seconds was cutting real applications off mid-render: preactjs.com
/// fetches its content, then needs five virtual seconds to render it, and was
/// being stopped at two with its own data already in hand.
pub(crate) const SETTLE_BUDGET_MS: u64 = 10_000;

/// How far the virtual clock advances per settle round.
///
/// Virtual rather than wall-clock, deliberately: an agent driving this engine
/// does not want to wait out a page's `setTimeout(1000)`, and a run whose
/// timing depends on how loaded the machine was is a run nobody can reproduce.
const TICK_MS: u64 = 16;

/// How long to wait, in *real* time, for requests that are actually on the wire.
///
/// Separate from [`SETTLE_BUDGET_MS`] because that budget is virtual: it counts
/// the time a page's timers believe has passed, and advancing it costs nothing.
/// A network round trip costs what it costs, and a page that fetches its content
/// is not idle while it waits — settling it early would report an empty page as
/// finished. Bounded all the same: a server that never answers must not become
/// this engine hanging.
const NETWORK_BUDGET_MS: u64 = 10_000;

/// How long to sleep between checks while waiting on the wire. Short enough not
/// to add measurable latency, long enough not to spin a core.
const NETWORK_POLL_MS: u64 = 2;

/// How deep script may recurse before the engine stops it.
///
/// Boa's default is 512 frames, which a production bundle exceeds during its
/// own initialisation — deeply nested module factories and framework internals
/// get there without anything being wrong. Next.js's chunk hit it while merely
/// starting up, and the page reported a runtime limit instead of rendering.
///
/// Still a bound: the point of the limit is that a runaway page cannot take the
/// stack with it, and this engine runs untrusted script inside a box with a
/// memory ceiling.
const RECURSION_LIMIT: usize = 4_000;

/// How many value slots the interpreter stack may hold.
///
/// Raised with the recursion limit, and it has to be: the default 10240 slots
/// runs out at roughly 800 frames, so raising the frame count alone changed
/// nothing — the *stack* limit was what a deep call actually hit. Each frame
/// costs several slots, so this is sized to the frame budget above with room
/// for the locals a real function holds.
const STACK_SIZE_LIMIT: usize = 128 * 1024;

/// How many times a single loop may go round before the engine stops it.
///
/// The settle loop bounds virtual time and network waiting, but neither covers
/// the one case that matters most: a single `eval` that never returns. A page
/// that loops forever would hang this engine indefinitely, and an agent waiting
/// on it has no way to tell that from a slow page.
///
/// Generous — a real page parsing or laying out its own data can legitimately
/// iterate millions of times — but finite, because "always returns" is a
/// property worth more than the last page that needed one more round. A page
/// that trips it still renders what it managed, and the limit is *reported*, so
/// a thin outline is explained rather than mysterious.
///
/// Worth being exact about what this does not do: it bounds one loop, not total
/// work. Boa exposes no wall-clock interrupt, so a page with a thousand slow
/// loops is still slow, and a caller that cannot wait must impose its own
/// timeout — the corpus harness does. Raising this to 50 million turned a site
/// that returned in three minutes into one that had not returned in four.
const LOOP_ITERATION_LIMIT: u64 = 5_000_000;

/// How long the job queue may run before the engine tells it to stop, when
/// nobody has said otherwise.
///
/// `Page::run_scripts` overrides this with whatever is left of the script
/// phase, so the two budgets do not add up. This value is the one a caller
/// driving a realm directly gets.
///
/// This is the wall-clock bound the other limits could not provide. A module
/// graph evaluates entirely inside `run_jobs`, so neither the settle budget nor
/// the script-phase budget ever got a turn — lit.dev spent **seven minutes**
/// there. Boa checks a cancellation token between jobs, and a watchdog thread
/// sets it, because by the time the deadline matters this thread is the one
/// that is stuck.
///
/// It bounds work *between* jobs, not inside one: a single job that never
/// returns is still beyond reach, and the loop-iteration limit is the only
/// guard there. Together they cover the two shapes that actually occur.
const JOB_QUEUE_BUDGET: Duration = Duration::from_secs(15);

/// Host hooks, for the one thing the default implementation drops on the floor.
///
/// A promise that rejects with nothing attached to it is how asynchronous code
/// dies, and it was completely silent here: a page could reject three times and
/// the console said nothing, so a half-built page came with no explanation at
/// all. That is the failure §8.3 exists to prevent, arriving through the one
/// channel the engine was not watching.
///
/// Only reported when the rejection goes *unhandled*. A rejection that is caught
/// later is not an error, and `Reject` fires before anyone has had the chance to
/// attach a handler, so reporting there would blame every page that uses
/// `.catch`.
#[derive(Debug)]
struct Hooks;

impl boa_engine::context::HostHooks for Hooks {
    fn promise_rejection_tracker(
        &self,
        promise: &boa_engine::object::JsObject<boa_engine::builtins::promise::Promise>,
        operation: boa_engine::builtins::promise::OperationType,
        context: &mut Context,
    ) {
        if !matches!(
            operation,
            boa_engine::builtins::promise::OperationType::Reject
        ) {
            return;
        }
        let Some(host) = context.get_data::<HostHandle>().cloned() else {
            return;
        };
        // Through `JsPromise`, because `Promise::state` is crate-private: the
        // public wrapper is the supported way to read a settled value.
        let state = boa_engine::object::builtins::JsPromise::from(promise.clone()).state();
        let reason = match state {
            boa_engine::builtins::promise::PromiseState::Rejected(value) => {
                value.display().to_string()
            }
            _ => "no reason given".to_string(),
        };
        host::push_console(
            &mut host.console.borrow_mut(),
            ConsoleLine::engine(
                "error",
                format!("a promise rejected and nothing handled it: {reason}"),
            ),
        );
    }
}

/// What the loop asks between rounds.
///
/// Two kinds because they cannot share a representation. A DOM condition is a
/// Rust closure over the document; a page condition needs the realm, which the
/// loop is already holding mutably, so it travels as source and is evaluated
/// by the loop itself.
enum Ready<'a> {
    Rust(&'a mut dyn FnMut() -> bool),
    Expr(String),
}

/// How a wait ended.
///
/// Four outcomes, kept apart because they ask the caller for different things.
/// `Met` is the answer. `Quiescent` means the page has nothing left to run, so
/// waiting longer cannot change anything — a different fact from `Budget`,
/// which means time ran out with work still pending and the condition might
/// yet have come true.
///
/// `Periodic` is the one the others hid. A page whose only remaining work is a
/// loop that re-arms itself — an animation frame, a poll, a carousel — is
/// neither finished nor converging. Reporting it as `Quiescent` would claim
/// nothing can change, which is false, because the loop runs and can touch the
/// DOM. Reporting it as `Budget` claims the page was on its way somewhere and
/// merely ran out of time, which is the claim that sent agents back to wait
/// again on a page that will never arrive.
///
/// Collapsing any of these into "timed out" is the lie this engine keeps
/// refusing elsewhere: it would report a page that had finished and a page that
/// was cut off as the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitEnd {
    /// The condition came true.
    Met,
    /// The page went quiet without it. Nothing is left that could change it.
    Quiescent,
    /// The budget ran out with work still pending.
    Budget,
    /// The page has only self-rescheduling work left. It is not converging, so
    /// waiting is not a strategy — but it is not finished either.
    Periodic,
}

impl WaitEnd {
    pub fn as_str(self) -> &'static str {
        match self {
            WaitEnd::Met => "met",
            WaitEnd::Quiescent => "quiescent",
            WaitEnd::Budget => "budget",
            WaitEnd::Periodic => "periodic",
        }
    }
}

/// What a wait did.
#[derive(Debug, Clone)]
pub struct Waited {
    /// Whether the condition was true when the wait stopped.
    pub met: bool,
    /// Whether the page changed while waiting.
    ///
    /// Set by the caller from the realm's dirty flag, because the settle's own
    /// counters do not see it: a socket message delivers on real time, so it
    /// advances neither the virtual clock nor the timer count. A wait satisfied
    /// by one therefore looked like a wait that did nothing, and every attached
    /// viewer kept showing the page from before the message.
    pub changed: bool,
    /// The settle underneath it, so the caller sees the same accounting a
    /// snapshot would have carried.
    pub settled: Settled,
    pub end: WaitEnd,
}

impl Waited {
    /// The one-line form, addressed to a reader deciding what to do next.
    pub fn render(&self) -> String {
        match self.end {
            WaitEnd::Met => format!("found after {}ms", self.settled.elapsed_ms),
            WaitEnd::Quiescent => format!(
                "not found, and the page has nothing left to run after {}ms — waiting longer \
                 cannot change this",
                self.settled.elapsed_ms
            ),
            WaitEnd::Budget => format!(
                "not found after {}ms, and the page was still working ({} timers pending) — \
                 it may yet appear",
                self.settled.elapsed_ms, self.settled.pending_timers
            ),
            WaitEnd::Periodic => format!(
                "not found after {}ms, and the only work left on this page is {} \
                 self-rescheduling timer(s) — an animation or polling loop, which will not \
                 converge no matter how long you wait",
                self.settled.elapsed_ms, self.settled.periodic_timers
            ),
        }
    }
}

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
    /// Timers still queued when it stopped, counting only the ones the page
    /// still owes: one-shot, and not so deep in a self-arming chain that they
    /// have stopped converging.
    pub pending_timers: usize,
    /// Timers armed but no longer holding the page open: intervals, and
    /// one-shots past the nesting limit. A page with these is running, but it
    /// is not running *towards* anything.
    pub periodic_timers: usize,
}

impl Settled {
    /// The one-line form that belongs next to a snapshot.
    pub fn render(&self) -> String {
        if self.cut_off {
            format!(
                "still busy after {}ms ({} timers pending) — this page had not finished",
                self.elapsed_ms, self.pending_timers
            )
        } else if self.periodic_timers > 0 {
            // Not "still busy": this page *did* finish everything it owed. The
            // note exists because a periodic loop makes two reads of one page
            // disagree without the agent having acted, which is the same
            // caveat `open_sockets` carries and for the same reason.
            format!(
                "settled after {}ms, with {} self-rescheduling timer(s) still running — \
                 this page has an animation or polling loop, so a later read may differ",
                self.elapsed_ms, self.periodic_timers
            )
        } else {
            format!("settled after {}ms", self.elapsed_ms)
        }
    }
}

/// What a realm is built with, beyond the document it is bound to.
///
/// A struct rather than more parameters on [`Script::new`], because every one
/// of these is a switch an *instrument* throws and nothing else does, and a
/// caller that wants none of them should not have to say so.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealmOptions {
    /// Install the WebIDL member decoration: enumerable interface members, and
    /// the brand check that makes an accessor reached on the prototype object
    /// itself throw rather than run against nothing.
    ///
    /// **For instruments.** `idlharness` checks both on every member of every
    /// interface, and nothing else does: a page reads `el.href`, it does not
    /// ask whether the descriptor is enumerable. Installing it walks every own
    /// property of every interface prototype and rebuilds each descriptor with
    /// two closures, which measured **15 ms of the 83 ms** a realm cost — paid
    /// by every page, observed by one harness.
    ///
    /// Not the `get x`/`set x` accessor naming, which reads as the third thing
    /// this does and is not: Boa names class accessors correctly on its own and
    /// the reflection tables name theirs, so that part is already true without
    /// this. `the_webidl_decoration_arrives_only_when_an_instrument_asks` pins
    /// all three so the distinction cannot rot.
    ///
    /// So it is off unless asked for, `wpt/run.py` asks for it, and the
    /// conformance number is unchanged. Same shape as `--script-seconds`: an
    /// instrument says what it is doing, and it changes nothing for anyone who
    /// does not pass it.
    pub webidl_conformance: bool,
}

/// A JavaScript realm bound to one document.
pub struct Script {
    context: Context,
    host: Rc<Host>,
    /// Module evaluations whose outcome is not known yet. See
    /// [`Script::collect_module_failures`].
    /// Modules whose evaluation is still outstanding, each with the specifier
    /// it was loaded under.
    ///
    /// The name travels with the promise because a failure reported as "module
    /// failed" names nothing — the same anonymity §8.3 removed from script
    /// errors, one level up. An agent cannot act on it and neither can we.
    pending_modules: Vec<(String, boa_engine::object::builtins::JsPromise)>,

    /// Set from a watchdog thread to make the job queue give up.
    ///
    /// The engine has to come back. Boa checks this between jobs, which covers
    /// the case that actually bites — a module graph is many jobs, and lit.dev
    /// spent seven minutes in one `run_jobs` call working through one.
    cancel: std::sync::Arc<portable_atomic::AtomicBool>,

    /// How long the job queue may run. Overridable so a test can prove the
    /// deadline fires without waiting the real budget out.
    job_budget: Duration,

    /// What building this realm cost, by phase. See [`RealmCost`].
    cost: RealmCost,
}

impl Script {
    /// Build a realm over `dom`, install the primitives and run the prelude.
    pub fn new(
        dom: Dom,
        broker: std::sync::Arc<dyn crate::broker::Broker>,
        base: &url::Url,
    ) -> Result<Self, String> {
        Self::with_options(dom, broker, base, RealmOptions::default())
    }

    /// The same, for a caller that has an instrument's switches to throw.
    pub fn with_options(
        dom: Dom,
        broker: std::sync::Arc<dyn crate::broker::Broker>,
        base: &url::Url,
        options: RealmOptions,
    ) -> Result<Self, String> {
        let mut cost = RealmCost::default();
        let at = std::time::Instant::now();
        let url = base.to_string();
        let host = Rc::new(Host::new(dom, broker, base.clone()));
        // The loader is built before the context because the context owns it,
        // and it needs the host to reach the broker. Nothing else in the realm
        // is allowed to fetch, so this is the only door modules have.
        let loader = Rc::new(modules::BrokerModuleLoader::new(host.clone()));
        // Our own executor, so its cancellation token is reachable. The token
        // is an `Arc<AtomicBool>` the executor checks *between jobs*, which is
        // the only wall-clock lever Boa offers: `run_jobs` otherwise returns
        // when it returns, and a module graph evaluates entirely inside it.
        let executor = std::rc::Rc::new(boa_engine::job::SimpleJobExecutor::new());
        let cancel = executor.get_cancellation_token();

        let mut context = Context::builder()
            .job_executor(executor)
            .module_loader(loader)
            .host_hooks(std::rc::Rc::new(Hooks))
            .build()
            .map_err(|e| format!("could not build the script realm: {e}"))?;
        // Boa's default recursion ceiling is low enough that a real production
        // bundle hits it: Next.js's chunk exceeded it while merely initialising,
        // and the page reported "exceeded maximum number of recursive calls"
        // instead of rendering. Raised, not removed — the limit is what stops a
        // runaway page from taking the stack with it, and this engine runs
        // untrusted script inside a box with a memory ceiling.
        context
            .runtime_limits_mut()
            .set_recursion_limit(RECURSION_LIMIT);
        context
            .runtime_limits_mut()
            .set_stack_size_limit(STACK_SIZE_LIMIT);
        context
            .runtime_limits_mut()
            .set_loop_iteration_limit(LOOP_ITERATION_LIMIT);

        cost.context = at.elapsed();

        let at = std::time::Instant::now();
        context.insert_data(HostHandle(host.clone()));

        dom_api::install(&mut context).map_err(|e| e.to_string())?;
        context
            .register_global_property(
                js_string!("__h5iUrl"),
                js_string!(url.as_str()),
                boa_engine::property::Attribute::empty(),
            )
            .map_err(|e| e.to_string())?;
        // Built by hand rather than through `register_global_callable` so it
        // carries the same attributes `__h5iUrl` does: a page walking its own
        // globals must not find this engine's machinery among them.
        let tier_loader = boa_engine::object::FunctionObjectBuilder::new(
            context.realm(),
            boa_engine::NativeFunction::from_fn_ptr(load_tier),
        )
        .name("__h5iTier")
        .length(1)
        .build();
        context
            .register_global_property(
                js_string!("__h5iTier"),
                tier_loader,
                boa_engine::property::Attribute::empty(),
            )
            .map_err(|e| e.to_string())?;
        // Read by the core prelude at the one point the decoration has to
        // happen: after every prototype is populated and before the interfaces
        // reach the page.
        context
            .register_global_property(
                js_string!("__h5iConformance"),
                options.webidl_conformance,
                boa_engine::property::Attribute::empty(),
            )
            .map_err(|e| e.to_string())?;
        cost.primitives = at.elapsed();

        // Compiled once for this thread, run once per realm.
        //
        // Parse and compile were 67 ms of the 83 a realm cost (§B8.9), paid
        // again by every page for a source that never changes. They are now
        // paid by the first page a renderer serves and by no other, taking a
        // later realm to ~15 ms — the largest saving this engine has taken.
        //
        // The sibling item, reusing the *realm* across navigations, stays
        // refused — see `Page::run_scripts`. This is not that. Only the
        // instructions are shared; every page still gets its own realm, its own
        // globals and its own prototypes, and nothing a page did is reachable
        // from the next one. See `bind_to_realm` in our Boa fork for what makes
        // the sharing safe, and §B15.12a for the reasoning it replaces.
        let at = std::time::Instant::now();
        let template = compiled_prelude()?;
        cost.prelude_compile = at.elapsed();

        let at = std::time::Instant::now();
        let realm = context.realm().clone();
        template
            .bind_to_realm(realm)
            .map_err(|e| format!("the browser prelude could not be bound to this realm: {e}"))?
            .evaluate(&mut context)
            .map_err(|e| format!("the browser prelude failed to load: {e}"))?;
        cost.prelude_run = at.elapsed();

        Ok(Self {
            context,
            cancel,
            job_budget: JOB_QUEUE_BUDGET,
            cost,
            host,
            pending_modules: Vec::new(),
        })
    }

    /// What building this realm cost, by phase.
    #[must_use]
    pub fn cost(&self) -> RealmCost {
        self.cost
    }

    /// Run one script from the page. An error is returned, not swallowed: a
    /// page whose script threw is a fact the agent needs.
    pub fn eval(&mut self, source: &str) -> Result<(), String> {
        self.eval_named(source, "inline script")
    }

    /// Run one script, under a name that will appear in its stack trace.
    ///
    /// The name matters more than it looks. Boa 0.21 reports a position per
    /// frame, but the *path* comes from the source it was given — and a source
    /// built from bytes has none, so every frame read `unknown at :2:18`. A line
    /// number with no file is barely better than no line number when a page has
    /// nine scripts.
    ///
    /// Lines are counted from the start of *this* script, not of the document,
    /// which is the only frame of reference an inline script has.
    pub fn eval_named(&mut self, source: &str, name: &str) -> Result<(), String> {
        let source = Source::from_reader(source.as_bytes(), Some(std::path::Path::new(name)));
        self.context
            .eval(source)
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
        self.pending_modules.push((path.to_string(), promise));
        Ok(())
    }

    /// Report any module that failed to load or threw, once the jobs that would
    /// settle it have run.
    fn collect_module_failures(&mut self) {
        let pending = std::mem::take(&mut self.pending_modules);
        let mut still_pending: Vec<(String, boa_engine::object::builtins::JsPromise)> = Vec::new();

        for (name, promise) in pending {
            match promise.state() {
                boa_engine::builtins::promise::PromiseState::Pending => {
                    still_pending.push((name, promise))
                }
                boa_engine::builtins::promise::PromiseState::Fulfilled(_) => {}
                boa_engine::builtins::promise::PromiseState::Rejected(reason) => {
                    // Through `JsError` rather than by stringifying the thrown
                    // value: the value renders as "TypeError: ..." and stops
                    // there, while the error carries the stack trace Boa 0.21
                    // records — which is the whole reason for being on 0.21.
                    let text = boa_engine::JsError::from_opaque(reason).to_string();
                    crate::script::host::push_console(
                &mut self.host.console.borrow_mut(),
                ConsoleLine::engine("error", format!("{name}: module failed: {text}")),
            );
                }
            }
        }

        // A module still pending when the page settled is one whose imports
        // never arrived. Said plainly, because an agent reading a thin outline
        // would otherwise blame the page.
        for (name, _) in &still_pending {
            crate::script::host::push_console(
                &mut self.host.console.borrow_mut(),
                ConsoleLine::engine(
                    "error",
                    format!(
                        "{name}: still loading when the page settled — its imports did not \
                         finish arriving"
                    ),
                ),
            );
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
        let budget = self.job_budget;
        let (mut settled, cut_short) =
            self.with_job_deadline(budget, |script| script.settle_inner(None).0);
        if cut_short {
            settled.cut_off = true;
            self.note_error(&format!(
                "this page's script was still working after {:.0?}, so the engine stopped it. \
                 What follows is what it had rendered by then.",
                budget
            ));
        }
        settled
    }

    /// Run until `ready` answers true, or until nothing is left to wait on.
    ///
    /// The wait an agent needs, and the reason it is the *same* loop rather
    /// than one beside it: the ordering rules in here were each paid for by a
    /// bug (wait on the network in real time before advancing the virtual
    /// clock; re-ask after the last `run_queued_jobs`; `max` then `min` rather
    /// than `clamp`). A second copy would start correct and drift.
    ///
    /// The rule borrowed from Lightpanda's `Runner`: **resolve when there is
    /// nothing left to wait on**, even when the condition never came true.
    /// Spinning to a timeout on a page that has gone quiet tells the caller
    /// nothing it did not already know, a whole budget later.
    pub fn settle_until(&mut self, ready: &mut dyn FnMut() -> bool) -> Waited {
        let mut ready = Ready::Rust(ready);
        self.settle_with(&mut ready)
    }

    /// The same wait, with the condition written in the page's own language.
    ///
    /// The expression is evaluated in the realm between rounds. A throw counts
    /// as *not yet*, not as an error: a condition that reads through a value
    /// the page has not built yet throws on the way there, and treating that as
    /// failure would make every useful condition unwritable.
    pub fn settle_until_expr(&mut self, expr: &str) -> Waited {
        let mut ready = Ready::Expr(expr.to_string());
        self.settle_with(&mut ready)
    }

    fn settle_with(&mut self, ready: &mut Ready<'_>) -> Waited {
        let budget = self.job_budget;
        // `with_job_deadline` returns (closure result, deadline fired), and the
        // closure itself returns (settled, met). Destructured in one pattern so
        // the two bools cannot be read in the wrong order — which is exactly
        // what a two-step destructure did on the first attempt, reporting every
        // met condition as a blown budget.
        let ((settled, met), cut_short) =
            self.with_job_deadline(budget, |script| script.settle_inner(Some(ready)));
        let end = if met {
            WaitEnd::Met
        } else if cut_short || settled.cut_off {
            WaitEnd::Budget
        } else if settled.periodic_timers > 0 {
            // The page paid off everything it owed and is still running a loop
            // that re-arms itself. Reporting this as `Quiescent` would tell the
            // caller nothing can change, which is false; reporting it as
            // `Budget` would tell them to wait again, which is worse, because
            // the loop will still be there next time.
            WaitEnd::Periodic
        } else {
            WaitEnd::Quiescent
        };
        Waited {
            met,
            settled,
            end,
            changed: false,
        }
    }

    /// Ask the predicate, whichever kind it is.
    ///
    /// A page-side condition is wrapped so a throw is `false`. The page is
    /// mid-build for most of a wait, so a condition that dereferences something
    /// not there yet is the normal case rather than a mistake.
    fn ask(&mut self, ready: &mut Ready<'_>) -> bool {
        match ready {
            Ready::Rust(f) => f(),
            Ready::Expr(expr) => {
                let wrapped = format!(
                    "(() => {{ try {{ return !!({expr}); }} catch (e) {{ return false; }} }})()"
                );
                self.eval_value(&wrapped).map(|v| v == "true").unwrap_or(false)
            }
        }
    }

    fn settle_inner(&mut self, mut ready: Option<&mut Ready<'_>>) -> (Settled, bool) {
        let mut clock = 0u64;
        let mut timers_run = 0usize;
        let network_started = std::time::Instant::now();

        // Asked before anything runs: a condition already true must not cost a
        // settle, and `wait_for` on a page that already shows the thing is the
        // common case in an agent loop.
        macro_rules! ready_now {
            () => {
                match ready.as_deref_mut() {
                    Some(r) => self.ask(r),
                    None => false,
                }
            };
        }
        if ready_now!() {
            return (
                Settled {
                    elapsed_ms: 0,
                    timers_run: 0,
                    cut_off: false,
                    pending_timers: self.pending_timers(),
                    periodic_timers: self.periodic_timers(),
                },
                true,
            );
        }

        loop {
            self.run_queued_jobs();

            // Layout observers are driven from here rather than from a frame
            // clock, because this engine has no frames at rest: an observer
            // that waited for a repaint would never fire at all.
            self.run_layout_observers();

            // Requests that have come back resolve their promises here, which
            // is what lets `fetch` be concurrent: the host starts up to six at
            // once and this is where the page learns any of them finished.
            let outstanding = self.drain_fetches();

            let ran = self.run_due_timers(clock);
            timers_run += ran;

            // Frames that arrived since the last round become events here.
            //
            // Counted as work, so a message that landed gets the page another
            // round to react to it — and a socket that is merely *open* does
            // not, which is what keeps a page holding one from being reported
            // as permanently busy. The interval precedent applies: a perpetual
            // thing that counts as pending makes every page that has one look
            // like it never finished.
            let delivered = self.drain_sockets();

            // After the round's work, before deciding whether to wait longer.
            if ready_now!() {
                return (
                    Settled {
                        elapsed_ms: clock,
                        timers_run,
                        cut_off: false,
                        pending_timers: self.pending_timers(),
                        periodic_timers: self.periodic_timers(),
                    },
                    true,
                );
            }

            if ran == 0 && delivered == 0 {
                // A page waiting on the network is not idle, and this is checked
                // before the clock moves rather than only when no timer is
                // armed. Advancing virtual time while a request is in the air
                // fires the very timeouts pages arm *against* the network —
                // testharness arms one on every file, so every test that
                // fetched timed itself out the instant its request was sent.
                // Wait in *real* time, since a round trip does not care about
                // our virtual clock.
                if outstanding > 0 {
                    if network_started.elapsed().as_millis() as u64 >= NETWORK_BUDGET_MS {
                        self.abandon_fetches();
                        self.run_queued_jobs();
                        self.collect_module_failures();
                        return (
                            Settled {
                                elapsed_ms: clock,
                                timers_run,
                                cut_off: true,
                                pending_timers: self.pending_timers(),
                                periodic_timers: self.periodic_timers(),
                            },
                            false,
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(NETWORK_POLL_MS));
                    continue;
                }

                // A *wait* may be waiting on a socket, which is the one thing
                // in this engine that arrives on real time rather than virtual.
                //
                // A plain settle must still terminate here — an open socket is
                // not pending work, and treating it as such would make every
                // page holding one report as permanently busy. But a wait on
                // such a page should give the wire its chance, or `wait_for`
                // could never see a message at all. So the real-time poll is
                // conditional on there being a predicate to satisfy.
                if ready.is_some() && self.open_sockets() > 0 && !ready_now!() {
                    if network_started.elapsed().as_millis() as u64 >= NETWORK_BUDGET_MS {
                        self.collect_module_failures();
                        return (
                            Settled {
                                elapsed_ms: clock,
                                timers_run,
                                cut_off: true,
                                pending_timers: self.pending_timers(),
                                periodic_timers: self.periodic_timers(),
                            },
                            false,
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(NETWORK_POLL_MS));
                    continue;
                }

                if self.pending_timers() == 0 {

                    // A reply that arrived in *this* pass resolved a promise,
                    // and the continuation it queued has not run yet. Returning
                    // here would report the page settled with its own `.then`
                    // still pending — which is exactly what a synchronous fetch
                    // used to hide, because it resolved during `eval` and the
                    // loop's first `run_jobs` picked the callback up.
                    self.run_queued_jobs();
                    if self.pending_timers() > 0 || self.drain_fetches() > 0 {
                        continue;
                    }

                    // Nothing the page still owes. For a plain settle that is
                    // the page being done; for a wait it is the answer, because
                    // the condition cannot become true without something
                    // running.
                    //
                    // "Nothing owed" is not the same as "nothing armed": an
                    // interval, or a one-shot chain past the nesting limit, is
                    // still going to fire. It is counted rather than waited on,
                    // and the count is what lets the caller be told which of
                    // the two answers it got.
                    self.collect_module_failures();
                    return (
                        Settled {
                            elapsed_ms: clock,
                            timers_run,
                            cut_off: false,
                            pending_timers: 0,
                            periodic_timers: self.periodic_timers(),
                        },
                        ready_now!(),
                    );
                }
                // Something is queued: jump the virtual clock to when it is
                // actually due rather than stepping toward it.
                //
                // Stepping 16ms at a time was not just slow, it was a wall. A
                // test harness that arms a ten-second timeout puts its timer at
                // exactly the settle budget, and the budget check below fired
                // before `run_due_timers` ever saw a clock that large — so the
                // timer never ran, the harness never timed itself out, and the
                // page reported *nothing at all*. That was the single largest
                // silent-failure bucket in WPT (§12.4).
                let next = self.next_timer_due().unwrap_or(clock + TICK_MS);
                // `max` then `min`, not `clamp`: `clamp` panics when its lower
                // bound exceeds its upper one, and here it can. A timer due
                // within one tick of the budget leaves `clock + TICK_MS` past
                // `SETTLE_BUDGET_MS`, and the engine aborted — taking the page,
                // the snapshot and the receipts with it, which is the failure
                // `insert_before` was hardened against three commits ago. Found
                // by review, reproduced with a 9,999ms timer and a 20,000ms one.
                clock = next.max(clock + TICK_MS).min(SETTLE_BUDGET_MS);
            }

            if clock >= SETTLE_BUDGET_MS {
                // One last pass at this clock before giving up, so a timer due
                // exactly at the budget is run rather than stranded one tick
                // short of its own deadline.
                timers_run += self.run_due_timers(clock);
                self.abandon_fetches();
                self.run_queued_jobs();
                self.collect_module_failures();
                return (
                    Settled {
                        elapsed_ms: clock,
                        timers_run,
                        cut_off: true,
                        pending_timers: self.pending_timers(),
                        periodic_timers: self.periodic_timers(),
                    },
                    ready_now!(),
                );
            }
        }
    }

    /// Shorten the job-queue deadline. For tests, and for a caller that knows
    /// it cannot wait the default out.
    pub fn set_job_budget(&mut self, budget: Duration) {
        self.job_budget = budget;
    }

    /// Run `body` with a wall-clock deadline on the job queue.
    ///
    /// A thread rather than a check in the loop, because the loop is exactly
    /// what is stuck: by the time the budget matters this thread is blocked
    /// inside `run_jobs`, and only something outside it can say stop.
    ///
    /// Returns whether the deadline fired, so the page can be told it was cut
    /// off rather than left to look merely thin.
    fn with_job_deadline<T>(&mut self, budget: Duration, body: impl FnOnce(&mut Self) -> T) -> (T, bool) {
        let cancel = self.cancel.clone();
        // A condition variable rather than a polled flag, because this thread is
        // *joined*: whatever it is still doing when the body finishes, the page
        // waits for.
        //
        // It used to sleep 20 ms between checks, so a page that settled in 50 µs
        // — which is most pages, most of the time — then sat in `join` until the
        // watchdog next woke up. **A 20 ms tax on every settle**, and on every
        // agent `wait` besides, spent asleep. It read as script time in the
        // profile and was not: the settle loop ran one round costing 50 µs
        // inside a phase that measured 20.4 ms. It was intermittent, too, which
        // is the worst way for a cost to present — a plain race between the body
        // and the watchdog's first check, so half the runs looked fine.
        let done = std::sync::Arc::new((
            std::sync::Mutex::new(false),
            std::sync::Condvar::new(),
        ));
        let watching = done.clone();
        let deadline = std::time::Instant::now() + budget;

        let watchdog = std::thread::Builder::new()
            .name("h5i-script-deadline".to_string())
            .spawn(move || {
                let (lock, wake) = &*watching;
                let mut finished = lock.lock().unwrap_or_else(|e| e.into_inner());
                while !*finished {
                    let Some(left) = deadline.checked_duration_since(std::time::Instant::now())
                    else {
                        break;
                    };
                    let (guard, _) = wake
                        .wait_timeout(finished, left)
                        .unwrap_or_else(|e| e.into_inner());
                    finished = guard;
                }
                if *finished {
                    return false;
                }
                cancel.store(true, portable_atomic::Ordering::Relaxed);
                true
            })
            .ok();

        let out = body(self);
        {
            let (lock, wake) = &*done;
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
            // Under the lock's release, not before it: a watchdog notified while
            // the flag was still false would go back to waiting out the budget,
            // and the join below would wait with it.
            wake.notify_all();
        }
        let fired = watchdog.and_then(|w| w.join().ok()).unwrap_or(false);

        // Boa clears the flag itself when it acts on it, but a deadline that
        // fired after the last job would leave it set for the next call.
        self.cancel
            .store(false, portable_atomic::Ordering::Relaxed);
        (out, fired)
    }

    /// Run the microtask queue, reporting anything that escaped it.
    ///
    /// Boa 0.21 returns a result here where 0.19 returned nothing. An error
    /// reaching this point is one no `.catch` handled — an unhandled rejection,
    /// or a job that threw — and it was previously invisible in both engines:
    /// 0.19 could not tell us, and swallowing it now would keep a page looking
    /// like it worked when part of it did not.
    fn run_queued_jobs(&mut self) {
        if let Err(error) = self.context.run_jobs() {
            self.note_error(&format!("unhandled in a queued job: {error}"));
        }
    }

    /// Resolve whatever has come back, and report how much is still owed.
    fn drain_fetches(&mut self) -> usize {
        match self
            .context
            .eval(Source::from_bytes("__h5iDrainFetches()"))
        {
            Ok(value) => value.as_number().unwrap_or(0.0).max(0.0) as usize,
            Err(_) => 0,
        }
    }

    /// Reject what will never be answered.
    ///
    /// A promise left pending forever is a page that looks like it is still
    /// working when nothing is, and an agent reading that has no way to tell
    /// the difference from a page that is merely slow.
    fn abandon_fetches(&mut self) {
        let _ = self.context.eval(Source::from_bytes(
            "__h5iAbandonFetches('the page stopped waiting for this request: it had not \
             answered when the settle budget ran out')",
        ));
    }

    fn run_layout_observers(&mut self) {
        let _ = self
            .context
            .eval(Source::from_bytes("__h5iRunLayoutObservers()"));
    }

    fn run_due_timers(&mut self, clock: u64) -> usize {
        let source = format!("__h5iRunTimers({clock})");
        match self.context.eval(Source::from_bytes(&source)) {
            Ok(value) => value.as_number().unwrap_or(0.0).max(0.0) as usize,
            Err(_) => 0,
        }
    }

    /// When the earliest waiting timer is due, in virtual milliseconds.
    fn next_timer_due(&mut self) -> Option<u64> {
        match self
            .context
            .eval(Source::from_bytes("__h5iNextTimerDue()"))
        {
            Ok(value) => match value.as_number() {
                Some(due) if due >= 0.0 => Some(due as u64),
                _ => None,
            },
            Err(_) => None,
        }
    }

    /// Turn arrived socket frames into page events, and say how many.
    ///
    /// The Rust-side check first is not premature. This runs on **every settle
    /// round of every page**, and almost no page opens a socket — so without it
    /// the whole corpus pays an `eval` per round for a feature it never uses.
    /// Measured: the library suite went 8.3s to 16.1s with the eval
    /// unconditional, and back with this guard.
    fn drain_sockets(&mut self) -> usize {
        if self.host.sockets.borrow().is_empty() && self.host.streams.borrow().is_empty() {
            return 0;
        }
        match self.context.eval(Source::from_bytes("__h5iDrainSockets()")) {
            Ok(value) => value.as_number().unwrap_or(0.0).max(0.0) as usize,
            Err(_) => 0,
        }
    }

    /// How many sockets this page holds open.
    ///
    /// Reported in the snapshot, because a page with a live socket is a page
    /// whose content can change between two reads without the agent having done
    /// anything — which is the one thing that makes a session here
    /// non-deterministic, and it should not be silent.
    pub fn open_sockets(&mut self) -> usize {
        // Answered from the Rust side, which is where the sockets actually
        // live; the prelude's map is a mirror for the page's benefit.
        self.host.sockets.borrow().len() + self.host.streams.borrow().len()
    }

    /// The same count, asked of the prelude's own map.
    ///
    /// Exists so a test can assert the Rust side and the page side agree: they
    /// are two records of one thing, and a page whose `WebSocket` map has
    /// drifted from the engine's would misreport `readyState` forever.
    #[cfg(test)]
    pub(crate) fn open_sockets_via_prelude(&mut self) -> usize {
        match self.context.eval(Source::from_bytes("__h5iOpenSockets()")) {
            Ok(value) => value.as_number().unwrap_or(0.0).max(0.0) as usize,
            Err(_) => 0,
        }
    }

    /// The host this realm is bound to.
    ///
    /// Exposed for the one consumer outside the realm: the canvas surfaces live
    /// on the host (they are not part of the DOM), and the engine has to reach
    /// them to composite them into the page.
    pub fn host(&self) -> Rc<Host> {
        self.host.clone()
    }

    fn pending_timers(&mut self) -> usize {
        match self
            .context
            .eval(Source::from_bytes("__h5iPendingTimers()"))
        {
            Ok(value) => value.as_number().unwrap_or(0.0).max(0.0) as usize,
            Err(_) => 0,
        }
    }

    /// Timers armed but no longer holding the page open.
    ///
    /// Asked only where a settle is about to return, so the common path pays
    /// nothing for it: a page with no periodic work answers zero and the
    /// reporting is unchanged.
    fn periodic_timers(&mut self) -> usize {
        match self
            .context
            .eval(Source::from_bytes("__h5iPeriodicTimers()"))
        {
            Ok(value) => value.as_number().unwrap_or(0.0).max(0.0) as usize,
            Err(_) => 0,
        }
    }

    /// Tell the realm what the document is written in.
    ///
    /// A setter rather than a constructor argument because a realm is built
    /// from a tree and a base URL, and the encoding belongs to the *response*
    /// those came from — several callers have a tree without ever having had
    /// bytes.
    pub fn set_encoding(&mut self, encoding: &'static encoding_rs::Encoding) {
        *self.host.encoding.borrow_mut() = encoding;
    }

    /// Install the page's `<script type="importmap">`, before anything imports.
    ///
    /// Set once, from the parsed tree, for the reason the specification gives:
    /// a map that arrived after the first import would change what that import
    /// had already meant. The engine reads it out of the document rather than
    /// letting script register one, so nothing a page does at runtime can move
    /// a module graph that is already resolving.
    ///
    /// A map that could not be parsed is reported on the console as an engine
    /// line and then ignored whole, because half a map resolves half a page's
    /// imports and leaves the rest failing for a reason nobody can see.
    pub fn set_import_map(&mut self, source: &str) {
        match crate::script::import_map::ImportMap::parse(source, &self.host.base) {
            Ok(map) => {
                *self.host.import_map.borrow_mut() = Some(map);
            }
            Err(reason) => self.note_error(&format!("import map ignored: {reason}")),
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

    /// Dispatch a key event carrying the key that was pressed.
    ///
    /// Separate from [`Self::dispatch`] because a `KeyboardEvent` with no
    /// `key` is the shape a handler most often branches on: `if (e.key ===
    /// "Enter")` is the commonest line in any form's script, and an event that
    /// answers `undefined` there takes the wrong branch silently.
    pub fn dispatch_key(
        &mut self,
        node_id: usize,
        event_type: &str,
        key: &str,
    ) -> Result<(), String> {
        // Through `serde_json` rather than by hand: the key is a string from
        // the agent, and a quote in it would otherwise end the literal and
        // leave the rest as code.
        let key = serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string());
        let source = format!(
            "(() => {{ const target = __h5iWrapById({node_id}); \
             if (target) target.dispatchEvent(new KeyboardEvent({event_type:?}, \
               {{ bubbles: true, cancelable: true, key: {key}, code: {key} }})); }})()"
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
        crate::script::host::push_console(
                &mut self.host.console.borrow_mut(),
                ConsoleLine::engine("error", text.to_string()),
            );
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

        // A global the page expected because a script we refused would have
        // defined it is not a binding this engine lacks. The corpus reported
        // `$` twice and it was jQuery from a denied CDN — listing that beside
        // real gaps invites building something nobody asked for. Say what
        // actually happened instead.
        let (first, count) = {
            let refused = self.host.refused_scripts.borrow();
            (refused.first().cloned(), refused.len())
        };
        if let Some(first) = first {
            let and_others = if count > 1 {
                format!(" (and {} other script{})", count - 1, if count > 2 { "s" } else { "" })
            } else {
                String::new()
            };
            crate::script::host::push_console(
                &mut self.host.console.borrow_mut(),
                ConsoleLine::engine(
                    "error",
                    format!(
                        "`{}` is missing because a script this page needed did not run: \
                         {first}{and_others}. This engine did not refuse the API, it refused \
                         the request.",
                        name.trim()
                    ),
                ),
            );
            return;
        }
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
    /// Remember a script that did not finish: refused by policy, or loaded and
    /// then threw.
    ///
    /// Either way its globals are undefined, so a later `ReferenceError` is
    /// explained by that rather than counted as a binding this engine lacks.
    pub fn note_refused_script(&self, url: &str) {
        self.host.refused_scripts.borrow_mut().push(url.to_string());
    }

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
    pub fn take_requests(&self) -> Vec<crate::script::host::RequestLink> {
        std::mem::take(&mut *self.host.requests.borrow_mut())
    }
}

#[cfg(test)]
mod tests;
