//! `h5i websec dom`: the DOM Invader analogue.
//!
//! Prototype pollution and DOM-XSS have their source in the page, not on the
//! wire, so the workbench's other verbs cannot see them. `scan` drives
//! client-side probes (the ppfuzz payload model) and reads what reached a sink;
//! `node` runs a confined Node target and reports which `Object.prototype`
//! pollutions reach a dangerous sink. Both land their results as findings.
//!
//! This module is the skeleton: the corpus, the result types, and the path from
//! a detection to a `finding::Entry`. The drivers that produce detections land
//! in later phases (proxy injection for `scan`, the box prober for `node`).

use crate::finding::{self, Findings};
use crate::read;
use h5i_core::browser_session as bs;

/// The property a probe leaves on `Object.prototype`, and the value it sets.
///
/// A navigation that reads back `Object.prototype.h5ipp == "reserved"` proves
/// the page walked attacker input into the root prototype.
pub const CANARY_PROP: &str = "h5ipp";
pub const CANARY_VALUE: &str = "reserved";

/// The Node gadget probe, loaded into the in-box target with `--require`.
const GADGET_PROBE_JS: &str = include_str!("gadget-probe.js");

/// Where the probe is written inside the box's `/work`, and run from.
const PROBE_IN_BOX: &str = "./.h5i-gadget-probe.js";

/// The prototype-pollution corpus. The two families ppfuzz calls "object"
/// (`__proto__`) and "pointer" (`constructor.prototype`), each in dot and
/// bracket form. `{p}` is the canary property, `{v}` its value. Each is tried
/// both as a query parameter and as a URL fragment (fragments never reach the
/// server, so only the proxied browser's navigation seeds them).
pub const PP_PAYLOADS: &[&str] = &[
    "__proto__.{p}={v}",
    "__proto__[{p}]={v}",
    "constructor.prototype.{p}={v}",
    "constructor[prototype][{p}]={v}",
];

/// A concrete payload string with the canary filled in.
pub fn render(template: &str) -> String {
    template
        .replace("{p}", CANARY_PROP)
        .replace("{v}", CANARY_VALUE)
}

/// One confirmed client-side result: a pollution, and where it landed.
#[derive(Debug, Clone)]
pub struct Hit {
    /// The origin the pollution was seen on, e.g. `https://app.example`. Part of
    /// the title so two different targets are two findings, not one.
    pub origin: String,
    /// The rendered payload that polluted, e.g. `__proto__[h5ipp]=reserved`.
    pub payload: String,
    /// The source the tainted value came from: `url`, `hash`, `name`, …
    pub source: String,
    /// The property left on `Object.prototype`.
    pub property: String,
    /// The sink the value reached, if a source→sink flow was seen.
    pub sink: Option<String>,
}

/// The scheme-host-port origin of a URL, or the whole URL if it will not parse.
fn origin_of(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => u.origin().ascii_serialization(),
        Err(_) => url.to_string(),
    }
}

/// Origin + path, dropping query and fragment — the key that matches a report's
/// URL to a captured request, since a fragment probe's `#…` never hits the wire.
fn url_key(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    Some(format!("{}{}", u.origin().ascii_serialization(), u.path()))
}

impl Hit {
    /// One line naming the finding. The origin keeps two targets distinct; the
    /// source and sink keep the different payload forms on one target together.
    fn title(&self) -> String {
        match &self.sink {
            Some(sink) => format!("prototype pollution: {} → {sink} ({})", self.source, self.origin),
            None => format!("prototype pollution via {} ({})", self.source, self.origin),
        }
    }

    /// What was learned, for the finding's note.
    fn note(&self) -> String {
        let mut note = format!(
            "payload `{}` set Object.prototype.{}",
            self.payload, self.property
        );
        if let Some(sink) = &self.sink {
            note.push_str(&format!("; reached sink {sink}"));
        }
        note
    }
}

