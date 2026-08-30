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
//! whole apparatus exists to prevent (roadmap-history.md §B8.3).

use std::sync::Arc;

use h5i_browser_light::engine::{PageFactory, PageOptions};
use h5i_browser_light::net::LocalBroker;
use h5i_browser_light::policy::Policy;
use h5i_browser_light::receipt::MemorySink;

/// Load a fixture with script enabled and settle it, as `open --script` does.
fn read(html: &str) -> Reading {
    let broker = LocalBroker::new(Policy::new(), Arc::new(MemorySink::new()), None)
        .expect("broker");
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
/// This construct is valid JavaScript and Boa rejects it — see ROADMAP.md
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

/// Text nodes must be writable. Every reactive UI updates text by assigning
/// `.data` or `.nodeValue` on the node it already holds — it is the single most
/// common mutation there is — and this engine was silently dropping all of it,
/// because writing to a text node took the path meant for elements: clear the
/// children (a text node has none) and append a new text child (meaningless).
#[test]
fn a_text_node_can_be_written_to() {
    let reading = read(
        "<html><body><p id='p'>original</p><output id='out'></output>\
         <script>\
           const t = document.querySelector('#p').firstChild;\
           const seen = [];\
           t.data = 'via data'; seen.push(t.data);\
           t.nodeValue = 'via nodeValue'; seen.push(t.data);\
           t.textContent = 'via textContent'; seen.push(t.parentNode.textContent);\
           const made = document.createTextNode('fresh'); made.data = 'edited';\
           seen.push(made.data);\
           document.querySelector('#out').textContent = seen.join(' | ');\
         </script></body></html>",
    );

    reading.assert_clean("writing to a text node");
    reading.assert_shows("via data | via nodeValue | via textContent | edited");
}

/// And writing to an *element* still replaces its subtree, which is the other
/// half of the same binding and what makes list rendering work.
#[test]
fn writing_to_an_element_still_replaces_its_subtree() {
    let reading = read(
        "<html><body><div id='d'><span>old</span><b>more</b></div>\
         <script>document.querySelector('#d').textContent = 'replaced';</script>\
         </body></html>",
    );

    reading.assert_clean("writing to an element");
    reading.assert_shows("replaced");
    assert!(
        !reading.outline.contains("old") && !reading.outline.contains("more"),
        "the old subtree is gone: {}",
        reading.outline
    );
}

/// Reading markup back out must not lose what was in it. A sanitizer or a
/// template library reads `innerHTML` and re-parses it, and preact and React
/// separate adjacent text with `<!-- -->` when rendering on the server — so
/// dropping comments quietly removed the markers hydration depends on.
#[test]
fn serialising_markup_keeps_comments_and_escapes_what_it_must() {
    let reading = read(
        "<html><body>\
         <div id='a'><a href='/r'>v<!-- -->1.0.0</a></div>\
         <div id='b'><img src='/x.png' alt='a &amp; b'><br></div>\
         <div id='c'><p>5 &lt; 6 &amp;&amp; 7</p></div>\
         <output id='out'></output>\
         <script>\
           const round = document.createElement('div');\
           round.innerHTML = document.querySelector('#a').innerHTML;\
           document.querySelector('#out').textContent = \
             'comment=' + document.querySelector('#a').innerHTML + \
             ' void=' + document.querySelector('#b').innerHTML + \
             ' text=' + document.querySelector('#c').innerHTML + \
             ' roundtrip=' + round.firstChild.childNodes.map(n => n.nodeType).join(',');\
         </script></body></html>",
    );

    reading.assert_clean("serialising markup");
    // The separator survives...
    reading.assert_shows("comment=<a href=\"/r\">v<!---->1.0.0</a>");
    // ...void elements do not grow a closing tag...
    reading.assert_shows("void=<img src=\"/x.png\" alt=\"a &amp; b\"><br>");
    // ...text that would reopen markup is escaped...
    reading.assert_shows("text=<p>5 &lt; 6 &amp;&amp; 7</p>");
    // ...and re-parsing the result gives back text, comment, text.
    reading.assert_shows("roundtrip=3,8,3");
}

/// The legacy surface every browser implements. Annex B is the standard's own
/// name for it, and leaving boa's feature off made this engine stricter than
/// any browser: excalidraw's colour parser calls `substr` and died on "not a
/// callable function".
#[test]
fn the_legacy_string_surface_a_browser_has_is_present() {
    let reading = read(
        "<html><body><output id='out'></output>\
         <script>\
           const s = '#AABBCC';\
           document.querySelector('#out').textContent = \
             'substr=' + s.substr(-2) + ' bold=' + ('x'.bold ? 'yes' : 'no');\
         </script></body></html>",
    );
    reading.assert_clean("annex-b string methods");
    reading.assert_shows("substr=CC bold=yes");
}

/// `DOMException` is the error the platform throws, and libraries construct it
/// and branch on `.name`. Without it an abort path throws a `ReferenceError`
/// instead, which is how excalidraw's bundle died before rendering anything.
#[test]
fn the_platform_error_type_can_be_constructed() {
    let reading = read(
        "<html><body><output id='out'></output>\
         <script>\
           const e = new DOMException('the operation was aborted', 'AbortError');\
           document.querySelector('#out').textContent = \
             e.name + '/' + e.code + '/' + (e instanceof Error) + '/' + e.message;\
         </script></body></html>",
    );
    reading.assert_clean("DOMException");
    reading.assert_shows("AbortError/20/true/the operation was aborted");
}

/// A panic in the layout engine must not end the process.
///
/// Blitz panics on the GNU bash manual — one megabyte of single-page HTML —
/// with `attempt to subtract with overflow` deep in layout construction. A
/// panic is the one outcome an agent cannot act on: not a thin page, not an
/// error it can read, but a dead process and no answer. This pins the *shape*
/// of the guard with a page that lays out normally; the real page is in the
/// structures corpus, where it now returns 500 lines and a note saying the
/// layout stage failed.
#[test]
fn layout_runs_behind_a_guard_that_reports_rather_than_aborts() {
    // Deeply nested and wide, which is the shape that provokes layout edge
    // cases, and must simply work.
    let mut html = String::from("<html><body>");
    for depth in 0..40 {
        html.push_str(&format!("<div class='d{depth}'><table><tr><td>"));
    }
    html.push_str("deep content");
    for _ in 0..40 {
        html.push_str("</td></tr></table></div>");
    }
    html.push_str("</body></html>");

    let reading = read(&html);
    reading.assert_shows("deep content");
    assert!(
        !reading.outline.contains("layout stage failed"),
        "this page lays out fine: {}",
        reading.outline
    );
}

/// `document.write`, emulated where it can be. A browser inserts at the
/// parser's position; this engine parses first and runs scripts after, so
/// "the parser's position" is the script doing the writing — which is the same
/// place for the one deliberate use, an inline script emitting markup in situ.
#[test]
fn document_write_puts_markup_where_the_script_is() {
    let reading = read(
        "<html><body><p>before</p>\
         <script>document.write('<p id=\"w\">written in place</p>');</script>\
         <p>after</p></body></html>",
    );

    reading.assert_clean("document.write");
    reading.assert_shows("written in place");
    // In place: between what preceded the script and what followed it.
    let outline = &reading.outline;
    let before = outline.find("before").unwrap();
    let written = outline.find("written in place").unwrap();
    let after = outline.find("after").unwrap();
    assert!(before < written && written < after, "order is wrong:\n{outline}");
}

/// Called with no script running, a browser would implicitly `document.open()`
/// and wipe the page. That is refused by name rather than emulated: the call
/// would have been harmless during parsing, and the difference is this engine's
/// script timing rather than the page's intent.
#[test]
fn document_write_after_parsing_refuses_rather_than_wiping_the_page() {
    let reading = read(
        "<html><body><p>keep me</p>\
         <script>setTimeout(() => document.write('<p>too late</p>'), 0);</script>\
         </body></html>",
    );

    reading.assert_shows("keep me");
    assert!(
        reading
            .unsupported
            .iter()
            .any(|(name, _)| name.contains("document.write")),
        "the refusal is named: {:?}",
        reading.unsupported
    );
}

/// A constructable stylesheet, backed by a real `<style>` so its rules reach
/// the style engine rather than being remembered and ignored.
#[test]
fn a_constructed_stylesheet_can_be_adopted() {
    let reading = read(
        "<html><head><title>t</title></head><body><output id='out'></output>\
         <script>\
           const sheet = new CSSStyleSheet();\
           sheet.replaceSync('.x { color: red }');\
           sheet.insertRule('.y { color: blue }');\
           document.adoptedStyleSheets = [sheet];\
           document.querySelector('#out').textContent = \
             'adopted=' + document.adoptedStyleSheets.length + \
             ' styles=' + document.getElementsByTagName('style').length;\
         </script></body></html>",
    );

    reading.assert_clean("constructable stylesheets");
    reading.assert_shows("adopted=1 styles=1");
}

// ── driving a page, not just reading one ─────────────────────────────────────
//
// Every corpus above loads a page and reads it. None of them clicks anything —
// and an agent's loop is read, act, read the difference, so two thirds of the
// product went unmeasured. These drive the page and assert on what the *delta*
// reports, because a change nobody can see is the same as no change.

/// Read the page, act on it, and read the difference.
struct Driver {
    page: h5i_browser_light::engine::Page,
    last: h5i_browser_light::snapshot::Snapshot,
}

impl Driver {
    fn open(html: &str) -> Self {
        let broker = LocalBroker::new(Policy::new(), Arc::new(MemorySink::new()), None)
            .expect("broker");
        let fonts = h5i_browser_light::fonts::load(
            &[],
            &h5i_browser_light::fonts::default_font_dirs(),
            Some(2),
        );
        let options = PageOptions {
            script: true,
            ..Default::default()
        };
        let factory = PageFactory::new(broker, fonts.sources.clone(), options);
        let base = url::Url::parse("https://fixture.example/").unwrap();
        let page = factory.from_html(html, &base);
        let last = page.snapshot();
        Self { page, last }
    }

    /// The node behind an `@ref`, which is how an agent names what to act on.
    fn node(&self, reference: &str) -> usize {
        self.last
            .resolve(reference)
            .unwrap_or_else(|| panic!("no such reference {reference} in:\n{}", self.last.render()))
            .node_id
    }

    /// A reference whose line contains `text`, so a test names things the way a
    /// reader would rather than by counting.
    fn find(&self, text: &str) -> String {
        self.last
            .lines
            .iter()
            .find(|line| line.text.contains(text))
            .and_then(|line| line.reference.clone())
            .unwrap_or_else(|| panic!("nothing actionable says {text:?} in:\n{}", self.last.render()))
    }

    fn click(&mut self, reference: &str) -> h5i_browser_light::snapshot::Delta {
        let node = self.node(reference);
        self.page.dispatch_event(node, "click");
        self.settle()
    }

    fn type_into(&mut self, reference: &str, text: &str) -> h5i_browser_light::snapshot::Delta {
        let node = self.node(reference);
        assert!(self.page.type_into(node, text), "typing was refused");
        self.settle()
    }

    fn settle(&mut self) -> h5i_browser_light::snapshot::Delta {
        self.page.refresh();
        let fresh = self.page.snapshot();
        let delta = fresh.delta(&self.last);
        self.last = fresh;
        delta
    }
}

/// The shape of every todo list: type, submit, see the item appear.
#[test]
fn typing_and_submitting_adds_an_item_the_delta_reports() {
    let mut driver = Driver::open(
        "<html><body><form id='f'><input id='new' placeholder='what needs doing'></form>\
         <ul id='list'></ul>\
         <script>\
           document.querySelector('#f').addEventListener('submit', (e) => {\
             e.preventDefault();\
             const field = document.querySelector('#new');\
             if (!field.value) return;\
             const item = document.createElement('li');\
             item.textContent = field.value;\
             document.querySelector('#list').appendChild(item);\
             field.value = '';\
           });\
         </script></body></html>",
    );

    let field = driver.find("what needs doing");
    driver.type_into(&field, "buy milk");

    // Submitting through the form's own handler, as pressing return would.
    let node = driver.node(&field);
    driver.page.dispatch_event(node, "submit");
    let delta = driver.settle();

    assert!(!delta.is_empty(), "the page changed and the delta says so");
    assert!(
        delta.added.iter().any(|line| line.text.contains("buy milk")),
        "the new item is in the delta rather than only in the page: {:?}",
        delta.added
    );
    assert!(
        delta.removed.len() < 3,
        "adding one item should not read as the page being replaced: {:?}",
        delta.removed
    );
}

/// Clicking a control that rewrites part of the page: the delta must report
/// what changed and *not* the parts that did not.
#[test]
fn clicking_a_filter_reports_only_what_changed() {
    let mut driver = Driver::open(
        "<html><body>\
         <button id='toggle'>show completed</button>\
         <ul id='list'><li>alpha</li><li>beta</li><li>gamma</li></ul>\
         <p id='footer'>3 items</p>\
         <script>\
           let showing = true;\
           document.querySelector('#toggle').addEventListener('click', () => {\
             showing = !showing;\
             document.querySelector('#list').innerHTML = showing\
               ? '<li>alpha</li><li>beta</li><li>gamma</li>'\
               : '<li>gamma</li>';\
           });\
         </script></body></html>",
    );

    let toggle = driver.find("show completed");
    let delta = driver.click(&toggle);

    assert!(
        delta.removed.iter().any(|l| l.text.contains("alpha")),
        "the filtered-out items are reported as removed: {:?}",
        delta.removed
    );
    assert!(
        !delta.removed.iter().any(|l| l.text.contains("3 items")),
        "the footer did not change and must not appear in the delta: {:?}",
        delta.removed
    );
    assert!(!delta.replaced, "one list changing is not the page being replaced");
}

/// An inert control is a result an agent needs: it did nothing, and the reading
/// should say so rather than hand back the page again to be re-read.
#[test]
fn clicking_something_inert_reports_no_change() {
    let mut driver = Driver::open(
        "<html><body><button id='dead'>does nothing</button><p>content</p></body></html>",
    );

    let dead = driver.find("does nothing");
    let delta = driver.click(&dead);

    assert!(delta.is_empty(), "nothing changed: {:?}", delta);
    assert!(
        delta.render().contains("no change"),
        "and the rendering says so plainly: {}",
        delta.render()
    );
}

/// Client-side routing: the address moves and the view changes together, which
/// is the pair an agent needs to trust that a click navigated.
#[test]
fn a_router_click_moves_both_the_view_and_the_address() {
    let mut driver = Driver::open(
        "<html><body><a id='go' href='/two'>go to page two</a>\
         <main id='view'>page one</main>\
         <script>\
           document.querySelector('#go').addEventListener('click', (e) => {\
             e.preventDefault();\
             history.pushState({}, '', '/two');\
             document.querySelector('#view').textContent = 'page two';\
           });\
         </script></body></html>",
    );

    let link = driver.find("go to page two");
    let delta = driver.click(&link);

    assert!(
        delta.added.iter().any(|l| l.text.contains("page two")),
        "the new view is reported: {:?}",
        delta.added
    );
    assert!(
        delta.removed.iter().any(|l| l.text.contains("page one")),
        "and the old one is reported gone: {:?}",
        delta.removed
    );
    assert_eq!(
        driver.page.url().path(),
        "/",
        "the document's own URL is unchanged — the router moved, not the fetch"
    );
}

/// Content the page does not display must not reach the reading.
///
/// Two reasons, and the second is the serious one: the outline claims to be an
/// account of what a page *shows*, and invisible text is the classic vehicle for
/// instructions aimed at whatever is reading it — which is the threat the
/// untrusted-content fence exists for, and which text a human never encounters
/// walks straight around.
#[test]
fn hidden_content_is_not_in_the_reading() {
    let reading = read(
        "<html><head><style>.gone { display: none } .shown { display: block }</style></head>\
         <body>\
         <p class='shown'>visible content</p>\
         <p class='gone'>hidden by a stylesheet</p>\
         <p style='display:none'>hidden by an inline style</p>\
         <div hidden><p>hidden by the attribute</p></div>\
         <div class='gone'><p>a child of a hidden parent</p></div>\
         </body></html>",
    );

    reading.assert_shows("visible content");
    for hidden in [
        "hidden by a stylesheet",
        "hidden by an inline style",
        "hidden by the attribute",
        "a child of a hidden parent",
    ] {
        assert!(
            !reading.outline.contains(hidden),
            "{hidden:?} is not displayed and must not be read:\n{}",
            reading.outline
        );
    }
}

/// `visibility: hidden` is deliberately *kept*. That content still occupies its
/// space, is routinely toggled by script, and is a shape off-screen
/// accessibility text sometimes takes — filtering it would risk deleting page
/// content to fix a smaller problem than `display: none` poses.
#[test]
fn visibility_hidden_is_deliberately_still_read() {
    let reading = read(
        "<html><head><style>.invis { visibility: hidden }</style></head>\
         <body><p class='invis'>still occupies its space</p></body></html>",
    );
    reading.assert_shows("still occupies its space");
}
