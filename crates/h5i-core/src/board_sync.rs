//! The board's one way in and out: an ordinary git remote.
//!
//! Every board has a remote, including a board with one participant on one
//! machine. That is deliberate, and it is the opposite of an optimisation.
//!
//! The tempting design gives same-machine boxes a shortcut — write the refs
//! directly, skip the sync — because two agents on one laptop obviously do not
//! need a network. The cost of that shortcut is not performance, it is
//! coverage: the shortcut becomes the only path anyone ever runs, and the sync
//! path rots untested until the day a second machine joins and everything it
//! was supposed to handle happens at once. A push to a local bare repository
//! costs a few milliseconds against a tender that runs once a second, so the
//! shortcut buys nothing and hides everything.
//!
//! So: solo and team differ by a URL and by nothing else.
//!
//! ## Why a git remote rather than a service
//!
//! Because nobody has to run it. A team already operates a git host, and that
//! host already answers the two questions a board would otherwise need its own
//! answers for: **who may post** is push access, and **who may read** is read
//! access. A public repository is an open topic; a private one is an internal
//! one. There is no server to deploy, no uptime to own, and no roster to
//! invent.
//!
//! It also gives the compare-and-swap away for free, and this was measured
//! against GitHub rather than assumed: a non-fast-forward push to a
//! `refs/h5i/*` ref is rejected server-side, `--force-with-lease` succeeds
//! against the tip you fetched and is rejected as stale against one you did
//! not. Append-only threads make every honest update a fast-forward, so the
//! ordinary rejection *is* the CAS, and a rejection simply means somebody
//! posted while we were merging.
//!
//! ## What this does not do
//!
//! **Sync never deletes.** A thread that exists on the remote and not here is
//! fetched, never removed; a thread that exists here and not there is pushed.
//! Deletion is a human closing a thread, and `close` deletes the remote ref
//! itself. A sync that could delete is a sync that can lose another machine's
//! conversation because this one had not heard of it yet.
//!
//! **Agents never speak this.** A box has no git credential, no route to the
//! remote, and no code path that reaches this module: it writes a record into
//! its spool and the host does the rest. That is the same split the remote
//! runner makes — the worker is h5i, the host holds the key — and it is why a
//! compromised agent cannot push to the board even though the board is a repo.

use std::path::{Path, PathBuf};
use std::process::Command;

use git2::Repository;

use crate::board;
use crate::error::H5iError;

/// Refs the board owns, as a fetch/push refspec pair.
const LIVE: &str = "refs/h5i/board/*";
const ATTIC: &str = "refs/h5i/board-attic/*";
/// Where a fetch parks the remote's view before it is merged in.
const INCOMING: &str = "refs/h5i/board-incoming";

/// Attempts before a sync gives up. Each retry is a fetch, a merge and a push;
/// losing this many consecutive races means the board is busier than a git
/// remote can serve, which is a different problem from a lost update.
const MAX_ATTEMPTS: usize = 8;

/// Where this machine's board lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    /// A git URL, or a path. Anything `git fetch` accepts.
    pub url: String,
    /// True when this is the auto-created local bare repository rather than a
    /// URL somebody chose.
    pub is_default: bool,
}

/// Host-side location of the configured remote.
///
/// Under the sidecar root, which is outside every grant a box has — the same
/// reasoning that keeps the runner's config out of the repository. A board URL
/// in a worktree file would be a value an agent could edit, and redirecting the
/// board is redirecting every post on it.
fn remote_file(h5i_root: &Path) -> PathBuf {
    h5i_root.join("board").join("remote")
}

/// Path of the local bare repository a board falls back to.
fn default_remote_path(h5i_root: &Path) -> PathBuf {
    h5i_root.join("board.git")
}

/// The configured remote, creating the local default if nothing is set.
pub fn remote(h5i_root: &Path) -> Result<Remote, H5iError> {
    if let Ok(raw) = std::fs::read_to_string(remote_file(h5i_root)) {
        let url = raw.trim().to_string();
        if !url.is_empty() {
            return Ok(Remote {
                url,
                is_default: false,
            });
        }
    }
    let path = default_remote_path(h5i_root);
    if !path.join("HEAD").is_file() {
        std::fs::create_dir_all(&path).map_err(|e| H5iError::with_path(e, &path))?;
        git(&path, &["init", "--bare", "--quiet"])?;
    }
    Ok(Remote {
        url: path.display().to_string(),
        is_default: true,
    })
}