/// Build the finding entry a client-side hit writes. Evidence ids are the
/// capture receipts the proxy recorded for the probing navigation.
pub fn hit_entry(id: &str, hit: &Hit, evidence: Vec<String>) -> anyhow::Result<finding::Entry> {
    finding::entry(
        id,
        Some(&hit.title()),
        Some("unconfirmed: client-side, found by dom scan"),
        Some(&hit.note()),
        evidence,
        None,
    )
}

/// `h5i websec dom`.
#[derive(clap::Subcommand)]
pub enum DomVerb {
    /// Client-side: seed prototype-pollution payloads and fold what reached a sink into findings.
    ///
    /// Arm the session first with `h5i browser proxy <url> --dom-instrument`, then
    /// drive the target through the printed agent-browser line. Each page the
    /// browser loads beacons a report back to the proxy; `scan` reads those
    /// reports. The payload URLs it prints are the ones to visit to pollute.
    Scan {
        /// Base URLs to build payload probes from. The corpus is appended to each.
        #[arg(value_name = "URL")]
        urls: Vec<String>,
        /// Do not drive the browser; only fold reports already beaconed back.
        /// By default `scan` opens each probe in the session's proxied Chromium.
        #[arg(long = "no-drive")]
        no_drive: bool,
        /// Milliseconds to wait after each navigation for the beacon to arrive.
        #[arg(long, value_name = "MS", default_value_t = 700)]
        settle: u64,
    },
    /// Server-side: run a Node target confined in a box and report gadget reachability.
    ///
    /// The target is run once per candidate `Object.prototype` property, each
    /// time polluting only that one (polluting several common names at once
    /// breaks most real targets). A server that does not exit on its own is
    /// stopped after `--timeout` seconds, and whatever it reached by then is
    /// reported.
    Node {
        /// The box the target runs in. `dom node` refuses to run outside one.
        #[arg(long = "box", value_name = "BOX")]
        box_id: Option<String>,
        /// Seconds to let each run go before stopping the target.
        #[arg(long, value_name = "SECS", default_value_t = 20)]
        timeout: u64,
        /// The target command, after `--`. Any launcher works (node, npm, …);
        /// the probe rides in on NODE_OPTIONS.
        #[arg(last = true, value_name = "ARG")]
        argv: Vec<String>,
    },
}

/// One line of the proxy's `dom-report.jsonl`: what the instrument beaconed.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Report {
    pub url: String,
    #[serde(default)]
    pub canary: bool,
    #[serde(default)]
    pub property: Option<String>,
    #[serde(default)]
    pub flows: Vec<Flow>,
    /// Inbound postMessages the page received (Chromium vehicle only). Kept as
    /// opaque values: `scan` only reports how many were logged.
    #[serde(default)]
    pub messages: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Flow {
    pub source: String,
    pub sink: String,
}

/// Turn a report into the hits worth recording: a pollution canary, and each
/// distinct source→sink flow. A report that proves nothing yields nothing.
pub fn hits_from(report: &Report) -> Vec<Hit> {
    let mut hits = Vec::new();
    let origin = origin_of(&report.url);
    let property = report
        .property
        .clone()
        .unwrap_or_else(|| CANARY_PROP.to_string());
    if report.canary {
        hits.push(Hit {
            origin: origin.clone(),
            payload: report.url.clone(),
            source: "url".into(),
            property: property.clone(),
            sink: None,
        });
    }
    for flow in &report.flows {
        hits.push(Hit {
            origin: origin.clone(),
            payload: report.url.clone(),
            source: flow.source.clone(),
            property: property.clone(),
            sink: Some(flow.sink.clone()),
        });
    }
    hits
}

/// Parse the report log, skipping any torn or malformed line.
fn parse_reports(text: &str) -> Vec<Report> {
    text.lines()
        .filter_map(|line| serde_json::from_str(line.trim()).ok())
        .collect()
}

