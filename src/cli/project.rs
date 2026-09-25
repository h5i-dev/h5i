//! `h5i project`: the durable engagement (docs/design/design-project.md).
//!
//! A browser session is disposable; a project is not. Notes, findings, the
//! evidence they stand on, the checklists they cover and the reports issued
//! from them all live under `~/.local/share/h5i/projects` and survive
//! `h5i browser rm`. h5i keeps the record and checks the mechanical facts —
//! that cited evidence exists, that a report's ids resolve. What the findings
//! mean, and how the report reads, is the agent's.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Args, Subcommand};
use h5i_core::browser_session as bs;
use h5i_core::project::{self, checklist, evidence, finding, glossary, note, render, report};
use serde_json::json;

// The finding verbs flatten many optional text fields, so one variant is much
// larger than the rest. Boxing it would break clap's derive, and the enum is
// built once per process — the same trade-off `Commands` makes.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum ProjectCommands {
    /// Start a project. Findings and reports hang off it from here on.
    Init {
        name: String,
        #[arg(long, value_name = "TEXT")]
        title: Option<String>,
        #[arg(long = "describe", value_name = "TEXT")]
        description: Option<String>,
        /// What is being assessed; repeatable.
        #[arg(long = "target", value_name = "TEXT")]
        targets: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Every project on this machine.
    List {
        #[arg(long)]
        json: bool,
    },
    /// A project's state: findings by severity, coverage, reports.
    Show {
        #[command(flatten)]
        which: Which,
        #[arg(long)]
        json: bool,
    },
    /// Free notes: hypotheses, what the owner said, what is missing.
    Note {
        #[command(subcommand)]
        what: NoteVerb,
    },
    /// What was concluded, and how it stands.
    Finding {
        #[command(subcommand)]
        what: FindingVerb,
    },
    /// Copies of the requests, responses and screenshots findings rest on.
    Evidence {
        #[command(subcommand)]
        what: EvidenceVerb,
    },
    /// Checks to cover, imported from Markdown.
    Checklist {
        #[command(subcommand)]
        what: ChecklistVerb,
    },
    /// The report: a free document with `{{finding F-1}}`-style references.
    Report {
        #[command(subcommand)]
        what: ReportVerb,
    },
    /// Look up a security term in plain language.
    Glossary {
        /// The term, or nothing to list them all.
        term: Option<String>,
        #[arg(long, short = 'p', value_name = "NAME")]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

/// Which project, from `-p` or `$H5I_PROJECT`.
#[derive(Args)]
pub struct Which {
    #[arg(long, short = 'p', value_name = "NAME")]
    project: Option<String>,
}

impl Which {
    fn open(&self) -> anyhow::Result<project::Project> {
        let root = project::root()?;
        let name = self
            .project
            .clone()
            .or_else(|| std::env::var("H5I_PROJECT").ok().filter(|s| !s.trim().is_empty()))
            .context("no project given: pass --project <name> or set $H5I_PROJECT")?;
        Ok(project::Project::open(&root, &name)?)
    }
}

#[derive(Subcommand)]
pub enum NoteVerb {
    /// Write one down.
    Add {
        #[command(flatten)]
        which: Which,
        /// The note. Read from stdin when omitted.
        text: Vec<String>,
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Every note, oldest first.
    List {
        #[command(flatten)]
        which: Which,
        /// Include archived notes.
        #[arg(long)]
        archived: bool,
        #[arg(long)]
        json: bool,
    },
    /// Change a note, tag it, or archive it.
    Edit {
        #[command(flatten)]
        which: Which,
        id: String,
        #[arg(long, value_name = "TEXT")]
        text: Option<String>,
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
        #[arg(long)]
        archive: bool,
        #[arg(long)]
        unarchive: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum FindingVerb {
    /// Write a finding.
    Create {
        #[command(flatten)]
        which: Which,
        #[command(flatten)]
        fields: FindingFields,
        #[arg(long)]
        json: bool,
    },
    /// Change one. Title, severity, status and the prose fields replace; notes
    /// and evidence add.
    Update {
        #[command(flatten)]
        which: Which,
        id: String,
        #[command(flatten)]
        fields: FindingFields,
        #[arg(long)]
        json: bool,
    },
    /// Every finding, worst first.
    List {
        #[command(flatten)]
        which: Which,
        #[arg(long)]
        json: bool,
    },
    /// One finding in full.
    Show {
        #[command(flatten)]
        which: Which,
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Copy a session finding into the project, with its evidence.
    ///
    /// The session finding's cited messages become project evidence, and the
    /// project finding records where it came from. Promoting again updates.
    Promote {
        #[command(flatten)]
        which: Which,
        /// Which session holds the finding(s).
        #[arg(long, short = 's', value_name = "NAME")]
        session: String,
        /// A session finding id (`finding_3` or `3`). With `--all`, every one.
        id: Option<String>,
        #[arg(long, conflicts_with = "id")]
        all: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args, Default)]
pub struct FindingFields {
    #[arg(long, value_name = "TEXT")]
    title: Option<String>,
    /// A plain-language explanation for a non-specialist reader.
    #[arg(long, value_name = "TEXT")]
    summary: Option<String>,
    /// How sure you are. Free text: nothing here reads it.
    #[arg(long, value_name = "TEXT")]
    state: Option<String>,
    /// critical, high, medium, low or info.
    #[arg(long, value_name = "LEVEL")]
    severity: Option<String>,
    #[arg(long = "severity-reason", value_name = "TEXT")]
    severity_reason: Option<String>,
    #[arg(long, value_name = "TEXT")]
    impact: Option<String>,
    #[arg(long, value_name = "TEXT")]
    affected: Option<String>,
    #[arg(long, value_name = "TEXT")]
    remediation: Option<String>,
    /// open, in-progress, fix-claimed, fixed-verified or risk-accepted.
    #[arg(long, value_name = "STATE")]
    status: Option<String>,
    #[arg(long, value_name = "NAME")]
    owner: Option<String>,
    #[arg(long, value_name = "DATE")]
    due: Option<String>,
    /// Free Markdown, for whatever else the finding needs said.
    #[arg(long, value_name = "TEXT")]
    body: Option<String>,
    /// Add a note; repeatable over the finding's life.
    #[arg(long, value_name = "TEXT")]
    note: Option<String>,
    /// Project evidence ids it rests on, as `E-1,E-2`.
    #[arg(long, value_name = "IDS")]
    evidence: Vec<String>,
    #[arg(long, value_name = "PATH")]
    repro: Option<String>,
}

impl FindingFields {
    fn into_change(self) -> finding::Change {
        finding::Change {
            title: self.title,
            summary: self.summary,
            state: self.state,
            severity: self.severity,
            severity_reason: self.severity_reason,
            impact: self.impact,
            affected: self.affected,
            remediation: self.remediation,
            status: self.status,
            owner: self.owner,
            due: self.due,
            body: self.body,
            note: self.note,
            evidence: self.evidence,
            repro: self.repro,
            sources: Vec::new(),
            from_note: None,
        }
    }
}

#[derive(Subcommand)]
pub enum EvidenceVerb {
    /// Copy one captured message into the project, secrets removed.
    Add {
        #[command(flatten)]
        which: Which,
        /// `req_42`, `res_42` or `42`.
        id: String,
        #[arg(long, short = 's', value_name = "NAME")]
        session: String,
        #[arg(long, value_name = "TEXT")]
        caption: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Keep a log excerpt or note as evidence. Text from stdin when omitted.
    AddText {
        #[command(flatten)]
        which: Which,
        text: Vec<String>,
        #[arg(long, value_name = "TEXT")]
        caption: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Keep a screenshot (png, jpeg, gif or webp) as evidence.
    AddFile {
        #[command(flatten)]
        which: Which,
        path: PathBuf,
        #[arg(long, value_name = "TEXT")]
        caption: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Every evidence copy.
    List {
        #[command(flatten)]
        which: Which,
        #[arg(long)]
        json: bool,
    },
    /// One evidence copy, as an HTTP message or text.
    Show {
        #[command(flatten)]
        which: Which,
        id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum ChecklistVerb {
    /// Import a Markdown checklist. Its list items become tracked items.
    Import {
        #[command(flatten)]
        which: Which,
        /// The Markdown file, or `-` for stdin.
        file: String,
        /// The short name to file it under. Derived from the title otherwise.
        #[arg(long, value_name = "SLUG")]
        name: Option<String>,
        /// Each item must end with a recorded outcome, rather than being advice.
        #[arg(long)]
        required: bool,
        /// Revise an existing checklist, keeping ids and recording what dropped.
        #[arg(long)]
        replace: bool,
        #[arg(long)]
        json: bool,
    },
    /// Every checklist and its coverage.
    List {
        #[command(flatten)]
        which: Which,
        #[arg(long)]
        json: bool,
    },
    /// One checklist, its items and their outcomes.
    Show {
        #[command(flatten)]
        which: Which,
        slug: String,
        #[arg(long)]
        json: bool,
    },
    /// Record an outcome against an item.
    Mark {
        #[command(flatten)]
        which: Which,
        slug: String,
        /// `C3` or `3`.
        item: String,
        /// open, in-progress, recorded, blocked or not-applicable.
        #[arg(long, value_name = "STATE")]
        status: Option<String>,
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
        /// Link the outcome to a finding, note or evidence id; repeatable.
        #[arg(long = "link", value_name = "ID")]
        links: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Coverage counts, for one checklist or all of them.
    Coverage {
        #[command(flatten)]
        which: Which,
        slug: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum ReportVerb {
    /// Start the report draft from a template.
    New {
        #[command(flatten)]
        which: Which,
        /// A Markdown template to start from, instead of the built-in one.
        #[arg(long, value_name = "FILE")]
        template: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// Print where the draft is, for an editor to open.
    Path {
        #[command(flatten)]
        which: Which,
    },
    /// Print the draft, directives unresolved.
    Show {
        #[command(flatten)]
        which: Which,
    },
    /// Replace the draft with this file (or stdin). How the report gets written.
    Set {
        #[command(flatten)]
        which: Which,
        /// The Markdown file, or `-` for stdin.
        #[arg(value_name = "FILE", default_value = "-")]
        file: String,
    },
    /// Lint the draft: dangling ids and missing outcomes are errors, gaps advice.
    Check {
        #[command(flatten)]
        which: Which,
        #[arg(long)]
        json: bool,
    },
    /// Freeze the draft as an issued version, with its evidence snapshot.
    Issue {
        #[command(flatten)]
        which: Which,
        /// Issue even while `check` reports errors.
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Every issued version.
    List {
        #[command(flatten)]
        which: Which,
        #[arg(long)]
        json: bool,
    },
    /// Write the report as HTML or PDF.
    Export {
        #[command(flatten)]
        which: Which,
        /// A version number, or the working draft when omitted.
        #[arg(long, value_name = "N")]
        version: Option<u32>,
        /// `html` or `pdf`. PDF needs a local Chromium.
        #[arg(long, value_name = "FORMAT", default_value = "html")]
        format: String,
        /// Where to write it. Defaults beside the report.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
    },
}

pub fn run(action: ProjectCommands) -> anyhow::Result<()> {
    project::refuse_in_box()?;
    match action {
        ProjectCommands::Init { name, title, description, targets, json } => {
            let root = project::root()?;
            let p = project::Project::init(
                &root,
                &name,
                title.as_deref().unwrap_or(""),
                description.as_deref().unwrap_or(""),
                targets,
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&p.meta)?);
            } else {
                println!("  created project {} at {}", p.meta.name, p.dir.display());
                println!("  next: h5i project report new -p {}", p.meta.name);
            }
            Ok(())
        }
        ProjectCommands::List { json } => {
            let root = project::root()?;
            let rows: Vec<project::Summary> = project::list(&root).iter().map(project::summary).collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&json!({ "projects": rows }))?);
            } else if rows.is_empty() {
                println!("  no projects yet. Start one with `h5i project init <name>`");
            } else {
                for s in &rows {
                    println!(
                        "  {:<20} {} finding(s), {} open, {} evidence, {} report(s)",
                        s.name, s.findings, s.open_findings, s.evidence, s.reports
                    );
                }
            }
            Ok(())
        }
        ProjectCommands::Show { which, json } => {
            let p = which.open()?;
            let s = project::summary(&p);
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
                return Ok(());
            }
            println!("  project  : {}", s.name);
            if !p.meta.title.is_empty() {
                println!("  title    : {}", p.meta.title);
            }
            if !p.meta.targets.is_empty() {
                println!("  targets  : {}", p.meta.targets.join(", "));
            }
            let sev = if s.by_severity.is_empty() {
                "none".to_string()
            } else {
                s.by_severity.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(", ")
            };
            println!("  findings : {} ({} open) — {sev}", s.findings, s.open_findings);
            println!("  notes    : {}", s.notes);
            println!("  evidence : {}", s.evidence);
            println!("  checklists: {}", s.checklists);
            println!("  reports  : {} issued", s.reports);
            Ok(())
        }
        ProjectCommands::Note { what } => notes(what),
        ProjectCommands::Finding { what } => findings(what),
        ProjectCommands::Evidence { what } => evidences(what),
        ProjectCommands::Checklist { what } => checklists(what),
        ProjectCommands::Report { what } => reports(what),
        ProjectCommands::Glossary { term, project, json } => glossary_lookup(term, project, json),
    }
}

fn stdin_string() -> anyhow::Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

fn text_or_stdin(parts: Vec<String>) -> anyhow::Result<String> {
    if parts.is_empty() {
        Ok(stdin_string()?.trim().to_string())
    } else {
        Ok(parts.join(" "))
    }
}

fn notes(what: NoteVerb) -> anyhow::Result<()> {
    match what {
        NoteVerb::Add { which, text, tags, json } => {
            let p = which.open()?;
            let n = note::add(&p, &text_or_stdin(text)?, tags)?;
            emit(json, &n, || println!("  {} written", n.id))
        }
        NoteVerb::List { which, archived, json } => {
            let p = which.open()?;
            let rows: Vec<note::Note> = note::read(&p)?.into_iter().filter(|n| archived || !n.archived).collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&json!({ "notes": rows }))?);
            } else if rows.is_empty() {
                println!("  no notes");
            } else {
                for n in &rows {
                    let tags = if n.tags.is_empty() { String::new() } else { format!("  [{}]", n.tags.join(", ")) };
                    let flag = if n.archived { " (archived)" } else { "" };
                    println!("  {:<5}{flag}{tags}  {}", n.id, first_line(&n.text));
                }
            }
            Ok(())
        }
        NoteVerb::Edit { which, id, text, tags, archive, unarchive, json } => {
            let p = which.open()?;
            let archived = match (archive, unarchive) {
                (true, true) => anyhow::bail!("--archive and --unarchive are opposites"),
                (true, false) => Some(true),
                (false, true) => Some(false),
                (false, false) => None,
            };
            let n = note::edit(&p, &id, text.as_deref(), tags, archived)?;
            emit(json, &n, || println!("  {} updated", n.id))
        }
    }
}

fn findings(what: FindingVerb) -> anyhow::Result<()> {
    match what {
        FindingVerb::Create { which, fields, json } => {
            let p = which.open()?;
            let f = finding::create(&p, fields.into_change())?;
            emit(json, &f, || println!("  {} {} [{}]", f.id, f.title, sev_or(&f.severity)))
        }
        FindingVerb::Update { which, id, fields, json } => {
            let p = which.open()?;
            let f = finding::update(&p, &id, fields.into_change())?;
            emit(json, &f, || println!("  {} updated", f.id))
        }
        FindingVerb::List { which, json } => {
            let p = which.open()?;
            let rows = finding::sorted(finding::read(&p)?);
            if json {
                println!("{}", serde_json::to_string_pretty(&json!({ "findings": rows }))?);
            } else if rows.is_empty() {
                println!("  no findings");
            } else {
                for f in &rows {
                    println!("  {:<6} {:<9} {:<14} {}", f.id, sev_or(&f.severity), f.status, f.title);
                }
            }
            Ok(())
        }
        FindingVerb::Show { which, id, json } => {
            let p = which.open()?;
            let f = finding::find(&p, &id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&f)?);
                return Ok(());
            }
            print_finding(&f);
            Ok(())
        }
        FindingVerb::Promote { which, session, id, all, json } => promote(which, &session, id, all, json),
    }
}

fn promote(which: Which, session: &str, id: Option<String>, all: bool, json: bool) -> anyhow::Result<()> {
    let p = which.open()?;
    let browser_root = bs::root()?;
    let sess = resolve_session(&browser_root, session)?;
    // The session's own findings log, folded the same way the console folds it.
    let sdir = bs::dir(&browser_root, &sess.id);
    let session_findings = h5i_core::session_view::findings(&sdir);
    if session_findings.is_empty() {
        anyhow::bail!("session {} has no findings to promote", session);
    }
    let wanted: Vec<h5i_core::session_view::FindingRow> = if all {
        session_findings
    } else {
        let id = id.context("name a session finding, or pass --all")?;
        let want = finding_session_id(&id).map_err(|e| anyhow::anyhow!("{e}"))?;
        match session_findings.into_iter().find(|f| f.id == want) {
            Some(f) => vec![f],
            None => anyhow::bail!("session {session} has no {want}"),
        }
    };

    let existing = finding::read(&p)?;
    let mut done = Vec::new();
    for sf in &wanted {
        // Copy every cited message into project evidence first.
        let mut ev_ids = Vec::new();
        for cite in &sf.evidence {
            if let Ok(seq) = crate::cli::websec_seq(cite) {
                let rec = evidence::add_http(&p, &browser_root, &sess, seq, &format!("{} — {}", sf.id, sf.title))?;
                ev_ids.push(rec.id);
            }
        }
        let source = finding::Source {
            session: sess.id.clone(),
            session_name: sess.name.clone(),
            finding: sf.id.clone(),
        };
        let notes_joined = sf.notes.iter().map(|n| n.text.clone()).collect::<Vec<_>>().join("\n");
        let change = finding::Change {
            title: Some(sf.title.clone()),
            state: (!sf.state.is_empty()).then(|| sf.state.clone()),
            body: (!notes_joined.is_empty()).then_some(notes_joined),
            evidence: ev_ids,
            repro: sf.repro.clone(),
            sources: vec![source],
            ..finding::Change::default()
        };
        let f = match finding::promoted_from(&existing, &sess.id, &sf.id) {
            Some(pf) => finding::update(&p, &pf.id, change)?,
            None => finding::create(&p, change)?,
        };
        done.push(json!({ "from": sf.id, "finding": f.id }));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&json!({ "promoted": done }))?);
    } else {
        for d in &done {
            println!("  {} -> {}", d["from"].as_str().unwrap_or(""), d["finding"].as_str().unwrap_or(""));
        }
        println!("  {} finding(s) into project {}", done.len(), p.meta.name);
    }
    Ok(())
}

/// A session finding id like `finding_3` or `3`, normalised the session way.
fn finding_session_id(id: &str) -> project::Result<String> {
    let bare = id.strip_prefix("finding_").unwrap_or(id);
    match bare.parse::<u64>() {
        Ok(n) => Ok(format!("finding_{n}")),
        Err(_) => Err(project::ProjectError(format!("`{id}` is not a session finding id: try `finding_3` or `3`"))),
    }
}

fn evidences(what: EvidenceVerb) -> anyhow::Result<()> {
    match what {
        EvidenceVerb::Add { which, id, session, caption, json } => {
            let p = which.open()?;
            let browser_root = bs::root()?;
            let sess = resolve_session(&browser_root, &session)?;
            let seq = crate::cli::websec_seq(&id)?;
            let rec = evidence::add_http(&p, &browser_root, &sess, seq, caption.as_deref().unwrap_or(""))?;
            emit(json, &rec, || println!("  {} copied ({} secret value(s) removed)", rec.id, rec.removed))
        }
        EvidenceVerb::AddText { which, text, caption, json } => {
            let p = which.open()?;
            let rec = evidence::add_text(&p, &text_or_stdin(text)?, caption.as_deref().unwrap_or(""))?;
            emit(json, &rec, || println!("  {} kept", rec.id))
        }
        EvidenceVerb::AddFile { which, path, caption, json } => {
            let p = which.open()?;
            let rec = evidence::add_file(&p, &path, caption.as_deref().unwrap_or(""))?;
            emit(json, &rec, || println!("  {} kept", rec.id))
        }
        EvidenceVerb::List { which, json } => {
            let p = which.open()?;
            let rows = evidence::list(&p);
            if json {
                println!("{}", serde_json::to_string_pretty(&json!({ "evidence": rows }))?);
            } else if rows.is_empty() {
                println!("  no evidence");
            } else {
                for e in &rows {
                    let what = e.url.clone().or_else(|| e.caption.clone().into()).unwrap_or_default();
                    println!("  {:<5} {}", e.id, first_line(&what));
                }
            }
            Ok(())
        }
        EvidenceVerb::Show { which, id, json } => {
            let p = which.open()?;
            let rec = evidence::get(&p, &id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rec)?);
            } else {
                println!("  {}  {}", rec.id, rec.caption);
                match rec.kind {
                    evidence::Kind::Http => println!("{}", evidence::http_text(&rec)),
                    evidence::Kind::Text => println!("{}", rec.text.as_deref().unwrap_or("")),
                    evidence::Kind::File => println!("  [{} — {}]", rec.media_type.as_deref().unwrap_or("file"), rec.file.as_deref().unwrap_or("")),
                }
            }
            Ok(())
        }
    }
}

fn checklists(what: ChecklistVerb) -> anyhow::Result<()> {
    match what {
        ChecklistVerb::Import { which, file, name, required, replace, json } => {
            let p = which.open()?;
            let (source, markdown) = if file == "-" {
                ("stdin".to_string(), stdin_string()?)
            } else {
                (file.clone(), std::fs::read_to_string(&file).with_context(|| format!("cannot read {file}"))?)
            };
            let slug = name.unwrap_or_else(|| checklist::slug_from(&checklist::parse(&markdown).0));
            let mode = if required { checklist::Mode::Required } else { checklist::Mode::Reference };
            let c = checklist::import(&p, &slug, &source, &markdown, mode, replace)?;
            emit(json, &c, || println!("  {} ({}, {} items) imported as `{}`", c.title, c.mode.as_str(), c.items.len(), c.slug))
        }
        ChecklistVerb::List { which, json } => {
            let p = which.open()?;
            let lists = checklist::list(&p);
            let cov: Vec<checklist::Coverage> = lists
                .iter()
                .filter_map(|c| checklist::outcomes(&p, c).ok().map(|o| checklist::coverage(c, &o)))
                .collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&json!({ "checklists": cov }))?);
            } else if cov.is_empty() {
                println!("  no checklists");
            } else {
                for c in &cov {
                    println!("  {:<16} {} [{}]  {}", c.slug, c.title, c.mode.as_str(), c.sentence());
                }
            }
            Ok(())
        }
        ChecklistVerb::Show { which, slug, json } => {
            let p = which.open()?;
            let c = checklist::get(&p, &slug)?;
            let outcomes = checklist::outcomes(&p, &c)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&json!({ "checklist": c, "outcomes": outcomes }))?);
                return Ok(());
            }
            println!("  {} [{}] v{}", c.title, c.mode.as_str(), c.version);
            let mut group = String::new();
            for o in &outcomes {
                if o.item.group != group {
                    group = o.item.group.clone();
                    if !group.is_empty() {
                        println!("  # {group}");
                    }
                }
                let note = o.notes.last().map(|(_, t)| format!("  — {}", first_line(t))).unwrap_or_default();
                println!("  {:<5} {:<14} {}{}", o.item.id, o.status, o.item.text, note);
            }
            Ok(())
        }
        ChecklistVerb::Mark { which, slug, item, status, note, links, json } => {
            let p = which.open()?;
            let o = checklist::mark(&p, &slug, &item, status.as_deref(), note.as_deref(), links)?;
            emit(json, &o, || println!("  {} {}", o.item.id, o.status))
        }
        ChecklistVerb::Coverage { which, slug, json } => {
            let p = which.open()?;
            let lists = checklist::list(&p);
            let selected: Vec<&checklist::Checklist> = match &slug {
                Some(s) => lists.iter().filter(|c| &c.slug == s).collect(),
                None => lists.iter().collect(),
            };
            let cov: Vec<checklist::Coverage> = selected
                .iter()
                .filter_map(|c| checklist::outcomes(&p, c).ok().map(|o| checklist::coverage(c, &o)))
                .collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&json!({ "coverage": cov }))?);
            } else if cov.is_empty() {
                println!("  nothing to report");
            } else {
                for c in &cov {
                    println!("  {}: {}", c.title, c.sentence());
                }
            }
            Ok(())
        }
    }
}

