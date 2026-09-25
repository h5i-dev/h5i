//! Project notes: hypotheses, what the owner said, what is missing. A note is
//! never counted as a finding; `finding create --from-note` makes one.

use serde::{Deserialize, Serialize};

use super::{Project, Result, append_line, bail, bounded, next_id, normalise_id, now, read_lines};

pub const FILE: &str = "notes.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Note {
    pub id: String,
    pub text: String,
    pub tags: Vec<String>,
    pub archived: bool,
    pub created: String,
    pub updated: String,
}

pub fn fold(entries: &[Entry]) -> Vec<Note> {
    let mut out: Vec<Note> = Vec::new();
    for e in entries {
        let slot = match out.iter().position(|n| n.id == e.id) {
            Some(i) => i,
            None => {
                out.push(Note {
                    id: e.id.clone(),
                    text: String::new(),
                    tags: Vec::new(),
                    archived: false,
                    created: e.at.clone(),
                    updated: e.at.clone(),
                });
                out.len() - 1
            }
        };
        let n = &mut out[slot];
        if let Some(text) = &e.text {
            n.text = text.clone();
        }
        for tag in &e.tags {
            if !n.tags.contains(tag) {
                n.tags.push(tag.clone());
            }
        }
        if let Some(archived) = e.archived {
            n.archived = archived;
        }
        n.updated = e.at.clone();
    }
    out
}

pub fn read(project: &Project) -> Result<Vec<Note>> {
    Ok(fold(&read_lines::<Entry>(&project.path(FILE))?))
}

pub fn find(project: &Project, id: &str) -> Result<Note> {
    let want = normalise_id("N", id)?;
    match read(project)?.into_iter().find(|n| n.id == want) {
        Some(n) => Ok(n),
        None => bail!("project {} has no {want}", project.meta.name),
    }
}

pub fn add(project: &Project, text: &str, tags: Vec<String>) -> Result<Note> {
    if text.trim().is_empty() {
        bail!("a note needs some text");
    }
    let notes = read(project)?;
    let id = next_id("N", notes.iter().map(|n| n.id.as_str()));
    append_line(
        &project.path(FILE),
        &Entry { id: id.clone(), at: now(), text: Some(bounded("note", text)?), tags, archived: None },
    )?;
    find(project, &id)
}

pub fn edit(project: &Project, id: &str, text: Option<&str>, tags: Vec<String>, archived: Option<bool>) -> Result<Note> {
    let note = find(project, id)?;
    if text.is_none() && tags.is_empty() && archived.is_none() {
        bail!("nothing to change on {}", note.id);
    }
    append_line(
        &project.path(FILE),
        &Entry {
            id: note.id.clone(),
            at: now(),
            text: text.map(|t| bounded("note", t)).transpose()?,
            tags,
            archived,
        },
    )?;
    find(project, &note.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_is_edited_and_archived_without_losing_its_id() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::init(tmp.path(), "p", "", "", vec![]).unwrap();
        let n = add(&p, "admin only?", vec!["scope".into()]).unwrap();
        assert_eq!(n.id, "N-1");
        edit(&p, "1", Some("admin only: confirmed by owner"), vec![], None).unwrap();
        let n = edit(&p, "N-1", None, vec![], Some(true)).unwrap();
        assert_eq!(n.text, "admin only: confirmed by owner");
        assert!(n.archived);
        assert_eq!(add(&p, "second", vec![]).unwrap().id, "N-2");
    }
}
