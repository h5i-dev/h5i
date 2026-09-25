//! Rendering a report to HTML, for the console and the PDF alike.
//!
//! The Markdown is the agent's; the `{{...}}` directives are h5i's, resolved
//! against the project so the numbers in the prose match the data. Raw HTML in
//! the source is escaped, and only `http(s)` and `mailto` links survive, so a
//! report built from a target's own strings cannot carry script into the page
//! that shows it.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd, html};

use super::report::{Context, Directive, directives};
use super::{Project, checklist, evidence, finding, glossary};

/// What the finished page needs to know about the issue.
#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub version: Option<u32>,
    pub issued: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TocEntry {
    pub level: u8,
    pub id: String,
    pub title: String,
}

/// The report content, plus what a surrounding page needs to build chrome.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Rendered {
    /// The report body as HTML: directives resolved, headings given ids.
    pub body: String,
    pub toc: Vec<TocEntry>,
    /// The glossary terms the report used, in first-seen order.
    pub terms: Vec<glossary::Term>,
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn slugify(text: &str) -> String {
    let mut s: String = text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    s.trim_matches('-').chars().take(64).collect::<String>()
}

/// Render, resolving directives and collecting a table of contents.
pub fn render(_project: &Project, source: &str, ctx: &Context, _meta: &Meta) -> Rendered {
    // Directives are replaced with tokens the parser will not touch, the body
    // is rendered, then each token becomes its block. Doing it after parse
    // keeps a finding block's own HTML out of the Markdown grammar.
    let ds = directives(source);
    let mut used_terms: Vec<String> = Vec::new();
    let mut prepared = source.to_string();
    for (i, d) in ds.iter().enumerate() {
        if d.kind == "term"
            && let Some(t) = ctx.glossary.lookup(&d.arg)
            && !used_terms.contains(&t.id)
        {
            used_terms.push(t.id.clone());
        }
        prepared = prepared.replacen(&d.raw, &token(i), 1);
    }

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);

    let mut events: Vec<Event> = Parser::new_ext(&prepared, options).collect();
    let mut toc: Vec<TocEntry> = Vec::new();
    assign_heading_ids(&mut events, &mut toc);
    let events = events.into_iter().map(sanitize);

    let mut body = String::new();
    html::push_html(&mut body, events);

    // Swap each directive token for its rendered block. The glossary is the
    // one directive that needs the whole used-term set, so it is filled here
    // rather than from a single directive's argument.
    for (i, d) in ds.iter().enumerate() {
        let rendered = if d.kind == "glossary" {
            glossary_block(ctx, &used_terms)
        } else {
            render_directive(d, ctx)
        };
        let tok = token(i);
        // A block directive on its own line became its own paragraph.
        body = body.replace(&format!("<p>{tok}</p>"), &rendered);
        body = body.replace(&tok, &rendered);
    }

    let terms = used_terms.iter().filter_map(|id| ctx.glossary.lookup(id).cloned()).collect();
    Rendered { body, toc, terms }
}

fn token(i: usize) -> String {
    format!("H5IDIRECTIVE{i}ENDH5I")
}

/// Escape raw HTML and drop links that are not `http(s)` or `mailto`.
fn sanitize(event: Event) -> Event {
    match event {
        // An HTML comment is the template's own scaffolding (and an author's
        // note-to-self); drop it rather than escape it into the page.
        Event::Html(s) | Event::InlineHtml(s) if s.trim_start().starts_with("<!--") => Event::Text("".into()),
        Event::Html(s) => Event::Text(s),
        Event::InlineHtml(s) => Event::Text(s),
        Event::Start(Tag::Link { link_type, dest_url, title, id }) => {
            let safe = link_ok(&dest_url);
            Event::Start(Tag::Link {
                link_type,
                dest_url: if safe { dest_url } else { "#".into() },
                title,
                id,
            })
        }
        Event::Start(Tag::Image { link_type, dest_url, title, id }) => {
            // No off-page image loads from report prose: an <img src> is a
            // request the viewer's browser would make to a URL the target
            // controls. Evidence images come through the evidence directive.
            let safe = dest_url.starts_with("data:image/") || dest_url.starts_with('#');
            Event::Start(Tag::Image {
                link_type,
                dest_url: if safe { dest_url } else { "#".into() },
                title,
                id,
            })
        }
        other => other,
    }
}

fn link_ok(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("mailto:")
        || url.starts_with('#')
}

