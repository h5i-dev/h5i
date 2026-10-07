//! Read-only app review. No checkout, subprocess, Lean parser or build occurs
//! on an HTTP request. Explanations and tool observations stay separate.
use h5i_app_report::{self as report, Model, Node, Run};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

const HISTORY_CAP: usize = 120;
const RUN_CAP: usize = 20;

#[derive(Serialize)]
pub struct Summary {
    pub id: String,
    pub title: String,
    pub description: String,
    pub guarantees: usize,
    pub issues: Vec<String>,
}

#[derive(Serialize)]
pub struct Detail {
    pub summary: Summary,
    pub model: Option<Model>,
    pub required_theorems: Vec<String>,
    pub files: Vec<String>,
    pub runs: Vec<RunView>,
    pub runs_total: usize,
    pub head: Option<String>,
    pub issues: Vec<String>,
}

#[derive(Serialize)]
pub struct RunView {
    pub run: Run,
    pub freshness: String,
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn project(root: &Path, id: &str) -> Result<PathBuf, String> {
    let p = if id == "." {
        root.canonicalize()
    } else {
        report::contained(root, id)
    }
    .map_err(|e| e.to_string())?;
    let manifest = p.join("h5i-app.toml");
    let canonical = manifest.canonicalize().map_err(|e| e.to_string())?;
    if !canonical.starts_with(root.canonicalize().map_err(|e| e.to_string())?) {
        return Err("manifest leaves repository".into());
    }
    if !manifest.is_file() {
        return Err("No h5i-app.toml at this project".into());
    }
    Ok(p)
}

fn model(root: &Path, p: &Path) -> Result<Option<Model>, String> {
    let path = p.join(report::MODEL);
    if !path.exists() {
        return Ok(None);
    }
    let path = report::contained(root, &relative(root, &path)).map_err(|e| e.to_string())?;
    let text = report::read_text(&path).map_err(|e| e.to_string())?;
    let m: Model = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", report::MODEL))?;
    m.validate()?;
    Ok(Some(m))
}

fn summary(root: &Path, p: &Path) -> Summary {
    let mut id = relative(root, p);
    if id.is_empty() {
        id = ".".into();
    }
    let mut s = Summary {
        id,
        title: p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        description: "Proof project · explanation not authored yet".into(),
        guarantees: 0,
        issues: vec![],
    };
    match model(root, p) {
        Ok(Some(m)) => {
            s.title = m.title;
            s.description = m.description;
            s.guarantees = m.nodes.iter().filter(|n| n.kind == "guarantee").count();
        }
        Err(e) => s.issues.push(e),
        _ => {}
    }
    s
}

pub fn discover(root: &Path) -> Result<Vec<Summary>, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let files = report::files(&root).map_err(|e| e.to_string())?;
    Ok(files
        .iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "h5i-app.toml"))
        .map(|p| summary(&root, p.parent().unwrap()))
        .collect())
}

fn source_files(root: &Path, p: &Path, m: Option<&Model>) -> Result<Vec<String>, String> {
    let mut files = BTreeSet::new();
    for path in report::files(p).map_err(|e| e.to_string())? {
        if path.extension().is_some_and(|e| e == "rs" || e == "lean")
            || path.file_name().is_some_and(|n| n == "h5i-app.toml")
        {
            files.insert(relative(root, &path));
        }
    }
    if let Some(m) = m {
        for n in &m.nodes {
            for s in &n.sources {
                files.insert(s.path.clone());
            }
        }
    }
    Ok(files.into_iter().collect())
}

