//! What a read of a page actually costs, walker against Read IR.
//!
//! Compares phase 0 and phase 1 wall time and allocations.
//!
//! Uses only std to stay lightweight in CI-shaped environments.
//!
//! `cargo bench -p h5i-browser-light --bench read`

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use h5i_browser_light::engine::{PageFactory, PageOptions};
use h5i_browser_light::net::LocalBroker;
use h5i_browser_light::policy::Policy;
use h5i_browser_light::read_ir::ReadTree;
use h5i_browser_light::receipt::MemorySink;
use h5i_browser_light::snapshot::Snapshot;

// ---------------------------------------------------------------- allocator

/// Counts allocations while armed.
///
/// Armed around one measured region at a time, so the counters describe the
/// read rather than the whole process. Relaxed ordering throughout: this is
/// single-threaded measurement, and a fence per allocation would be measuring
/// the instrument.
struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(
                new_size.saturating_sub(layout.size()) as u64,
                Ordering::Relaxed,
            );
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Allocation count and bytes for one call.
#[derive(Clone, Copy, Default)]
struct Allocs {
    count: u64,
    bytes: u64,
}

fn counted<T>(body: impl FnOnce() -> T) -> (T, Allocs) {
    ALLOCS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
    let out = body();
    ARMED.store(false, Ordering::Relaxed);
    (
        out,
        Allocs {
            count: ALLOCS.load(Ordering::Relaxed),
            bytes: BYTES.load(Ordering::Relaxed),
        },
    )
}

// ------------------------------------------------------------------ timing

/// Median wall time over `RUNS`, after a discarded warm-up.
fn median(mut body: impl FnMut()) -> Duration {
    body();
    let mut samples: Vec<Duration> = (0..RUNS)
        .map(|_| {
            let start = Instant::now();
            body();
            start.elapsed()
        })
        .collect();
    samples.sort();
    samples[samples.len() / 2]
}

const RUNS: usize = 9;

/// One operation, measured both ways.
#[derive(Clone, Copy)]
struct Pair {
    walker: Duration,
    walker_allocs: Allocs,
    ir: Duration,
    ir_allocs: Allocs,
}

// ---------------------------------------------------------------- fixtures

/// A long document: headings, prose, lists and links, no forms.
fn large_static(sections: usize) -> String {
    let mut html = String::from("<html><head><title>Large</title></head><body>");
    for s in 0..sections {
        html.push_str(&format!(
            "<section><h2>Section {s}</h2>\
             <p>Paragraph one of section {s}, with <a href='/link/{s}'>a link</a> inside it.</p>\
             <p>Paragraph two of section {s}, longer, so the text arena has something to hold \
             beyond a handful of words and the collapse pass has real input to chew on.</p>\
             <ul><li>first item</li><li>second item</li><li>third item</li></ul>\
             </section>"
        ));
    }
    html.push_str("</body></html>");
    html
}

/// Deeply nested anonymous containers.
///
/// Every level is a `div`, which the walk flattens: no output line, no depth
/// increment, and one `children.clone()` per level on the way through. The
/// fixture that isolates walk overhead from output size.
fn deep(depth: usize, leaves: usize) -> String {
    let mut html = String::from("<html><body>");
    for _ in 0..depth {
        html.push_str("<div>");
    }
    for l in 0..leaves {
        html.push_str(&format!("<p>leaf {l}</p>"));
    }
    for _ in 0..depth {
        html.push_str("</div>");
    }
    html.push_str("</body></html>");
    html
}

/// Many labelled controls.
///
/// The shape that makes the accessible-name computation expensive: each control
/// named only by its `<label for=...>`, which today is resolved by scanning the
/// whole document once per control.
fn forms(fields: usize) -> String {
    let mut html = String::from("<html><body><form>");
    for f in 0..fields {
        html.push_str(&format!(
            "<div class='row'>\
             <label for='field{f}'>Field number {f}</label>\
             <input id='field{f}' name='field{f}' type='text'>\
             </div>"
        ));
    }
    html.push_str("<button type='submit'>Send</button></form></body></html>");
    html
}