fn assign_heading_ids(events: &mut [Event], toc: &mut Vec<TocEntry>) {
    let mut i = 0;
    while i < events.len() {
        if let Event::Start(Tag::Heading { level, .. }) = &events[i] {
            let level = *level;
            // Gather the heading's text.
            let mut title = String::new();
            let mut j = i + 1;
            while j < events.len() && !matches!(events[j], Event::End(TagEnd::Heading(_))) {
                if let Event::Text(t) | Event::Code(t) = &events[j] {
                    title.push_str(t);
                }
                j += 1;
            }
            let mut id = slugify(&title);
            if id.is_empty() {
                id = format!("section-{}", toc.len() + 1);
            }
            while toc.iter().any(|e| e.id == id) {
                id = format!("{id}-{}", toc.len() + 1);
            }
            if let Event::Start(Tag::Heading { id: slot, .. }) = &mut events[i] {
                *slot = Some(id.clone().into());
            }
            let n = match level {
                HeadingLevel::H1 => 1,
                HeadingLevel::H2 => 2,
                HeadingLevel::H3 => 3,
                _ => 4,
            };
            if n <= 3 && !title.trim().is_empty() {
                toc.push(TocEntry { level: n, id, title });
            }
            i = j;
        }
        i += 1;
    }
}

fn severity_chip(sev: &str) -> String {
    let label = if sev.is_empty() { "unrated" } else { sev };
    format!("<span class=\"sev sev-{}\">{}</span>", escape(if sev.is_empty() { "unrated" } else { sev }), escape(label))
}

fn render_directive(d: &Directive, ctx: &Context) -> String {
    match d.kind.as_str() {
        "finding" => match ctx.finding(&d.arg) {
            Some(f) => finding_block(f, ctx),
            None => missing(&d.raw),
        },
        "findings" => findings_table(&finding::sorted(ctx.findings.clone())),
        "evidence" => match ctx.evidence(&d.arg) {
            Some(e) => evidence_block(e),
            None => missing(&d.raw),
        },
        "checklist" => checklist_block(ctx, &d.arg),
        "coverage" => coverage_block(ctx, &d.arg),
        "term" => match ctx.glossary.lookup(&d.arg) {
            Some(t) => term_chip(t, &d.arg),
            None => escape(&d.arg),
        },
        // Filled by `render`, which holds the whole used-term set.
        "glossary" => glossary_block(ctx, &[]),
        _ => missing(&d.raw),
    }
}

fn missing(raw: &str) -> String {
    format!("<span class=\"missing\">[{}: not in this project]</span>", escape(raw))
}

fn finding_block(f: &finding::Finding, ctx: &Context) -> String {
    let mut out = format!(
        "<section class=\"finding\" id=\"{id}\"><div class=\"finding-head\"><span class=\"fid\">{id}</span>\
         <h3>{title}</h3>{sev}</div>",
        id = escape(&f.id),
        title = escape(&f.title),
        sev = severity_chip(&f.severity),
    );
    out.push_str("<dl class=\"finding-meta\">");
    let row = |label: &str, value: &str| {
        if value.trim().is_empty() {
            String::new()
        } else {
            format!("<dt>{}</dt><dd>{}</dd>", escape(label), escape(value))
        }
    };
    let status = if f.status.is_empty() { "open" } else { &f.status };
    out.push_str(&row("Status", status));
    if !f.state.is_empty() {
        out.push_str(&row("Confidence", &f.state));
    }
    out.push_str(&row("Affected", &f.affected));
    out.push_str(&row("Severity basis", &f.severity_reason));
    out.push_str(&row("Owner", &f.owner));
    out.push_str(&row("Due", &f.due));
    out.push_str("</dl>");

    let para = |label: &str, value: &str| {
        if value.trim().is_empty() {
            String::new()
        } else {
            format!("<div class=\"finding-part\"><h4>{}</h4><p>{}</p></div>", escape(label), escape(value))
        }
    };
    out.push_str(&para("What it means", &f.summary));
    out.push_str(&para("Impact", &f.impact));
    out.push_str(&para("Remediation", &f.remediation));

    if !f.body.trim().is_empty() {
        // The free body is Markdown too; render it with the same rules.
        let inner = render(&dummy_project(), &f.body, ctx, &Meta::default());
        out.push_str(&format!("<div class=\"finding-body\">{}</div>", inner.body));
    }

    if !f.evidence.is_empty() {
        out.push_str("<div class=\"finding-part\"><h4>Evidence</h4><ul class=\"evidence-refs\">");
        for id in &f.evidence {
            match ctx.evidence(id) {
                Some(e) => out.push_str(&format!(
                    "<li><a href=\"#{id}\">{id}</a>{cap}</li>",
                    id = escape(id),
                    cap = if e.caption.is_empty() { String::new() } else { format!(" — {}", escape(&e.caption)) }
                )),
                None => out.push_str(&format!("<li>{}</li>", escape(id))),
            }
        }
        out.push_str("</ul></div>");
    }
    if !f.notes.is_empty() {
        out.push_str("<details class=\"finding-notes\"><summary>Assessor notes</summary><ul>");
        for n in &f.notes {
            out.push_str(&format!("<li><time>{}</time> {}</li>", escape(&n.at), escape(&n.text)));
        }
        out.push_str("</ul></details>");
    }
    out.push_str("</section>");
    out
}