fn reports(what: ReportVerb) -> anyhow::Result<()> {
    match what {
        ReportVerb::New { which, template, force } => {
            let p = which.open()?;
            report::new_draft(&p, template.as_deref(), force)?;
            println!("  draft at {}", p.path(report::DRAFT).display());
            println!("  write it, then: h5i project report check -p {}", p.meta.name);
            Ok(())
        }
        ReportVerb::Path { which } => {
            let p = which.open()?;
            println!("{}", p.path(report::DRAFT).display());
            Ok(())
        }
        ReportVerb::Show { which } => {
            let p = which.open()?;
            print!("{}", report::draft(&p)?);
            Ok(())
        }
        ReportVerb::Set { which, file } => {
            let p = which.open()?;
            let body = if file == "-" { stdin_string()? } else { std::fs::read_to_string(&file)? };
            report::save_draft(&p, &body)?;
            println!("  draft set ({} bytes)", body.len());
            Ok(())
        }
        ReportVerb::Check { which, json } => {
            let p = which.open()?;
            let problems = report::check(&p)?;
            let errors = problems.iter().filter(|p| p.severity == "error").count();
            if json {
                println!("{}", serde_json::to_string_pretty(&json!({ "problems": problems, "errors": errors }))?);
            } else if problems.is_empty() {
                println!("  clean: no errors, no advice");
            } else {
                for pr in &problems {
                    println!("  {:<7} {}", pr.severity, pr.message);
                }
                println!("  {errors} error(s), {} advisory", problems.len() - errors);
            }
            if errors > 0 {
                std::process::exit(1);
            }
            Ok(())
        }
        ReportVerb::Issue { which, force, json } => {
            let p = which.open()?;
            let (version, dir) = report::issue(&p, force)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&json!({ "version": version, "dir": dir }))?);
            } else {
                println!("  issued v{version} at {}", dir.display());
                println!("  export it: h5i project report export -p {} --version {version} --format pdf", p.meta.name);
            }
            Ok(())
        }
        ReportVerb::List { which, json } => {
            let p = which.open()?;
            let rows = report::issued(&p);
            if json {
                println!("{}", serde_json::to_string_pretty(&json!({ "reports": rows }))?);
            } else if rows.is_empty() {
                println!("  no issued reports. `h5i project report issue` freezes the draft");
            } else {
                for r in &rows {
                    println!("  v{:<3} {}  {} finding(s)", r.version, r.issued, r.findings);
                }
            }
            Ok(())
        }
        ReportVerb::Export { which, version, format, out } => export(which, version, &format, out),
    }
}

