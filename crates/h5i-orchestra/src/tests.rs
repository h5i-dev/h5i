//! Orchestra kernel tests: journal step keys + replay, resume without
//! re-execution, and the ensemble pattern end-to-end at workspace tier with a
//! scripted launcher standing in for the agents (no LLM, no sandbox probing —
//! envs are fabricated exactly like `team::tests` does).

use super::patterns::ensemble;
use super::*;
use h5i_core::team::{TeamVerdict, PHASE_SEALED_SUBMIT};
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
type AskFn = dyn Fn(&TurnContext, &h5i_core::team::TeamRun) -> String + Send + Sync;

struct Script {
    turns: AtomicUsize,
    review_body: Mutex<String>,
    ask_replies: Mutex<Vec<String>>,
    ask_via_spool: bool,
    /// When set, computes the ask reply from the live run state (for judges
    /// that must cite real ids). Takes precedence over `ask_replies`.
    ask_fn: Option<Box<AskFn>>,
}

fn scripted(review_body: &str) -> Arc<Script> {
    Arc::new(Script {
        turns: AtomicUsize::new(0),
        review_body: Mutex::new(review_body.into()),
        ask_replies: Mutex::new(Vec::new()),
        ask_via_spool: false,
        ask_fn: None,
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
                    let body = if let Some(f) = &script.ask_fn {
                        let run = team::status(&repo, &turn.run_id)?.run;
                        f(turn, &run)
                    } else {
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
        let mut latest: Vec<&h5i_core::team::TeamArtifact> = run
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

    // Item 6: every journaled step records its wall-clock cost, rendered in
    // the trace as evidence of whether orchestration paid for itself.
    let step_ev = events.iter().find(|e| e.kind == "orch_step").unwrap();
    assert!(
        step_ev.payload.get("duration_ms").and_then(|v| v.as_u64()).is_some(),
        "step must carry duration_ms"
    );
    assert!(text.contains("s)"), "trace must render the step's duration:\n{text}");
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
        ask_fn: None,
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
        ask_fn: None,
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
        ask_fn: None,
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

#[tokio::test]
async fn roster_binds_enrolled_seats_for_a_driver() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    fabricate_env(&repo, &h5i_root, "claude", "r");
    fabricate_env(&repo, &h5i_root, "codex", "r");

    // Enrollment happens in one process (auto-create / add-env)…
    let script = scripted("APPROVE");
    let c1 = conductor(dir.path(), "rost", Script::launcher(&script));
    c1.agent("claude").env("env/claude/r").hire().await.unwrap();
    c1.agent("codex").env("env/codex/r").hire().await.unwrap();

    // …and a separate driver (h5i team run) binds the seats without hiring.
    let c2 = conductor(dir.path(), "rost", Script::launcher(&script));
    let agents = c2.roster().await.unwrap();
    let mut ids: Vec<&str> = agents.iter().map(|a| a.id()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["claude", "codex"]);

    // A roster-bound handle drives turns exactly like a hired one.
    let a = agents.iter().find(|a| a.id() == "claude").unwrap();
    let artifact = a.work("do the thing").await.unwrap();
    assert_eq!(artifact.owner_agent, "claude");
}

#[tokio::test]
async fn concurrent_same_label_steps_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let c = conductor(dir.path(), "guard", Arc::new(Attach));

    // Two overlapping steps under one label: the second allocation must fail
    // loudly (nondeterministic seq numbers would corrupt replay pairing).
    let slow = c.step("dup", || {
        std::thread::sleep(Duration::from_millis(300));
        Ok(1u32)
    });
    let fast = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        c.step("dup", || Ok(2u32)).await
    };
    let (a, b) = tokio::join!(slow, fast);
    assert!(a.is_ok());
    let err = b.unwrap_err().to_string();
    assert!(err.contains("concurrent steps under one label"), "{err}");

    // A failed allocation must not wedge the label: sequential reuse works.
    let v: u32 = c.step("dup", || Ok(3)).await.unwrap();
    assert_eq!(v, 3);

    // Scoped parallel loops are the sanctioned shape.
    let (x, y) = tokio::join!(
        c.scope("item/1").step("fetch", || Ok(10u32)),
        c.scope("item/2").step("fetch", || Ok(20u32)),
    );
    assert_eq!((x.unwrap(), y.unwrap()), (10, 20));

    // Scoped keys replay stably on resume.
    let c2 = conductor(dir.path(), "guard", Arc::new(Attach));
    let r: u32 = c2
        .scope("item/1")
        .step("fetch", || Err(H5iError::Metadata("re-executed".into())))
        .await
        .unwrap();
    assert_eq!(r, 10);
}

#[tokio::test]
async fn expect_independent_validates_at_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    fabricate_env(&repo, &h5i_root, "claude", "ei");
    fabricate_env(&repo, &h5i_root, "codex", "ei");
    fabricate_env(&repo, &h5i_root, "mira", "ei");

    let script = scripted("APPROVE");
    let c = conductor(dir.path(), "indep", Script::launcher(&script));
    let a = c.agent("claude").env("env/claude/ei").hire().await.unwrap();
    let b = c.agent("codex").env("env/codex/ei").hire().await.unwrap();
    let m = c.agent("mira").env("env/mira/ei").hire().await.unwrap();

    // Pre-freeze first attempts are independent by construction.
    let pa = a.work("part A").expect_independent().await.unwrap();
    let pb = b.work("part B").expect_independent().await.unwrap();
    c.freeze().await.unwrap();

    // Contradictory combination is refused up front.
    let err = m
        .work("merge")
        .with_materials([&pa])
        .expect_independent()
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("contradicts"), "{err}");

    // A genuinely influenced turn fails the expectation at runtime: deliver
    // material to mira, then demand independence from its next artifact.
    let merged_err = {
        // materials via a normal (unguarded) request first, to influence mira
        let _ = m.work("merge for real").with_materials([&pa, &pb]).await.unwrap();
        // mira is now a discussion recipient this round; a further "expected
        // independent" turn must fail the stamp check.
        m.work("another attempt").expect_independent().await.unwrap_err().to_string()
    };
    assert!(merged_err.contains("expected independent"), "{merged_err}");
}

