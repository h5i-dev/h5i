//! Reports: a free Markdown document the agent writes, with directives h5i
//! resolves against the project so counts and ids cannot drift from the prose.
//!
//! `check` is deterministic: unknown directives, dangling ids, required
//! checklist items with no outcome, findings with no evidence. `issue` freezes
//! the draft with a snapshot of everything it referenced, so a later change to
//! a finding never rewrites a report that was already handed over.

use serde::{Deserialize, Serialize};

use super::{Project, Result, bail, checklist, evidence, finding, glossary, now, write_private};

pub const DIR: &str = "reports";
pub const DRAFT: &str = "reports/draft.md";

/// One `{{...}}` in the source and where it was.
#[derive(Debug, Clone, PartialEq)]
pub struct Directive {
    pub kind: String,
    pub arg: String,
    pub raw: String,
}

/// Find every `{{ kind arg }}`. Anything inside a fenced code block is left as
/// literal text, so a report can show the directive syntax it documents.
pub fn directives(markdown: &str) -> Vec<Directive> {
    let mut out = Vec::new();
    let mut fenced = false;
    for line in markdown.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let mut rest = line;
        while let Some(start) = rest.find("{{") {
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else { break };
            let inner = after[..end].trim();
            let raw = format!("{{{{{inner}}}}}");
            let (kind, arg) = match inner.split_once(char::is_whitespace) {
                Some((k, a)) => (k.trim().to_string(), a.trim().to_string()),
                None => (inner.to_string(), String::new()),
            };
            out.push(Directive { kind, arg, raw });
            rest = &after[end + 2..];
        }
    }
    out
}

pub const KINDS: [&str; 7] = ["finding", "findings", "evidence", "checklist", "coverage", "term", "glossary"];

/// A problem `check` found. An error must be fixed; advice is the reviewer's
/// judgement to weigh (design-project.md P7).
#[derive(Debug, Clone, Serialize)]
pub struct Problem {
    pub severity: String,
    pub message: String,
}

impl Problem {
    fn error(message: String) -> Problem {
        Problem { severity: "error".into(), message }
    }
    fn advice(message: String) -> Problem {
        Problem { severity: "advice".into(), message }
    }
}

/// Read the draft.
pub fn draft(project: &Project) -> Result<String> {
    match std::fs::read_to_string(project.path(DRAFT)) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("project {} has no report draft. Start one with `h5i project report new`", project.meta.name)
        }
        Err(e) => bail!("cannot read the draft: {e}"),
    }
}

pub fn has_draft(project: &Project) -> bool {
    project.path(DRAFT).exists()
}

/// Create the draft from a template. Refuses to clobber an edited one.
pub fn new_draft(project: &Project, template: Option<&str>, force: bool) -> Result<()> {
    if has_draft(project) && !force {
        bail!("project {} already has a report draft: edit it, or pass --force to reseed it", project.meta.name);
    }
    let body = match template {
        Some(path) => std::fs::read_to_string(path).map_err(|e| super::ProjectError(format!("cannot read template {path}: {e}")))?,
        None => default_template(project),
    };
    super::private_dir(&project.path(DIR))?;
    write_private(&project.path(DRAFT), body.as_bytes())
}

/// Save an edited draft (the console's editor writes through here).
pub fn save_draft(project: &Project, body: &str) -> Result<()> {
    super::private_dir(&project.path(DIR))?;
    write_private(&project.path(DRAFT), body.as_bytes())
}

/// The starting document. An editable outline, not a form: the agent may add,
/// drop or merge any of it (design-project.md P2).
pub fn default_template(project: &Project) -> String {
    let name = if project.meta.title.is_empty() { &project.meta.name } else { &project.meta.title };
    format!(
        r#"# Security Assessment Report: {name}

<!-- A starting point, not a form. Add, remove or reorder sections to fit what
you found. h5i fills counts, ids and outcomes from the double-brace markers; you
write everything else. The markers you can use:

```
{{{{findings}}}}          a table of every finding
{{{{finding F-1}}}}       one finding in full
{{{{evidence E-1}}}}      the redacted request/response it rests on
{{{{coverage}}}}          coverage counts for every checklist
{{{{checklist SLUG}}}}    one checklist's items and outcomes
{{{{term IDOR}}}}         mark a term for the glossary and a definition panel
{{{{glossary}}}}          define every term the report used
```

Delete this comment before issuing. -->

**Target:** {targets}
**Assessed:** <dates>
**Prepared by:** <name>
**Status:** draft

## Executive summary

<Two or three paragraphs for a reader who will not read the rest: what matters
most, and what to fix first. State what this covers and what it does not.>

## Scope and limits

<What was in scope, what was excluded, and what could not be reached. A clean
result on something not tested is not a clean result.>

## What was done

{{{{coverage}}}}

## Findings

{{{{findings}}}}

<Then a section per finding you want to expand, writing the narrative around it.>

## Remediation plan

<Ordered by what to fix first. Reference findings by id.>

## Appendix: terms

{{{{glossary}}}}
"#,
        targets = if project.meta.targets.is_empty() { "<target>".into() } else { project.meta.targets.join(", ") }
    )
}

