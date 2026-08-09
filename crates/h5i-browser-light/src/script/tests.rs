use super::*;
use crate::engine::{PageFactory, PageOptions};
use crate::net::Broker;
use crate::policy::Policy;
use crate::receipt::MemorySink;
use std::sync::Arc;

fn page_and_script(html: &str) -> (crate::engine::Page, Script) {
    let broker = Arc::new(
        Broker::new(Policy::new(), Arc::new(MemorySink::new()), None).expect("broker"),
    );
    let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(2));
    let factory = PageFactory::new(broker, fonts.sources.clone(), PageOptions::default());
    let base = url::Url::parse("https://app.example/").unwrap();
    let page = factory.from_html(html, &base);
    let script = Script::new(page.dom(), factory.broker().clone(), &base).expect("realm");
    (page, script)
}

#[test]
fn script_reads_the_same_tree_the_snapshot_does() {
    let (_page, mut script) = page_and_script(
        "<html><body><h1 id='t'>hello</h1><p class='x'>one</p><p class='x'>two</p></body></html>",
    );

    assert_eq!(
        script.eval_value("document.querySelector('#t').textContent").unwrap(),
        "hello"
    );
    assert_eq!(
        script.eval_value("document.querySelectorAll('.x').length").unwrap(),
        "2"
    );
    assert_eq!(
        script.eval_value("document.querySelector('#t').tagName").unwrap(),
        "H1"
    );
}

#[test]
fn a_mutation_from_script_is_visible_to_the_agent() {
    // The whole point: there is one tree. If the snapshot could not see this,
    // the engine would have two models of the page and no way to say which is
    // right.
    let (mut page, mut script) = page_and_script("<html><body><ul id='list'></ul></body></html>");

    script
        .eval(
            "const li = document.createElement('li'); \
             li.textContent = 'from script'; \
             document.querySelector('#list').appendChild(li);",
        )
        .expect("runs");
    assert!(script.take_dirty(), "the mutation was noticed");
    page.refresh();

    let rendered = page.snapshot().render();
    assert!(rendered.contains("from script"), "{rendered}");
}

#[test]
fn attributes_and_classlist_round_trip_through_the_real_dom() {
    let (_page, mut script) = page_and_script("<html><body><div id='d'></div></body></html>");
    script
        .eval(
            "const d = document.querySelector('#d'); \
             d.setAttribute('data-x', '1'); \
             d.classList.add('a', 'b'); d.classList.toggle('a');",
        )
        .expect("runs");

    assert_eq!(script.eval_value("document.querySelector('#d').getAttribute('data-x')").unwrap(), "1");
    assert_eq!(script.eval_value("document.querySelector('#d').className").unwrap(), "b");
    assert_eq!(script.eval_value("document.querySelector('.b') !== null").unwrap(), "true");
}

#[test]
fn a_click_runs_a_listener_and_bubbles() {
    let (_page, mut script) = page_and_script(
        "<html><body><div id='outer'><button id='b'>go</button></div></body></html>",
    );
    script
        .eval(
            "globalThis.log = []; \
             document.querySelector('#b').addEventListener('click', () => log.push('button')); \
             document.querySelector('#outer').addEventListener('click', () => log.push('outer')); \
             document.querySelector('#b').click();",
        )
        .expect("runs");

    assert_eq!(script.eval_value("log.join(',')").unwrap(), "button,outer");
}

#[test]
fn capture_runs_before_bubble() {
    let (_page, mut script) = page_and_script(
        "<html><body><div id='outer'><button id='b'>go</button></div></body></html>",
    );
    script
        .eval(
            "globalThis.log = []; \
             const outer = document.querySelector('#outer'); \
             outer.addEventListener('click', () => log.push('capture'), true); \
             outer.addEventListener('click', () => log.push('bubble')); \
             document.querySelector('#b').click();",
        )
        .expect("runs");

    assert_eq!(script.eval_value("log.join(',')").unwrap(), "capture,bubble");
}