fn export(which: Which, version: Option<u32>, format: &str, out: Option<PathBuf>) -> anyhow::Result<()> {
    let p = which.open()?;
    // A version renders from its frozen snapshot's markdown; the draft renders
    // live. Either way the HTML is built the same way.
    let (html, default_stem) = match version {
        Some(v) => {
            let dir = p.path(report::DIR).join(format!("v{v}"));
            let html_path = dir.join("report.html");
            let html = std::fs::read_to_string(&html_path)
                .with_context(|| format!("no issued v{v}: run `h5i project report issue` first"))?;
            (html, format!("{}-v{v}", p.meta.name))
        }
        None => {
            let source = report::draft(&p)?;
            let ctx = report::Context::load(&p)?;
            let html = render::to_html(&p, &source, &ctx, &render::Meta::default());
            (html, format!("{}-draft", p.meta.name))
        }
    };

    match format {
        "html" => {
            let path = out.unwrap_or_else(|| p.path(report::DIR).join(format!("{default_stem}.html")));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, inline_assets(&p, version, &html)?)?;
            println!("  {}", path.display());
            Ok(())
        }
        "pdf" => {
            let path = out.unwrap_or_else(|| p.path(report::DIR).join(format!("{default_stem}.pdf")));
            to_pdf(&inline_assets(&p, version, &html)?, &path)?;
            println!("  {}", path.display());
            Ok(())
        }
        other => anyhow::bail!("`{other}` is not a format: use `html` or `pdf`"),
    }
}