#[tokio::test]
async fn preflight_reports_dead_sessions_and_weak_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    fabricate_env(&repo, &h5i_root, "claude", "pf");
    fabricate_env(&repo, &h5i_root, "codex", "pf");

    let script = scripted("APPROVE");
    let c = conductor(dir.path(), "pf", Script::launcher(&script));
    let a = c.agent("claude").env("env/claude/pf").hire().await.unwrap();
    let b = c.agent("codex").env("env/codex/pf").hire().await.unwrap();

    // Nothing holds the env writer locks and the fabricated claims are
    // workspace: both configured checks must fail, together, in one report.
    let err = c
        .preflight()
        .require_live([&a, &b])
        .require_isolation("process")
        .run()
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no live session for 'claude'"), "{err}");
    assert!(err.contains("no live session for 'codex'"), "{err}");
    assert!(err.contains("below the required 'process'"), "{err}");

    // Hold one env's writer lock (a stand-in for a resident session): that
    // agent passes, the other still fails.
    let lock_path = env::find(&h5i_root, "env/claude/pf")
        .unwrap()
        .dir(&h5i_root)
        .join("run.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    unsafe {
        use std::os::unix::io::AsRawFd;
        assert_eq!(libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX), 0);
    }
    let err = c
        .preflight()
        .require_live([&a, &b])
        .run()
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains("'claude'"), "claude session is live: {err}");
    assert!(err.contains("no live session for 'codex'"), "{err}");

    // Clean-worktree check: dirty the tree, expect the failure named.
    std::fs::write(dir.path().join("scratch.txt"), "dirty\n").unwrap();
    let err = c
        .preflight()
        .require_clean_worktree()
        .run()
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not clean"), "{err}");
    std::fs::remove_file(dir.path().join("scratch.txt")).unwrap();
    c.preflight().require_clean_worktree().run().await.unwrap();
}

