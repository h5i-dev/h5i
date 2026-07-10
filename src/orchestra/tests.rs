//! Orchestra kernel tests: journal step keys + replay, resume without
//! re-execution, and the ensemble pattern end-to-end at workspace tier with a
//! scripted launcher standing in for the agents (no LLM, no sandbox probing —
//! envs are fabricated exactly like `team::tests` does).

use super::patterns::ensemble;
use super::*;
use crate::team::{TeamVerdict, PHASE_SEALED_SUBMIT};
use git2::Oid;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

fn sig() -> git2::Signature<'static> {
    git2::Signature::now("Test", "test@example.com").unwrap()
}

fn init_repo(dir: &Path) -> Repository {
    let repo = Repository::init(dir).unwrap();
    {
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
    }
    commit_file(&repo, "README.md", "hello\n");
    repo
}

fn commit_file(repo: &Repository, name: &str, body: &str) -> Oid {
    let work = repo.workdir().unwrap();
    fs::write(work.join(name), body).unwrap();
    let mut idx = repo.index().unwrap();
    idx.add_path(Path::new(name)).unwrap();
    idx.write().unwrap();
    let tree_oid = idx.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let parents = match repo.head().ok().and_then(|h| h.target()) {
        Some(oid) => vec![repo.find_commit(oid).unwrap()],
        None => vec![],
    };
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
    repo.commit(Some("HEAD"), &sig(), &sig(), "commit", &tree, &parent_refs)
        .unwrap()
}

/// Commit `file` onto `branch` via the object db only — no workdir, no index —
/// so parallel scripted agents never race and the tree stays clean for apply.
fn commit_on_branch(
    repo: &Repository,
    branch: &str,
    file: &str,
    body: &str,
) -> Result<Oid, H5iError> {
    let parent_oid = repo.refname_to_id(branch)?;
    let parent = repo.find_commit(parent_oid)?;
    let blob = repo.blob(body.as_bytes())?;
    let mut tb = repo.treebuilder(Some(&parent.tree()?))?;
    tb.insert(file, blob, 0o100644)?;
    let tree = repo.find_tree(tb.write()?)?;
    let oid = repo.commit(None, &sig(), &sig(), "scripted work", &tree, &[&parent])?;
    repo.reference(branch, oid, true, "scripted work")?;
    Ok(oid)
}

/// Fabricate a local env (manifest + branch ref) without `env::create`'s
/// sandbox probing — the same shortcut `team::tests` uses.
fn fabricate_env(repo: &Repository, h5i_root: &Path, agent: &str, slug: &str) -> env::EnvManifest {
    let head = repo.head().unwrap().target().unwrap().to_string();
    let branch = format!("refs/heads/h5i/env/{agent}/{slug}");
    repo.reference(&branch, Oid::from_str(&head).unwrap(), true, "env")
        .unwrap();
    let m = env::EnvManifest {
        id: format!("env/{agent}/{slug}"),
        agent: agent.into(),
        slug: slug.into(),
        base_commit: head.clone(),
        base_tree: repo
            .find_commit(Oid::from_str(&head).unwrap())
            .unwrap()
            .tree_id()
            .to_string(),
        parent_branch: "main".into(),
        branch,
        parent_context_branch: "main".into(),
        context_branch: format!("env/{agent}/{slug}"),
        profile: "workspace".into(),
        policy_digest: "policy".into(),
        isolation_claim: "workspace".into(),
        backend: "worktree".into(),
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
        status: env::ST_IDLE.into(),
        captures: vec![],
        service_digest: None,
        persona_digest: None,
        pr: None,
        pr_head_ref: None,
    };
    let dir = m.dir(h5i_root);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&m).unwrap(),
    )
    .unwrap();
    m
}

/// Scripted stand-in for resident agent sessions: on a Work/Revise turn it
/// commits a file onto the agent's env branch and submits; on a Review turn it
/// posts `review_body`. Counts every turn so resume tests can assert zero
/// re-execution.
struct Script {
    turns: AtomicUsize,
    review_body: Mutex<String>,
}