#[test]
fn a_listener_that_throws_does_not_stop_the_others() {
    let (_page, mut script) = page_and_script("<html><body><button id='b'>go</button></body></html>");
    script
        .eval(
            "globalThis.ran = false; \
             const b = document.querySelector('#b'); \
             b.addEventListener('click', () => { throw new Error('bad') }); \
             b.addEventListener('click', () => { ran = true }); \
             b.click();",
        )
        .expect("runs");

    assert_eq!(script.eval_value("ran").unwrap(), "true");
    assert!(
        script.console().iter().any(|line| line.text.contains("bad")),
        "the throw is reported rather than lost: {:?}",
        script.console()
    );
}

#[test]
fn a_settle_runs_timers_and_says_it_finished() {
    let (_page, mut script) = page_and_script("<html><body></body></html>");
    script
        .eval("globalThis.hits = 0; setTimeout(() => { hits++; setTimeout(() => hits++, 50) }, 10);")
        .expect("runs");

    let settled = script.settle();
    assert_eq!(script.eval_value("hits").unwrap(), "2", "a chained timer ran too");
    assert!(!settled.cut_off, "{settled:?}");
    assert_eq!(settled.timers_run, 2);
    assert!(settled.render().starts_with("settled after"), "{}", settled.render());
}

#[test]
fn a_page_that_never_settles_is_cut_off_and_says_so() {
    // The failure this reports rather than hides: a snapshot taken here
    // describes a page that had not finished, and an agent reading it without
    // that sentence would treat a half-built DOM as the final one.
    let (_page, mut script) = page_and_script("<html><body></body></html>");
    script
        .eval("function again(){ setTimeout(again, 1) } again();")
        .expect("runs");

    let settled = script.settle();
    assert!(settled.cut_off, "{settled:?}");
    assert!(settled.pending_timers > 0);
    assert!(settled.render().contains("still busy"), "{}", settled.render());
}

#[test]
fn promises_settle_before_the_page_is_called_settled() {
    let (_page, mut script) = page_and_script("<html><body></body></html>");
    script
        .eval("globalThis.out=''; (async () => { out += 'a'; await null; out += 'b'; })();")
        .expect("runs");

    script.settle();
    assert_eq!(script.eval_value("out").unwrap(), "ab");
}

#[test]
fn a_missing_web_api_is_recorded_rather_than_silently_stubbed() {
    // An agent has to be able to tell "this page is empty" from "this page
    // needed an API I do not have", so the count reaches the snapshot.
    let (_page, mut script) = page_and_script("<html><body><div id='d'></div></body></html>");
    script
        .eval(
            "new IntersectionObserver(() => {}); new IntersectionObserver(() => {}); \
             matchMedia('(min-width: 1px)'); getComputedStyle(document.querySelector('#d'));",
        )
        .expect("runs");

    let reported = script.unsupported();
    let names: Vec<&str> = reported.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"IntersectionObserver"), "{reported:?}");
    assert!(names.contains(&"matchMedia"), "{reported:?}");
    assert!(names.contains(&"getComputedStyle"), "{reported:?}");
    // Most-used first, because forty calls is likelier to be the problem than one.
    assert_eq!(reported[0].0, "IntersectionObserver");
    assert_eq!(reported[0].1, 2);
}

#[test]
fn element_scoped_queries_do_not_escape_their_element() {
    // Blitz's selector engine always searches from the root, so the scoping is
    // ours to enforce. Getting a match from another panel would look like it
    // worked, which is worse than an error.
    let (_page, mut script) = page_and_script(
        "<html><body><div id='a'><span class='x'>in-a</span></div>\
         <div id='b'><span class='x'>in-b</span></div></body></html>",
    );

    assert_eq!(
        script.eval_value("document.querySelector('#a').querySelector('.x').textContent").unwrap(),
        "in-a"
    );
    assert_eq!(
        script.eval_value("document.querySelector('#b').querySelectorAll('.x').length").unwrap(),
        "1"
    );
    assert_eq!(
        script.eval_value("document.querySelectorAll('.x').length").unwrap(),
        "2"
    );
}