fn findings_table(findings: &[finding::Finding]) -> String {
    if findings.is_empty() {
        return "<p class=\"muted\">No findings recorded.</p>".into();
    }
    let mut out = String::from(
        "<table class=\"findings-table\"><thead><tr><th>ID</th><th>Finding</th><th>Severity</th><th>Status</th></tr></thead><tbody>",
    );
    for f in findings {
        out.push_str(&format!(
            "<tr><td><a href=\"#{id}\">{id}</a></td><td>{title}</td><td>{sev}</td><td>{status}</td></tr>",
            id = escape(&f.id),
            title = escape(&f.title),
            sev = severity_chip(&f.severity),
            status = escape(if f.status.is_empty() { "open" } else { &f.status }),
        ));
    }
    out.push_str("</tbody></table>");
    out
}

fn evidence_block(e: &evidence::Record) -> String {
    let mut out = format!("<figure class=\"evidence\" id=\"{}\">", escape(&e.id));
    let body = match e.kind {
        evidence::Kind::Http => format!("<pre class=\"http\">{}</pre>", escape(&evidence::http_text(e))),
        evidence::Kind::Text => format!("<pre>{}</pre>", escape(e.text.as_deref().unwrap_or(""))),
        evidence::Kind::File => match &e.file {
            // The console rewrites this src to its API; the PDF path inlines it.
            Some(name) => format!("<img alt=\"{}\" src=\"evidence/{}\">", escape(&e.caption), escape(name)),
            None => String::new(),
        },
    };
    out.push_str(&body);
    let mut cap = format!("<b>{}</b>", escape(&e.id));
    if !e.caption.is_empty() {
        cap.push_str(&format!(" {}", escape(&e.caption)));
    }
    if e.removed > 0 {
        cap.push_str(&format!(" <span class=\"muted\">({} credential value(s) removed)</span>", e.removed));
    }
    out.push_str(&format!("<figcaption>{cap}</figcaption></figure>"));
    out
}

fn checklist_block(ctx: &Context, slug: &str) -> String {
    let Some((c, outcomes)) = ctx.checklist(slug) else {
        return missing(&format!("{{{{checklist {slug}}}}}"));
    };
    let mut out = format!("<table class=\"checklist\"><caption>{}</caption><thead><tr><th>Item</th><th>Outcome</th><th>Note</th></tr></thead><tbody>", escape(&c.title));
    for o in outcomes {
        let note = o.notes.last().map(|(_, t)| t.as_str()).unwrap_or("");
        out.push_str(&format!(
            "<tr><td>{item}</td><td class=\"st st-{stc}\">{st}</td><td>{note}</td></tr>",
            item = escape(&o.item.text),
            stc = escape(&o.status),
            st = escape(&o.status),
            note = escape(note),
        ));
    }
    out.push_str("</tbody></table>");
    out
}

fn coverage_block(ctx: &Context, slug: &str) -> String {
    let lists: Vec<&(checklist::Checklist, Vec<checklist::Outcome>)> = if slug.is_empty() {
        ctx.checklists.iter().collect()
    } else {
        ctx.checklist(slug).into_iter().collect()
    };
    if lists.is_empty() {
        return "<p class=\"muted\">No checklist was imported, so coverage is not tracked. Findings below are what was looked at, not a plan.</p>".into();
    }
    let mut out = String::from("<div class=\"coverage\">");
    for (c, outcomes) in lists {
        let cov = checklist::coverage(c, outcomes);
        out.push_str(&format!("<p><b>{}</b>: {}.</p>", escape(&c.title), escape(&cov.sentence())));
    }
    out.push_str("</div>");
    out
}

fn term_chip(t: &glossary::Term, shown: &str) -> String {
    let label = if shown.is_empty() { &t.term } else { shown };
    // The console upgrades this to a button that opens a panel; on its own the
    // title attribute is the short definition, and the class carries the id.
    format!(
        "<span class=\"term\" data-term=\"{id}\" title=\"{short}\">{label}</span>",
        id = escape(&t.id),
        short = escape(&t.short),
        label = escape(label),
    )
}

