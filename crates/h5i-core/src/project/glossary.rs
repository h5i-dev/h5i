//! The glossary a report links terms to: built in, versioned, and extended or
//! overridden by a project's own `glossary.toml`.

use serde::{Deserialize, Serialize};

use super::{Project, Result};

const BUILTIN: &str = include_str!("glossary.toml");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Term {
    pub id: String,
    pub term: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub short: String,
    pub explain: String,
    /// Where a reader can learn more: an OWASP page, Wikipedia, MDN. One line
    /// in a definition panel, a footnote in the PDF. Optional, and only ever
    /// `http(s)`; the renderer drops anything else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// What to call the link ("OWASP", "Wikipedia"). Defaults to the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_label: Option<String>,
    /// `builtin` or `project`.
    #[serde(default)]
    pub origin: String,
}

impl Term {
    /// The link only when it is a plain http(s) URL, with a label to show for
    /// it. A project glossary is data the agent wrote, so a `javascript:` here
    /// is refused rather than rendered.
    pub fn reference(&self) -> Option<(&str, String)> {
        let url = self.link.as_deref()?;
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("http://")) {
            return None;
        }
        let label = self
            .link_label
            .clone()
            .or_else(|| url::Url::parse(url).ok().and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_string())))
            .unwrap_or_else(|| "reference".into());
        Some((url, label))
    }
}

#[derive(Debug, Clone, Deserialize)]
struct File {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    term: Vec<Term>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Glossary {
    pub version: u32,
    pub terms: Vec<Term>,
}

impl Glossary {
    pub fn builtin() -> Glossary {
        let file: File = toml::from_str(BUILTIN).expect("the built-in glossary parses");
        Glossary {
            version: file.version,
            terms: file.term.into_iter().map(|t| Term { origin: "builtin".into(), ..t }).collect(),
        }
    }

    /// The built-in glossary with the project's terms laid over it.
    pub fn for_project(project: &Project) -> Result<Glossary> {
        let mut g = Glossary::builtin();
        let path = project.path("glossary.toml");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let file: File = toml::from_str(&text)
                .map_err(|e| super::ProjectError(format!("{} is not a usable glossary: {e}", path.display())))?;
            for t in file.term {
                let t = Term { origin: "project".into(), ..t };
                match g.terms.iter_mut().find(|have| have.id == t.id) {
                    Some(have) => *have = t,
                    None => g.terms.push(t),
                }
            }
        }
        Ok(g)
    }

    /// A term by id, name or alias, ignoring case.
    pub fn lookup(&self, key: &str) -> Option<&Term> {
        let k = key.trim().to_lowercase();
        self.terms.iter().find(|t| {
            t.id == k || t.term.to_lowercase() == k || t.aliases.iter().any(|a| a.to_lowercase() == k)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_glossary_parses_and_ids_are_unique() {
        let g = Glossary::builtin();
        assert!(g.terms.len() >= 30);
        let mut ids: Vec<&str> = g.terms.iter().map(|t| t.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), g.terms.len());
        assert_eq!(g.lookup("Insecure Direct Object Reference").unwrap().id, "idor");
        assert_eq!(g.lookup("xss").unwrap().id, "xss");
    }

    #[test]
    fn a_project_term_overrides_and_extends() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::init(tmp.path(), "p", "", "", vec![]).unwrap();
        std::fs::write(
            p.path("glossary.toml"),
            "[[term]]\nid = \"idor\"\nterm = \"IDOR\"\nshort = \"ours\"\nexplain = \"ours\"\n\
             [[term]]\nid = \"tenant\"\nterm = \"Tenant\"\nshort = \"a customer org\"\nexplain = \"x\"\n",
        )
        .unwrap();
        let g = Glossary::for_project(&p).unwrap();
        assert_eq!(g.lookup("idor").unwrap().short, "ours");
        assert_eq!(g.lookup("tenant").unwrap().origin, "project");
    }
}