/// Three hundred links, side by side.
///
/// The shape the agent-loop bisect found: output size costs nothing, refs cost
/// everything. This is here so the same page can be measured from inside the
/// process, which says whether the whole per-ref cost is the selector pass or
/// only part of it.
fn many_links(count: usize) -> String {
    let body: String = (0..count)
        .map(|n| format!("<a href='/l{n}'>link {n}</a>"))
        .collect();
    format!("<html><head><title>T</title></head><body>{body}</body></html>")
}

/// The same page with one list item inserted near the top.
fn large_static_mutated(sections: usize) -> String {
    large_static(sections).replacen(
        "<ul><li>first item</li>",
        "<ul><li>inserted item</li><li>first item</li>",
        1,
    )
}

// -------------------------------------------------------------------- harness

const BUDGET: usize = 500;

struct Bench {
    factory: PageFactory,
    base: url::Url,
}

impl Bench {
    fn new() -> Self {
        let broker =
            LocalBroker::new(Policy::new(), Arc::new(MemorySink::new()), None).expect("broker");
        let fonts = h5i_browser_light::fonts::load(
            &[],
            &h5i_browser_light::fonts::default_font_dirs(),
            Some(2),
        );
        let factory = PageFactory::new(broker, fonts.sources.clone(), PageOptions::default());
        Self {
            factory,
            base: url::Url::parse("https://fixture.example/").unwrap(),
        }
    }

