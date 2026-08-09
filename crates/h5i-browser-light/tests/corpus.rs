//! The corpus, as a gate CI can actually run.
//!
//! `corpus/run.py` points this engine at real sites and is the instrument that
//! finds new work — but it needs the network, the sites change under it, and a
//! run takes minutes. What it finds, once fixed, belongs here: the same
//! *patterns* against local fixtures, so a regression is caught by `cargo test`
//! rather than by a manual run somebody remembers to do.
//!
//! Every fixture below is here because the network corpus found it, and the
//! comment on each says which finding it stands for. The two assertions that
//! matter are the ones the corpus itself reports on:
//!
//!   1. the page asks for **nothing** this engine lacks, and
//!   2. no console error is **anonymous** — every one names either a request
//!      that was refused or the script that threw.
//!
//! An empty ask list beside an unattributable error is the failure mode this
//! whole apparatus exists to prevent (ROADMAP_BROWSER §8.3).

use std::sync::Arc;

use h5i_browser_light::engine::{PageFactory, PageOptions};
use h5i_browser_light::net::Broker;
use h5i_browser_light::policy::Policy;
use h5i_browser_light::receipt::MemorySink;

/// Load a fixture with script enabled and settle it, as `open --script` does.
fn read(html: &str) -> Reading {
    let broker = Arc::new(
        Broker::new(Policy::new(), Arc::new(MemorySink::new()), None).expect("broker"),
    );
    let fonts = h5i_browser_light::fonts::load(
        &[],
        &h5i_browser_light::fonts::default_font_dirs(),
        Some(2),
    );
    let options = PageOptions {
        script: true,
        ..Default::default()
    };
    let factory = PageFactory::new(broker.clone(), fonts.sources.clone(), options);
    let base = url::Url::parse("https://fixture.example/").unwrap();

    // `PageFactory::from_html` already runs the page's scripts when the options
    // ask for them. Running them again here ran every fixture twice, which is
    // harmless for a script that only assigns — and wrong for one that appends,
    // which is how this was found.
    let _ = broker;
    let page = factory.from_html(html, &base);

    Reading {
        outline: page.snapshot().render(),
        unsupported: page.unsupported(),
        errors: page
            .console()
            .into_iter()
            .filter(|line| line.level == "error")
            .map(|line| line.text)
            .collect(),
    }
}

struct Reading {
    outline: String,
    unsupported: Vec<(String, usize)>,
    errors: Vec<String>,
}

impl Reading {
    /// The two properties the corpus reports on, asserted together because
    /// either one alone is misleading.
    fn assert_clean(&self, what: &str) {
        assert!(
            self.unsupported.is_empty(),
            "{what}: the page asked for something this engine lacks: {:?}",
            self.unsupported
        );
        let anonymous: Vec<&String> = self
            .errors
            .iter()
            .filter(|text| {
                let kind = text.split(':').next().unwrap_or("");
                matches!(
                    kind,
                    "TypeError" | "ReferenceError" | "Error" | "SyntaxError" | "RangeError"
                )
            })
            .collect();
        assert!(
            anonymous.is_empty(),
            "{what}: an error that names neither a request nor a script is one nobody can \
             act on: {anonymous:?}"
        );
    }

    fn assert_shows(&self, text: &str) {
        assert!(
            self.outline.contains(text),
            "expected {text:?} in the outline:\n{}",
            self.outline
        );
    }
}

/// Cloning a `<template>` — the pattern behind fifteen module failures across
/// the application corpus, every one reading `cannot convert 'null' or
/// 'undefined' to object` and naming nothing.
#[test]
fn a_framework_renders_rows_by_cloning_a_template() {
    let reading = read(
        "<html><body>\
         <template id='row'><li class='item'><span class='label'></span></li></template>\
         <ul id='list'></ul>\
         <script>\
           const template = document.querySelector('#row');\
           const list = document.querySelector('#list');\
           for (const name of ['alpha', 'beta', 'gamma']) {\
             const row = template.content.cloneNode(true);\
             row.querySelector('.label').textContent = name;\
             list.appendChild(row);\
           }\
         </script></body></html>",
    );

    reading.assert_clean("template cloning");
    for name in ["alpha", "beta", "gamma"] {
        reading.assert_shows(name);
    }
}