/// Append `payload` as a query parameter, keeping any fragment after it.
fn probe_query(base: &str, payload: &str) -> String {
    let (pre, frag) = match base.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (base, None),
    };
    let sep = if pre.contains('?') { "&" } else { "?" };
    let mut out = format!("{pre}{sep}{payload}");
    if let Some(frag) = frag {
        out.push('#');
        out.push_str(frag);
    }
    out
}

/// Replace the fragment with `payload`.
fn probe_fragment(base: &str, payload: &str) -> String {
    let pre = base.split_once('#').map(|(p, _)| p).unwrap_or(base);
    format!("{pre}#{payload}")
}

/// Every probe URL for one base: each payload as a query parameter, then each
/// as a URL fragment.
pub fn probes(base: &str) -> Vec<String> {
    let mut out = Vec::new();
    for t in PP_PAYLOADS {
        out.push(probe_query(base, &render(t)));
    }
    for t in PP_PAYLOADS {
        out.push(probe_fragment(base, &render(t)));
    }
    out
}

/// One gadget the in-box probe saw a pollution reach.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct GadgetFinding {
    pub gadget: String,
    pub property: String,
    pub sink: String,
    /// The unique sentinel carried into the sink argument. A kernel observer
    /// that records this exact string at the sink confirms the flow by identity.
    pub marker: String,
    #[serde(default)]
    pub detail: String,
}

/// The probe's JSON report. The shim also writes a `run`/`property` field; they
/// are ignored here (serde drops unknown fields), the findings are what matter.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct GadgetReport {
    pub findings: Vec<GadgetFinding>,
}

/// Find the probe's report in a box run's output: the last line that parses as a
/// report, so target chatter before it does not get in the way.
pub fn parse_gadget_report(output: &str) -> Option<GadgetReport> {
    output
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<GadgetReport>(line.trim()).ok())
}

/// Build the finding a server-side gadget writes. `confirmed` is true when a
/// kernel observer saw the marker at the sink; otherwise it is observed only.
pub fn gadget_entry(
    id: &str,
    g: &GadgetFinding,
    confirmed: bool,
    evidence: Vec<String>,
) -> anyhow::Result<finding::Entry> {
    let state = if confirmed {
        "confirmed: marker reached the sink (kernel-observed)"
    } else {
        "observed: pollution reaches the sink option (not kernel-confirmed)"
    };
    let note = format!(
        "Object.prototype.{} reached {} ({}); marker `{}`",
        g.property, g.sink, g.detail, g.marker
    );
    finding::entry(
        id,
        Some(&format!(
            "prototype pollution gadget {}: {} → {}",
            g.gadget, g.property, g.sink
        )),
        Some(state),
        Some(&note),
        evidence,
        None,
    )
}

/// Dispatch a `dom` verb.
pub fn run(
    root: &std::path::Path,
    selector: Option<&str>,
    what: &DomVerb,
    json_out: bool,
) -> anyhow::Result<()> {
    match what {
        DomVerb::Scan {
            urls,
            no_drive,
            settle,
        } => scan(root, selector, urls, *no_drive, *settle, json_out),
        DomVerb::Node {
            box_id,
            timeout,
            argv,
        } => node(root, selector, box_id.as_deref(), *timeout, argv, json_out),
    }
}

fn h5i_bin() -> std::ffi::OsString {
    std::env::var_os("H5I_BIN").unwrap_or_else(|| std::ffi::OsString::from("h5i"))
}

/// Run `h5i box run <box> -- <argv>` and return its combined output.
fn box_run(box_id: &str, argv: &[&str]) -> anyhow::Result<String> {
    let out = std::process::Command::new(h5i_bin())
        .args(["box", "run", box_id, "--"])
        .args(argv)
        .output()
        .map_err(|e| anyhow::anyhow!("could not run h5i box run: {e}"))?;
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(combined)
}

/// The candidate properties probed, one per run. One at a time so a pollution
/// that breaks the target only breaks the run that was testing it.
const GADGET_PROPS: &[&str] = &["shell", "NODE_OPTIONS", "main", "method"];

