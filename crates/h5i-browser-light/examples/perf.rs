//! What this engine costs, measured rather than assumed.
//!
//! ROADMAP.md §B5 Tier 4. Nothing here had numbers after script landed: not
//! the time to read a page, not the memory a page holds, and not the price of
//! the reporting proxy that now sits in front of every DOM property read.
//!
//!     cargo run --release --example perf
//!
//! Deliberately not a `#[test]`: a timing that fails CI on a loaded runner
//! teaches nothing, and a number nobody looks at is worse than no number. Run it
//! when changing the hot paths, and put the result in the commit.

use std::sync::Arc;
use std::time::{Duration, Instant};

use h5i_browser_light::engine::{PageFactory, PageOptions};
use h5i_browser_light::net::LocalBroker;
use h5i_browser_light::policy::Policy;
use h5i_browser_light::receipt::MemorySink;

/// A page shaped like a real one: nested structure, text, links, attributes.
fn document(rows: usize) -> String {
    let mut html = String::from("<html><head><title>bench</title></head><body><main>");
    for row in 0..rows {
        html.push_str(&format!(
            "<section class='row r{row}' data-index='{row}'>\
             <h2>Section {row}</h2>\
             <p>Some prose for row {row}, long enough to need shaping and wrapping.</p>\
             <ul><li><a href='/item/{row}/a'>first</a></li>\
             <li><a href='/item/{row}/b'>second</a></li></ul></section>"
        ));
    }
    html.push_str("</main></body></html>");
    html
}

/// The same page, with a script on it.
///
/// The plain document has none, so once a scriptless page stopped building a
/// realm the "script" column measured nothing at all. A column that reports a
/// cost nobody pays is worse than no column.
fn document_with_script(rows: usize) -> String {
    let mut html = document(rows);
    let script = "<script>        let seen = 0;         for (const a of document.querySelectorAll('a')) seen += a.textContent.length;         document.querySelector('h2').setAttribute('data-seen', String(seen));        </script>";
    html.insert_str(html.len() - "</body></html>".len(), script);
    html
}

fn factory(script: bool) -> (PageFactory, Arc<LocalBroker>) {
    let broker = LocalBroker::new(Policy::new(), Arc::new(MemorySink::new()), None)
        .expect("broker");
    let fonts = h5i_browser_light::fonts::load(
        &[],
        &h5i_browser_light::fonts::default_font_dirs(),
        Some(2),
    );
    let options = PageOptions {
        script,
        ..Default::default()
    };
    (
        PageFactory::new(broker.clone(), fonts.sources.clone(), options),
        broker,
    )
}

/// Resident set size, in kibibytes. Linux only; elsewhere the column is blank.
fn rss_kib() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4)
}

fn median(mut runs: Vec<Duration>) -> Duration {
    runs.sort();
    runs[runs.len() / 2]
}

fn time<F: FnMut()>(rounds: usize, mut body: F) -> Duration {
    // Median of several, because the first run pays for lazily built font and
    // style caches and reporting that as the cost would overstate it.
    let mut runs = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let started = Instant::now();
        body();
        runs.push(started.elapsed());
    }
    median(runs)
}