impl Script {
    fn launcher(script: &Arc<Self>) -> Arc<dyn RuntimeLauncher> {
        let script = script.clone();
        Arc::new(FnLauncher(move |turn: &TurnContext| {
            script.turns.fetch_add(1, Ordering::SeqCst);
            let repo = Repository::open(&turn.repo_workdir)?;
            let branch = format!("refs/heads/h5i/{}", turn.env_id);
            match &turn.kind {
                TurnKind::Work => {
                    commit_on_branch(
                        &repo,
                        &branch,
                        &format!("{}.txt", turn.agent_id),
                        "work\n",
                    )?;
                    team::submit(
                        &repo,
                        &turn.h5i_root,
                        &turn.run_id,
                        &turn.agent_id,
                        None,
                        Some("first attempt".into()),
                        &turn.agent_id,
                    )?;
                }
                TurnKind::Review { target } => {
                    let body = script.review_body.lock().unwrap().clone();
                    team::submit_review(
                        &repo,
                        &turn.h5i_root,
                        &turn.run_id,
                        &turn.agent_id,
                        target,
                        body,
                        &turn.agent_id,
                    )?;
                }
                TurnKind::Revise => {
                    commit_on_branch(
                        &repo,
                        &branch,
                        &format!("{}-revised.txt", turn.agent_id),
                        "revised\n",
                    )?;
                    team::submit(
                        &repo,
                        &turn.h5i_root,
                        &turn.run_id,
                        &turn.agent_id,
                        None,
                        Some("revised".into()),
                        &turn.agent_id,
                    )?;
                }
            }
            Ok(())
        }))
    }
}

fn conductor(dir: &Path, run: &str, launcher: Arc<dyn RuntimeLauncher>) -> Conductor {
    Conductor::builder(dir, run)
        .actor("human")
        .launcher(launcher)
        .poll_interval(Duration::from_millis(25))
        .turn_timeout(Duration::from_secs(20))
        .without_score_digest()
        .launch()
        .unwrap()
}

/// Smallest-diff policy that (unlike the built-in) needs no verifier evidence —
/// keeps the sandboxed verifier out of unit tests.
fn smallest_diff_policy() -> impl VerdictPolicy {
    policy::from_fn("smallest_diff_unverified", |run| {
        let mut latest: Vec<&crate::team::TeamArtifact> = run
            .agents
            .iter()
            .filter_map(|a| {
                a.latest_submission_id
                    .as_ref()
                    .and_then(|id| run.submissions.iter().find(|s| &s.id == id))
            })
            .collect();
        latest.sort_by(|a, b| {
            (a.files_changed, a.insertions, &a.id).cmp(&(b.files_changed, b.insertions, &b.id))
        });
        Ok(TeamVerdict {
            selected_submission: latest.first().map(|s| s.id.clone()),
            method: "rule:SmallestDiffUnverified".into(),
            decided_by: "test-policy".into(),
            can_auto_apply: !latest.is_empty(),
            reasons: vec!["smallest diff (test policy, no verifier gate)".into()],
        })
    })
}

#[tokio::test]
async fn step_journals_and_replays_per_label() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    let c = conductor(dir.path(), "steps", Arc::new(Attach));
    let a1: u32 = c.step("fetch", || Ok(1)).await.unwrap();
    let a2: u32 = c.step("fetch", || Ok(2)).await.unwrap();
    let b1: u32 = c.step("other", || Ok(9)).await.unwrap();
    assert_eq!((a1, a2, b1), (1, 2, 9));

    // Resume: same labels replay the recorded results; the closures must not
    // run (they would error).
    let c2 = conductor(dir.path(), "steps", Arc::new(Attach));
    let r1: u32 = c2
        .step("fetch", || Err(H5iError::Metadata("re-executed".into())))
        .await
        .unwrap();
    let r2: u32 = c2
        .step("fetch", || Err(H5iError::Metadata("re-executed".into())))
        .await
        .unwrap();
    let r3: u32 = c2
        .step("other", || Err(H5iError::Metadata("re-executed".into())))
        .await
        .unwrap();
    assert_eq!((r1, r2, r3), (1, 2, 9));

    // A third step under a replayed label runs live (journal miss).
    let a3: u32 = c2.step("fetch", || Ok(3)).await.unwrap();
    assert_eq!(a3, 3);

    // Divergence: replaying a journaled result as an incompatible type fails
    // closed and names the step key.
    let c3 = conductor(dir.path(), "steps", Arc::new(Attach));
    let err = c3
        .step::<String, _>("fetch", || Ok("nope".into()))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("divergence") && err.to_string().contains("fetch#1"),
        "unexpected divergence error: {err}"
    );
}