// ── the vertical slice: a page that fetches and re-renders ─────────────────

/// A server with an API the page's script calls.
fn api_server() -> (u16, std::thread::JoinHandle<()>) {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // Exactly as many connections as the test makes, and no more. One more
    // than that and the thread blocks in `accept` forever, which the `join`
    // below turns into a hung test rather than a failing one — the worst shape
    // a test can have, because it looks like a slow build.
    let handle = std::thread::spawn(move || {
        for _ in 0..1 {
            let Ok((stream, _)) = listener.accept() else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                    break;
                }
            }
            let body = r#"{"name":"kelp"}"#;
            let mut stream = stream;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.flush();
        }
    });
    (port, handle)
}

#[test]
fn a_click_runs_script_that_fetches_and_the_agent_sees_the_result() {
    // The vertical slice ROADMAP §12.4 is built around: an agent clicks, page
    // script runs, its request goes through the broker and is receipted, the
    // DOM changes, and the change is in the outline the agent reads.
    let (port, server) = api_server();
    let sink = Arc::new(MemorySink::new());
    let broker = Arc::new(
        Broker::new(Policy::new(), sink.clone(), None).expect("broker"),
    );
    let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(2));
    let factory = PageFactory::new(broker.clone(), fonts.sources.clone(), PageOptions::default());
    let base = url::Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();

    let html = r#"<html><body>
      <button id="add">Add</button><ul id="list"></ul>
      <script>
        document.querySelector('#add').addEventListener('click', async () => {
          const item = await fetch('/api/items').then(r => r.json());
          const li = document.createElement('li');
          li.textContent = item.name;
          document.querySelector('#list').appendChild(li);
        });
      </script>
    </body></html>"#;

    let mut page = factory.from_html(html, &base);
    page.run_scripts(broker.clone()).expect("scripts run");
    assert!(page.has_script());

    let button = page
        .snapshot()
        .refs
        .iter()
        .find(|r| r.name == "Add")
        .expect("the button has a ref")
        .node_id;

    let requests = page.dispatch_event(button, "click").expect("dispatched");

    // The agent's own view of the page carries what the click produced.
    let rendered = page.snapshot().render();
    assert!(rendered.contains("kelp"), "the list re-rendered:\n{rendered}");

    // And the causal link is stamped by the one component that knows it.
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(requests[0].ends_with("/api/items"), "{requests:?}");

    // Every byte the script moved is in the request log, like any other fetch.
    let logged = sink.fetched_urls();
    assert!(
        logged.iter().any(|u| u.ends_with("/api/items")),
        "script traffic is receipted like the parser's: {logged:?}"
    );

    assert!(!page.settled().expect("settled").cut_off);
    let _ = server.join();
}

#[test]
fn an_external_script_is_fetched_through_the_broker_before_it_runs() {
    // A script file is a subresource like any other: policy-checked and
    // receipted before a line of it executes. An engine that fetched script
    // outside its own broker would have one request class with no record, which
    // is the hole this whole design exists to close.
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                break;
            }
        }
        let body = "document.querySelector('#out').textContent = 'from an external file';";
        let mut stream = stream;
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.flush();
    });

    let sink = Arc::new(MemorySink::new());
    let broker = Arc::new(Broker::new(Policy::new(), sink.clone(), None).expect("broker"));
    let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(2));
    let options = PageOptions { script: true, ..Default::default() };
    let factory = PageFactory::new(broker, fonts.sources.clone(), options);
    let base = url::Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();

    let page = factory.from_html(
        r#"<html><body><p id="out">before</p><script src="/app.js"></script></body></html>"#,
        &base,
    );

    let rendered = page.snapshot().render();
    assert!(rendered.contains("from an external file"), "{rendered}");
    assert!(
        sink.fetched_urls().iter().any(|u| u.ends_with("/app.js")),
        "the script file is in the request log: {:?}",
        sink.fetched_urls()
    );
    let _ = server.join();
}