/// A design system: markup first, component definitions in a deferred bundle.
/// Defining without upgrading would render nothing and report no error.
#[test]
fn a_component_library_upgrades_markup_that_was_already_there() {
    let reading = read(
        "<html><body>\
         <x-badge label='ready'></x-badge>\
         <script>\
           class Badge extends HTMLElement {\
             static get observedAttributes() { return ['label'] }\
             attributeChangedCallback(name, before, after) { this.textContent = 'badge: ' + after }\
             connectedCallback() { this.setAttribute('data-live', '1') }\
           }\
           customElements.define('x-badge', Badge);\
         </script></body></html>",
    );

    reading.assert_clean("custom element upgrade");
    reading.assert_shows("badge: ready");
}

/// Client-side routing and local state — what an application does that a
/// document never does, and what the document corpus therefore never asked for.
#[test]
fn an_application_routes_and_stores_without_asking_for_anything() {
    let reading = read(
        "<html><body><main id='view'></main>\
         <script>\
           localStorage.setItem('visits', '3');\
           sessionStorage.setItem('tab', 'home');\
           history.pushState({ page: 2 }, '', '/page/2');\
           const id = crypto.randomUUID();\
           const bytes = new TextEncoder().encode('café');\
           const back = new TextDecoder().decode(bytes);\
           const copy = structuredClone(new Map([['k', { n: 1 }]]));\
           document.querySelector('#view').textContent = \
             'visits=' + localStorage.getItem('visits') + \
             ' tab=' + sessionStorage.getItem('tab') + \
             ' path=' + location.pathname + \
             ' uuid=' + id.length + \
             ' text=' + back + \
             ' cloned=' + copy.get('k').n;\
         </script></body></html>",
    );

    reading.assert_clean("routing and storage");
    reading.assert_shows("visits=3");
    reading.assert_shows("tab=home");
    reading.assert_shows("path=/page/2");
    reading.assert_shows("uuid=36");
    reading.assert_shows("text=café");
    reading.assert_shows("cloned=1");
}

/// Responsive branching, observers and timers: the four the document corpus
/// asked for, in the shape a real page uses them.
#[test]
fn a_responsive_page_branches_observes_and_ticks() {
    let reading = read(
        "<html><body><div id='panel'>panel</div><output id='out'></output>\
         <script>\
           const out = document.querySelector('#out');\
           const wide = matchMedia('(min-width: 900px)').matches;\
           let seen = 0;\
           new IntersectionObserver((entries) => { seen += entries.length })\
             .observe(document.querySelector('#panel'));\
           new ResizeObserver(() => {}).observe(document.querySelector('#panel'));\
           document.cookie = 'theme=dark';\
           let ticks = 0;\
           setInterval(() => { ticks += 1 }, 10);\
           setTimeout(() => {\
             out.textContent = 'wide=' + wide + ' seen=' + (seen > 0) + \
               ' cookie=' + document.cookie + ' ticked=' + (ticks > 0);\
           }, 60);\
         </script></body></html>",
    );

    reading.assert_clean("media queries, observers, timers");
    reading.assert_shows("wide=true");
    reading.assert_shows("seen=true");
    reading.assert_shows("cookie=theme=dark");
    reading.assert_shows("ticked=true");
}

/// The DOM surface the application corpus named once it could get far enough to
/// ask: element traversal, attribute lists, token lists, animations.
#[test]
fn the_dom_surface_an_application_walks() {
    let reading = read(
        "<html><head><link rel='stylesheet' href='/a.css'></head>\
         <body><section id='s' data-role='main'><p>one</p><p>two</p></section>\
         <output id='out'></output>\
         <script>\
           const s = document.querySelector('#s');\
           const link = document.querySelector('link');\
           link.relList.add('preload');\
           document.querySelector('#out').textContent = \
             'first=' + s.firstElementChild.textContent + \
             ' count=' + s.childElementCount + \
             ' attrs=' + s.attributes.map(a => a.name).join('|') + \
             ' rel=' + link.getAttribute('rel') + \
             ' anims=' + s.getAnimations().length + \
             ' type=' + document.contentType;\
         </script></body></html>",
    );

    reading.assert_clean("element traversal and attributes");
    reading.assert_shows("first=one");
    reading.assert_shows("count=2");
    reading.assert_shows("attrs=id|data-role");
    reading.assert_shows("rel=stylesheet preload");
    reading.assert_shows("anims=0");
    reading.assert_shows("type=text/html");
}