/// The report file the shim writes inside the box, relative to `/work`.
const REPORT_IN_BOX: &str = "./.h5i-gadget-report.json";

fn node(
    root: &std::path::Path,
    selector: Option<&str>,
    box_id: Option<&str>,
    timeout_secs: u64,
    argv: &[String],
    json_out: bool,
) -> anyhow::Result<()> {
    // Confinement is the whole point: a gadget probe pollutes Object.prototype
    // and runs the target's code, so it only ever runs inside a box.
    let box_id = box_id
        .ok_or_else(|| anyhow::anyhow!("dom node runs a target inside a box: pass --box <id>"))?;
    if argv.is_empty() {
        anyhow::bail!("dom node needs a target command after `--`, e.g. `-- node server.js`");
    }

    // Place the probe in the box's /work (persists between runs), base64 so no
    // shell quoting of the source is needed. Confirm it landed, so a missing
    // box or a box without sh/base64 is a clear error, not a later "no report".
    let encoded =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, GADGET_PROBE_JS);
    let placed = box_run(
        box_id,
        &[
            "sh",
            "-c",
            &format!("echo {encoded} | base64 -d > {PROBE_IN_BOX} && echo h5i-placed"),
        ],
    )?;
    if !placed.contains("h5i-placed") {
        anyhow::bail!(
            "could not place the probe in box {box_id} (needs sh + base64). Output:\n{}",
            placed.trim()
        );
    }

    // One run per property. The probe rides in on NODE_OPTIONS so any launcher
    // (node, npm, ts-node) picks it up; `timeout` stops a server that never
    // exits; `"$@"` passes the target argv without re-quoting. The report is
    // read from a file so a timeout-killed run still yields what it found.
    let mut all: Vec<GadgetFinding> = Vec::new();
    let mut dedup: std::collections::HashSet<(String, String)> = Default::default();
    let mut ran = 0usize;
    for prop in GADGET_PROPS {
        // The target's own stdout/stderr is discarded: the report is read from
        // a file, and a chatty server over the timeout window would otherwise
        // buffer without bound in the parent.
        let script = format!(
            "H5I_GADGET_PROP={prop} H5I_GADGET_REPORT={REPORT_IN_BOX} \
             NODE_OPTIONS=\"--require {PROBE_IN_BOX}\" timeout {timeout_secs} \"$@\" \
             >/dev/null 2>&1; true"
        );
        let mut run_argv: Vec<&str> = vec!["sh", "-c", &script, "h5i-probe"];
        run_argv.extend(argv.iter().map(String::as_str));
        box_run(box_id, &run_argv)?;
        let out = box_run(
            box_id,
            &["sh", "-c", &format!("cat {REPORT_IN_BOX} 2>/dev/null; rm -f {REPORT_IN_BOX}")],
        )?;
        if let Some(report) = parse_gadget_report(&out) {
            ran += 1;
            for g in report.findings {
                if dedup.insert((g.gadget.clone(), g.sink.clone())) {
                    all.push(g);
                }
            }
        }
    }
    if ran == 0 {
        anyhow::bail!(
            "the probe wrote no report on any of {} run(s). The box needs `node` on PATH (an \
             agent image has Node 22) and `sh`/`timeout`/`base64`.",
            GADGET_PROPS.len()
        );
    }

    let session = read::resolve_for_reading(root, selector)?;
    let log = Findings::open(root, &session.id)?;
    let mut recorded = Vec::new();
    for g in &all {
        let id = log.next_id()?;
        // Records "observed"; the eBPF detect lane upgrades to "confirmed" in
        // sub-phase 2b, which is gated on CAP_BPF.
        log.append(&gadget_entry(&id, g, false, Vec::new())?)?;
        recorded.push(id);
    }

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "session": session.id,
                "box": box_id,
                "gadgets": all.len(),
                "findings": recorded,
            }))?
        );
        return Ok(());
    }
    if recorded.is_empty() {
        println!("  no prototype-pollution gadgets reached a sink in box {box_id}");
    } else {
        println!(
            "  recorded {} gadget finding(s) in box {box_id}: {}",
            recorded.len(),
            recorded.join(", ")
        );
    }
    Ok(())
}