#[test]
fn script_is_off_unless_it_is_asked_for() {
    // The gate ROADMAP §12.5 asks for: a page whose script would change it is
    // left alone, and the outline shows what the server actually sent.
    let broker = Arc::new(Broker::new(Policy::new(), Arc::new(MemorySink::new()), None).unwrap());
    let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(2));
    let factory = PageFactory::new(broker, fonts.sources.clone(), PageOptions::default());
    let base = url::Url::parse("https://app.example/").unwrap();

    let page = factory.from_html(
        "<html><body><p id='out'>before</p><script>document.querySelector('#out').textContent='after'</script></body></html>",
        &base,
    );

    assert!(!page.has_script(), "script must be opt-in");
    let rendered = page.snapshot().render();
    assert!(rendered.contains("before"), "{rendered}");
    assert!(!rendered.contains("after"), "{rendered}");
}

#[test]
fn the_snapshot_says_when_a_page_needed_an_api_this_engine_lacks() {
    // The routing signal. Without it an agent sees a thin outline and cannot
    // tell an empty page from one that needed the other engine.
    let broker = Arc::new(Broker::new(Policy::new(), Arc::new(MemorySink::new()), None).unwrap());
    let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(2));
    let options = PageOptions { script: true, ..Default::default() };
    let factory = PageFactory::new(broker, fonts.sources.clone(), options);

    let page = factory.from_html(
        "<html><body><div id='d'>x</div><script>\
         new IntersectionObserver(() => {}); matchMedia('(min-width: 1px)');\
         </script></body></html>",
        &url::Url::parse("https://app.example/").unwrap(),
    );

    let rendered = page.snapshot().render();
    assert!(rendered.contains("Web APIs this engine does not have"), "{rendered}");
    assert!(rendered.contains("IntersectionObserver"), "{rendered}");
    // Outside the fence, because it is a fact about the reading, not the page.
    let fence = rendered.find(crate::snapshot::CONTENT_BEGIN).unwrap();
    assert!(rendered.find("note:").unwrap() < fence, "{rendered}");
}

#[test]
fn a_page_that_never_settles_says_so_in_the_outline() {
    let broker = Arc::new(Broker::new(Policy::new(), Arc::new(MemorySink::new()), None).unwrap());
    let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(2));
    let options = PageOptions { script: true, ..Default::default() };
    let factory = PageFactory::new(broker, fonts.sources.clone(), options);

    let page = factory.from_html(
        "<html><body><p>hi</p><script>function again(){setTimeout(again,1)}again();</script></body></html>",
        &url::Url::parse("https://app.example/").unwrap(),
    );

    let rendered = page.snapshot().render();
    assert!(rendered.contains("still busy"), "{rendered}");
}

// ── the surface added 2026-08-09 ───────────────────────────────────────────

#[test]
fn inner_html_round_trips_instead_of_stripping_every_tag() {
    // The bug this replaces: the getter returned textContent, so this exact
    // assignment silently destroyed the subtree.
    let (_page, mut script) = page_and_script(
        "<html><body><div id='d'><b>bold</b> and <i>italic</i></div></body></html>",
    );

    let before = script.eval_value("document.querySelector('#d').innerHTML").unwrap();
    assert!(before.contains("<b>bold</b>"), "markup survives: {before}");

    script
        .eval("const d = document.querySelector('#d'); d.innerHTML = d.innerHTML;")
        .expect("round trip");
    let after = script.eval_value("document.querySelector('#d').innerHTML").unwrap();
    assert!(after.contains("<b>bold</b>"), "and survives the round trip: {after}");
    assert_eq!(
        script.eval_value("document.querySelectorAll('#d b').length").unwrap(),
        "1"
    );
}