fn main() {
    println!("h5i-browser-light — what it costs\n");
    let profile = if cfg!(debug_assertions) {
        "debug (numbers are several times the release cost)"
    } else {
        "release"
    };
    println!("build: {profile}\n");

    // ── reading a page ──────────────────────────────────────────────────────
    println!("{:<34} {:>10} {:>10} {:>12}", "reading a page", "no script", "script", "outline");
    for rows in [10usize, 100, 500] {
        let html = document(rows);
        let url = url::Url::parse("https://bench.example/").unwrap();

        let (plain, _) = factory(false);
        let bare = time(5, || {
            let mut page = plain.from_html(&html, &url);
            page.refresh();
            let _ = page.snapshot();
        });

        let (scripted, broker) = factory(true);
        let scripted_html = document_with_script(rows);
        let with_script = time(5, || {
            let mut page = scripted.from_html(&scripted_html, &url);
            page.run_scripts(broker.clone()).expect("realm");
            page.refresh();
            let _ = page.snapshot();
        });

        let lines = {
            let mut page = plain.from_html(&html, &url);
            page.refresh();
            page.snapshot().lines.len()
        };

        println!(
            "{:<34} {:>9.1?} {:>9.1?} {:>9} lines",
            format!("{rows} sections (~{} nodes)", rows * 9),
            bare,
            with_script,
            lines
        );
    }

    // ── what a page holds ───────────────────────────────────────────────────
    println!();
    if let Some(before) = rss_kib() {
        let html = document(500);
        let url = url::Url::parse("https://bench.example/").unwrap();
        let (plain, _) = factory(false);
        let mut pages = Vec::new();
        for _ in 0..4 {
            let mut page = plain.from_html(&html, &url);
            page.refresh();
            pages.push(page);
        }
        let after = rss_kib().unwrap_or(before);
        println!(
            "memory: 4 x 500-section page costs {} MiB resident ({:.1} MiB each)",
            (after.saturating_sub(before)) / 1024,
            (after.saturating_sub(before)) as f64 / 4.0 / 1024.0
        );
        drop(pages);
    }

    // ── starting the realm ──────────────────────────────────────────────────
    //
    // A fixed cost paid once per page, which dominates a small one: the prelude
    // is parsed and evaluated from scratch every time.
    {
        let url = url::Url::parse("https://bench.example/").unwrap();
        let (factory, broker) = factory(true);
        let page = factory.from_html(&document(10), &url);

        let start = time(5, || {
            let _ = h5i_browser_light::script::Script::new(page.dom(), broker.clone(), &url)
                .expect("realm");
        });
        println!("\nstarting the script realm: {start:.1?} per page");
        println!(
            "  prelude is {} lines / {} KiB of JavaScript, parsed and evaluated each time",
            include_str!("../src/script/prelude.js").lines().count(),
            include_str!("../src/script/prelude.js").len() / 1024,
        );
    }

    // ── queries and collections ─────────────────────────────────────────────
    {
        let url = url::Url::parse("https://bench.example/").unwrap();
        let (factory, broker) = factory(true);
        let mut page = factory.from_html(&document(200), &url);
        page.run_scripts(broker.clone()).expect("realm");
        let mut script =
            h5i_browser_light::script::Script::new(page.dom(), broker, &url).expect("realm");

        let document_wide = time(5, || {
            script
                .eval("(() => { let n = 0; for (let i = 0; i < 200; i++) \
                        n += document.querySelectorAll('a').length; return n })()")
                .expect("runs");
        });
        let scoped = time(5, || {
            script
                .eval("(() => { const s = document.querySelector('section'); let n = 0; \
                        for (let i = 0; i < 200; i++) n += s.querySelectorAll('a').length; \
                        return n })()")
                .expect("runs");
        });
        let iterate = time(5, || {
            script
                .eval("(() => { const all = document.querySelectorAll('a'); let n = 0; \
                        for (let i = 0; i < 200; i++) for (const a of all) n += 1; return n })()")
                .expect("runs");
        });

        println!("\nqueries over a 200-section page (1800 nodes), 200 calls each:");
        println!("  document.querySelectorAll('a')  {document_wide:>9.1?}  ({:.0} us each)",
            document_wide.as_nanos() as f64 / 200.0 / 1000.0);
        println!("  section.querySelectorAll('a')   {scoped:>9.1?}  ({:.0} us each)",
            scoped.as_nanos() as f64 / 200.0 / 1000.0);
        println!("  iterating a 400-node result     {iterate:>9.1?}  ({:.0} us each)",
            iterate.as_nanos() as f64 / 200.0 / 1000.0);
    }

    // ── the reporting proxy ─────────────────────────────────────────────────
    //
    // Every DOM property read goes through a `get` trap so that an unknown name
    // can report itself. That is a real cost on the hottest path in the engine,
    // and it had never been measured.
    println!();
    let url = url::Url::parse("https://bench.example/").unwrap();
    let (scripted, broker) = factory(true);
    let mut page = scripted.from_html(&document(20), &url);
    page.run_scripts(broker).expect("realm");
    let mut script =
        h5i_browser_light::script::Script::new(page.dom(), {
            let (_, broker) = factory(true);
            broker
        }, &url)
        .expect("realm");

    let reads = 20_000;
    let mut bench = |source: &str| {
        let owned = source.to_string();
        time(5, || {
            script.eval(&owned).expect("runs");
        })
    };

    // Three measurements, because one number cannot say where the cost is.
    let plain = bench(
        "(() => { let n = 0; const raw = { tagName: 'H2' }; \
           for (let i = 0; i < 20000; i++) n += raw.tagName.length; return n })()",
    );
    // A property the prototype answers, with no native call behind it: this
    // isolates the trap from the cost of reaching into the tree.
    let trapped = bench(
        "(() => { let n = 0; const el = document.querySelector('h2'); \
           for (let i = 0; i < 20000; i++) n += el.cloneNode ? 1 : 0; return n })()",
    );
    let native = bench(
        "(() => { let n = 0; const el = document.querySelector('h2'); \
           for (let i = 0; i < 20000; i++) n += el.tagName.length; return n })()",
    );

    let per = |d: Duration| d.as_nanos() as f64 / reads as f64;
    println!("where a DOM property read goes, over {reads} reads:");
    println!("  plain object, no proxy        {plain:>10.1?}  ({:.0} ns each)", per(plain));
    println!(
        "  watched node, known property  {trapped:>10.1?}  ({:.0} ns each)",
        per(trapped)
    );
    println!(
        "  watched node, read from tree  {native:>10.1?}  ({:.0} ns each)",
        per(native)
    );
    println!(
        "\n  the proxy trap itself         {:>10.1?}  ({:.0} ns each)",
        trapped.saturating_sub(plain),
        per(trapped.saturating_sub(plain))
    );
    println!(
        "  reaching into the tree        {:>10.1?}  ({:.0} ns each)",
        native.saturating_sub(trapped),
        per(native.saturating_sub(trapped))
    );

    println!(
        "\nThe trap is the price of naming what a page asked for and we lack; the tree read is\n\
         the price of there being one real DOM rather than a JavaScript copy of one. Neither is\n\
         free, and the second is the larger.\n\n\
         Measured and rejected: precomputing the set of known property names, so the trap does a\n\
         hash lookup instead of walking the prototype chain, changed nothing. The cost is Boa\n\
         dispatching into a JavaScript trap at all, not the test inside it — so the only way to\n\
         remove it is to stop watching, which would cost the naming that makes a gap reportable."
    );
}