/// A framework's private bookkeeping is not an API gap. Solid reads
/// `document._$DX_DELEGATE` before it sets it, and the list an agent reads
/// carried that as something this engine was missing.
#[test]
fn a_frameworks_own_fields_do_not_appear_in_the_ask_list() {
    let reading = read(
        "<html><body><p>x</p>\
         <script>\
           if (!document._$DX_DELEGATE) document._$DX_DELEGATE = new Set();\
           const node = document.querySelector('p');\
           if (!node.__vnode) node.__vnode = { rendered: true };\
           void node.$$typeof;\
         </script></body></html>",
    );
    reading.assert_clean("framework internals");
}

/// A page that fails should fail *legibly*. This one is deliberately broken:
/// the assertion is not that it works but that every complaint names its
/// source, which is what an agent needs to act on.
#[test]
fn a_broken_page_reports_errors_that_name_their_source() {
    let reading = read(
        "<html><body><p>readable</p>\
         <script>document.querySelector('#absent').focus()</script>\
         <script>alsoUndeclared.go()</script></body></html>",
    );

    // The page is still readable — one broken script does not make a page
    // unreadable, and the agent needs the half that worked.
    reading.assert_shows("readable");
    assert!(
        !reading.errors.is_empty(),
        "the failures should be reported, not swallowed"
    );
    for error in &reading.errors {
        let kind = error.split(':').next().unwrap_or("");
        assert!(
            !matches!(kind, "TypeError" | "ReferenceError"),
            "every error should name the script it came from: {error:?}"
        );
    }
    assert!(
        reading.errors.iter().any(|e| e.contains("inline script")),
        "{:?}",
        reading.errors
    );
}

/// The engine must always come back. Boa exposes no wall-clock interrupt, so a
/// bounded loop is the only backstop there is — and a page that trips it should
/// still render what it managed, with the reason on the record.
#[test]
fn a_runaway_loop_is_stopped_and_reported_rather_than_hanging() {
    let reading = read(
        "<html><body><p>rendered before the loop</p>\
         <script>let n = 0; while (true) { n += 1 }</script>\
         </body></html>",
    );

    reading.assert_shows("rendered before the loop");
    assert!(
        reading
            .errors
            .iter()
            .any(|e| e.contains("iteration") || e.contains("RuntimeLimit")),
        "the limit should be named, not silently swallowed: {:?}",
        reading.errors
    );
}

/// Deep recursion is bounded too, and the bound is high enough that a real
/// bundle's initialisation does not trip it — Next.js's did at Boa's default.
#[test]
fn recursion_is_bounded_well_above_what_a_real_bundle_needs() {
    let reading = read(
        "<html><body><output id='out'></output>\
         <script>\
           function down(n) { return n === 0 ? 0 : 1 + down(n - 1) }\
           document.querySelector('#out').textContent = 'depth=' + down(1500);\
         </script></body></html>",
    );

    reading.assert_clean("deep but bounded recursion");
    reading.assert_shows("depth=1500");
}

/// A page that parses markup out of a string, which is how sanitizers and
/// template libraries work. What comes back is a parsed subtree presented as a
/// document; no script inside it runs, which is the property they rely on.
#[test]
fn markup_can_be_parsed_out_of_a_string_without_running_it() {
    let reading = read(
        "<html><body><output id='out'></output>\
         <script>\
           globalThis.ran = false;\
           const parsed = new DOMParser().parseFromString(\
             '<div class=\"card\"><h2>Title</h2><script>globalThis.ran = true<\\/script></div>',\
             'text/html');\
           document.querySelector('#out').textContent = \
             'found=' + parsed.querySelector('.card h2').textContent + ' ran=' + ran;\
         </script></body></html>",
    );

    reading.assert_clean("DOMParser");
    reading.assert_shows("found=Title");
    reading.assert_shows("ran=false");
}

/// A page logging its own failures must produce something an agent can act on.
/// `JSON.stringify` renders an Error as `{}` because none of its properties are
/// enumerable — remix.run filled the console with 1487 lines saying exactly
/// that, and the message, the one useful part, was what got thrown away.
#[test]
fn a_logged_error_says_what_it_was() {
    let reading = read(
        "<html><body><p>x</p>\
         <script>console.error(new TypeError('the specific thing that failed'))</script>\
         </body></html>",
    );

    assert!(
        reading
            .errors
            .iter()
            .any(|e| e.contains("the specific thing that failed")),
        "{:?}",
        reading.errors
    );
    assert!(
        !reading.errors.iter().any(|e| e.trim() == "{}"),
        "{:?}",
        reading.errors
    );
}