    fn measure(&self, name: &str, html: &str, mutated: Option<&str>) -> Row {
        let url = self.base.as_str();

        // What the whole load costs, so the read can be read against it. A
        // faster read of a page that took a millisecond to parse is worth less
        // than the same speedup on a page that took ten.
        let load = median(|| {
            std::hint::black_box(self.factory.from_html(html, &self.base));
        });

        let page = self.factory.from_html(html, &self.base);
        let dom = page.dom();
        let doc = dom.borrow();

        let (walker, walker_capture_allocs) =
            counted(|| Snapshot::capture(&doc, url, BUDGET, false));
        let (tree, ir_capture_allocs) = counted(|| ReadTree::capture(&doc, url, BUDGET, false));

        // The gate, restated where the numbers are printed: a faster reading
        // that disagrees is not a faster reading of the same page.
        assert_eq!(tree.render(), walker.render(), "{name}: IR output diverged");
        assert_eq!(tree.ref_entries(), walker.refs, "{name}: IR refs diverged");

        let capture = Pair {
            walker: median(|| {
                std::hint::black_box(Snapshot::capture(&doc, url, BUDGET, false));
            }),
            walker_allocs: walker_capture_allocs,
            ir: median(|| {
                std::hint::black_box(ReadTree::capture(&doc, url, BUDGET, false));
            }),
            ir_allocs: ir_capture_allocs,
        };

        let (_, walker_render_allocs) = counted(|| walker.render());
        let (_, ir_render_allocs) = counted(|| tree.render());
        let render = Pair {
            walker: median(|| {
                std::hint::black_box(walker.render());
            }),
            walker_allocs: walker_render_allocs,
            ir: median(|| {
                std::hint::black_box(tree.render());
            }),
            ir_allocs: ir_render_allocs,
        };

        // What a `snapshot` verb actually does: read the page and hand over
        // text. The number an agent's turn is billed for.
        let (_, walker_read_allocs) =
            counted(|| Snapshot::capture(&doc, url, BUDGET, false).render());
        let (_, ir_read_allocs) = counted(|| ReadTree::capture(&doc, url, BUDGET, false).render());
        let read = Pair {
            walker: median(|| {
                std::hint::black_box(Snapshot::capture(&doc, url, BUDGET, false).render());
            }),
            walker_allocs: walker_read_allocs,
            ir: median(|| {
                std::hint::black_box(ReadTree::capture(&doc, url, BUDGET, false).render());
            }),
            ir_allocs: ir_read_allocs,
        };

        // The durable CSS selector the snapshot verb computes for every ref.
        //
        // Measured separately because the agent-loop benchmark
        // (`scripts/bench_agent_loop.py`) put roughly 0.1 ms per ref somewhere
        // in the verb, and this is the only part of it that scales with the
        // ref count. Either this is that cost or the arithmetic was pointing
        // at the wrong thing.
        let (_, selector_allocs) = counted(|| {
            let mut cache = h5i_browser_light::selector::Cache::new();
            walker
                .refs
                .iter()
                .map(|entry| {
                    h5i_browser_light::selector::for_node_cached(&doc, entry.node_id, &mut cache)
                })
                .collect::<Vec<_>>()
        });
        let selectors = median(|| {
            let mut cache = h5i_browser_light::selector::Cache::new();
            for entry in &walker.refs {
                std::hint::black_box(h5i_browser_light::selector::for_node_cached(
                    &doc,
                    entry.node_id,
                    &mut cache,
                ));
            }
        });
        let selectors = Pair {
            walker: selectors,
            walker_allocs: selector_allocs,
            ir: selectors,
            ir_allocs: selector_allocs,
        };

        // The integration question, asked as a number: if `Page::snapshot`
        // built the IR and then materialised the walker's own type from it,
        // would that be cheaper than the walker? The IR walk is cheaper and the
        // materialisation is not free, so this is the only honest way to know.
        let (_, walker_snapshot_allocs) = counted(|| Snapshot::capture(&doc, url, BUDGET, false));
        let (_, ir_snapshot_allocs) =
            counted(|| ReadTree::capture(&doc, url, BUDGET, false).to_snapshot());
        let to_snapshot = Pair {
            walker: capture.walker,
            walker_allocs: walker_snapshot_allocs,
            ir: median(|| {
                std::hint::black_box(ReadTree::capture(&doc, url, BUDGET, false).to_snapshot());
            }),
            ir_allocs: ir_snapshot_allocs,
        };

        // What an action verb needs from its internal capture, and all it
        // needs: the refs. The lines are built and thrown away today.
        let (_, walker_refs_allocs) =
            counted(|| Snapshot::capture(&doc, url, BUDGET, false).refs);
        let (_, ir_refs_allocs) =
            counted(|| ReadTree::capture(&doc, url, BUDGET, false).ref_entries());
        let refs_only = Pair {
            walker: median(|| {
                std::hint::black_box(Snapshot::capture(&doc, url, BUDGET, false).refs);
            }),
            walker_allocs: walker_refs_allocs,
            ir: median(|| {
                std::hint::black_box(ReadTree::capture(&doc, url, BUDGET, false).ref_entries());
            }),
            ir_allocs: ir_refs_allocs,
        };

        // Unchanged: the commonest step in an agent loop.
        //
        // Against a *separately captured* reading of the same document, not
        // against itself. Comparing a reading with itself hands every string
        // comparison two identical pointers, which is not what an agent's
        // second snapshot looks like and could let `memcmp` answer without
        // reading the bytes. The two readings here are equal and unrelated,
        // which is the real case.
        let walker_again = Snapshot::capture(&doc, url, BUDGET, false);
        let tree_again = ReadTree::capture(&doc, url, BUDGET, false);
        assert!(
            walker_again.delta(&walker).is_empty(),
            "{name}: two readings of one document should not differ"
        );
        let (_, walker_unchanged_allocs) = counted(|| walker_again.delta(&walker));
        let (_, ir_unchanged_allocs) = counted(|| tree_again.delta(&tree));
        let delta_unchanged = Pair {
            walker: median(|| {
                std::hint::black_box(walker_again.delta(&walker));
            }),
            walker_allocs: walker_unchanged_allocs,
            ir: median(|| {
                std::hint::black_box(tree_again.delta(&tree));
            }),
            ir_allocs: ir_unchanged_allocs,
        };

        let delta_small = mutated.map(|after_html| {
            let after_page = self.factory.from_html(after_html, &self.base);
            let after_dom = after_page.dom();
            let after_doc = after_dom.borrow();
            let after = Snapshot::capture(&after_doc, url, BUDGET, false);
            let after_tree = ReadTree::capture(&after_doc, url, BUDGET, false);
            assert!(
                !after.delta(&walker).added.is_empty(),
                "{name}: the mutated fixture must actually differ"
            );
            assert_eq!(
                after_tree.delta(&tree),
                after.delta(&walker),
                "{name}: changed delta diverged"
            );

            let (_, walker_allocs) = counted(|| after.delta(&walker));
            let (_, ir_allocs) = counted(|| after_tree.delta(&tree));
            Pair {
                walker: median(|| {
                    std::hint::black_box(after.delta(&walker));
                }),
                walker_allocs,
                ir: median(|| {
                    std::hint::black_box(after_tree.delta(&tree));
                }),
                ir_allocs,
            }
        });

        Row {
            name: name.to_string(),
            lines: walker.lines.len(),
            refs: walker.refs.len(),
            rendered_bytes: walker.render().len(),
            ir_text_bytes: tree.text_bytes(),
            load,
            capture,
            render,
            read,
            selectors,
            to_snapshot,
            refs_only,
            delta_unchanged,
            delta_small,
        }
    }
}