/// The glossary appendix: only the terms the report used, each with its
/// plain-language definition and, when one exists, a link to where a reader
/// can learn more.
fn glossary_block(ctx: &Context, used: &[String]) -> String {
    if used.is_empty() {
        return String::new();
    }
    let mut out = String::from("<dl class=\"glossary\">");
    for t in used.iter().filter_map(|id| ctx.glossary.lookup(id)) {
        let link = t
            .reference()
            .map(|(url, label)| format!(" <a class=\"ext\" href=\"{}\">{}</a>", escape(url), escape(&label)))
            .unwrap_or_default();
        out.push_str(&format!(
            "<dt id=\"term-{id}\">{term}</dt><dd>{explain}{link}</dd>",
            id = escape(&t.id),
            term = escape(&t.term),
            explain = escape(&t.explain),
        ));
    }
    out.push_str("</dl>");
    out
}

fn dummy_project() -> Project {
    Project {
        meta: super::Meta { name: "_".into(), title: String::new(), description: String::new(), targets: vec![], created: String::new() },
        dir: std::path::PathBuf::new(),
    }
}

/// A full, standalone HTML page: cover, table of contents, the body, and the
/// glossary appendix. Print CSS makes it a report rather than a screenshot.
pub fn to_html(project: &Project, source: &str, ctx: &Context, meta: &Meta) -> String {
    let rendered = render(project, source, ctx, meta);
    let title = if project.meta.title.is_empty() { &project.meta.name } else { &project.meta.title };

    let toc = if rendered.toc.is_empty() {
        String::new()
    } else {
        let items = rendered
            .toc
            .iter()
            .map(|e| format!("<li class=\"lvl{}\"><a href=\"#{}\">{}</a></li>", e.level, escape(&e.id), escape(&e.title)))
            .collect::<String>();
        format!("<nav class=\"toc\"><h2>Contents</h2><ol>{items}</ol></nav>")
    };

    let body = &rendered.body;

    let issued = meta
        .version
        .map(|v| {
            let when = meta.issued.as_deref().unwrap_or("");
            format!("<p class=\"cover-meta\">Version {v}{}</p>", if when.is_empty() { String::new() } else { format!(" · issued {}", escape(when)) })
        })
        .unwrap_or_else(|| "<p class=\"cover-meta\">Draft — not yet issued</p>".into());

    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title}</title><style>{css}</style></head><body>\
         <header class=\"cover\"><h1>{title}</h1>{issued}</header>{toc}\
         <main class=\"report\">{body}</main></body></html>",
        title = escape(title),
        css = PRINT_CSS,
    )
}

