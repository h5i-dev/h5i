//! Checklists imported from Markdown: an internal standard, a client's list,
//! last round's notes. h5i keeps the original, parses its list items, and
//! records an outcome against each. It does not say how an item is tested.
//!
//! `reference` items are context. `required` items must each end with an
//! outcome (`recorded`, `blocked`, `not-applicable`), and replacing the list
//! keeps a record of every item that was dropped.

use serde::{Deserialize, Serialize};

use super::{Project, Result, append_line, bail, bounded, now, read_lines, sha256_hex, write_private};

pub const DIR: &str = "checklists";
pub const STATUSES: [&str; 5] = ["open", "in-progress", "recorded", "blocked", "not-applicable"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Reference,
    Required,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Mode> {
        match s {
            "reference" => Ok(Mode::Reference),
            "required" => Ok(Mode::Required),
            _ => bail!("`{s}` is not a checklist mode: use `reference` or `required`"),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Reference => "reference",
            Mode::Required => "required",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    #[serde(default)]
    pub group: String,
    pub text: String,
}

/// An item a replacement dropped, and when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dropped {
    pub item: Item,
    pub at: String,
    pub version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checklist {
    pub slug: String,
    pub title: String,
    pub mode: Mode,
    pub version: u32,
    pub imported: String,
    pub source: String,
    pub digest: String,
    pub items: Vec<Item>,
    #[serde(default)]
    pub dropped: Vec<Dropped>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeEntry {
    pub item: String,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub item: Item,
    pub status: String,
    pub notes: Vec<(String, String)>,
    pub links: Vec<String>,
    pub updated: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Coverage {
    pub slug: String,
    pub title: String,
    pub mode: Mode,
    pub version: u32,
    pub total: usize,
    pub by_status: Vec<(String, usize)>,
    /// Items with a final outcome: recorded, blocked or not applicable.
    pub closed: usize,
    pub dropped: usize,
}

impl Coverage {
    pub fn count(&self, status: &str) -> usize {
        self.by_status.iter().find(|(s, _)| s == status).map(|(_, n)| *n).unwrap_or(0)
    }

    /// "18 of 24 have an outcome; 4 open; 2 blocked". Never a percentage.
    pub fn sentence(&self) -> String {
        let mut parts = vec![format!("{} of {} items have an outcome", self.closed, self.total)];
        for s in ["open", "in-progress", "blocked", "not-applicable"] {
            let n = self.count(s);
            if n > 0 {
                parts.push(format!("{n} {s}"));
            }
        }
        if self.dropped > 0 {
            parts.push(format!("{} dropped in a revision", self.dropped));
        }
        parts.join("; ")
    }
}

/// Parse a Markdown checklist. Every list item is an item; the nearest heading
/// is its group. Code fences are skipped.
pub fn parse(markdown: &str) -> (String, Vec<(String, String)>) {
    let mut title = String::new();
    let mut group = String::new();
    let mut items = Vec::new();
    let mut fenced = false;
    for line in markdown.lines() {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || t.is_empty() {
            continue;
        }
        if let Some(h) = t.strip_prefix('#') {
            let h = h.trim_start_matches('#').trim().to_string();
            if title.is_empty() {
                title = h.clone();
            }
            group = h;
            continue;
        }
        let body = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("+ "))
            .or_else(|| {
                let digits = t.chars().take_while(char::is_ascii_digit).count();
                (digits > 0).then(|| &t[digits..]).and_then(|r| r.strip_prefix(". ").or_else(|| r.strip_prefix(") ")))
            });
        let Some(body) = body else { continue };
        let body = body
            .strip_prefix("[ ] ")
            .or_else(|| body.strip_prefix("[x] "))
            .or_else(|| body.strip_prefix("[X] "))
            .unwrap_or(body)
            .trim();
        if !body.is_empty() {
            items.push((group.clone(), body.to_string()));
        }
    }
    (title, items)
}

fn slug_ok(slug: &str) -> Result<&str> {
    if slug.is_empty()
        || slug.len() > 48
        || !slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!("`{slug}` is not a usable checklist name: lowercase letters, digits and `-`");
    }
    Ok(slug)
}

pub fn slug_from(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-').to_string();
    if s.is_empty() { "checklist".into() } else { s.chars().take(48).collect() }
}

fn meta_path(project: &Project, slug: &str) -> std::path::PathBuf {
    project.path(DIR).join(format!("{slug}.json"))
}

pub fn list(project: &Project) -> Vec<Checklist> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(project.path(DIR)) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json")
            && let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(c) = serde_json::from_str::<Checklist>(&text)
        {
            out.push(c);
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

pub fn get(project: &Project, slug: &str) -> Result<Checklist> {
    let slug = slug_ok(slug)?;
    match std::fs::read_to_string(meta_path(project, slug)) {
        Ok(text) => Ok(serde_json::from_str(&text)?),
        Err(_) => bail!("project {} has no checklist `{slug}`", project.meta.name),
    }
}

/// Import, or with `replace` revise, a checklist. A revision keeps the id of
/// every item whose text did not change.
pub fn import(project: &Project, slug: &str, source: &str, markdown: &str, mode: Mode, replace: bool) -> Result<Checklist> {
    let slug = slug_ok(slug)?;
    let (title, parsed) = parse(markdown);
    if parsed.is_empty() {
        bail!("{source} has no list items, so there is nothing to track");
    }
    let previous = get(project, slug).ok();
    if previous.is_some() && !replace {
        bail!("project {} already has checklist `{slug}`: pass --replace to revise it", project.meta.name);
    }
    let at = now();
    let mut next_n = previous
        .iter()
        .flat_map(|p| p.items.iter().chain(p.dropped.iter().map(|d| &d.item)))
        .filter_map(|i| i.id.strip_prefix('C')?.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    let mut items = Vec::new();
    for (group, text) in parsed {
        let reused = previous
            .as_ref()
            .and_then(|p| p.items.iter().find(|i| i.text == text && !items.iter().any(|x: &Item| x.id == i.id)));
        let id = match reused {
            Some(i) => i.id.clone(),
            None => {
                next_n += 1;
                format!("C{next_n}")
            }
        };
        items.push(Item { id, group, text: bounded("checklist item", &text)? });
    }
    let version = previous.as_ref().map(|p| p.version + 1).unwrap_or(1);
    let mut dropped = previous.as_ref().map(|p| p.dropped.clone()).unwrap_or_default();
    if let Some(p) = &previous {
        for old in &p.items {
            if !items.iter().any(|i| i.id == old.id) {
                dropped.push(Dropped { item: old.clone(), at: at.clone(), version });
            }
        }
        // The superseded original stays readable beside the new one.
        let _ = std::fs::copy(
            project.path(DIR).join(format!("{slug}.md")),
            project.path(DIR).join(format!("{slug}.v{}.md", p.version)),
        );
    }
    let checklist = Checklist {
        slug: slug.to_string(),
        title: if title.is_empty() { slug.to_string() } else { title },
        mode,
        version,
        imported: at,
        source: source.to_string(),
        digest: format!("sha256:{}", sha256_hex(markdown.as_bytes())),
        items,
        dropped,
    };
    write_private(&project.path(DIR).join(format!("{slug}.md")), markdown.as_bytes())?;
    write_private(&meta_path(project, slug), serde_json::to_string_pretty(&checklist)?.as_bytes())?;
    Ok(checklist)
}

fn outcomes_path(project: &Project, slug: &str) -> std::path::PathBuf {
    project.path(DIR).join(format!("{slug}.jsonl"))
}

/// Record against `C3` (or `3`) of `slug`.
pub fn mark(project: &Project, slug: &str, item: &str, status: Option<&str>, note: Option<&str>, links: Vec<String>) -> Result<Outcome> {
    let c = get(project, slug)?;
    let want = if item.starts_with('C') || item.starts_with('c') { item[1..].to_string() } else { item.to_string() };
    let want = format!("C{}", want.parse::<u64>().map_err(|_| super::ProjectError(format!("`{item}` is not an item id: try `C3`")))?);
    if !c.items.iter().any(|i| i.id == want) {
        bail!("checklist `{slug}` has no {want}");
    }
    if let Some(s) = status
        && !STATUSES.contains(&s)
    {
        bail!("`{s}` is not an item status: use one of {}", STATUSES.join(", "));
    }
    if status.is_none() && note.is_none() && links.is_empty() {
        bail!("nothing to record on {want}");
    }
    if matches!(status, Some("blocked" | "not-applicable")) && note.is_none_or(|n| n.trim().is_empty()) {
        bail!("say why with --note: a blocked or not-applicable item without a reason reads as skipped");
    }
    for link in &links {
        check_link(project, link)?;
    }
    append_line(
        &outcomes_path(project, slug),
        &OutcomeEntry {
            item: want.clone(),
            at: now(),
            status: status.map(str::to_string),
            note: note.map(|n| bounded("note", n)).transpose()?,
            links,
        },
    )?;
    outcomes(project, &c)?
        .into_iter()
        .find(|o| o.item.id == want)
        .ok_or_else(|| super::ProjectError("the outcome was written but did not read back".into()))
}

fn check_link(project: &Project, link: &str) -> Result<()> {
    let found = match link.chars().next() {
        Some('F') => super::finding::find(project, link).is_ok(),
        Some('N') => super::note::find(project, link).is_ok(),
        Some('E') => super::evidence::get(project, link).is_ok(),
        _ => false,
    };
    if !found {
        bail!("`{link}` names nothing in project {}: link an F-, N- or E- id that exists", project.meta.name);
    }
    Ok(())
}

pub fn outcomes(project: &Project, c: &Checklist) -> Result<Vec<Outcome>> {
    let entries: Vec<OutcomeEntry> = read_lines(&outcomes_path(project, &c.slug))?;
    Ok(c.items
        .iter()
        .map(|item| {
            let mut o = Outcome { item: item.clone(), status: "open".into(), notes: Vec::new(), links: Vec::new(), updated: None };
            for e in entries.iter().filter(|e| e.item == item.id) {
                if let Some(s) = &e.status {
                    o.status = s.clone();
                }
                if let Some(n) = &e.note {
                    o.notes.push((e.at.clone(), n.clone()));
                }
                for l in &e.links {
                    if !o.links.contains(l) {
                        o.links.push(l.clone());
                    }
                }
                o.updated = Some(e.at.clone());
            }
            o
        })
        .collect())
}

pub fn coverage(c: &Checklist, outcomes: &[Outcome]) -> Coverage {
    let by_status: Vec<(String, usize)> = STATUSES
        .iter()
        .map(|s| (s.to_string(), outcomes.iter().filter(|o| o.status == *s).count()))
        .filter(|(_, n)| *n > 0)
        .collect();
    Coverage {
        slug: c.slug.clone(),
        title: c.title.clone(),
        mode: c.mode,
        version: c.version,
        total: c.items.len(),
        closed: outcomes.iter().filter(|o| matches!(o.status.as_str(), "recorded" | "blocked" | "not-applicable")).count(),
        by_status,
        dropped: c.dropped.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "# Web checks\n\n## Auth\n- [ ] Login rate limit\n- [x] Session fixation\n\n## Access\n1. Cross-tenant reads\n\n```\n- not an item\n```\n";

    #[test]
    fn list_items_become_items_grouped_by_heading() {
        let (title, items) = parse(LIST);
        assert_eq!(title, "Web checks");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], ("Auth".to_string(), "Login rate limit".to_string()));
        assert_eq!(items[2].0, "Access");
    }

    #[test]
    fn a_revision_keeps_ids_and_records_what_it_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::init(tmp.path(), "p", "", "", vec![]).unwrap();
        let c = import(&p, "web", "web.md", LIST, Mode::Required, false).unwrap();
        assert_eq!(c.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["C1", "C2", "C3"]);
        assert!(import(&p, "web", "web.md", LIST, Mode::Required, false).is_err(), "no silent overwrite");
        let revised = "## Auth\n- Login rate limit\n- Password reset\n";
        let c = import(&p, "web", "web.md", revised, Mode::Required, true).unwrap();
        assert_eq!(c.items[0].id, "C1");
        assert_eq!(c.items[1].id, "C4", "a new item gets a new id");
        assert_eq!(c.dropped.len(), 2);
        assert_eq!(c.version, 2);
    }

    #[test]
    fn a_blocked_item_needs_a_reason_and_coverage_counts_it_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::init(tmp.path(), "p", "", "", vec![]).unwrap();
        let c = import(&p, "web", "web.md", LIST, Mode::Required, false).unwrap();
        assert!(mark(&p, "web", "C1", Some("blocked"), None, vec![]).is_err());
        mark(&p, "web", "C1", Some("blocked"), Some("no admin account"), vec![]).unwrap();
        mark(&p, "web", "2", Some("recorded"), Some("fixation not possible: id rotates"), vec![]).unwrap();
        assert!(mark(&p, "web", "C3", Some("recorded"), None, vec!["F-9".into()]).is_err());
        let cov = coverage(&c, &outcomes(&p, &c).unwrap());
        assert_eq!((cov.closed, cov.total, cov.count("open")), (2, 3, 1));
        assert!(cov.sentence().starts_with("2 of 3 items have an outcome"));
    }
}