#[tokio::test]
async fn work_review_revise_and_resume_without_reexecution() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    fabricate_env(&repo, &h5i_root, "claude", "fix");
    fabricate_env(&repo, &h5i_root, "codex", "fix");

    let script = Arc::new(Script {
        turns: AtomicUsize::new(0),
        review_body: Mutex::new("Needs work: rename the helper before this can land.".into()),
    });

    // The score, as a reusable closure over any conductor bound to the run.
    async fn score(c: &Conductor) -> (TeamArtifact, TeamArtifact, TeamReview, TeamArtifact) {
        let a = c
            .agent("claude")
            .runtime("claude")
            .env("env/claude/fix")
            .hire()
            .await
            .unwrap();
        let b = c
            .agent("codex")
            .runtime("codex")
            .env("env/codex/fix")
            .hire()
            .await
            .unwrap();
        let (art_a, art_b) =
            tokio::try_join!(a.work("add feature"), b.work("add feature")).unwrap();
        let run = c.freeze().await.unwrap();
        assert_eq!(run.phase, PHASE_SEALED_SUBMIT);
        let review = b.review(&art_a).await.unwrap();
        assert!(!approves(&review));
        let revised = a.revise(&art_a, &review).await.unwrap();
        assert_ne!(revised.id, art_a.id);
        (art_a, art_b, review, revised)
    }

    let c1 = conductor(dir.path(), "flow", Script::launcher(&script));
    let first = score(&c1).await;
    let live_turns = script.turns.load(Ordering::SeqCst);
    assert_eq!(live_turns, 4, "2 work + 1 review + 1 revise turns");

    // Resume: an identical score replays every step from the journal — the
    // launcher must not receive a single turn, and results are identical.
    let c2 = conductor(dir.path(), "flow", Script::launcher(&script));
    let second = score(&c2).await;
    assert_eq!(script.turns.load(Ordering::SeqCst), live_turns);
    assert_eq!(first.0.id, second.0.id);
    assert_eq!(first.1.id, second.1.id);
    assert_eq!(first.2.body, second.2.body);
    assert_eq!(first.3.id, second.3.id);
}

#[tokio::test]
async fn ensemble_verdict_and_mediated_apply() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    fabricate_env(&repo, &h5i_root, "claude", "ens");
    fabricate_env(&repo, &h5i_root, "codex", "ens");

    let script = Arc::new(Script {
        turns: AtomicUsize::new(0),
        review_body: Mutex::new("APPROVE — clean and minimal.".into()),
    });
    let c = conductor(dir.path(), "ens", Script::launcher(&script));
    let a = c
        .agent("claude")
        .env("env/claude/ens")
        .hire()
        .await
        .unwrap();
    let b = c.agent("codex").env("env/codex/ens").hire().await.unwrap();

    let outcome = ensemble(&c, "add feature")
        .agents([a, b])
        .rounds(2)
        .judge(smallest_diff_policy())
        .run()
        .await
        .unwrap();

    assert_eq!(outcome.artifacts.len(), 2);
    assert_eq!(outcome.reviews.len(), 2, "one review per ordered pair");
    assert_eq!(outcome.rounds_run, 1, "full approval exits early");
    let verdict = outcome.verdict.expect("policy verdict");
    let winner_id = verdict.selected_submission.expect("selected winner");
    let winner = outcome
        .artifacts
        .iter()
        .find(|s| s.id == winner_id)
        .expect("winner among artifacts");

    // Mediated apply: gated on the recorded auto-applicable verdict, lands the
    // winning patch on the current branch.
    let applied = c.apply(winner).await.unwrap();
    assert_eq!(applied.submission_id, winner_id);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.id().to_string(), applied.target_commit_oid);
    assert!(head
        .tree()
        .unwrap()
        .get_name(&format!("{}.txt", winner.owner_agent))
        .is_some());

    // Applying a non-selected artifact without force stays refused.
    let loser = outcome.artifacts.iter().find(|s| s.id != winner_id).unwrap();
    assert!(c.apply(loser).await.is_err());
}
