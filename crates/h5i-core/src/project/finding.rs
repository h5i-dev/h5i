//! Project findings: the session finding's shape plus what a report needs.
//!
//! Severity, confidence (`state`) and fix status are three fields on purpose:
//! "high but unconfirmed" and "low but confirmed" are different facts, and so
//! are "fix claimed" and "fix verified". `state` stays free text, as in the
//! session log; severity and status are small fixed sets because a report
//! counts and sorts by them.

use serde::{Deserialize, Serialize};

use super::{Project, Result, append_line, bail, bounded, evidence, next_id, normalise_id, now, read_lines};

pub const FILE: &str = "findings.jsonl";

pub const SEVERITIES: [&str; 5] = ["critical", "high", "medium", "low", "info"];
pub const STATUSES: [&str; 5] = ["open", "in-progress", "fix-claimed", "fixed-verified", "risk-accepted"];

/// Which session finding a project finding came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    pub finding: String,
}

/// One write. Named fields replace; notes, evidence and sources accumulate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repro: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Remark {
    pub at: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub title: String,
    /// A plain-language explanation for a reader who is not a specialist.
    pub summary: String,
    /// The agent's word for how sure it is. Free text.
    pub state: String,
    pub severity: String,
    pub severity_reason: String,
    pub impact: String,
    pub affected: String,
    pub remediation: String,
    /// Where the fix stands, one of [`STATUSES`].
    pub status: String,
    pub owner: String,
    pub due: String,
    /// Free Markdown: whatever else the finding needs said.
    pub body: String,
    pub notes: Vec<Remark>,
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repro: Option<String>,
    pub sources: Vec<Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_note: Option<String>,
    pub created: String,
    pub updated: String,
}

impl Finding {
    pub fn is_open(&self) -> bool {
        matches!(self.status.as_str(), "" | "open" | "in-progress" | "fix-claimed")
    }

    pub fn severity_rank(&self) -> usize {
        SEVERITIES.iter().position(|s| *s == self.severity).unwrap_or(SEVERITIES.len())
    }
}

pub fn fold(entries: &[Entry]) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    for e in entries {
        let slot = match out.iter().position(|f| f.id == e.id) {
            Some(i) => i,
            None => {
                out.push(Finding {
                    id: e.id.clone(),
                    status: "open".into(),
                    created: e.at.clone(),
                    updated: e.at.clone(),
                    ..Finding::default()
                });
                out.len() - 1
            }
        };
        let f = &mut out[slot];
        let set = |field: &mut String, value: &Option<String>| {
            if let Some(v) = value {
                *field = v.clone();
            }
        };
        set(&mut f.title, &e.title);
        set(&mut f.summary, &e.summary);
        set(&mut f.state, &e.state);
        set(&mut f.severity, &e.severity);
        set(&mut f.severity_reason, &e.severity_reason);
        set(&mut f.impact, &e.impact);
        set(&mut f.affected, &e.affected);
        set(&mut f.remediation, &e.remediation);
        set(&mut f.status, &e.status);
        set(&mut f.owner, &e.owner);
        set(&mut f.due, &e.due);
        set(&mut f.body, &e.body);
        if let Some(repro) = &e.repro {
            f.repro = Some(repro.clone());
        }
        if let Some(n) = &e.from_note {
            f.from_note = Some(n.clone());
        }
        if let Some(note) = &e.note {
            f.notes.push(Remark { at: e.at.clone(), text: note.clone() });
        }
        for id in &e.evidence {
            if !f.evidence.contains(id) {
                f.evidence.push(id.clone());
            }
        }
        for s in &e.sources {
            if !f.sources.contains(s) {
                f.sources.push(s.clone());
            }
        }
        f.updated = e.at.clone();
    }
    out
}

pub fn read(project: &Project) -> Result<Vec<Finding>> {
    Ok(fold(&read_lines::<Entry>(&project.path(FILE))?))
}

pub fn find(project: &Project, id: &str) -> Result<Finding> {
    let want = normalise_id("F", id)?;
    match read(project)?.into_iter().find(|f| f.id == want) {
        Some(f) => Ok(f),
        None => bail!("project {} has no {want}", project.meta.name),
    }
}