#[test]
fn a_document_fragment_inserts_its_children_and_not_itself() {
    // The bug this replaces: `createDocumentFragment` returned a <div>, so
    // every fragment insert added an element the page never created, breaking
    // `.parent > .child` and the layout under it.
    let (mut page, mut script) = page_and_script("<html><body><ul id='l'></ul></body></html>");
    script
        .eval(
            "const f = document.createDocumentFragment(); \
             for (const n of ['a','b']) { const li = document.createElement('li'); \
               li.textContent = n; f.appendChild(li); } \
             document.querySelector('#l').appendChild(f);",
        )
        .expect("runs");
    page.refresh();

    assert_eq!(script.eval_value("document.querySelectorAll('#l > li').length").unwrap(), "2");
    assert_eq!(
        script.eval_value("document.querySelectorAll('#l > div').length").unwrap(),
        "0",
        "no stray element from the fragment"
    );
}

#[test]
fn element_style_is_backed_by_the_style_attribute() {
    // One source of truth: what script sets is what the cascade sees and what
    // getAttribute returns, rather than a parallel object that can disagree.
    let (_page, mut script) = page_and_script("<html><body><div id='d'></div></body></html>");
    script
        .eval(
            "const d = document.querySelector('#d'); \
             d.style.display = 'none'; d.style.backgroundColor = 'red';",
        )
        .expect("runs");

    let attr = script.eval_value("document.querySelector('#d').getAttribute('style')").unwrap();
    assert!(attr.contains("display: none"), "{attr}");
    assert!(attr.contains("background-color: red"), "camelCase reaches the dashed name: {attr}");
    assert_eq!(script.eval_value("document.querySelector('#d').style.display").unwrap(), "none");

    script.eval("document.querySelector('#d').style.display = ''").expect("clears");
    assert_eq!(script.eval_value("document.querySelector('#d').style.display").unwrap(), "");
}

#[test]
fn bounding_rects_come_from_the_layout_the_engine_already_computed() {
    // Zeros — which is what this returned before — send a positioning library
    // into a loop that never converges.
    let (_page, mut script) = page_and_script(
        "<html><body><div id='a' style='height:40px'>a</div>\
         <div id='b' style='height:40px'>b</div></body></html>",
    );

    let width = script.eval_value("document.querySelector('#a').getBoundingClientRect().width").unwrap();
    assert_ne!(width, "0", "a laid-out block has width");

    let top_a: f64 = script.eval_value("document.querySelector('#a').getBoundingClientRect().top").unwrap().parse().unwrap();
    let top_b: f64 = script.eval_value("document.querySelector('#b').getBoundingClientRect().top").unwrap().parse().unwrap();
    assert!(top_b > top_a, "the second block is below the first: {top_a} then {top_b}");
}

#[test]
fn dataset_closest_and_matches_work_off_the_real_tree() {
    let (_page, mut script) = page_and_script(
        "<html><body><section class='panel'><button id='b' data-item-id='7'>go</button>\
         </section></body></html>",
    );

    assert_eq!(script.eval_value("document.querySelector('#b').dataset.itemId").unwrap(), "7");
    assert_eq!(script.eval_value("document.querySelector('#b').matches('#b')").unwrap(), "true");
    assert_eq!(script.eval_value("document.querySelector('#b').matches('.panel')").unwrap(), "false");
    assert_eq!(
        script.eval_value("document.querySelector('#b').closest('.panel').tagName").unwrap(),
        "SECTION"
    );
}

#[test]
fn insert_adjacent_html_places_markup_where_it_was_told() {
    let (mut page, mut script) = page_and_script("<html><body><ul id='l'><li>one</li></ul></body></html>");
    script
        .eval(
            "const l = document.querySelector('#l'); \
             l.insertAdjacentHTML('beforeend', '<li>last</li>'); \
             l.insertAdjacentHTML('afterbegin', '<li>first</li>');",
        )
        .expect("runs");
    page.refresh();

    let items = script
        .eval_value("[...document.querySelectorAll('#l > li')].map(n => n.textContent).join(',')")
        .unwrap();
    assert_eq!(items, "first,one,last");
}