struct Row {
    name: String,
    lines: usize,
    refs: usize,
    rendered_bytes: usize,
    ir_text_bytes: usize,
    load: Duration,
    capture: Pair,
    render: Pair,
    read: Pair,
    selectors: Pair,
    to_snapshot: Pair,
    refs_only: Pair,
    delta_unchanged: Pair,
    delta_small: Option<Pair>,
}

fn ms(d: Duration) -> String {
    format!("{:.3}", d.as_secs_f64() * 1000.0)
}

fn kb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1024.0)
}

fn speedup(pair: &Pair) -> String {
    let before = pair.walker.as_secs_f64();
    let after = pair.ir.as_secs_f64();
    if after <= 0.0 {
        return "-".to_string();
    }
    format!("{:.1}x", before / after)
}

fn timing_row(label: &str, pair: &Pair) {
    println!(
        "  {:<16} {:>10} {:>10} {:>9}",
        label,
        ms(pair.walker),
        ms(pair.ir),
        speedup(pair)
    );
}

fn alloc_row(label: &str, pair: &Pair) {
    println!(
        "  {:<16} {:>10} {:>10} {:>12} {:>12}",
        label,
        pair.walker_allocs.count,
        pair.ir_allocs.count,
        kb(pair.walker_allocs.bytes),
        kb(pair.ir_allocs.bytes),
    );
}

fn main() {
    let bench = Bench::new();

    let large = large_static(120);
    let large_after = large_static_mutated(120);
    let deep_html = deep(200, 40);
    let forms_html = forms(150);
    let links_html = many_links(300);

    let rows = vec![
        bench.measure("large-static", &large, Some(&large_after)),
        bench.measure("deep-nesting", &deep_html, None),
        bench.measure("form-heavy", &forms_html, None),
        bench.measure("links300", &links_html, None),
    ];

    println!("\nh5i read: walker vs Read IR (design-h5i-ir.md phases 0 and 1)");
    println!("median of {RUNS} runs after a warm-up; allocations counted over one call");
    println!("output asserted byte-identical between the two on every fixture\n");

    println!(
        "{:<14} {:>6} {:>5} {:>9} {:>10} {:>10}",
        "fixture", "lines", "refs", "out(B)", "ir-text(B)", "load(ms)"
    );
    for row in &rows {
        println!(
            "{:<14} {:>6} {:>5} {:>9} {:>10} {:>10}",
            row.name,
            row.lines,
            row.refs,
            row.rendered_bytes,
            row.ir_text_bytes,
            ms(row.load),
        );
    }

    for row in &rows {
        println!("\n{} — wall time (ms)", row.name);
        println!(
            "  {:<16} {:>10} {:>10} {:>9}",
            "operation", "walker", "read-ir", "speedup"
        );
        timing_row("capture", &row.capture);
        timing_row("render", &row.render);
        timing_row("capture+render", &row.read);
        println!(
            "  {:<16} {:>10} {:>10} {:>9}",
            "selectors/refs",
            ms(row.selectors.walker),
            format!("{} refs", row.refs),
            "",
        );
        timing_row("capture->Snapshot", &row.to_snapshot);
        timing_row("capture->refs", &row.refs_only);
        timing_row("delta unchanged", &row.delta_unchanged);
        if let Some(small) = &row.delta_small {
            timing_row("delta small", small);
        }

        println!("\n{} — allocations (count, then KiB)", row.name);
        println!(
            "  {:<16} {:>10} {:>10} {:>12} {:>12}",
            "operation", "walker", "read-ir", "walker KiB", "ir KiB"
        );
        alloc_row("capture", &row.capture);
        alloc_row("render", &row.render);
        alloc_row("capture+render", &row.read);
        alloc_row("capture->Snapshot", &row.to_snapshot);
        alloc_row("capture->refs", &row.refs_only);
        alloc_row("delta unchanged", &row.delta_unchanged);
        if let Some(small) = &row.delta_small {
            alloc_row("delta small", small);
        }
    }
    println!();
}
