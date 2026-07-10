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
/// commits a per-turn file onto the agent's env branch and submits; on a
/// Review turn it posts `review_body`; on an Ask turn it pops the next queued
/// reply (via the in-box spool when `ask_via_spool`, else recorded directly).
/// Counts every turn so resume tests can assert zero re-execution.
struct Script {
    turns: AtomicUsize,
    review_body: Mutex<String>,
    ask_replies: Mutex<Vec<String>>,
    ask_via_spool: bool,
}

fn scripted(review_body: &str) -> Arc<Script> {
    Arc::new(Script {
        turns: AtomicUsize::new(0),
        review_body: Mutex::new(review_body.into()),
        ask_replies: Mutex::new(Vec::new()),
        ask_via_spool: false,
    })
}

impl Script {
    fn launcher(script: &Arc<Self>) -> Arc<dyn RuntimeLauncher> {
        let script = script.clone();
        Arc::new(FnLauncher(move |turn: &TurnContext| {
            let n = script.turns.fetch_add(1, Ordering::SeqCst);
            let repo = Repository::open(&turn.repo_workdir)?;
            let branch = format!("refs/heads/h5i/{}", turn.env_id);
            match &turn.kind {
                TurnKind::Work | TurnKind::Revise => {
                    let stage = if turn.kind == TurnKind::Work { "work" } else { "revised" };
                    commit_on_branch(
                        &repo,
                        &branch,
                        &format!("{}-{stage}-{n}.txt", turn.agent_id),
                        "content\n",
                    )?;
                    team::submit(
                        &repo,
                        &turn.h5i_root,
                        &turn.run_id,
                        &turn.agent_id,
                        None,
                        Some(stage.into()),
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
                TurnKind::Ask => {
                    let body = {
                        let mut q = script.ask_replies.lock().unwrap();
                        if q.is_empty() {
                            "{}".to_string()
                        } else {
                            q.remove(0)
                        }
                    };
                    if script.ask_via_spool {
                        // The real box path: stage the reply in the env spool;
                        // the wait loop's sync_outbound ingests it host-side.
                        let m = env::find(&turn.h5i_root, &turn.env_id)?;
                        let spool = m.dir(&turn.h5i_root).join("spool");
                        env::write_team_reply_spool(
                            &spool,
                            &env::TeamReplySpool { body },
                        )?;
                    } else {
                        team::record_agent_reply(&repo, &turn.run_id, &turn.agent_id, body)?;
                    }
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

    let script = scripted("Needs work: rename the helper before this can land.");

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

    let script = scripted("APPROVE — clean and minimal.");
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
    let prefix = format!("{}-work-", winner.owner_agent);
    assert!(
        head.tree()
            .unwrap()
            .iter()
            .filter_map(|e| e.name().map(String::from))
            .any(|n| n.starts_with(&prefix)),
        "applied tree must contain the winner's work file"
    );

    // Applying a non-selected artifact without force stays refused.
    let loser = outcome.artifacts.iter().find(|s| s.id != winner_id).unwrap();
    assert!(c.apply(loser).await.is_err());
}

#[tokio::test]
async fn gate_delivers_once_and_resumes_from_journal() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let repo_path = dir.path().to_path_buf();

    // A background "human": polls the msg store for the gate ASK, replies once.
    let responder = std::thread::spawn(move || {
        for _ in 0..400 {
            let repo = Repository::open(&repo_path).unwrap();
            let asks: Vec<_> = msg::read_messages(&repo)
                .into_iter()
                .filter(|m| m.body.starts_with("[gate]"))
                .collect();
            if let Some(ask) = asks.first() {
                msg::send_msg(
                    &repo,
                    &repo.commondir().join(".h5i"),
                    "human",
                    &ask.from,
                    "APPROVE ship it",
                    msg::SendOpts {
                        reply_to: Some(ask.id.clone()),
                        ..Default::default()
                    },
                )
                .unwrap();
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("gate ASK never appeared");
    });

    let c1 = conductor(dir.path(), "gated", Arc::new(Attach));
    let approved = c1.gate("apply the winner?").approve().await.unwrap();
    assert!(approved);
    responder.join().unwrap();

    let count_gate_asks = |repo: &Repository| {
        msg::read_messages(repo)
            .into_iter()
            .filter(|m| m.body.starts_with("[gate]"))
            .count()
    };
    let repo = Repository::open(dir.path()).unwrap();
    assert_eq!(count_gate_asks(&repo), 1);

    // Resume: the gate replays ask + answer from the journal — no new ASK is
    // sent and no responder is needed.
    let c2 = conductor(dir.path(), "gated", Arc::new(Attach));
    let answer = c2.gate("apply the winner?").answer().await.unwrap();
    assert!(answer.approved());
    assert_eq!(answer.from, "human");
    assert_eq!(count_gate_asks(&repo), 1, "resume must not re-ask");
}

#[tokio::test]
async fn patched_keeps_migration_branches_consistent() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    // v1 of the score journals two steps and no marker.
    let c1 = conductor(dir.path(), "patch", Arc::new(Attach));
    let _: u32 = c1.step("a", || Ok(1)).await.unwrap();
    let _: u32 = c1.step("b", || Ok(2)).await.unwrap();

    // v2 inserts patched() checks. Mid-replay of the v1 journal the first
    // marker must pick the OLD path (false: step "b" is still un-replayed);
    // once the old journal is exhausted the second marker picks the new path.
    let c2 = conductor(dir.path(), "patch", Arc::new(Attach));
    let _: u32 = c2.step("a", || Ok(0)).await.unwrap();
    assert!(!c2.patched("mid-run-change").await.unwrap());
    let _: u32 = c2.step("b", || Ok(0)).await.unwrap();
    assert!(c2.patched("post-journal-change").await.unwrap());

    // Every later resume replays the recorded choices verbatim.
    let c3 = conductor(dir.path(), "patch", Arc::new(Attach));
    let _: u32 = c3.step("a", || Ok(0)).await.unwrap();
    assert!(!c3.patched("mid-run-change").await.unwrap());
    let _: u32 = c3.step("b", || Ok(0)).await.unwrap();
    assert!(c3.patched("post-journal-change").await.unwrap());
}

#[tokio::test]
async fn trace_renders_steps_and_phases() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let c = conductor(dir.path(), "traced", Arc::new(Attach));
    let _: u32 = c.step("fetch", || Ok(7)).await.unwrap();
    c.note("halfway").await.unwrap();

    let events = team::read_events(&repo, "traced").unwrap();
    let text = super::trace::render_trace("traced", &events);
    assert!(text.contains("step fetch#1"), "missing step line:\n{text}");
    assert!(text.contains("note: halfway"), "missing note line:\n{text}");
    assert!(text.contains("run created"), "missing created line:\n{text}");

    let dot = super::trace::render_trace_dot("traced", &events);
    assert!(dot.starts_with("digraph"));
    assert!(dot.contains("label=\"fetch\""), "missing lane cluster:\n{dot}");
    assert!(dot.contains("\"fetch#1\""));
}

#[tokio::test]
async fn ask_via_spool_ingests_and_parses() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    fabricate_env(&repo, &h5i_root, "claude", "ask");

    let script = Arc::new(Script {
        turns: AtomicUsize::new(0),
        review_body: Mutex::new(String::new()),
        ask_replies: Mutex::new(vec!["{\"score\": 8, \"verdict\": \"solid\"}".into()]),
        ask_via_spool: true,
    });
    let c = conductor(dir.path(), "ask-spool", Script::launcher(&script));
    let a = c.agent("claude").env("env/claude/ask").hire().await.unwrap();

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Card {
        score: u32,
        verdict: String,
    }
    let card: Card = a.ask("Rate the design 0-10.").await.unwrap();
    assert_eq!(card.score, 8);
    assert_eq!(card.verdict, "solid");
    assert_eq!(script.turns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ask_reasks_on_unparseable_reply() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    fabricate_env(&repo, &h5i_root, "codex", "ask2");

    let script = Arc::new(Script {
        turns: AtomicUsize::new(0),
        review_body: Mutex::new(String::new()),
        ask_replies: Mutex::new(vec![
            "sorry, here you go: nothing useful".into(),
            "```json\n{\"n\": 42}\n```".into(),
        ]),
        ask_via_spool: false,
    });
    let c = conductor(dir.path(), "ask-retry", Script::launcher(&script));
    let a = c.agent("codex").env("env/codex/ask2").hire().await.unwrap();

    #[derive(serde::Serialize, serde::Deserialize)]
    struct N {
        n: u32,
    }
    let v: N = a.ask("Answer with {\"n\": <int>}.").await.unwrap();
    assert_eq!(v.n, 42);
    assert_eq!(script.turns.load(Ordering::SeqCst), 2, "one re-ask");
}

#[tokio::test]
async fn with_materials_stamps_influence_edges() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    fabricate_env(&repo, &h5i_root, "claude", "m");
    fabricate_env(&repo, &h5i_root, "codex", "m");
    fabricate_env(&repo, &h5i_root, "mira", "m");

    let script = scripted("APPROVE");
    let c = conductor(dir.path(), "mats", Script::launcher(&script));
    let a = c.agent("claude").env("env/claude/m").hire().await.unwrap();
    let b = c.agent("codex").env("env/codex/m").hire().await.unwrap();
    let integrator = c.agent("mira").env("env/mira/m").hire().await.unwrap();

    let (pa, pb) = tokio::try_join!(a.work("part A"), b.work("part B")).unwrap();
    assert!(pa.independent && pb.independent);
    c.freeze().await.unwrap();

    let merged = integrator
        .work("merge the parts")
        .with_materials([&pa, &pb])
        .await
        .unwrap();
    assert!(!merged.independent, "material-fed work must not claim independence");
    assert!(merged.influence_artifact_ids.contains(&pa.id));
    assert!(merged.influence_artifact_ids.contains(&pb.id));

    // The scoped-visibility audit event is recorded.
    let events = team::read_events(&repo, "mats").unwrap();
    assert!(events.iter().any(|e| e.kind == "materials_granted"));
}

#[tokio::test]
async fn pattern_pipeline_chains_materials() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    fabricate_env(&repo, &h5i_root, "claude", "p");
    fabricate_env(&repo, &h5i_root, "codex", "p");

    let script = scripted("APPROVE");
    let c = conductor(dir.path(), "pipe", Script::launcher(&script));
    let architect = c.agent("claude").env("env/claude/p").hire().await.unwrap();
    let implementer = c.agent("codex").env("env/codex/p").hire().await.unwrap();

    let artifacts = super::patterns::pipeline(
        &c,
        vec![
            (architect, "design the module".to_string()),
            (implementer, "implement the design".to_string()),
        ],
    )
    .await
    .unwrap();
    assert_eq!(artifacts.len(), 2);
    assert!(artifacts[0].independent);
    assert!(!artifacts[1].independent, "stage 2 is influenced by stage 1");
    assert!(artifacts[1].influence_artifact_ids.contains(&artifacts[0].id));
}

#[tokio::test]
async fn pattern_arena_ranks_and_judges() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    fabricate_env(&repo, &h5i_root, "claude", "ar");
    fabricate_env(&repo, &h5i_root, "codex", "ar");

    let script = scripted("APPROVE");
    let c = conductor(dir.path(), "arena", Script::launcher(&script));
    let a = c.agent("claude").env("env/claude/ar").hire().await.unwrap();
    let b = c.agent("codex").env("env/codex/ar").hire().await.unwrap();

    let outcome = super::patterns::arena(&c, "solve it")
        .agents([a, b])
        .judge(smallest_diff_policy())
        .run()
        .await
        .unwrap();
    assert_eq!(outcome.artifacts.len(), 2);
    assert_eq!(outcome.rows.len(), 2);
    assert!(outcome.verdict.unwrap().selected_submission.is_some());
}

#[tokio::test]
async fn pattern_map_reduce_merges_via_integrator() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    fabricate_env(&repo, &h5i_root, "claude", "mr");
    fabricate_env(&repo, &h5i_root, "codex", "mr");
    fabricate_env(&repo, &h5i_root, "mira", "mr");

    let script = scripted("APPROVE");
    let c = conductor(dir.path(), "mapred", Script::launcher(&script));
    let a = c.agent("claude").env("env/claude/mr").hire().await.unwrap();
    let b = c.agent("codex").env("env/codex/mr").hire().await.unwrap();
    let m = c.agent("mira").env("env/mira/mr").hire().await.unwrap();

    let outcome = super::patterns::map_reduce(&c)
        .map(a.clone(), "item 1")
        .map(b, "item 2")
        .map(a, "item 3") // same agent twice: must run sequentially, not race
        .reduce(m, "merge all items")
        .run()
        .await
        .unwrap();
    assert_eq!(outcome.parts.len(), 3);
    let merged = outcome.merged.unwrap();
    assert!(!merged.independent);
    assert_eq!(merged.influence_artifact_ids.len(), 3);
}

#[tokio::test]
async fn pattern_debate_argues_and_concludes() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    fabricate_env(&repo, &h5i_root, "claude", "d");
    fabricate_env(&repo, &h5i_root, "codex", "d");
    fabricate_env(&repo, &h5i_root, "mira", "d");

    let script = Arc::new(Script {
        turns: AtomicUsize::new(0),
        review_body: Mutex::new(String::new()),
        ask_replies: Mutex::new(vec![
            "\"tabs are configurable\"".into(),
            "\"spaces render identically everywhere\"".into(),
            "{\"winner\": \"codex\", \"rationale\": \"portability won\"}".into(),
        ]),
        ask_via_spool: false,
    });
    let c = conductor(dir.path(), "debate", Script::launcher(&script));
    let pro = c.agent("claude").env("env/claude/d").hire().await.unwrap();
    let con = c.agent("codex").env("env/codex/d").hire().await.unwrap();
    let moderator = c.agent("mira").env("env/mira/d").hire().await.unwrap();

    let outcome = super::patterns::debate(&c, "tabs or spaces?")
        .sides([pro, con])
        .moderator(moderator)
        .rounds(1)
        .run()
        .await
        .unwrap();
    assert_eq!(outcome.transcript.len(), 2);
    assert_eq!(outcome.transcript[0].1, "tabs are configurable");
    let conclusion = outcome.conclusion.unwrap();
    assert_eq!(conclusion.winner, "codex");
}