/// The fields a create or an update may carry, before they are checked.
#[derive(Debug, Clone, Default)]
pub struct Change {
    pub title: Option<String>,
    pub summary: Option<String>,
    pub state: Option<String>,
    pub severity: Option<String>,
    pub severity_reason: Option<String>,
    pub impact: Option<String>,
    pub affected: Option<String>,
    pub remediation: Option<String>,
    pub status: Option<String>,
    pub owner: Option<String>,
    pub due: Option<String>,
    pub body: Option<String>,
    pub note: Option<String>,
    pub evidence: Vec<String>,
    pub repro: Option<String>,
    pub sources: Vec<Source>,
    pub from_note: Option<String>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        let Change {
            title, summary, state, severity, severity_reason, impact, affected, remediation, status, owner, due, body,
            note, evidence, repro, sources, from_note,
        } = self;
        [title, summary, state, severity, severity_reason, impact, affected, remediation, status, owner, due, body, note, repro, from_note]
            .iter()
            .all(|f| f.is_none())
            && evidence.is_empty()
            && sources.is_empty()
    }

    fn into_entry(self, project: &Project, id: &str) -> Result<Entry> {
        let text = |what: &str, v: Option<String>| v.map(|v| bounded(what, &v)).transpose();
        if let Some(s) = &self.severity
            && !SEVERITIES.contains(&s.as_str())
        {
            bail!("`{s}` is not a severity: use one of {}", SEVERITIES.join(", "));
        }
        if let Some(s) = &self.status
            && !STATUSES.contains(&s.as_str())
        {
            bail!("`{s}` is not a fix status: use one of {}", STATUSES.join(", "));
        }
        let evidence = evidence::check_ids(project, &self.evidence)?;
        Ok(Entry {
            id: id.to_string(),
            at: now(),
            title: text("title", self.title)?,
            summary: text("summary", self.summary)?,
            state: text("state", self.state)?,
            severity: self.severity,
            severity_reason: text("severity reason", self.severity_reason)?,
            impact: text("impact", self.impact)?,
            affected: text("affected", self.affected)?,
            remediation: text("remediation", self.remediation)?,
            status: self.status,
            owner: text("owner", self.owner)?,
            due: text("due", self.due)?,
            body: text("body", self.body)?,
            note: text("note", self.note)?,
            evidence,
            repro: self.repro,
            sources: self.sources,
            from_note: self.from_note,
        })
    }
}

pub fn create(project: &Project, change: Change) -> Result<Finding> {
    if change.title.as_deref().is_none_or(|t| t.trim().is_empty()) {
        bail!("a finding needs a title");
    }
    let id = next_id("F", read(project)?.iter().map(|f| f.id.as_str()));
    append_line(&project.path(FILE), &change.into_entry(project, &id)?)?;
    find(project, &id)
}

pub fn update(project: &Project, id: &str, change: Change) -> Result<Finding> {
    let f = find(project, id)?;
    if change.is_empty() {
        bail!("`finding update` with nothing to change would only move its clock");
    }
    append_line(&project.path(FILE), &change.into_entry(project, &f.id)?)?;
    find(project, &f.id)
}

/// The project finding already promoted from this session finding, if any.
pub fn promoted_from<'a>(findings: &'a [Finding], session: &str, finding: &str) -> Option<&'a Finding> {
    findings.iter().find(|f| f.sources.iter().any(|s| s.session == session && s.finding == finding))
}

/// `(severity, count)` in severity order, unset last, empty buckets dropped.
pub fn severity_counts(findings: &[Finding]) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = SEVERITIES
        .iter()
        .map(|s| (s.to_string(), findings.iter().filter(|f| f.severity == *s).count()))
        .collect();
    out.push(("unrated".into(), findings.iter().filter(|f| f.severity.is_empty()).count()));
    out.retain(|(_, n)| *n > 0);
    out
}

/// Worst first, then oldest first.
pub fn sorted(mut findings: Vec<Finding>) -> Vec<Finding> {
    findings.sort_by(|a, b| a.severity_rank().cmp(&b.severity_rank()).then_with(|| a.created.cmp(&b.created)));
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> (tempfile::TempDir, Project) {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::init(tmp.path(), "p", "", "", vec![]).unwrap();
        (tmp, p)
    }

    #[test]
    fn severity_confidence_and_status_are_separate_fields() {
        let (_t, p) = project();
        let f = create(
            &p,
            Change {
                title: Some("invoice readable across tenants".into()),
                severity: Some("high".into()),
                state: Some("suspected".into()),
                ..Change::default()
            },
        )
        .unwrap();
        assert_eq!((f.severity.as_str(), f.state.as_str(), f.status.as_str()), ("high", "suspected", "open"));
        let f = update(&p, "F-1", Change { status: Some("fix-claimed".into()), ..Change::default() }).unwrap();
        assert_eq!(f.severity, "high");
        assert!(f.is_open(), "a claimed fix is not a verified one");
        let f = update(&p, "1", Change { status: Some("fixed-verified".into()), ..Change::default() }).unwrap();
        assert!(!f.is_open());
    }

    #[test]
    fn an_unknown_severity_or_status_is_refused() {
        let (_t, p) = project();
        let bad = Change { title: Some("x".into()), severity: Some("severe".into()), ..Change::default() };
        assert!(create(&p, bad).is_err());
        let bad = Change { title: Some("x".into()), status: Some("done".into()), ..Change::default() };
        assert!(create(&p, bad).is_err());
    }

    #[test]
    fn evidence_must_exist_in_the_project() {
        let (_t, p) = project();
        let c = Change { title: Some("x".into()), evidence: vec!["E-4".into()], ..Change::default() };
        assert!(create(&p, c).unwrap_err().to_string().contains("E-4"));
    }

    #[test]
    fn worst_first() {
        let mk = |id: &str, sev: &str| Finding { id: id.into(), severity: sev.into(), ..Finding::default() };
        let s = sorted(vec![mk("F-1", "low"), mk("F-2", ""), mk("F-3", "critical")]);
        assert_eq!(s.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["F-3", "F-1", "F-2"]);
    }
}