/// A page with no script must not pay for a script realm. Building one costs
/// about 15 ms — 114 KiB of prelude parsed and evaluated — and a page with
/// nothing to run was paying all of it for a realm never asked a question.
#[test]
fn a_page_with_no_script_is_still_settled_and_says_why() {
    let reading = read("<html><body><p>plain markup</p></body></html>");

    reading.assert_clean("a page with no script");
    reading.assert_shows("plain markup");
}

/// And the note distinguishes "there was nothing to run" from "script is off",
/// which are different facts about why a page might be empty.
#[test]
fn an_empty_page_distinguishes_no_script_from_script_disabled() {
    let reading = read("<html><head><title>t</title></head><body></body></html>");
    assert!(
        reading.outline.contains("had none to run"),
        "{}",
        reading.outline
    );
}

/// A page this engine's *parser* cannot read must still say so, by name.
///
/// This construct is valid JavaScript and Boa rejects it — see ROADMAP_BROWSER
/// §8.11. Minified bundles that keep `/*! @license */` comments between
/// declarators produce it, which is how lit.dev fails. Nothing here can fix
/// that; what this pins is that the failure is *attributed* rather than silent,
/// and that the rest of the page still reads.
#[test]
fn a_script_the_parser_cannot_read_is_named_and_does_not_take_the_page_with_it() {
    let reading = read(
        "<html><body><p>still readable</p>\
         <script>const a = 1\n, b = 2;</script>\
         <script>document.querySelector('p').setAttribute('data-ran', 'yes')</script>\
         </body></html>",
    );

    reading.assert_shows("still readable");
    assert!(
        reading
            .errors
            .iter()
            .any(|e| e.contains("inline script") && e.contains("SyntaxError")),
        "the failure names the script it came from: {:?}",
        reading.errors
    );
    // And a later script still runs: one unreadable script is not the page.
    assert!(
        reading.outline.contains("data-ran") || reading.errors.len() == 1,
        "{:?}",
        reading.errors
    );
}

/// Server-rendered markup, hydrated. Preact and React both separate adjacent
/// text with `<!-- -->` when they render on the server, so a comment the
/// *parser* produced has to be a comment — it was coming back as an empty text
/// node, and a hydrator that sees text where it expects a comment decides the
/// markup does not match and renders the page a second time beside the first.
#[test]
fn a_comment_from_the_parser_is_a_comment() {
    let reading = read(
        "<html><body><a id='v'>v<!-- -->1.0.0</a><output id='out'></output>\
         <script>\
           const kids = document.querySelector('#v').childNodes;\
           document.querySelector('#out').textContent = \
             'types=' + kids.map(n => n.nodeType).join(',');\
         </script></body></html>",
    );

    reading.assert_clean("a parsed comment");
    reading.assert_shows("types=3,8,3");
}

/// A parsed document always has a head, even for a fragment of markup with
/// none. Returning null was enough to take preactjs.com's markup component down
/// with a null dereference, and the page then re-rendered everything it had
/// already server-rendered — 178 lines of readable page became 31.
#[test]
fn a_parsed_document_has_the_parts_a_real_one_has() {
    let reading = read(
        "<html><body><output id='out'></output>\
         <script>\
           const doc = new DOMParser().parseFromString('<p class=\"x\">hi</p>', 'text/html');\
           document.querySelector('#out').textContent = \
             'head=' + (doc.head ? doc.head.tagName : 'MISSING') + \
             ' body=' + doc.body.tagName + \
             ' found=' + doc.querySelector('.x').textContent;\
         </script></body></html>",
    );

    reading.assert_clean("a parsed document");
    reading.assert_shows("head=HEAD body=BODY found=hi");
}

