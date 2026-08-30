//! What this engine costs, measured rather than assumed.
//!
//! roadmap-history.md §B5 Tier 4. Nothing here had numbers after script landed: not
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

        let (scripted, _) = factory(true);
        let scripted_html = document_with_script(rows);
        // No `run_scripts` here: a script-enabled factory has already run them
        // inside `from_html`, through `finish_page`. Calling it again does not
        // no-op — it builds a *second* realm and runs the page's scripts again
        // — so this column counted the realm twice for as long as it has
        // existed. At 15.9 ms a realm that was a third of the number; at 58 ms
        // it was most of it, which is how it was finally noticed.
        let with_script = time(5, || {
            let mut page = scripted.from_html(&scripted_html, &url);
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

    // ── the realm, by phase ─────────────────────────────────────────────────
    //
    // The prelude is compiled once per thread and run once per realm, so the
    // first realm on a thread is the only one that pays for the compile. That
    // makes these two columns a before-and-after of the change, measured in one
    // run rather than argued across two builds.
    //
    // On a thread of its own because the template is per-thread, and a realm
    // built anywhere above would already have paid the compile.
    {
        let phases = std::thread::spawn(|| {
            let url = url::Url::parse("https://bench.example/").unwrap();
            let (factory, broker) = factory(true);
            let page = factory.from_html(&document(10), &url);
            let first = h5i_browser_light::script::Script::new(page.dom(), broker.clone(), &url)
                .expect("realm")
                .cost();
            let later = h5i_browser_light::script::Script::new(page.dom(), broker.clone(), &url)
                .expect("realm")
                .cost();
            (first, later)
        })
        .join()
        .expect("realm phases");

        println!("\nstarting the script realm, by phase");
        println!("                       first realm   later realms");
        let row = |name: &str, first: Duration, later: Duration| {
            println!("  {name:<19} {first:>10.1?}   {later:>10.1?}");
        };
        row("context", phases.0.context, phases.1.context);
        row("primitives", phases.0.primitives, phases.1.primitives);
        row(
            "prelude compile",
            phases.0.prelude_compile,
            phases.1.prelude_compile,
        );
        row("prelude run", phases.0.prelude_run, phases.1.prelude_run);
        row("total", phases.0.total(), phases.1.total());
    }

    // ── starting the realm ──────────────────────────────────────────────────
    //
    // A fixed cost paid once per page, which dominates a small one: the prelude
    // is run from scratch for every realm, even though it is compiled only once.
    {
        let url = url::Url::parse("https://bench.example/").unwrap();
        let (factory, broker) = factory(true);
        let page = factory.from_html(&document(10), &url);

        let start = time(5, || {
            let _ = h5i_browser_light::script::Script::new(page.dom(), broker.clone(), &url)
                .expect("realm");
        });
        println!("\nstarting the script realm: {start:.1?} per page");
        // Code, not bytes. Blanking every comment in the prelude — 164 KiB of
        // 448, a third of the file — changed parse time by nothing at all, so
        // the number that predicts this cost is what the parser has to
        // tokenise, and the documentation is free.
        let code = |source: &str| -> usize {
            source
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .map(|line| line.len() + 1)
                .sum()
        };
        let core = include_str!("../src/script/prelude.js");
        let tiers = [
            ("conformance", include_str!("../src/script/prelude/conformance.js")),
            ("sockets", include_str!("../src/script/prelude/sockets.js")),
            ("has", include_str!("../src/script/prelude/has.js")),
        ];
        println!(
            "  the core prelude is {} KiB of code ({} KiB with its comments), compiled once \
             per thread and run per realm",
            code(core) / 1024,
            core.len() / 1024
        );
        let deferred: usize = tiers.iter().map(|(_, source)| code(source)).sum();
        println!(
            "  {} KiB more sits in {} tiers ({}), parsed only when a page asks",
            deferred / 1024,
            tiers.len(),
            tiers.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")
        );
    }

    // ── what a scripted page costs before it has any content ────────────────
    //
    // One section, so the number is almost entirely fixed cost: build the realm,
    // run one trivial script, fire the load lifecycle, settle. It is the floor
    // under every scripted page, and the place a fixed cost that is nobody's
    // feature shows up as itself.
    //
    // It found one. The deadline watchdog polled a flag every 20 ms and was
    // *joined*, so a settle that finished in 50 us then waited for a sleeping
    // thread to notice — up to 20 ms on every settle, and on every agent `wait`
    // besides. It read as script time in a phase profile, and it was
    // intermittent, because whether the watchdog had reached its first sleep was
    // a race with the body finishing.
    {
        let url = url::Url::parse("https://bench.example/").unwrap();
        let (scripted, _) = factory(true);
        let minimal = document_with_script(1);
        let floor = time(9, || {
            let _ = scripted.from_html(&minimal, &url);
        });
        println!("\nthe floor under a scripted page (1 section): {floor:.1?}");
    }

    // ── queries and collections ─────────────────────────────────────────────
    {
        let url = url::Url::parse("https://bench.example/").unwrap();
        let (factory, broker) = factory(true);
        // Same as below: this document carries no script, so the only realm is
        // the one built here, which is the one the queries run in.
        let page = factory.from_html(&document(200), &url);
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

    // ── where a DOM property read goes ──────────────────────────────────────
    //
    // This section is why the reporting moved. Every DOM property read used to
    // go through a `get` trap on a proxy in front of every wrapper, so that an
    // unknown name could report itself — 799 ns of the 882 ns a *known*
    // property cost, against 82 ns for a plain object. The reporting now sits
    // at the end of the prototype chain instead, where only a read that missed
    // arrives, and a known read never meets it.
    println!();
    let url = url::Url::parse("https://bench.example/").unwrap();
    let (scripted, broker) = factory(true);
    // The document has no script of its own, so `from_html` builds no realm for
    // it (§B8.9: a page with nothing to run stopped paying for a realm). The
    // one below is the only one, and it is the one being measured through.
    let page = scripted.from_html(&document(20), &url);
    let mut script = h5i_browser_light::script::Script::new(page.dom(), broker, &url)
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
    // A property that genuinely goes to the tree on every read. It used to be
    // `tagName`, which stopped being one: a tag cannot change, so the wrapper
    // remembers it and the second read never leaves JavaScript. That is the
    // point of the fourth line below, and the reason this one had to move.
    let native = bench(
        "(() => { let n = 0; const el = document.querySelector('section'); \
           for (let i = 0; i < 20000; i++) n += el.className.length; return n })()",
    );
    let remembered = bench(
        "(() => { let n = 0; const el = document.querySelector('section'); \
           for (let i = 0; i < 20000; i++) n += el.tagName.length; return n })()",
    );

    let per = |d: Duration| d.as_nanos() as f64 / reads as f64;
    println!("where a DOM property read goes, over {reads} reads:");
    println!("  plain object                  {plain:>10.1?}  ({:.0} ns each)", per(plain));
    println!(
        "  watched node, known property  {trapped:>10.1?}  ({:.0} ns each)",
        per(trapped)
    );
    println!(
        "  watched node, read from tree  {native:>10.1?}  ({:.0} ns each)",
        per(native)
    );
    println!(
        "  watched node, remembered      {remembered:>10.1?}  ({:.0} ns each)",
        per(remembered)
    );
    println!(
        "\n  the object model itself       {:>10.1?}  ({:.0} ns each)",
        trapped.saturating_sub(plain),
        per(trapped.saturating_sub(plain))
    );
    println!(
        "  reaching into the tree        {:>10.1?}  ({:.0} ns each)",
        native.saturating_sub(trapped),
        per(native.saturating_sub(trapped))
    );

    // ── inside one native call ──────────────────────────────────────────────
    //
    // "Reaching into the tree" is four things at once, and which of them
    // dominates decides what is worth fixing: Boa dispatching to a native
    // function, converting the arguments, finding the host and the node, and
    // building the answer. Three primitives with the same shape and different
    // amounts of work at the end separate them.
    let kind = bench(
        "(() => { let n = 0; const id = document.querySelector('h2')._id; \
           for (let i = 0; i < 20000; i++) n += __h5i.nodeKind(id); return n })()",
    );
    let tag = bench(
        "(() => { let n = 0; const id = document.querySelector('h2')._id; \
           for (let i = 0; i < 20000; i++) n += __h5i.tagName(id).length; return n })()",
    );
    let attr = bench(
        "(() => { let n = 0; const id = document.querySelector('h2')._id; \
           for (let i = 0; i < 20000; i++) n += String(__h5i.getAttr(id, 'class')).length; \
           return n })()",
    );
    println!("\nwhere a native call goes, over {reads} calls:");
    println!("  nodeKind: a number back        {kind:>10.1?}  ({:.0} ns each)", per(kind));
    println!("  tagName: a string back         {tag:>10.1?}  ({:.0} ns each)", per(tag));
    println!("  getAttr: a string each way     {attr:>10.1?}  ({:.0} ns each)", per(attr));

    println!(
        "\nThe object model is the price of a node being an object rather than a number; the tree\n\
         read is the price of there being one real DOM rather than a JavaScript copy of one. A\n\
         read the wrapper can answer from what it already knows pays neither, which is what the\n\
         fourth line is: a tag cannot change, so it is asked for once.\n\n\
         Measured and rejected, so nobody tries again. **Precomputing the known property names**,\n\
         so the old proxy trap did a hash lookup instead of walking the prototype chain: no\n\
         change — the cost was Boa dispatching into a JavaScript trap at all, which is why the\n\
         fix in the end was to stop being in front of the object. **Interning the uppercased tag\n\
         names**, and building the attribute answer without its two intermediate `String`s: no\n\
         change either, and the line above says why — a native call costs ~150 ns before it does\n\
         anything at all, so shaving the work at the far end of it is shaving the small half.\n\
         The allocations went anyway; the cache did not, because it was state for nothing."
    );
}