#[test]
fn typed_events_carry_the_fields_a_page_reads() {
    // A single generic Event left `detail` and `key` undefined, which a
    // framework notices immediately and silently.
    let (_page, mut script) = page_and_script("<html><body><button id='b'>go</button></body></html>");
    script
        .eval(
            "globalThis.seen = {}; const b = document.querySelector('#b'); \
             b.addEventListener('pick', (e) => { seen.detail = e.detail }); \
             b.addEventListener('keydown', (e) => { seen.key = e.key }); \
             b.dispatchEvent(new CustomEvent('pick', { detail: { id: 3 } })); \
             b.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));",
        )
        .expect("runs");

    assert_eq!(script.eval_value("seen.detail.id").unwrap(), "3");
    assert_eq!(script.eval_value("seen.key").unwrap(), "Enter");
    assert_eq!(
        script.eval_value("document.querySelector('#b').click() === undefined").unwrap(),
        "true",
        "a synthetic click is a MouseEvent and does not throw"
    );
}

#[test]
fn storage_is_real_and_in_memory_only() {
    let (_page, mut script) = page_and_script("<html><body></body></html>");
    script
        .eval("localStorage.setItem('k', 'v'); sessionStorage.setItem('s', '1');")
        .expect("runs");

    assert_eq!(script.eval_value("localStorage.getItem('k')").unwrap(), "v");
    assert_eq!(script.eval_value("localStorage.length").unwrap(), "1");
    assert_eq!(script.eval_value("localStorage.getItem('absent')").unwrap(), "null");
    assert_eq!(script.eval_value("sessionStorage.getItem('s')").unwrap(), "1");

    // A fresh realm starts empty: nothing was written anywhere durable.
    let (_page2, mut fresh) = page_and_script("<html><body></body></html>");
    assert_eq!(fresh.eval_value("localStorage.getItem('k')").unwrap(), "null");
}

#[test]
fn history_records_routing_and_fires_popstate() {
    // SPAs route through pushState. A stub meant client-side navigation
    // silently did nothing at all.
    let (_page, mut script) = page_and_script("<html><body></body></html>");
    script
        .eval(
            "globalThis.popped = null; \
             addEventListener('popstate', (e) => { popped = e.state }); \
             history.pushState({ page: 1 }, '', '/one'); \
             history.pushState({ page: 2 }, '', '/two');",
        )
        .expect("runs");

    assert_eq!(script.eval_value("history.state.page").unwrap(), "2");
    assert_eq!(script.eval_value("history.length").unwrap(), "3");

    script.eval("history.back()").expect("goes back");
    assert_eq!(script.eval_value("history.state.page").unwrap(), "1");
    assert_eq!(script.eval_value("popped.page").unwrap(), "1", "popstate carried the state");
}

#[test]
fn a_page_from_the_web_may_not_reach_the_boxs_dev_server() {
    // The hole script introduced: loopback is allowed by default because the
    // dev server is the point, and it bypasses the egress proxy. Without an
    // origin the policy cannot tell "the dev server's own page" from "a page
    // that would like to read it".
    use crate::policy::Policy;
    let policy = Policy::new();
    let loopback = url::Url::parse("http://127.0.0.1:3000/src/main.rs").unwrap();

    let from_web = url::Url::parse("https://evil.example/page").unwrap();
    assert!(
        policy.check_from(&loopback, Some(&from_web)).reason().is_some(),
        "a web page must not reach loopback"
    );

    let from_dev_server = url::Url::parse("http://127.0.0.1:3000/index.html").unwrap();
    assert!(
        policy.check_from(&loopback, Some(&from_dev_server)).reason().is_none(),
        "the dev server's own page still may"
    );

    // No document is the agent naming a URL itself, which is not a page
    // reaching for one.
    assert!(policy.check_from(&loopback, None).reason().is_none());
}