fn read_runs(root: &Path, p: &Path) -> (Vec<RunView>, usize, Vec<String>) {
    let mut issues = vec![];
    let dir = p.join(report::RUNS);
    if !dir.exists() {
        return (vec![], 0, issues);
    }
    if report::contained(root, &relative(root, &dir)).is_err() {
        return (vec![], 0, vec!["Run directory leaves repository".into()]);
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => return (vec![], 0, vec![e.to_string()]),
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    let total = paths.len();
    paths.sort();
    paths.reverse();
    paths.truncate(RUN_CAP);
    let current = report::inputs(root);
    let runs = paths
        .into_iter()
        .filter_map(|p| {
            let result = report::contained(root, &relative(root, &p.join("run.json")))
                .and_then(|p| report::read_text(&p));
            let result = result
                .map_err(|e| e.to_string())
                .and_then(|s| serde_json::from_str::<Run>(&s).map_err(|e| e.to_string()));
            match result {
                Ok(run) if run.version == 1 => {
                    let freshness = if run.inputs_changed {
                        "changed_during_run"
                    } else if !run.inputs.errors.is_empty() || !current.errors.is_empty() {
                        "unknown"
                    } else if current.digest == run.inputs.digest {
                        "matches_repository_inputs"
                    } else {
                        "stale"
                    };
                    Some(RunView {
                        run,
                        freshness: freshness.into(),
                    })
                }
                Ok(_) => {
                    issues.push(format!("{}: unsupported record version", p.display()));
                    None
                }
                Err(e) => {
                    issues.push(format!(
                        "{}: {e}",
                        p.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    None
                }
            }
        })
        .collect();
    (runs, total, issues)
}

pub fn detail(root: &Path, id: &str) -> Result<Detail, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let p = project(&root, id)?;
    let summary = summary(&root, &p);
    let mut issues = summary.issues.clone();
    let model = model(&root, &p).unwrap_or(None);
    let text = report::read_text(&p.join("h5i-app.toml")).map_err(|e| e.to_string())?;
    let manifest = text.parse::<toml::Value>().map_err(|e| e.to_string())?;
    let required_theorems = manifest
        .get("check")
        .and_then(|v| v.get("theorems"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let files = source_files(&root, &p, model.as_ref())?;
    if let Some(m) = &model {
        for n in &m.nodes {
            for source in &n.sources {
                match report::contained(&root, &source.path).and_then(|p| report::read_text(&p)) {
                    Ok(text) => {
                        if !source.anchor.is_empty() && text.matches(&source.anchor).count() != 1 {
                            issues.push(format!(
                                "{}: source anchor missing or ambiguous in {}",
                                n.title, source.path
                            ));
                        }
                        if source
                            .digest
                            .as_ref()
                            .is_some_and(|d| d != &report::digest(text.as_bytes()))
                        {
                            issues.push(format!(
                                "{}: explanation's source fingerprint is stale",
                                n.title
                            ));
                        }
                    }
                    Err(e) => issues.push(format!("{}: {}: {e}", n.title, source.path)),
                }
            }
        }
    }
    let (runs, runs_total, run_issues) = read_runs(&root, &p);
    issues.extend(run_issues);
    let head = git2::Repository::discover(&root).ok().and_then(|r| {
        r.head()
            .ok()
            .and_then(|h| h.target().map(|id| id.to_string()))
    });
    Ok(Detail {
        summary,
        model,
        required_theorems,
        files,
        runs,
        runs_total,
        head,
        issues,
    })
}

#[derive(Serialize)]
pub struct SourceView {
    pub path: String,
    pub revision: String,
    pub text: String,
    pub start_line: usize,
    pub total_lines: usize,
    pub anchor_found: bool,
    pub anchor_line: Option<usize>,
}

fn blob(repo: &git2::Repository, tree: &git2::Tree, path: &str) -> Option<String> {
    let entry = tree.get_path(Path::new(path)).ok()?;
    if entry.filemode() == 0o120000 {
        return None;
    }
    let blob = repo.find_blob(entry.id()).ok()?;
    if blob.size() as u64 > report::MAX_FILE {
        return None;
    }
    std::str::from_utf8(blob.content()).ok().map(str::to_string)
}

pub fn source(
    root: &Path,
    id: &str,
    path: &str,
    revision: Option<&str>,
    anchor: &str,
) -> Result<SourceView, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let p = project(&root, id)?;
    let m = model(&root, &p)?;
    if !source_files(&root, &p, m.as_ref())?
        .iter()
        .any(|f| f == path)
    {
        return Err("Source is not in this project's inventory".into());
    }
    let (text, revision) = match revision.filter(|r| *r != "working") {
        Some(rev) => {
            let repo = git2::Repository::discover(&root).map_err(|e| e.to_string())?;
            let commit = repo
                .revparse_single(rev)
                .and_then(|o| o.peel_to_commit())
                .map_err(|e| e.to_string())?;
            (
                blob(&repo, &commit.tree().map_err(|e| e.to_string())?, path)
                    .ok_or("Source absent or unreadable at this revision")?,
                commit.id().to_string(),
            )
        }
        None => (
            report::read_text(&report::contained(&root, path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
            "working".into(),
        ),
    };
    let lines: Vec<_> = text.lines().collect();
    let matching: Vec<_> = if anchor.is_empty() {
        vec![]
    } else {
        lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains(anchor))
            .map(|(i, _)| i)
            .collect()
    };
    let found = anchor.is_empty() || matching.len() == 1;
    let start = matching.first().copied().unwrap_or(0).saturating_sub(3);
    Ok(SourceView {
        path: path.into(),
        revision,
        text: lines
            .iter()
            .skip(start)
            .take(160)
            .copied()
            .collect::<Vec<_>>()
            .join("\n"),
        start_line: start + 1,
        total_lines: lines.len(),
        anchor_found: found,
        anchor_line: if matching.len() == 1 {
            Some(matching[0] + 1)
        } else {
            None
        },
    })
}

#[derive(Serialize)]
pub struct Change {
    pub node: String,
    pub title: String,
    pub reason: String,
    pub path: Option<String>,
    pub before: String,
    pub after: String,
    pub affected: Vec<String>,
}
#[derive(Serialize)]
pub struct Revision {
    pub commit: String,
    pub parent: Option<String>,
    pub time: i64,
    pub message: String,
    pub changes: Vec<Change>,
}
#[derive(Serialize)]
pub struct History {
    pub baseline: String,
    pub comparison: Vec<Change>,
    pub catalog_comparison: Vec<Change>,
    pub catalog_note: String,
    pub revisions: Vec<Revision>,
    pub scanned: usize,
    pub truncated: bool,
    pub shallow: bool,
    pub notes: Vec<String>,
}

fn compare_catalogs(model: &Model, old: &Run, new: &Run) -> Vec<Change> {
    let mut changes = Vec::new();
    let names: BTreeSet<_> = old
        .declarations
        .iter()
        .chain(&new.declarations)
        .map(|d| &d.name)
        .collect();
    for name in names {
        let before = old.declarations.iter().find(|d| &d.name == name);
        let after = new.declarations.iter().find(|d| &d.name == name);
        let signature = |d: Option<&report::Declaration>| {
            d.map(|d| format!("{}\n{}", d.signature, d.definition.as_deref().unwrap_or("")))
                .unwrap_or_default()
        };
        let b = signature(before);
        let a = signature(after);
        if b == a {
            continue;
        }
        let mut reached = BTreeSet::from([name.to_string()]);
        loop {
            let before = reached.len();
            for d in old.declarations.iter().chain(&new.declarations) {
                if d.dependencies.iter().any(|n| reached.contains(n)) {
                    reached.insert(d.name.clone());
                }
            }
            if reached.len() == before {
                break;
            }
        }
        let mut guarantees = BTreeSet::new();
        for n in &model.nodes {
            if n.symbol.as_ref().is_some_and(|s| reached.contains(s)) {
                guarantees.extend(affected(model, &n.id));
            }
        }
        changes.push(Change {
            node: name.to_string(),
            title: name.to_string(),
            reason: "Recorded elaborated type or definition changed".into(),
            path: None,
            before: b,
            after: a,
            affected: guarantees.into_iter().collect(),
        });
    }
    changes
}

fn affected(model: &Model, id: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([id.to_string()]);
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        for e in &model.edges {
            if e.view == "proof" && e.to == id {
                queue.push_back(e.from.clone());
            }
        }
    }
    model
        .nodes
        .iter()
        .filter(|n| n.kind == "guarantee" && seen.contains(&n.id))
        .map(|n| n.title.clone())
        .collect()
}

fn excerpt_change(before: &str, after: &str) -> (String, String) {
    let b: Vec<_> = before.lines().collect();
    let a: Vec<_> = after.lines().collect();
    let first = b
        .iter()
        .zip(&a)
        .take_while(|(x, y)| x == y)
        .count()
        .saturating_sub(3);
    let excerpt = |ls: &[&str]| {
        ls.iter()
            .skip(first)
            .take(50)
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
    };
    (excerpt(&b), excerpt(&a))
}

fn compare(
    current: &Model,
    previous: Option<&Model>,
    before: impl Fn(&str) -> Option<String>,
    after: impl Fn(&str) -> Option<String>,
) -> Vec<Change> {
    let mut changes = vec![];
    let old: BTreeMap<_, _> = previous
        .map(|m| m.nodes.iter().map(|n| (&n.id, n)).collect())
        .unwrap_or_default();
    let mut nodes: Vec<&Node> = current.nodes.iter().collect();
    if let Some(p) = previous {
        nodes.extend(
            p.nodes
                .iter()
                .filter(|n| !current.nodes.iter().any(|c| c.id == n.id)),
        );
    }
    let mut watched = BTreeSet::new();
    for n in nodes {
        let new = current.nodes.iter().find(|c| c.id == n.id);
        if previous.is_some() && new != old.get(&n.id).copied() {
            let b = old
                .get(&n.id)
                .map(|n| serde_json::to_string_pretty(n).unwrap())
                .unwrap_or_default();
            let a = new
                .map(|n| serde_json::to_string_pretty(n).unwrap())
                .unwrap_or_default();
            let (before, after) = excerpt_change(&b, &a);
            changes.push(Change {
                node: n.id.clone(),
                title: n.title.clone(),
                reason: if new.is_none() {
                    "Node removed"
                } else if !old.contains_key(&n.id) {
                    "Node added"
                } else {
                    "Authored conditions or explanation changed"
                }
                .into(),
                path: None,
                before,
                after,
                affected: affected(current, &n.id),
            });
        }
        for s in &n.sources {
            if !watched.insert((n.id.clone(), s.path.clone())) {
                continue;
            }
            let b = before(&s.path);
            let a = after(&s.path);
            if b == a {
                continue;
            }
            let (before, after) = excerpt_change(
                b.as_deref().unwrap_or("[absent or unreadable]"),
                a.as_deref().unwrap_or("[absent or unreadable]"),
            );
            changes.push(Change {
                node: n.id.clone(),
                title: n.title.clone(),
                reason: "Referenced file changed · semantic impact needs review".into(),
                path: Some(s.path.clone()),
                before,
                after,
                affected: affected(current, &n.id),
            });
        }
    }
    if previous.is_some_and(|p| p.edges != current.edges || p.exclusions != current.exclusions) {
        changes.push(Change {
            node: "relationships".into(),
            title: "Relationships and exclusions".into(),
            reason: "Authored scope changed".into(),
            path: None,
            before: serde_json::to_string_pretty(&(
                previous.unwrap().edges.clone(),
                &previous.unwrap().exclusions,
            ))
            .unwrap(),
            after: serde_json::to_string_pretty(&(&current.edges, &current.exclusions)).unwrap(),
            affected: current
                .nodes
                .iter()
                .filter(|n| n.kind == "guarantee")
                .map(|n| n.title.clone())
                .collect(),
        });
    }
    changes
}

pub fn history(root: &Path, id: &str, baseline: &str) -> Result<History, String> {
    if baseline.len() > 200 {
        return Err("Revision is too long".into());
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let p = project(&root, id)?;
    let repo = git2::Repository::discover(&root).map_err(|e| e.to_string())?;
    let mut current = model(&root, &p)?.unwrap_or(Model {
        version: 1,
        title: id.into(),
        description: String::new(),
        author: "manifest inventory".into(),
        exclusions: vec![],
        nodes: vec![],
        edges: vec![],
    });
    // Without an authored map, still expose source history, never a fabricated
    // assumption inventory.
    if current.nodes.is_empty() {
        for path in source_files(&root, &p, None)?.into_iter().take(200) {
            current.nodes.push(Node {
                id: path.clone(),
                kind: "implementation".into(),
                title: path.clone(),
                description: String::new(),
                symbol: None,
                sources: vec![report::Source {
                    path,
                    anchor: String::new(),
                    digest: None,
                }],
                excludes: vec![],
            });
        }
    }
    let model_path = relative(&root, &p.join(report::MODEL));
    let read_model = |t: &git2::Tree| {
        blob(&repo, t, &model_path)
            .and_then(|s| serde_json::from_str::<Model>(&s).ok())
            .filter(|m| m.validate().is_ok())
    };
    let base = repo
        .revparse_single(baseline)
        .and_then(|o| o.peel_to_commit())
        .map_err(|e| e.to_string())?;
    let base_tree = base.tree().map_err(|e| e.to_string())?;
    let comparison = compare(
        &current,
        read_model(&base_tree).as_ref(),
        |path| blob(&repo, &base_tree, path),
        |path| {
            report::contained(&root, path)
                .ok()
                .and_then(|p| report::read_text(&p).ok())
        },
    );
    let (runs, _, _) = read_runs(&root, &p);
    let old_run = runs.iter().find(|v| {
        v.run.kind == "check"
            && v.run.status == "passed"
            && !v.run.inputs.dirty
            && !v.run.inputs_changed
            && v.run.inputs.head.as_deref() == Some(&base.id().to_string())
    });
    let new_run = runs.iter().find(|v| {
        v.run.kind == "check"
            && v.run.status == "passed"
            && v.freshness == "matches_repository_inputs"
    });
    let (catalog_comparison, catalog_note) = match (old_run, new_run) {
        (Some(old), Some(new)) => (compare_catalogs(&current, &old.run, &new.run), format!("Tool-observed declarations: {} → {}. Pretty-printed types/definitions are compared; logical implication is not inferred.", old.run.id, new.run.id)),
        _ => (vec![], "Semantic comparison unavailable: requires a passing check recorded on a clean baseline commit and a passing check matching current repository inputs, within the 20 most recent records.".into()),
    };
    let mut cursor = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|e| e.to_string())?;
    let mut revisions = Vec::new();
    let mut scanned = 0;
    let mut truncated = false;
    loop {
        if scanned == HISTORY_CAP {
            truncated = true;
            break;
        }
        scanned += 1;
        let tree = cursor.tree().map_err(|e| e.to_string())?;
        let parent = cursor.parent(0).ok();
        let Some(parent) = parent else { break };
        let previous_tree = parent.tree().map_err(|e| e.to_string())?;
        let at = read_model(&tree).unwrap_or_else(|| current.clone());
        let old = read_model(&previous_tree);
        let changes = compare(
            &at,
            old.as_ref(),
            |path| blob(&repo, &previous_tree, path),
            |path| blob(&repo, &tree, path),
        );
        if !changes.is_empty() {
            revisions.push(Revision {
                commit: cursor.id().to_string(),
                parent: Some(parent.id().to_string()),
                time: cursor.time().seconds(),
                message: cursor.summary().unwrap_or("").into(),
                changes,
            });
        }
        cursor = parent;
    }
    Ok(History { baseline: base.id().to_string(), comparison, catalog_comparison, catalog_note, revisions, scanned, truncated, shallow: repo.is_shallow(), notes: vec![
        "Source-based review: file changes can affect a theorem without changing its signature. No claim of stronger/weaker assumptions is inferred.".into(),
        "First-parent history, up to 120 commits. Where historical explanations are absent, current source mappings are used; renamed symbols are not matched automatically.".into(),
        "Diff excerpts show up to 50 lines around the first difference. Missing historical catalogs leave semantic comparisons unknown.".into(),
    ] })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn model_fixture() -> Model {
        serde_json::from_str(r#"{"version":1,"title":"Booking","description":"","author":"agent","nodes":[{"id":"g","kind":"guarantee","title":"No overlap","description":""},{"id":"t","kind":"theorem","title":"Invariant","description":""},{"id":"a","kind":"assumption","title":"Reachable","description":"","sources":[{"path":"Spec.lean"}]}],"edges":[{"from":"g","to":"t","kind":"proved by"},{"from":"t","to":"a","kind":"assumes"}]}"#).unwrap()
    }
    #[test]
    fn indirect_changes_reach_guarantees_and_cycles_terminate() {
        let mut m = model_fixture();
        m.edges.push(report::Edge {
            from: "a".into(),
            to: "t".into(),
            kind: "references".into(),
            view: "proof".into(),
        });
        let c = compare(
            &m,
            Some(&m),
            |_| Some("def Reachable := True".into()),
            |_| Some("def Reachable := False".into()),
        );
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].affected, ["No overlap"]);
        assert!(c[0].before.contains("True"));
    }
    #[test]
    fn removing_an_authored_condition_is_not_silently_unchanged() {
        let old = model_fixture();
        let mut new = old.clone();
        new.nodes.pop();
        new.edges.pop();
        let changes = compare(&new, Some(&old), |_| None, |_| None);
        assert!(changes.iter().any(|c| c.reason == "Node removed"));
    }

    #[test]
    fn catalog_definition_changes_propagate_through_unmapped_helpers() {
        let mut m = model_fixture();
        m.nodes[1].symbol = Some("App.safe".into());
        let d = tempfile::tempdir().unwrap();
        let r = report::Recorder::start(d.path(), d.path(), "check", vec![]).unwrap();
        let mut before = r.run.clone();
        before.declarations = vec![
            report::Declaration {
                name: "App.safe".into(),
                kind: "theorem".into(),
                signature: "Safe".into(),
                definition: None,
                dependencies: vec!["Hidden.condition".into()],
                axioms: vec![],
            },
            report::Declaration {
                name: "Hidden.condition".into(),
                kind: "definition".into(),
                signature: "Prop".into(),
                definition: Some("True".into()),
                dependencies: vec![],
                axioms: vec![],
            },
        ];
        let mut after = before.clone();
        after.declarations[1].definition = Some("False".into());
        let changes = compare_catalogs(&m, &before, &after);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].affected, ["No overlap"]);
        assert!(changes[0].before.contains("True"));
        assert!(changes[0].after.contains("False"));
    }
    #[test]
    fn source_reports_only_unique_anchor_lines() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("h5i-app.toml"), "").unwrap();
        std::fs::write(d.path().join("Proof.lean"), "-- header\n-- context\ntheorem safe : True := by trivial\n-- repeated\n-- repeated\n").unwrap();
        let view = source(d.path(), ".", "Proof.lean", None, "theorem safe").unwrap();
        assert_eq!(view.anchor_line, Some(3));
        assert_eq!(view.start_line, 1);
        assert!(view.anchor_found);
        for anchor in ["missing", "repeated", ""] {
            let view = source(d.path(), ".", "Proof.lean", None, anchor).unwrap();
            assert_eq!(view.anchor_line, None);
            assert_eq!(view.anchor_found, anchor.is_empty());
        }
    }

    #[test]
    fn source_cannot_read_arbitrary_repository_or_host_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("h5i-app.toml"), "").unwrap();
        std::fs::write(d.path().join("secret.txt"), "secret").unwrap();
        assert!(source(d.path(), ".", "secret.txt", None, "").is_err());
        assert!(source(d.path(), "..", "secret.txt", None, "").is_err());
    }
}