/// The print stylesheet. Kept here so the console's inline view and the PDF are
/// the same document.
pub const PRINT_CSS: &str = r#"
:root { --ink:#1a1c22; --muted:#6b7280; --line:#e5e7eb; --accent:#3b4a6b;
  --crit:#7f1d1d; --high:#b91c1c; --med:#b45309; --low:#3f6212; --info:#374151; --unrated:#6b7280; }
* { box-sizing: border-box; }
body { font: 15px/1.6 -apple-system, "Segoe UI", Roboto, sans-serif; color: var(--ink);
  max-width: 46rem; margin: 0 auto; padding: 2rem 1rem 4rem; }
h1 { font-size: 2rem; } h2 { font-size: 1.4rem; margin-top: 2.2rem; border-bottom: 2px solid var(--line); padding-bottom: .3rem; }
h3 { font-size: 1.15rem; } h4 { font-size: .95rem; margin: .8rem 0 .2rem; color: var(--muted); text-transform: uppercase; letter-spacing: .04em; }
a { color: var(--accent); } a.ext { font-size: .85em; }
code, pre { font-family: "SF Mono", "Cascadia Code", Consolas, monospace; }
pre { background: #f6f7f9; border: 1px solid var(--line); border-radius: 6px; padding: .8rem; overflow-wrap: anywhere; white-space: pre-wrap; font-size: .82rem; }
table { border-collapse: collapse; width: 100%; margin: 1rem 0; font-size: .9rem; }
th, td { border: 1px solid var(--line); padding: .4rem .6rem; text-align: left; vertical-align: top; }
th { background: #f6f7f9; }
caption { text-align: left; font-weight: 600; margin-bottom: .3rem; }
.cover { border-bottom: 3px solid var(--accent); margin-bottom: 1.5rem; padding-bottom: 1rem; }
.cover-meta { color: var(--muted); }
.toc { background: #f9fafb; border: 1px solid var(--line); border-radius: 8px; padding: 1rem 1.4rem; margin: 1.5rem 0; }
.toc ol { list-style: none; padding-left: 0; margin: 0; } .toc li.lvl3 { padding-left: 1.2rem; font-size: .9rem; } .toc li.lvl2 { margin-top: .2rem; }
.finding { border: 1px solid var(--line); border-left: 4px solid var(--accent); border-radius: 8px; padding: 1rem 1.2rem; margin: 1.2rem 0; }
.finding-head { display: flex; align-items: center; gap: .6rem; flex-wrap: wrap; } .finding-head h3 { margin: 0; flex: 1; }
.fid { font-family: monospace; color: var(--muted); }
.finding-meta { display: grid; grid-template-columns: max-content 1fr; gap: .1rem .8rem; margin: .6rem 0; font-size: .88rem; }
.finding-meta dt { color: var(--muted); } .finding-meta dd { margin: 0; }
.evidence-refs { margin: .2rem 0; }
.sev { font-size: .75rem; font-weight: 700; text-transform: uppercase; letter-spacing: .04em; color: #fff; padding: .1rem .5rem; border-radius: 4px; }
.sev-critical { background: var(--crit); } .sev-high { background: var(--high); } .sev-medium { background: var(--med); } .sev-low { background: var(--low); } .sev-info, .sev-unrated { background: var(--info); }
.st { font-weight: 600; } .st-blocked { color: var(--med); } .st-open, .st-in-progress { color: var(--muted); } .st-recorded { color: var(--low); } .st-not-applicable { color: var(--muted); font-style: italic; }
.evidence { margin: 1rem 0; } .evidence img { max-width: 100%; border: 1px solid var(--line); border-radius: 6px; } figcaption { font-size: .85rem; color: var(--ink); margin-top: .3rem; }
.term { border-bottom: 1px dotted var(--accent); cursor: help; }
.muted, .missing { color: var(--muted); } .missing { font-style: italic; }
.glossary dt { font-weight: 600; margin-top: .6rem; } .glossary dd { margin: 0 0 .2rem; color: #33363d; }
@media print {
  body { max-width: none; padding: 0; font-size: 11pt; }
  .cover { page-break-after: always; } .toc { page-break-after: always; }
  h2 { page-break-after: avoid; } .finding, table, figure { page-break-inside: avoid; }
  a { color: var(--ink); text-decoration: none; } a[href^="http"]::after { content: " (" attr(href) ")"; font-size: .8em; color: var(--muted); word-break: break-all; }
  .term { border: none; }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::finding::Change;

    fn ctx(p: &Project) -> Context {
        Context::load(p).unwrap()
    }

    #[test]
    fn raw_html_and_bad_links_are_neutralised() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::init(tmp.path(), "p", "", "", vec![]).unwrap();
        let src = "hi <script>alert(1)</script> [x](javascript:alert(1)) [ok](https://a.test)";
        let r = render(&p, src, &ctx(&p), &Meta::default());
        assert!(!r.body.contains("<script>"), "{}", r.body);
        assert!(r.body.contains("&lt;script&gt;"));
        assert!(!r.body.contains("javascript:"));
        assert!(r.body.contains("https://a.test"));
    }

    #[test]
    fn a_finding_directive_renders_its_block_and_the_table_lists_it() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::init(tmp.path(), "p", "", "", vec![]).unwrap();
        finding::create(
            &p,
            Change {
                title: Some("cross-tenant read".into()),
                severity: Some("high".into()),
                summary: Some("another customer's invoice was readable".into()),
                ..Change::default()
            },
        )
        .unwrap();
        let r = render(&p, "## Detail\n{{finding F-1}}\n\n{{findings}}", &ctx(&p), &Meta::default());
        assert!(r.body.contains("cross-tenant read"));
        assert!(r.body.contains("sev-high"));
        assert!(r.body.contains("findings-table"));
        assert!(r.toc.iter().any(|e| e.title == "Detail" && e.id == "detail"));
    }

    #[test]
    fn a_term_is_marked_and_the_appendix_defines_only_used_terms() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::init(tmp.path(), "p", "", "", vec![]).unwrap();
        let html = to_html(&p, "An {{term IDOR}} was found.\n\n{{glossary}}", &ctx(&p), &Meta { version: Some(1), issued: Some("2026-09-25".into()) });
        assert!(html.contains("data-term=\"idor\""));
        assert!(html.contains("id=\"term-idor\""));
        assert!(html.contains("owasp.org"), "the external reference is linked");
        assert!(!html.contains("term-xss"), "unused terms are not defined");
        assert!(html.contains("Version 1"));
    }
}