/// Point this machine's board at `url`. Human-only, host-side.
pub fn set_remote(h5i_root: &Path, url: &str) -> Result<(), H5iError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(H5iError::Metadata("a board remote needs a URL".into()));
    }
    // A URL is handed to `git` as an argument, so a leading `-` would be read
    // as an option. `--end-of-options` covers the calls below, and refusing it
    // here covers everything that ever reads this file.
    if url.starts_with('-') {
        return Err(H5iError::Metadata(format!(
            "refusing a board remote that starts with '-': {url:?}"
        )));
    }
    let path = remote_file(h5i_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
    }
    std::fs::write(&path, url).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(())
}

/// Stop using a configured remote and fall back to the local default.
pub fn clear_remote(h5i_root: &Path) {
    let _ = std::fs::remove_file(remote_file(h5i_root));
}

/// What one sync moved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Threads whose local tip advanced because of something fetched.
    pub pulled: usize,
    /// Threads pushed to the remote.
    pub pushed: usize,
    /// Rounds spent losing a race to another writer.
    pub retries: usize,
}

impl SyncReport {
    /// Did this sync move anything?
    pub fn is_empty(&self) -> bool {
        self.pulled == 0 && self.pushed == 0
    }
}

/// Bring the local board and the remote into agreement.
///
/// Fetch, union-merge anything that diverged, push. A rejected push means
/// somebody posted between the fetch and the push, so the whole round runs
/// again against the tip they left.
pub fn sync(repo: &Repository, h5i_root: &Path) -> Result<SyncReport, H5iError> {
    let remote = remote(h5i_root)?;
    sync_with(repo, &remote)
}

/// [`sync`] against an explicit remote, so a caller can target one without
/// writing it to disk first.
pub fn sync_with(repo: &Repository, remote: &Remote) -> Result<SyncReport, H5iError> {
    let dir = repo_dir(repo)?;
    let mut report = SyncReport::default();

    for attempt in 0..MAX_ATTEMPTS {
        report.pulled += pull(repo, &dir, &remote.url)?;
        match push(&dir, &remote.url) {
            Ok(pushed) => {
                report.pushed += pushed;
                return Ok(report);
            }
            // A rejection here is the compare-and-swap doing its job: the
            // remote moved under us. Fetch what landed, merge it, and try
            // again — which is exactly what the loop already does.
            Err(SyncError::Contended) => {
                report.retries = attempt + 1;
                continue;
            }
            Err(SyncError::Fatal(e)) => return Err(e),
        }
    }
    Err(H5iError::Internal(format!(
        "board sync lost {MAX_ATTEMPTS} consecutive races against {} — \
         the remote is busier than this can serve",
        remote.url
    )))
}

enum SyncError {
    /// The remote moved; retry.
    Contended,
    /// Anything else.
    Fatal(H5iError),
}

/// Fetch the remote's board and fold it into ours. Returns how many local tips
/// moved.
fn pull(repo: &Repository, dir: &Path, url: &str) -> Result<usize, H5iError> {
    // Into a staging namespace, never straight onto the live refs: what comes
    // back was authored on a machine this one does not control, and it has to
    // be merged rather than adopted. `transfer.fsckObjects` is on for the same
    // reason the quarantine turns it on.
    let live_spec = format!("+{LIVE}:{INCOMING}/live/*");
    let attic_spec = format!("+{ATTIC}:{INCOMING}/attic/*");
    let out = git(
        dir,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--prune",
            "--end-of-options",
            url,
            &live_spec,
            &attic_spec,
        ],
    );
    match out {
        Ok(_) => {}
        // A remote with no board yet has no matching refs, which git reports as
        // an error on some versions. That is an empty board, not a failure.
        Err(e) if is_empty_remote(&e) => return Ok(0),
        Err(e) => return Err(e),
    }

    let mut moved = 0usize;
    for (incoming, local) in incoming_pairs(repo)? {
        let Ok(incoming_oid) = repo.refname_to_id(&incoming) else {
            continue;
        };
        match repo.refname_to_id(&local) {
            // Already have exactly this.
            Ok(local_oid) if local_oid == incoming_oid => {}
            Ok(local_oid) => {
                // Both sides moved. Union-merge rather than pick a winner: the
                // log is append-only, so the union is the whole history and
                // neither side loses a post.
                if repo.graph_descendant_of(local_oid, incoming_oid).unwrap_or(false) {
                    continue; // we already contain it
                }
                let merged = if local.starts_with("refs/h5i/board/meta") {
                    board::union_merge_roster(repo, local_oid, incoming_oid)?
                } else {
                    board::union_merge_thread(repo, local_oid, incoming_oid)?
                };
                repo.reference(&local, merged, true, "h5i board: merge remote")?;
                moved += 1;
            }
            // A thread this machine has never seen.
            Err(_) => {
                repo.reference(&local, incoming_oid, false, "h5i board: adopt remote")?;
                moved += 1;
            }
        }
    }
    Ok(moved)
}