#[tokio::test]
async fn judge_panel_scores_over_evidence_and_validates_citations() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    fabricate_env(&repo, &h5i_root, "claude", "jp");
    fabricate_env(&repo, &h5i_root, "codex", "jp");
    fabricate_env(&repo, &h5i_root, "mira", "jp");
    fabricate_env(&repo, &h5i_root, "theo", "jp");

    // Two judges vote; each cites the candidate ids from live run state. The
    // first turn per judge deliberately cites a bogus id to exercise the
    // re-ask, then a valid ballot on the retry.
    let attempts = std::sync::Arc::new(AtomicUsize::new(0));
    let attempts_cl = attempts.clone();
    let script = Arc::new(Script {
        turns: AtomicUsize::new(0),
        review_body: Mutex::new(String::new()),
        ask_replies: Mutex::new(Vec::new()),
        ask_via_spool: false,
        ask_fn: Some(Box::new(move |_turn, run| {
            let subs: Vec<&h5i_core::team::TeamArtifact> = run.submissions.iter().collect();
            let first = subs.first().map(|s| s.id.clone()).unwrap_or_default();
            let attempt = attempts_cl.fetch_add(1, Ordering::SeqCst);
            // Each judge's first ask cites a hallucinated id → must re-ask.
            if attempt.is_multiple_of(2) {
                return serde_json::json!({
                    "ballots": [{"artifact_id": first, "score": 9,
                                 "rationale": "cites nothing real",
                                 "cited_ids": ["bogus-id-123"]}]
                })
                .to_string();
            }
            // Valid ballot: score every candidate, prefer the first.
            let ballots: Vec<_> = subs
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    serde_json::json!({
                        "artifact_id": s.id,
                        "score": if i == 0 { 9 } else { 5 },
                        "rationale": format!("grounded in {}", s.id),
                        "cited_ids": [s.id],
                    })
                })
                .collect();
            serde_json::json!({ "ballots": ballots }).to_string()
        })),
    });

    let c = conductor(dir.path(), "panel", Script::launcher(&script));
    let a = c.agent("claude").env("env/claude/jp").hire().await.unwrap();
    let b = c.agent("codex").env("env/codex/jp").hire().await.unwrap();
    let j1 = c.agent("mira").env("env/mira/jp").hire().await.unwrap();
    let j2 = c.agent("theo").env("env/theo/jp").hire().await.unwrap();

    let (pa, _pb) = tokio::try_join!(a.work("attempt A"), b.work("attempt B")).unwrap();
    c.freeze().await.unwrap();

    let outcome = super::patterns::judge_panel(&c, "pick the cleanest solution")
        .judges([j1, j2])
        .run()
        .await
        .unwrap();

    // Each judge re-asked once (bogus citation) then produced a valid ballot.
    assert_eq!(outcome.ballots.len(), 2);
    for (_judge, ballots) in &outcome.ballots {
        assert_eq!(ballots.len(), 2, "each judge scores both candidates");
        for ballot in ballots {
            for cited in &ballot.cited_ids {
                assert!(
                    !cited.starts_with("bogus"),
                    "validated ballots must not carry hallucinated citations"
                );
            }
        }
    }
    // First submission got the 9s → it wins the panel, and the verdict is
    // recorded on the event log (advisory: not auto-applicable).
    assert_eq!(outcome.verdict.selected_submission.as_deref(), Some(pa.id.as_str()));
    assert!(!outcome.verdict.can_auto_apply);
    let recorded = c.status().await.unwrap().run.verdict.unwrap();
    assert_eq!(recorded.selected_submission, Some(pa.id));
}

#[test]
fn approves_recognizes_common_verdict_forms() {
    use super::approves;
    let yes = |b: &str| approves(&h5i_core::team::TeamReview {
        reviewer: "r".into(), target: "t".into(), round: 1,
        body: b.into(), referenced_artifacts: vec![],
    });
    // Plain and labeled approvals (the live run produced "Verdict: approve").
    assert!(yes("APPROVE"));
    assert!(yes("Verdict: approve\n\nReviewed the diff, looks good."));
    assert!(yes("Verdict: APPROVE"));
    assert!(yes("LGTM"));
    assert!(yes("Decision: OK to merge"));
    assert!(yes("approved — clean and minimal"));
    // Not approvals: approval token not leading the (delabeled) first line.
    assert!(!yes("Needs work: rename the helper first"));
    assert!(!yes("I can't approve this yet"));
    assert!(!yes("Changes required before I approve"));
    assert!(!yes(""));
}