/// Turn `<img src="evidence/E-2.png">` into inline `data:` URIs, so an exported
/// file stands alone with no directory beside it.
fn inline_assets(p: &project::Project, _version: Option<u32>, html: &str) -> anyhow::Result<String> {
    use base64::Engine as _;
    let mut out = html.to_string();
    for e in evidence::list(p) {
        if let Some((bytes, media)) = evidence::file_bytes(p, &e)
            && let Some(name) = &e.file
        {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            out = out.replace(&format!("src=\"evidence/{name}\""), &format!("src=\"data:{media};base64,{b64}\""));
        }
    }
    Ok(out)
}

/// Print the HTML to PDF with a local headless Chromium. Not h5i's own engine:
/// it does not print, and a report is a page a person hands over, so the widely
/// installed renderer is the right one.
fn to_pdf(html: &str, out: &Path) -> anyhow::Result<()> {
    let chrome = find_chrome().context(
        "no Chrome or Chromium found for PDF. Set $H5I_CHROME to one, or export `--format html` \
         and print to PDF from a browser",
    )?;
    let tmp = std::env::temp_dir().join(format!("h5i-report-{}.html", std::process::id()));
    std::fs::write(&tmp, html)?;
    let profile = std::env::temp_dir().join(format!("h5i-chrome-{}", std::process::id()));
    let status = std::process::Command::new(&chrome)
        .args([
            "--headless",
            "--no-sandbox",
            "--disable-gpu",
            "--no-pdf-header-footer",
            &format!("--user-data-dir={}", profile.display()),
            &format!("--print-to-pdf={}", out.display()),
        ])
        .arg(format!("file://{}", tmp.display()))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_dir_all(&profile);
    match status {
        Ok(s) if s.success() && out.exists() => Ok(()),
        Ok(_) | Err(_) => anyhow::bail!(
            "{} could not print the PDF. Export `--format html` and print from a browser instead",
            chrome.display()
        ),
    }
}