fn scan(
    root: &std::path::Path,
    selector: Option<&str>,
    urls: &[String],
    no_drive: bool,
    settle: u64,
    json_out: bool,
) -> anyhow::Result<()> {
    let session = read::resolve_for_reading(root, selector)?;
    let report_path = bs::dir(root, &session.id).join("dom-report.jsonl");
    if !report_path.exists() {
        anyhow::bail!(
            "session {} is not armed for DOM instrumentation. Start one with:\n  \
             h5i browser proxy <url> --dom-instrument\nthen drive the target through \
             the printed agent-browser line.",
            session.id
        );
    }

    let all_probes: Vec<String> = urls.iter().flat_map(|u| probes(u)).collect();
    if !no_drive && !all_probes.is_empty() {
        // Drive the real Chromium the proxy session owns: each open is fetched
        // through h5i, so the injected instrument runs and beacons a report.
        let mut failures = 0usize;
        let mut last_err = None;
        for probe in &all_probes {
            if let Err(e) = crate::ask_browser(&["proxy-open", probe], Some(&session.id)) {
                failures += 1;
                last_err = Some(e);
            }
            std::thread::sleep(std::time::Duration::from_millis(settle));
        }
        // Every open failing is not a clean target; it is a broken drive (no
        // agent-browser, no Chrome, dead session). Say so rather than report
        // "nothing found", which reads as an all-clear.
        if failures == all_probes.len() {
            anyhow::bail!(
                "could not drive the browser for any of {} probe(s); is agent-browser installed \
                 and the proxy session live?{}",
                all_probes.len(),
                last_err.map(|e| format!("\n  last error: {e}")).unwrap_or_default()
            );
        }
    }

    // The browser beacons its reports as it loads each page, which is before
    // `scan` runs, so fold every report. The file is append-only across scans;
    // bound the read so a long engagement (or a hostile flood of beacons) can't
    // make it an unbounded load.
    const MAX_REPORT_BYTES: u64 = 16 * 1024 * 1024;
    if std::fs::metadata(&report_path).map(|m| m.len()).unwrap_or(0) > MAX_REPORT_BYTES {
        anyhow::bail!(
            "{} exceeds {MAX_REPORT_BYTES} bytes; close and reopen the proxy session to reset it",
            report_path.display()
        );
    }
    let text = std::fs::read_to_string(&report_path).unwrap_or_default();
    let reports = parse_reports(&text);
    let total_messages: usize = reports.iter().map(|r| r.messages.len()).sum();

    // Correlate each report to the capture receipt of the navigation that
    // produced it, so a finding cites real evidence. The proxy records every
    // probe load; key by origin+path because a fragment probe's `#…` never
    // reaches the wire (so the stored URL lacks it). Last seq wins = latest load.
    let store = bs::dir(root, &session.id).join(bs::MESSAGES_DIR);
    let mut seq_by_key: std::collections::HashMap<String, u64> = Default::default();
    for seq in h5i_wire::read::sequences(&store) {
        if let Ok(req) = h5i_wire::read::read_json::<h5i_wire::message::StoredRequest>(
            &store.join(format!("{seq}.request.json")),
        ) && let Some(key) = url_key(&req.url)
        {
            seq_by_key.insert(key, seq);
        }
    }
    let evidence_for = |report_url: &str| -> Vec<String> {
        url_key(report_url)
            .and_then(|k| seq_by_key.get(&k))
            .map(|seq| vec![format!("res_{seq}")])
            .unwrap_or_default()
    };

    // Dedup by the finding title (one per origin + source→sink pair): the four
    // payload forms that all pollute `via url` are one vulnerability, and a
    // title already in the log is the same finding again. Seed only from prior
    // DOM findings so an unrelated manual finding can't suppress a scan result.
    let log = Findings::open(root, &session.id)?;
    let mut seen: std::collections::HashSet<String> = log
        .read()?
        .iter()
        .map(|f| f.title.clone())
        .filter(|t| t.starts_with("prototype pollution"))
        .collect();
    let mut recorded: Vec<String> = Vec::new();
    for report in &reports {
        let evidence = evidence_for(&report.url);
        for hit in hits_from(report) {
            if !seen.insert(hit.title()) {
                continue;
            }
            let id = log.next_id()?;
            log.append(&hit_entry(&id, &hit, evidence.clone())?)?;
            recorded.push(id);
        }
    }

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "session": session.id,
                "reports_on_file": reports.len(),
                "post_messages": total_messages,
                "findings": recorded,
                "probes": all_probes,
            }))?
        );
        return Ok(());
    }
    if recorded.is_empty() {
        if no_drive {
            println!(
                "  no new DOM findings on file. Drive these probes through the proxied \
                 browser (or drop --no-drive), then scan again:"
            );
            for probe in all_probes.iter().take(PP_PAYLOADS.len() * 2) {
                println!("    {probe}");
            }
        } else {
            println!(
                "  drove {} probe(s); nothing showed pollution reaching a sink.",
                all_probes.len()
            );
        }
    } else {
        println!(
            "  recorded {} finding(s): {}",
            recorded.len(),
            recorded.join(", ")
        );
    }
    if total_messages > 0 {
        println!("  postMessage: {total_messages} inbound message(s) logged");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_corpus_renders_both_families_in_both_forms() {
        let rendered: Vec<String> = PP_PAYLOADS.iter().map(|t| render(t)).collect();
        assert_eq!(
            rendered,
            vec![
                "__proto__.h5ipp=reserved",
                "__proto__[h5ipp]=reserved",
                "constructor.prototype.h5ipp=reserved",
                "constructor[prototype][h5ipp]=reserved",
            ]
        );
    }

    #[test]
    fn a_hit_with_a_sink_names_the_flow() {
        let hit = Hit {
            origin: "https://app.example".into(),
            payload: "__proto__[h5ipp]=reserved".into(),
            source: "hash".into(),
            property: "h5ipp".into(),
            sink: Some("innerHTML".into()),
        };
        let entry = hit_entry("finding_1", &hit, vec!["res_7".into()]).unwrap();
        assert_eq!(entry.id, "finding_1");
        assert_eq!(
            entry.title.as_deref(),
            Some("prototype pollution: hash → innerHTML (https://app.example)")
        );
        assert_eq!(entry.evidence, vec!["res_7".to_string()]);
        let note = entry.note.unwrap();
        assert!(note.contains("Object.prototype.h5ipp"));
        assert!(note.contains("reached sink innerHTML"));
    }

    #[test]
    fn a_hit_without_a_sink_is_pollution_only() {
        let hit = Hit {
            origin: "https://app.example".into(),
            payload: "__proto__.h5ipp=reserved".into(),
            source: "url".into(),
            property: "h5ipp".into(),
            sink: None,
        };
        let entry = hit_entry("finding_2", &hit, vec![]).unwrap();
        assert_eq!(
            entry.title.as_deref(),
            Some("prototype pollution via url (https://app.example)")
        );
        assert!(entry.note.unwrap().contains("Object.prototype.h5ipp"));
    }

    #[test]
    fn probes_cover_the_query_and_fragment_corpora() {
        let p = probes("https://t.test/app?a=1");
        assert_eq!(p.len(), PP_PAYLOADS.len() * 2);
        assert!(p.contains(&"https://t.test/app?a=1&__proto__[h5ipp]=reserved".to_string()));
        assert!(p.contains(&"https://t.test/app?a=1#__proto__[h5ipp]=reserved".to_string()));
    }

    #[test]
    fn a_query_probe_keeps_an_existing_fragment_after_the_param() {
        assert_eq!(
            probe_query("https://t.test/#tab", "__proto__.h5ipp=reserved"),
            "https://t.test/?__proto__.h5ipp=reserved#tab"
        );
    }

    #[test]
    fn a_report_yields_a_canary_hit_and_one_hit_per_flow() {
        let line = r#"{"url":"https://t.test/?x","canary":true,"property":"h5ipp","flows":[{"source":"hash","sink":"innerHTML"}]}"#;
        let reports = parse_reports(&format!("{line}\nnot json\n"));
        assert_eq!(reports.len(), 1);
        let hits = hits_from(&reports[0]);
        assert_eq!(hits.len(), 2);
        assert!(hits[0].sink.is_none()); // the canary
        assert_eq!(hits[1].sink.as_deref(), Some("innerHTML"));
    }

    #[test]
    fn the_origin_distinguishes_two_targets_but_not_two_payload_forms() {
        let a = Report { url: "https://app.one/x?__proto__[h5ipp]=reserved".into(), canary: true, property: None, flows: vec![], messages: vec![] };
        let b = Report { url: "https://app.one/x#constructor[prototype][h5ipp]=reserved".into(), canary: true, property: None, flows: vec![], messages: vec![] };
        let c = Report { url: "https://app.two/y?__proto__[h5ipp]=reserved".into(), canary: true, property: None, flows: vec![], messages: vec![] };
        // Same origin, different payload form → same title (collapses).
        assert_eq!(hits_from(&a)[0].title(), hits_from(&b)[0].title());
        // Different origin → different title (kept apart).
        assert_ne!(hits_from(&a)[0].title(), hits_from(&c)[0].title());
        assert_eq!(origin_of("https://app.one/x?q=1#f"), "https://app.one");
    }

    #[test]
    fn url_key_drops_query_and_fragment_so_query_and_fragment_probes_share_it() {
        let q = url_key("https://app.one/app?__proto__[h5ipp]=reserved").unwrap();
        let f = url_key("https://app.one/app#__proto__[h5ipp]=reserved").unwrap();
        assert_eq!(q, "https://app.one/app");
        assert_eq!(q, f);
    }

    #[test]
    fn a_report_that_proves_nothing_yields_no_hits() {
        let report = Report {
            url: "https://t.test/".into(),
            canary: false,
            property: None,
            flows: vec![],
            messages: vec![],
        };
        assert!(hits_from(&report).is_empty());
    }

    #[test]
    fn the_gadget_report_is_the_last_json_line_amid_target_chatter() {
        let output = "starting server\nlistening on 3000\n\
            {\"run\":\"ab12\",\"findings\":[{\"gadget\":\"shell\",\"property\":\"shell\",\"sink\":\"child_process.execSync\",\"marker\":\"h5iGADGET_shell_ab12\",\"detail\":\"inherited options.shell\"}]}\n";
        let report = parse_gadget_report(output).unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].sink, "child_process.execSync");
    }

    #[test]
    fn a_gadget_entry_distinguishes_observed_from_confirmed() {
        let g = GadgetFinding {
            gadget: "NODE_OPTIONS".into(),
            property: "env".into(),
            sink: "child_process.execSync".into(),
            marker: "h5iGADGET_env_ab12".into(),
            detail: "inherited env.NODE_OPTIONS".into(),
        };
        let observed = gadget_entry("finding_1", &g, false, vec![]).unwrap();
        assert_eq!(
            observed.title.as_deref(),
            Some("prototype pollution gadget NODE_OPTIONS: env → child_process.execSync")
        );
        assert!(observed.state.unwrap().starts_with("observed"));
        let confirmed = gadget_entry("finding_1", &g, true, vec![]).unwrap();
        assert!(confirmed.state.unwrap().starts_with("confirmed"));
        assert!(observed.note.unwrap().contains("h5iGADGET_env_ab12"));
    }
}
