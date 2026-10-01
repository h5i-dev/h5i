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

/// The prototype-pollution corpus, as query-parameter fragments.
///
/// The two families ppfuzz calls "object" (`__proto__`) and "pointer"
/// (`constructor.prototype`), each in dot and bracket form. `{p}` is the canary
/// property, `{v}` its value.
pub const QUERY_PAYLOADS: &[&str] = &[
    "__proto__.{p}={v}",
    "__proto__[{p}]={v}",
    "constructor.prototype.{p}={v}",
    "constructor[prototype][{p}]={v}",
];

/// The same families as URL fragments. Fragments never reach the server, so the
/// proxy cannot see these: the navigation surface seeds them into the URL.
pub const FRAGMENT_PAYLOADS: &[&str] = &[
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
    /// The rendered payload that polluted, e.g. `__proto__[h5ipp]=reserved`.
    pub payload: String,
    /// The source the tainted value came from: `url`, `hash`, `name`, …
    pub source: String,
    /// The property left on `Object.prototype`.
    pub property: String,
    /// The sink the value reached, if a source→sink flow was seen.
    pub sink: Option<String>,
    /// A per-library DOM-XSS gadget that matched the polluted page, if any.
    pub gadget: Option<String>,
}

impl Hit {
    /// One line naming the finding.
    fn title(&self) -> String {
        match &self.sink {
            Some(sink) => format!("prototype pollution: {} → {sink}", self.source),
            None => format!("prototype pollution via {}", self.source),
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
        if let Some(gadget) = &self.gadget {
            note.push_str(&format!("; library gadget: {gadget}"));
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
        /// Also navigate the session to each probe. For a proxied agent-browser
        /// this seeds the pollution; for the bare engine it does nothing useful.
        #[arg(long)]
        navigate: bool,
        /// Milliseconds to wait after each navigation for the beacon to arrive.
        #[arg(long, value_name = "MS", default_value_t = 500)]
        settle: u64,
    },
    /// Server-side: run a Node target confined in a box and report gadget reachability.
    Node {
        /// The box the target runs in. `dom node` refuses to run outside one.
        #[arg(long = "box", value_name = "BOX")]
        box_id: Option<String>,
        /// The target command, after `--`.
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
    let property = report
        .property
        .clone()
        .unwrap_or_else(|| CANARY_PROP.to_string());
    if report.canary {
        hits.push(Hit {
            payload: report.url.clone(),
            source: "url".into(),
            property: property.clone(),
            sink: None,
            gadget: None,
        });
    }
    for flow in &report.flows {
        hits.push(Hit {
            payload: report.url.clone(),
            source: flow.source.clone(),
            property: property.clone(),
            sink: Some(flow.sink.clone()),
            gadget: None,
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

/// Every probe URL for one base: the query corpus, then the fragment corpus.
pub fn probes(base: &str) -> Vec<String> {
    let mut out = Vec::new();
    for t in QUERY_PAYLOADS {
        out.push(probe_query(base, &render(t)));
    }
    for t in FRAGMENT_PAYLOADS {
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

/// The probe's JSON report.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct GadgetReport {
    #[serde(default)]
    pub run: String,
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
            navigate,
            settle,
        } => scan(root, selector, urls, *navigate, *settle, json_out),
        DomVerb::Node { box_id, argv } => node(root, selector, box_id.as_deref(), argv, json_out),
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

fn node(
    root: &std::path::Path,
    selector: Option<&str>,
    box_id: Option<&str>,
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
    // shell quoting of the source is needed, then load it ahead of the target.
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, GADGET_PROBE_JS);
    let place = format!("echo {encoded} | base64 -d > {PROBE_IN_BOX}");
    box_run(box_id, &["sh", "-c", &place])?;

    // The target is `node --require <probe> <its args>`. A leading `node` in the
    // user's argv is theirs to keep; we always prepend the probe require.
    let mut target: Vec<&str> = argv.iter().map(String::as_str).collect();
    let first_is_node = target.first().is_some_and(|a| *a == "node" || a.ends_with("/node"));
    if first_is_node {
        target.remove(0);
    }
    let mut run_argv: Vec<&str> = vec!["node", "--require", PROBE_IN_BOX];
    run_argv.extend(target);
    let output = box_run(box_id, &run_argv)?;

    let report = parse_gadget_report(&output).ok_or_else(|| {
        anyhow::anyhow!(
            "the probe wrote no report. The box needs `node` on PATH (an agent image has \
             Node 22). Output was:\n{}",
            output.trim()
        )
    })?;

    let session = read::resolve_for_reading(root, selector)?;
    let log = Findings::open(root, &session.id)?;
    let mut recorded = Vec::new();
    for g in &report.findings {
        let id = log.next_id()?;
        // Phase 2 records "observed"; the eBPF detect lane upgrades to
        // "confirmed" in 2b, which is gated on CAP_BPF.
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
                "probe_run": report.run,
                "gadgets": report.findings.len(),
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
    navigate: bool,
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

    // Only fold reports that arrive from here on, so a re-scan does not re-raise
    // what a previous one already recorded.
    let seen_before = std::fs::read_to_string(&report_path)
        .map(|t| t.lines().count())
        .unwrap_or(0);

    let all_probes: Vec<String> = urls.iter().flat_map(|u| probes(u)).collect();
    if navigate {
        for probe in &all_probes {
            let _ = crate::ask_browser(&["navigate", probe], Some(&session.id));
            std::thread::sleep(std::time::Duration::from_millis(settle));
        }
    }

    let text = std::fs::read_to_string(&report_path).unwrap_or_default();
    let fresh: Vec<Report> = parse_reports(&text).into_iter().skip(seen_before).collect();

    let log = Findings::open(root, &session.id)?;
    let mut recorded: Vec<String> = Vec::new();
    let mut dedup: std::collections::BTreeSet<(String, Option<String>)> = Default::default();
    for report in &fresh {
        for hit in hits_from(report) {
            let key = (hit.source.clone(), hit.sink.clone());
            if !dedup.insert(key) {
                continue;
            }
            let id = log.next_id()?;
            log.append(&hit_entry(&id, &hit, Vec::new())?)?;
            recorded.push(id);
        }
    }

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "session": session.id,
                "reports_read": fresh.len(),
                "findings": recorded,
                "probes": all_probes,
            }))?
        );
        return Ok(());
    }
    if recorded.is_empty() {
        println!(
            "  no new DOM findings from {} report(s). Drive these probes through the \
             proxied browser, then scan again:",
            fresh.len()
        );
        for probe in all_probes.iter().take(QUERY_PAYLOADS.len() + FRAGMENT_PAYLOADS.len()) {
            println!("    {probe}");
        }
    } else {
        println!(
            "  recorded {} finding(s) from {} report(s): {}",
            recorded.len(),
            fresh.len(),
            recorded.join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_corpus_renders_both_families_in_both_forms() {
        let rendered: Vec<String> = QUERY_PAYLOADS.iter().map(|t| render(t)).collect();
        assert_eq!(
            rendered,
            vec![
                "__proto__.h5ipp=reserved",
                "__proto__[h5ipp]=reserved",
                "constructor.prototype.h5ipp=reserved",
                "constructor[prototype][h5ipp]=reserved",
            ]
        );
        // The fragment family carries the same shapes; the navigation surface
        // puts them after the `#`, where the proxy never sees them.
        assert_eq!(FRAGMENT_PAYLOADS.len(), QUERY_PAYLOADS.len());
        assert_eq!(render(FRAGMENT_PAYLOADS[1]), "__proto__[h5ipp]=reserved");
    }

    #[test]
    fn a_hit_with_a_sink_names_the_flow() {
        let hit = Hit {
            payload: "__proto__[h5ipp]=reserved".into(),
            source: "hash".into(),
            property: "h5ipp".into(),
            sink: Some("innerHTML".into()),
            gadget: None,
        };
        let entry = hit_entry("finding_1", &hit, vec!["res_7".into()]).unwrap();
        assert_eq!(entry.id, "finding_1");
        assert_eq!(entry.title.as_deref(), Some("prototype pollution: hash → innerHTML"));
        assert_eq!(entry.evidence, vec!["res_7".to_string()]);
        let note = entry.note.unwrap();
        assert!(note.contains("Object.prototype.h5ipp"));
        assert!(note.contains("reached sink innerHTML"));
    }

    #[test]
    fn a_hit_without_a_sink_is_pollution_only() {
        let hit = Hit {
            payload: "__proto__.h5ipp=reserved".into(),
            source: "url".into(),
            property: "h5ipp".into(),
            sink: None,
            gadget: Some("jquery: __proto__[url]=data:,alert(1)//".into()),
        };
        let entry = hit_entry("finding_2", &hit, vec![]).unwrap();
        assert_eq!(entry.title.as_deref(), Some("prototype pollution via url"));
        assert!(entry.note.unwrap().contains("library gadget: jquery"));
    }

    #[test]
    fn probes_cover_the_query_and_fragment_corpora() {
        let p = probes("https://t.test/app?a=1");
        assert_eq!(p.len(), QUERY_PAYLOADS.len() + FRAGMENT_PAYLOADS.len());
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
    fn a_report_that_proves_nothing_yields_no_hits() {
        let report = Report {
            url: "https://t.test/".into(),
            canary: false,
            property: None,
            flows: vec![],
        };
        assert!(hits_from(&report).is_empty());
    }

    #[test]
    fn the_gadget_report_is_the_last_json_line_amid_target_chatter() {
        let output = "starting server\nlistening on 3000\n\
            {\"run\":\"ab12\",\"findings\":[{\"gadget\":\"shell\",\"property\":\"shell\",\"sink\":\"child_process.execSync\",\"marker\":\"h5iGADGET_shell_ab12\",\"detail\":\"inherited options.shell\"}]}\n";
        let report = parse_gadget_report(output).unwrap();
        assert_eq!(report.run, "ab12");
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