fn find_chrome() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("H5I_CHROME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(explicit));
    }
    let candidates = [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ];
    for c in candidates {
        let path = PathBuf::from(c);
        if path.is_absolute() {
            if path.exists() {
                return Some(path);
            }
        } else if let Ok(found) = which_on_path(c) {
            return Some(found);
        }
    }
    None
}

fn which_on_path(bin: &str) -> anyhow::Result<PathBuf> {
    let paths = std::env::var_os("PATH").context("no PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("not on PATH")
}

fn glossary_lookup(term: Option<String>, project_name: Option<String>, json: bool) -> anyhow::Result<()> {
    let glossary = match project_name {
        Some(name) => glossary::Glossary::for_project(&project::Project::open(&project::root()?, &name)?)?,
        None => glossary::Glossary::builtin(),
    };
    match term {
        Some(key) => match glossary.lookup(&key) {
            Some(t) => {
                if json {
                    println!("{}", serde_json::to_string_pretty(t)?);
                } else {
                    println!("  {}", t.term);
                    println!("  {}", t.explain);
                    if let Some((url, label)) = t.reference() {
                        println!("  more: {label} — {url}");
                    }
                }
                Ok(())
            }
            None => anyhow::bail!("no glossary term for `{key}`. `h5i project glossary` lists them"),
        },
        None => {
            if json {
                println!("{}", serde_json::to_string_pretty(&json!({ "terms": glossary.terms }))?);
            } else {
                for t in &glossary.terms {
                    println!("  {:<28} {}", t.term, t.short);
                }
            }
            Ok(())
        }
    }
}

/// The session a reader means, live or ended (the promote/evidence subject is
/// usually the run that just finished).
fn resolve_session(root: &Path, selector: &str) -> anyhow::Result<bs::Session> {
    match bs::resolve(root, Some(selector)) {
        Ok(s) => Ok(s),
        Err(bs::SessionGone::Ended { id, .. }) => Ok(bs::read(root, &id)?),
        Err(gone) => match bs::find_ended_by_name(root, selector) {
            Some(ended) => Ok(ended),
            None => anyhow::bail!("{gone}"),
        },
    }
}

fn emit<T: serde::Serialize>(json: bool, value: &T, human: impl FnOnce()) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        human();
    }
    Ok(())
}