/// The project data one render or issue needs.
pub struct Context {
    pub findings: Vec<finding::Finding>,
    pub evidence: Vec<evidence::Record>,
    pub checklists: Vec<(checklist::Checklist, Vec<checklist::Outcome>)>,
    pub glossary: glossary::Glossary,
}

impl Context {
    pub fn load(project: &Project) -> Result<Context> {
        let checklists = checklist::list(project)
            .into_iter()
            .map(|c| {
                let outcomes = checklist::outcomes(project, &c)?;
                Ok((c, outcomes))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Context {
            findings: finding::read(project)?,
            evidence: evidence::list(project),
            checklists,
            glossary: glossary::Glossary::for_project(project)?,
        })
    }

    pub fn finding(&self, id: &str) -> Option<&finding::Finding> {
        let want = super::normalise_id("F", id).ok()?;
        self.findings.iter().find(|f| f.id == want)
    }
    pub fn evidence(&self, id: &str) -> Option<&evidence::Record> {
        let want = super::normalise_id("E", id).ok()?;
        self.evidence.iter().find(|e| e.id == want)
    }
    pub fn checklist(&self, slug: &str) -> Option<&(checklist::Checklist, Vec<checklist::Outcome>)> {
        self.checklists.iter().find(|(c, _)| c.slug == slug)
    }
}

/// Lint the draft. Errors first, then advice.
pub fn check(project: &Project) -> Result<Vec<Problem>> {
    let source = draft(project)?;
    let ctx = Context::load(project)?;
    let mut problems = Vec::new();

    for d in directives(&source) {
        if !KINDS.contains(&d.kind.as_str()) {
            problems.push(Problem::error(format!("`{}` is not a directive: use one of {}", d.raw, KINDS.join(", "))));
            continue;
        }
        match d.kind.as_str() {
            "finding" if ctx.finding(&d.arg).is_none() => {
                problems.push(Problem::error(format!("{} names a finding this project does not have", d.raw)));
            }
            "evidence" if ctx.evidence(&d.arg).is_none() => {
                problems.push(Problem::error(format!("{} names evidence this project does not have", d.raw)));
            }
            "checklist" | "coverage" if !d.arg.is_empty() && ctx.checklist(&d.arg).is_none() => {
                problems.push(Problem::error(format!("{} names a checklist this project does not have", d.raw)));
            }
            "term" if ctx.glossary.lookup(&d.arg).is_none() => {
                problems.push(Problem::advice(format!("{} is not a glossary term; it will render as plain text", d.raw)));
            }
            _ => {}
        }
    }

    // Findings the report should probably say more about.
    let named: Vec<String> = directives(&source)
        .into_iter()
        .filter(|d| d.kind == "finding")
        .map(|d| super::normalise_id("F", &d.arg).unwrap_or(d.arg))
        .collect();
    let renders_table = directives(&source).iter().any(|d| d.kind == "findings");
    for f in &ctx.findings {
        if f.evidence.is_empty() && f.repro.is_none() {
            problems.push(Problem::advice(format!("{} ({}) cites no evidence and has no repro", f.id, f.title)));
        }
        if f.severity.is_empty() {
            problems.push(Problem::advice(format!("{} ({}) has no severity", f.id, f.title)));
        } else if f.severity_reason.is_empty() {
            problems.push(Problem::advice(format!("{} is rated {} with no reason given", f.id, f.severity)));
        }
        if f.remediation.is_empty() {
            problems.push(Problem::advice(format!("{} ({}) has no remediation", f.id, f.title)));
        }
        if !named.contains(&f.id) && !renders_table {
            problems.push(Problem::advice(format!("{} ({}) is not mentioned in the report", f.id, f.title)));
        }
    }

    // Required checklist items with no outcome.
    for (c, outcomes) in &ctx.checklists {
        if c.mode != checklist::Mode::Required {
            continue;
        }
        for o in outcomes {
            if matches!(o.status.as_str(), "open" | "in-progress") {
                problems.push(Problem::error(format!(
                    "required checklist `{}` item {} ({}) has no recorded outcome",
                    c.slug, o.item.id, o.item.text
                )));
            }
        }
    }

    problems.sort_by(|a, b| b.severity.cmp(&a.severity));
    Ok(problems)
}

/// What an issued report is on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issued {
    pub version: u32,
    pub issued: String,
    pub title: String,
    pub findings: usize,
    pub glossary_version: u32,
}

pub fn issued(project: &Project) -> Vec<Issued> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(project.path(DIR)) else {
        return out;
    };
    for entry in entries.flatten() {
        let meta = entry.path().join("issued.json");
        if let Ok(text) = std::fs::read_to_string(&meta)
            && let Ok(rec) = serde_json::from_str::<Issued>(&text)
        {
            out.push(rec);
        }
    }
    out.sort_by_key(|r| r.version);
    out
}

pub fn next_version(project: &Project) -> u32 {
    issued(project).iter().map(|r| r.version).max().unwrap_or(0) + 1
}