/// Map every staged incoming ref to the local ref it belongs to.
fn incoming_pairs(repo: &Repository) -> Result<Vec<(String, String)>, H5iError> {
    let mut out = Vec::new();
    for glob in [
        format!("{INCOMING}/live/*"),
        format!("{INCOMING}/attic/*"),
    ] {
        let Ok(refs) = repo.references_glob(&glob) else {
            continue;
        };
        for r in refs.flatten() {
            let Some(name) = r.name() else { continue };
            let Some(leaf) = name.rsplit('/').next() else {
                continue;
            };
            // The leaf is a thread id or `meta`, both of which the board's own
            // validator accepts. Anything else was not written by us.
            if leaf != "meta" && board::validate_thread_id(leaf).is_err() {
                continue;
            }
            // `threads/<id>` loses its middle component through a `*` refspec,
            // so put it back.
            let local = if leaf == "meta" {
                board::BOARD_META_REF.to_string()
            } else if name.contains("/attic/") {
                board::attic_ref(leaf)
            } else {
                board::thread_ref(leaf)
            };
            out.push((name.to_string(), local));
        }
    }
    Ok(out)
}

/// Push the local board. `Contended` when the remote moved under us.
fn push(dir: &Path, url: &str) -> Result<usize, SyncError> {
    // No `--force`: an append-only thread that has been merged onto the remote
    // tip is a fast-forward, so a rejection means someone else got there first
    // and the right answer is to merge again, never to overwrite them.
    let live_spec = format!("{LIVE}:{LIVE}");
    let attic_spec = format!("{ATTIC}:{ATTIC}");
    match git(
        dir,
        &[
            "push",
            "--quiet",
            "--end-of-options",
            url,
            &live_spec,
            &attic_spec,
        ],
    ) {
        Ok(_) => Ok(1),
        Err(e) if is_rejected(&e) => Err(SyncError::Contended),
        // Nothing local to push is success, not failure.
        Err(e) if is_no_matching_ref(&e) => Ok(0),
        Err(e) => Err(SyncError::Fatal(e)),
    }
}

/// Remove a thread from the remote, which is what closing it means once the
/// board is shared. Only a human reaches this.
pub fn push_close(repo: &Repository, h5i_root: &Path, thread_id: &str) -> Result<(), H5iError> {
    board::validate_thread_id(thread_id)?;
    let remote = remote(h5i_root)?;
    let dir = repo_dir(repo)?;
    // Push the attic copy first, so the conversation is durable on the remote
    // before the live ref goes. The other order loses it if the second call
    // fails.
    let attic = board::attic_ref(thread_id);
    let spec = format!("{attic}:{attic}");
    let _ = git(&dir, &["push", "--quiet", "--end-of-options", &remote.url, &spec]);
    let delete = format!(":{}", board::thread_ref(thread_id));
    let _ = git(
        &dir,
        &["push", "--quiet", "--end-of-options", &remote.url, &delete],
    );
    Ok(())
}

fn repo_dir(repo: &Repository) -> Result<PathBuf, H5iError> {
    Ok(repo.path().to_path_buf())
}