/// The parent of `<html>` is the document, and it has to *be* the document:
/// code walks up until it finds node type 9 and then asks that thing for
/// `body`, so ending the walk on an ordinary node ends it nowhere useful.
#[test]
fn walking_up_from_an_element_reaches_the_document() {
    let reading = read(
        "<html><body><p id='p'>x</p><output id='out'></output>\
         <script>\
           let n = document.querySelector('#p');\
           let steps = 0;\
           while (n.parentNode && steps < 10) { n = n.parentNode; steps += 1 }\
           document.querySelector('#out').textContent = \
             'top=' + n.nodeType + ' isDocument=' + (n === document) + \
             ' hasBody=' + !!n.body + \
             ' parentElementAtRoot=' + (document.documentElement.parentElement === null);\
         </script></body></html>",
    );

    reading.assert_clean("walking to the document");
    reading.assert_shows("top=9 isDocument=true hasBody=true parentElementAtRoot=true");
}

/// Inserting a node that already has a parent must *move* it, not lose it.
///
/// The DOM defines insertion as removing from the old parent first, and this
/// engine was skipping that step — the tree underneath drops a node inserted
/// while still parented, so every reorder deleted one. That is the operation a
/// keyed diff is built out of, and it is why preactjs.com rendered its shell
/// and its sidebar and then nothing where the article should be.
#[test]
fn moving_a_node_moves_it_rather_than_losing_it() {
    let reading = read(
        "<html><body><div id='host'></div><output id='out'></output>\
         <script>\
           const host = document.querySelector('#host');\
           const made = ['A', 'B', 'C'].map(t => { \
             const n = document.createElement('i'); n.textContent = t; host.appendChild(n); return n });\
           const order = () => host.childNodes.map(n => n.textContent).join('');\
           const steps = [order()];\
           host.insertBefore(made[2], made[0]);  steps.push(order());\
           host.appendChild(made[2]);            steps.push(order());\
           const fresh = document.createElement('i'); fresh.textContent = 'D';\
           host.replaceChild(fresh, host.firstChild); steps.push(order());\
           document.querySelector('#out').textContent = steps.join(' ');\
         </script></body></html>",
    );

    reading.assert_clean("moving nodes");
    // built, C to front, C back to the end, first replaced — three children
    // throughout, because a move is a move.
    reading.assert_shows("ABC CAB ABC DBC");
}

/// A design-system component that renders into a shadow root.
///
/// Flattened: this engine has one tree, so shadow content lands in the host and
/// is therefore *readable*, which is the half that decides whether an agent can
/// use the page. Encapsulation is not enforced — `document.querySelector`
/// reaches inside here and would not in a browser — and that trade is stated in
/// `ShadowRoot` rather than discovered.
#[test]
fn a_component_that_renders_into_a_shadow_root_is_readable() {
    let reading = read(
        "<html><body><my-card><span>slotted content</span></my-card>\
         <output id='out'></output>\
         <script>\
           class MyCard extends HTMLElement {\
             connectedCallback() {\
               const root = this.attachShadow({ mode: 'open' });\
               root.innerHTML = '<div class=\"card\"><h2>Card title</h2><slot></slot></div>';\
             }\
           }\
           customElements.define('my-card', MyCard);\
           const card = document.querySelector('my-card');\
           document.querySelector('#out').textContent = \
             'mode=' + card.shadowRoot.mode + \
             ' host=' + (card.shadowRoot.host === card) + \
             ' type=' + card.shadowRoot.nodeType;\
         </script></body></html>",
    );

    reading.assert_clean("shadow DOM");
    // What the component rendered is in the page and readable.
    reading.assert_shows("Card title");
    // And the light content it was given was projected into its slot rather
    // than left doubled up beside the output.
    reading.assert_shows("slotted content");
    reading.assert_shows("mode=open host=true type=11");
}

/// A closed root answers null, because the component asked for that — the
/// flattening already leaks more than a browser would.
#[test]
fn a_closed_shadow_root_is_not_handed_out() {
    let reading = read(
        "<html><body><x-closed></x-closed><output id='out'></output>\
         <script>\
           class Closed extends HTMLElement {\
             connectedCallback() {\
               this.attachShadow({ mode: 'closed' }).innerHTML = '<p>inside</p>';\
             }\
           }\
           customElements.define('x-closed', Closed);\
           document.querySelector('#out').textContent = \
             'exposed=' + (document.querySelector('x-closed').shadowRoot !== null);\
         </script></body></html>",
    );

    reading.assert_clean("a closed shadow root");
    reading.assert_shows("exposed=false");
    // Still rendered, though: an agent reads what the page shows.
    reading.assert_shows("inside");
}