/// Freeze the draft. Refuses while `check` reports an error unless `force`.
/// Returns the version directory.
pub fn issue(project: &Project, force: bool) -> Result<(u32, std::path::PathBuf)> {
    let source = draft(project)?;
    let problems = check(project)?;
    let errors: Vec<&Problem> = problems.iter().filter(|p| p.severity == "error").collect();
    if !errors.is_empty() && !force {
        let list = errors.iter().map(|p| format!("  - {}", p.message)).collect::<Vec<_>>().join("\n");
        bail!("this report has {} error(s); fix them or pass --force:\n{list}", errors.len());
    }
    let ctx = Context::load(project)?;
    let version = next_version(project);
    let dir = project.path(DIR).join(format!("v{version}"));
    super::private_dir(&dir)?;
    write_private(&dir.join("report.md"), source.as_bytes())?;

    // The snapshot: only what the report referenced, so an issued report is a
    // closed object that later edits cannot reach into.
    let ds = directives(&source);
    let wants_all = ds.iter().any(|d| d.kind == "findings");
    let mut findings: Vec<&finding::Finding> = if wants_all {
        ctx.findings.iter().collect()
    } else {
        ds.iter()
            .filter(|d| d.kind == "finding")
            .filter_map(|d| ctx.finding(&d.arg))
            .collect()
    };
    findings.dedup_by_key(|f| f.id.clone());
    let evidence_ids: std::collections::BTreeSet<String> = findings
        .iter()
        .flat_map(|f| f.evidence.iter().cloned())
        .chain(ds.iter().filter(|d| d.kind == "evidence").map(|d| super::normalise_id("E", &d.arg).unwrap_or_default()))
        .filter(|id| !id.is_empty())
        .collect();
    let snapshot = serde_json::json!({
        "project": project.meta,
        "findings": findings,
        "evidence": evidence_ids.iter().filter_map(|id| ctx.evidence(id)).collect::<Vec<_>>(),
        "checklists": ctx.checklists.iter().map(|(c, o)| serde_json::json!({"checklist": c, "outcomes": o})).collect::<Vec<_>>(),
        "glossary_version": ctx.glossary.version,
    });
    write_private(&dir.join("snapshot.json"), serde_json::to_string_pretty(&snapshot)?.as_bytes())?;

    let html = super::render::to_html(project, &source, &ctx, &super::render::Meta { version: Some(version), issued: Some(now()) });
    write_private(&dir.join("report.html"), html.as_bytes())?;

    let record = Issued {
        version,
        issued: now(),
        title: project.meta.title.clone(),
        findings: findings.len(),
        glossary_version: ctx.glossary.version,
    };
    write_private(&dir.join("issued.json"), serde_json::to_string_pretty(&record)?.as_bytes())?;
    Ok((version, dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::finding::Change;

    fn project() -> (tempfile::TempDir, Project) {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::init(tmp.path(), "p", "ACME", "", vec![]).unwrap();
        (tmp, p)
    }

    #[test]
    fn directives_are_found_outside_code_fences() {
        let src = "see {{finding F-1}} and {{coverage}}.\n\n```\nnot {{finding F-2}} here\n```\n{{term IDOR}}";
        let ds = directives(src);
        assert_eq!(ds.len(), 3);
        assert_eq!(ds[0], Directive { kind: "finding".into(), arg: "F-1".into(), raw: "{{finding F-1}}".into() });
        assert!(ds.iter().all(|d| d.arg != "F-2"));
    }

    #[test]
    fn check_flags_dangling_ids_as_errors_and_gaps_as_advice() {
        let (_t, p) = project();
        finding::create(&p, Change { title: Some("x".into()), ..Change::default() }).unwrap();
        save_draft(&p, "{{finding F-9}}\n{{bogus}}\n{{finding F-1}}").unwrap();
        let problems = check(&p).unwrap();
        assert!(problems.iter().any(|p| p.severity == "error" && p.message.contains("F-9")));
        assert!(problems.iter().any(|p| p.severity == "error" && p.message.contains("bogus")));
        assert!(problems.iter().any(|p| p.severity == "advice" && p.message.contains("no evidence")));
    }

    #[test]
    fn issue_refuses_on_error_then_freezes_and_versions() {
        let (_t, p) = project();
        finding::create(&p, Change { title: Some("x".into()), severity: Some("low".into()), ..Change::default() }).unwrap();
        save_draft(&p, "{{finding F-9}}").unwrap();
        assert!(issue(&p, false).is_err());
        save_draft(&p, "# Report\n{{finding F-1}}").unwrap();
        let (v1, dir) = issue(&p, false).unwrap();
        assert_eq!(v1, 1);
        assert!(dir.join("report.html").exists() && dir.join("snapshot.json").exists());
        // A later change does not touch the issued snapshot.
        finding::update(&p, "F-1", Change { title: Some("renamed".into()), ..Change::default() }).unwrap();
        let snap: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("snapshot.json")).unwrap()).unwrap();
        assert_eq!(snap["findings"][0]["title"], "x");
        let (v2, _) = issue(&p, false).unwrap();
        assert_eq!(v2, 2);
        assert_eq!(issued(&p).len(), 2);
    }
}