#[tokio::test]
async fn revise_completes_on_unchanged_resubmit() {
    // The live-run deadlock: an agent told to revise finds nothing to change
    // and re-submits the SAME candidate (same tree → same id). revise() must
    // treat that as a valid response (a new submission event), not wait for a
    // changed id forever.
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    fabricate_env(&repo, &h5i_root, "claude", "rr");

    // A launcher whose Revise turn re-submits the existing tip unchanged.
    let turns = Arc::new(AtomicUsize::new(0));
    let turns_cl = turns.clone();
    let launcher: Arc<dyn RuntimeLauncher> = Arc::new(FnLauncher(move |turn: &TurnContext| {
        turns_cl.fetch_add(1, Ordering::SeqCst);
        let repo = Repository::open(&turn.repo_workdir)?;
        let branch = format!("refs/heads/h5i/{}", turn.env_id);
        match &turn.kind {
            TurnKind::Work => {
                commit_on_branch(&repo, &branch, "answer.txt", "ok\n")?;
                team::submit(&repo, &turn.h5i_root, &turn.run_id, &turn.agent_id,
                    None, Some("first".into()), &turn.agent_id)?;
            }
            TurnKind::Revise => {
                // No commit — re-submit the same tip (same tree → same id).
                team::submit(&repo, &turn.h5i_root, &turn.run_id, &turn.agent_id,
                    None, Some("no change needed".into()), &turn.agent_id)?;
            }
            _ => {}
        }
        Ok(())
    }));

    let c = conductor(dir.path(), "rr", launcher);
    let a = c.agent("claude").env("env/claude/rr").hire().await.unwrap();
    let first = a.work("do it").await.unwrap();
    c.freeze().await.unwrap();

    let review = h5i_core::team::TeamReview {
        reviewer: "peer".into(), target: "claude".into(), round: 1,
        body: "please rename the helper".into(), referenced_artifacts: vec![],
    };
    // Must return promptly (not hang to timeout) with the unchanged candidate.
    let revised = a.revise(&first, &review).await.unwrap();
    assert_eq!(revised.id, first.id, "unchanged re-submit returns the same candidate");
    assert_eq!(turns.load(Ordering::SeqCst), 2, "one work + one revise turn");
}

#[tokio::test]
async fn hire_fails_closed_when_the_seat_env_was_removed() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    let m = fabricate_env(&repo, &h5i_root, "claude", "gone");

    let script = scripted("fine");
    let c1 = conductor(dir.path(), "stale", Script::launcher(&script));
    c1.agent("claude")
        .runtime("claude")
        .env("env/claude/gone")
        .hire()
        .await
        .unwrap();

    // The env vanishes between runs (an `h5i env rm` / abort during cleanup).
    fs::remove_dir_all(m.dir(&h5i_root)).unwrap();

    // Resume: the journaled hire replays, but must fail closed on the dead
    // env instead of letting turns dispatch into the void.
    let c2 = conductor(dir.path(), "stale", Script::launcher(&script));
    let err = match c2
        .agent("claude")
        .runtime("claude")
        .env("env/claude/gone")
        .hire()
        .await
    {
        Ok(_) => panic!("hire must fail closed on a removed env"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(msg.contains("env/claude/gone"), "names the env: {msg}");
    assert!(msg.contains("no longer exists"), "says why: {msg}");
    assert!(msg.contains("fresh run id"), "says how to recover: {msg}");
}

#[tokio::test]
async fn launch_resident_fails_closed_on_a_missing_env() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    let h5i_root = repo.commondir().join(".h5i");
    let turn = TurnContext {
        run_id: "r".into(),
        agent_id: "claude".into(),
        env_id: "env/claude/never-created".into(),
        kind: TurnKind::Work,
        instruction: "task".into(),
        repo_workdir: dir.path().to_path_buf(),
        h5i_root,
        work_dir: None,
        runtime: Some("claude".into()),
        model: None,
        effort: None,
        };
    let err = LaunchResident.on_turn(&turn).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("env/claude/never-created"), "names the env: {msg}");
    assert!(msg.contains("no longer exists"), "says why: {msg}");
}

#[test]
fn resident_command_maps_effort_per_adapter() {
    let turn = |runtime: &str, effort: Option<&str>| TurnContext {
        run_id: "r".into(),
        agent_id: "a".into(),
        env_id: "env/a/r-a".into(),
        kind: TurnKind::Work,
        instruction: "task".into(),
        repo_workdir: PathBuf::from("."),
        h5i_root: PathBuf::from("."),
        work_dir: None,
        runtime: Some(runtime.into()),
        model: Some("m1".into()),
        effort: effort.map(Into::into),
    };

    // codex: effort rides the config-override flag (wins over config.toml).
    let cmd = launcher::resident_command(&turn("codex", Some("medium"))).unwrap();
    assert!(cmd.contains("--model 'm1'"), "{cmd}");
    assert!(cmd.contains("-c model_reasoning_effort='medium'"), "{cmd}");

    // codex without effort: no flag, the box's own config decides.
    let cmd = launcher::resident_command(&turn("codex", None)).unwrap();
    assert!(!cmd.contains("model_reasoning_effort"), "{cmd}");

    // claude has no effort flag: fail closed, never silently drop the knob.
    let err = launcher::resident_command(&turn("claude", Some("medium"))).unwrap_err();
    assert!(err.to_string().contains("no reasoning-effort"), "{err}");
    launcher::resident_command(&turn("claude", None)).unwrap();
}