fn sev_or(sev: &str) -> &str {
    if sev.is_empty() { "unrated" } else { sev }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("");
    if line.chars().count() > 80 {
        format!("{}…", line.chars().take(79).collect::<String>())
    } else {
        line.to_string()
    }
}

fn print_finding(f: &finding::Finding) {
    println!("  {}  {}", f.id, f.title);
    println!("  severity : {} ({})", sev_or(&f.severity), if f.severity_reason.is_empty() { "no reason given" } else { &f.severity_reason });
    println!("  status   : {}", f.status);
    if !f.state.is_empty() {
        println!("  confidence: {}", f.state);
    }
    for (label, value) in [("summary", &f.summary), ("impact", &f.impact), ("affected", &f.affected), ("remediation", &f.remediation)] {
        if !value.is_empty() {
            println!("  {label:<9}: {value}");
        }
    }
    if !f.evidence.is_empty() {
        println!("  evidence : {}", f.evidence.join(", "));
    }
    if let Some(repro) = &f.repro {
        println!("  repro    : {repro}");
    }
    for s in &f.sources {
        println!("  from     : {} of session {}", s.finding, s.session_name.as_deref().unwrap_or(&s.session));
    }
    for n in &f.notes {
        println!("  note     : {} ({})", n.text, n.at);
    }
    if !f.body.trim().is_empty() {
        println!("\n{}", f.body);
    }
}
