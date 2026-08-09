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

    let mut page = factory.from_html(html, &base);
    page.run_scripts(broker).expect("the realm starts");

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