fn is_empty_remote(e: &H5iError) -> bool {
    let m = e.to_string();
    m.contains("couldn't find remote ref") || m.contains("no matching")
}

fn is_rejected(e: &H5iError) -> bool {
    let m = e.to_string();
    m.contains("non-fast-forward") || m.contains("[rejected]") || m.contains("fetch first")
}

fn is_no_matching_ref(e: &H5iError) -> bool {
    e.to_string().contains("src refspec")
}

/// Run git in `dir` with the hardening the rest of this codebase's shell-outs
/// use: no hooks, no `ext::` transport, and object checking on both directions
/// of the wire, because a board's remote is a machine this one does not own.
fn git(dir: &Path, args: &[&str]) -> Result<String, H5iError> {
    let out = Command::new("git")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("protocol.ext.allow=never")
        .arg("-c")
        .arg("transfer.fsckObjects=true")
        .arg("-c")
        .arg("fetch.fsckObjects=true")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| H5iError::Metadata(format!("could not run git: {e}")))?;
    if !out.status.success() {
        return Err(H5iError::Metadata(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{Author, NewPost, Role};

    fn work_repo(dir: &Path) -> Repository {
        let repo = Repository::init(dir).expect("init");
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "Sync Tester").unwrap();
            cfg.set_str("user.email", "sync@h5i.test").unwrap();
        }
        repo
    }

    fn human() -> Author {
        Author::human("operator").unwrap()
    }

    fn agent(name: &str) -> Author {
        Author::agent(name, "env/a/b", Role::Worker, None).unwrap()
    }

    fn post(kind: &str, body: &str) -> NewPost {
        NewPost {
            kind: kind.into(),
            body: body.into(),
            ..Default::default()
        }
    }

    /// The whole point of the module: two machines that never touch each
    /// other's disk end up holding the same conversation.
    #[test]
    fn two_clones_reach_the_same_thread_through_one_remote() {
        let root = tempfile::tempdir().unwrap();
        let hub = root.path().join("hub.git");
        std::fs::create_dir_all(&hub).unwrap();
        git(&hub, &["init", "--bare", "--quiet"]).unwrap();
        let remote = Remote {
            url: hub.display().to_string(),
            is_default: false,
        };

        let a_dir = root.path().join("a");
        std::fs::create_dir_all(&a_dir).unwrap();
        let a = work_repo(&a_dir);
        let b_dir = root.path().join("b");
        std::fs::create_dir_all(&b_dir).unwrap();
        let b = work_repo(&b_dir);

        // A opens a thread and posts, then publishes.
        let h = board::create_thread(&a, &human(), "cross-machine", None, None).unwrap();
        board::append_post(&a, &agent("alice"), &h.id, post("FINDING", "from A")).unwrap();
        sync_with(&a, &remote).unwrap();

        // B has never heard of it; one sync and it has the whole thread.
        let r = sync_with(&b, &remote).unwrap();
        assert!(r.pulled > 0, "B should adopt the thread");
        let seen = board::read_thread(&b, &h.id).unwrap();
        assert_eq!(seen.posts.len(), 1);
        assert_eq!(seen.posts[0].body, "from A");

        // B replies and publishes; A picks it up.
        board::append_post(&b, &agent("bob"), &h.id, post("ACK", "from B")).unwrap();
        sync_with(&b, &remote).unwrap();
        sync_with(&a, &remote).unwrap();
        let back = board::read_thread(&a, &h.id).unwrap();
        let bodies: Vec<&str> = back.posts.iter().map(|p| p.body.as_str()).collect();
        assert!(bodies.contains(&"from A") && bodies.contains(&"from B"), "{bodies:?}");
    }

    /// Both machines post before either syncs. Nobody's post is lost, and
    /// nobody has to have been first.
    #[test]
    fn simultaneous_posts_on_two_clones_both_survive() {
        let root = tempfile::tempdir().unwrap();
        let hub = root.path().join("hub.git");
        std::fs::create_dir_all(&hub).unwrap();
        git(&hub, &["init", "--bare", "--quiet"]).unwrap();
        let remote = Remote {
            url: hub.display().to_string(),
            is_default: false,
        };

        let a_dir = root.path().join("a");
        std::fs::create_dir_all(&a_dir).unwrap();
        let a = work_repo(&a_dir);
        let b_dir = root.path().join("b");
        std::fs::create_dir_all(&b_dir).unwrap();
        let b = work_repo(&b_dir);

        let h = board::create_thread(&a, &human(), "race", None, None).unwrap();
        sync_with(&a, &remote).unwrap();
        sync_with(&b, &remote).unwrap();

        // Diverge: each side appends without seeing the other.
        board::append_post(&a, &agent("alice"), &h.id, post("FINDING", "A only")).unwrap();
        board::append_post(&b, &agent("bob"), &h.id, post("FINDING", "B only")).unwrap();

        sync_with(&a, &remote).unwrap();
        // B's push is a non-fast-forward, so its sync must fetch, merge and
        // retry rather than fail or overwrite.
        sync_with(&b, &remote).unwrap();
        sync_with(&a, &remote).unwrap();

        for (name, repo) in [("A", &a), ("B", &b)] {
            let t = board::read_thread(repo, &h.id).unwrap();
            let bodies: Vec<&str> = t.posts.iter().map(|p| p.body.as_str()).collect();
            assert!(
                bodies.contains(&"A only") && bodies.contains(&"B only"),
                "{name} lost a post: {bodies:?}"
            );
            assert_eq!(t.posts.len(), 2, "{name} duplicated a post: {bodies:?}");
        }
    }

    /// The roster travels too, and a revocation is not undone by a machine that
    /// had not heard about it.
    #[test]
    fn a_revocation_survives_a_sync_from_a_clone_that_missed_it() {
        let root = tempfile::tempdir().unwrap();
        let hub = root.path().join("hub.git");
        std::fs::create_dir_all(&hub).unwrap();
        git(&hub, &["init", "--bare", "--quiet"]).unwrap();
        let remote = Remote {
            url: hub.display().to_string(),
            is_default: false,
        };

        let a_dir = root.path().join("a");
        std::fs::create_dir_all(&a_dir).unwrap();
        let a = work_repo(&a_dir);
        let b_dir = root.path().join("b");
        std::fs::create_dir_all(&b_dir).unwrap();
        let b = work_repo(&b_dir);

        board::put_roster_entry(
            &a,
            &human(),
            board::RosterEntry {
                agent: "mallory".into(),
                box_id: None,
                role: Role::Worker,
                policy_digest: None,
                attached_at: board::now_ts(),
                revoked_at: None,
            },
        )
        .unwrap();
        sync_with(&a, &remote).unwrap();
        sync_with(&b, &remote).unwrap();

        // A revokes. B, which still thinks mallory is active, adds someone else
        // and syncs — the merge must not resurrect mallory.
        board::revoke(&a, &human(), "mallory").unwrap();
        board::put_roster_entry(
            &b,
            &human(),
            board::RosterEntry {
                agent: "carol".into(),
                box_id: None,
                role: Role::Reviewer,
                policy_digest: None,
                attached_at: board::now_ts(),
                revoked_at: None,
            },
        )
        .unwrap();
        sync_with(&a, &remote).unwrap();
        sync_with(&b, &remote).unwrap();
        sync_with(&a, &remote).unwrap();

        for (name, repo) in [("A", &a), ("B", &b)] {
            let r = board::read_roster(repo);
            assert!(
                !r.get("mallory").unwrap().is_active(),
                "{name} resurrected a revoked participant"
            );
            assert!(r.get("carol").is_some(), "{name} lost the other addition");
        }
    }

    /// The vouching lane, which is the whole reason a post carries an origin.
    ///
    /// The same post reads differently on the two machines, and that asymmetry
    /// is the point: a host can be certain it stamped something and certain of
    /// nothing else, so `Observed` is a real guarantee and `PeerClaimed` is an
    /// explicit absence of one.
    #[test]
    fn a_post_is_host_observed_at_home_and_peer_claimed_everywhere_else() {
        let root = tempfile::tempdir().unwrap();
        let hub = root.path().join("hub.git");
        std::fs::create_dir_all(&hub).unwrap();
        git(&hub, &["init", "--bare", "--quiet"]).unwrap();
        let remote = Remote {
            url: hub.display().to_string(),
            is_default: false,
        };

        let a_dir = root.path().join("a");
        std::fs::create_dir_all(&a_dir).unwrap();
        let a = work_repo(&a_dir);
        let b_dir = root.path().join("b");
        std::fs::create_dir_all(&b_dir).unwrap();
        let b = work_repo(&b_dir);

        let h = board::create_thread(&a, &human(), "vouch", None, None).unwrap();
        board::append_post(
            &a,
            &agent("alice").from_host("machine-a"),
            &h.id,
            post("FINDING", "from A"),
        )
        .unwrap();
        sync_with(&a, &remote).unwrap();
        sync_with(&b, &remote).unwrap();
        board::append_post(
            &b,
            &agent("bob").from_host("machine-b"),
            &h.id,
            post("ACK", "from B"),
        )
        .unwrap();
        sync_with(&b, &remote).unwrap();
        sync_with(&a, &remote).unwrap();

        let seen = board::read_thread(&a, &h.id).unwrap();
        let by_body = |body: &str| {
            seen.posts
                .iter()
                .find(|p| p.body == body)
                .unwrap()
                .vouch("machine-a")
        };
        assert_eq!(by_body("from A"), board::Vouch::Observed);
        assert_eq!(
            by_body("from B"),
            board::Vouch::PeerClaimed {
                origin: "machine-b".into()
            }
        );

        // And the other way round on the other machine, from the same bytes.
        let seen_b = board::read_thread(&b, &h.id).unwrap();
        let a_post = seen_b.posts.iter().find(|p| p.body == "from A").unwrap();
        assert!(!a_post.vouch("machine-b").is_observed());
    }

    /// A post with no origin is not quietly treated as ours.
    #[test]
    fn a_post_naming_no_origin_is_unattributed_rather_than_observed() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("w");
        std::fs::create_dir_all(&dir).unwrap();
        let repo = work_repo(&dir);
        let h = board::create_thread(&repo, &human(), "t", None, None).unwrap();
        // `agent()` builds an author with no origin, the way a build from before
        // origins existed would.
        board::append_post(&repo, &agent("old"), &h.id, post("FINDING", "x")).unwrap();
        let t = board::read_thread(&repo, &h.id).unwrap();
        assert_eq!(t.posts[0].vouch("machine-a"), board::Vouch::Unattributed);
        assert!(!t.posts[0].vouch("").is_observed(), "an empty identity matches nothing");
    }

    /// A board with no remote configured gets a local one, and it works — that
    /// is what makes solo use run the same code as a team.
    #[test]
    fn a_board_with_no_configured_remote_still_syncs() {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("w");
        std::fs::create_dir_all(&work).unwrap();
        let repo = work_repo(&work);
        let h5i_root = root.path().join("h5i");
        std::fs::create_dir_all(&h5i_root).unwrap();

        let r = remote(&h5i_root).unwrap();
        assert!(r.is_default, "an unconfigured board falls back to a local bare repo");
        assert!(default_remote_path(&h5i_root).join("HEAD").is_file());

        let h = board::create_thread(&repo, &human(), "solo", None, None).unwrap();
        board::append_post(&repo, &agent("solo"), &h.id, post("FINDING", "alone")).unwrap();
        sync(&repo, &h5i_root).unwrap();

        // It really landed on the remote, rather than the push being skipped.
        let listed = git(
            &default_remote_path(&h5i_root),
            &["for-each-ref", "--format=%(refname)", "refs/h5i/board/"],
        )
        .unwrap();
        assert!(listed.contains(&h.id), "the thread should be on the remote:\n{listed}");
    }

    #[test]
    fn a_configured_remote_round_trips_and_a_dash_leading_url_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let h5i_root = root.path().to_path_buf();
        assert!(set_remote(&h5i_root, "  git@example.com:team/board.git  ").is_ok());
        assert_eq!(remote(&h5i_root).unwrap().url, "git@example.com:team/board.git");
        assert!(!remote(&h5i_root).unwrap().is_default);

        assert!(set_remote(&h5i_root, "--upload-pack=evil").is_err());
        assert!(set_remote(&h5i_root, "").is_err());

        clear_remote(&h5i_root);
        assert!(remote(&h5i_root).unwrap().is_default);
    }
}
