//! The forum's one way in and out: an ordinary git remote.
//!
//! Every forum has one, including a one-person forum on one machine. Letting
//! same-machine boxes write the refs directly would cost coverage rather than
//! performance: the shortcut becomes the only path anyone runs, and the sync
//! path rots until a second machine joins. Solo and team differ by a URL.
//!
//! A remote rather than a service because nobody has to run it. A team's git
//! host already answers **who may post** (push access) and **who may read**
//! (read access), and it gives compare-and-swap away free: a non-fast-forward
//! push to `refs/h5i/*` is rejected server-side, and append-only threads make
//! every honest update a fast-forward, so the ordinary rejection *is* the CAS.
//! Measured against GitHub rather than assumed.
//!
//! **Nothing here deletes, and nothing depends on a ref being absent.** Closing
//! a thread is a `CLOSED` post, which is what makes a human's decision survive a
//! peer that had not heard about it, and it declaws the obvious attack: push
//! access is enough to `git push --delete` a thread ref, but the next sync from
//! any clone that still holds it puts it back, because the push is driven by
//! what we have rather than by what the remote lacks. Deletion buys a window.
//!
//! **Agents never speak this.** A box has no git credential, no route to the
//! remote and no code path here; it writes into its spool and the host does the
//! rest. Same split as the remote runner: the worker is h5i, the host holds the
//! key.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use git2::Repository;

use crate::forum;
use crate::error::H5iError;

/// Wall-clock ceiling on any one git invocation.
///
/// A fetch or push of a small forum is subsecond against a healthy remote;
/// this is far past that, so it never trips on a slow-but-alive host. What it
/// bounds is a *dead* one. Git has no timeout of its own for an unresponsive
/// SSH or TCP peer, and the tender runs `sync` on a thread the session joins
/// on shutdown — so without this ceiling a partitioned remote turns "the box
/// exited" into "the box will not exit", and a plain `h5i forum sync` into a
/// command that never returns. A killed git leaves no corruption: its writes
/// are lockfile-and-rename, so a bounded push simply did not land, and the
/// next sync retries.
const GIT_TIMEOUT: Duration = Duration::from_secs(45);

/// How often the wait loop polls for the child while bounding it.
const GIT_POLL: Duration = Duration::from_millis(50);

/// Refs the forum owns locally. The remote side is [`Remote::namespace`].
const LIVE: &str = "refs/h5i/forum/*";

/// Default remote namespace: the same custom one used locally.
///
/// Invisible to `git branch`, and adequate for a local bare repository or any
/// remote where nobody hostile has push access.
pub const NS_CUSTOM: &str = "refs/h5i/forum";

/// Remote namespace that a forge will protect.
///
/// The reason to want it: a custom ref namespace gets **no** server-side
/// protection. GitHub's branch protection and rulesets only apply to
/// `refs/heads/**`, so under `NS_CUSTOM` anyone with push access can
/// `git push --delete` a thread or force-push over it, and nothing at the
/// server refuses. Publishing under branches instead lets a repository admin
/// switch on "restrict deletions" and "block force pushes" for the pattern
/// `h5i-forum/**`, which turns this design's *self-healing* into actual
/// prevention.
///
/// The cost is that threads show up in `git branch -a` and in branch pickers.
/// Namespacing contains that; it does not remove it. So it is a choice, and a
/// solo forum on a local bare repository has no reason to pay it.
pub const NS_BRANCH: &str = "refs/heads/h5i-forum";
/// Where a fetch parks the remote's view before it is merged in.
const INCOMING: &str = "refs/h5i/forum-incoming";

/// Attempts before a sync gives up. Each retry is a fetch, a merge and a push;
/// losing this many consecutive races means the forum is busier than a git
/// remote can serve, which is a different problem from a lost update.
const MAX_ATTEMPTS: usize = 8;

/// Where this machine's forum lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    /// A git URL, or a path. Anything `git fetch` accepts.
    pub url: String,
    /// True when this is the auto-created local bare repository rather than a
    /// URL somebody chose.
    pub is_default: bool,
    /// Ref namespace on the remote: [`NS_CUSTOM`] or [`NS_BRANCH`]. The local
    /// mirror is always [`LIVE`], so this changes only what is published.
    pub namespace: String,
}

impl Remote {
    /// Are the forum's threads published as branches a forge can protect?
    pub fn is_branch_namespace(&self) -> bool {
        self.namespace == NS_BRANCH
    }
}

/// Host-side location of the configured remote.
///
/// Under the sidecar root, which is outside every grant a box has — the same
/// reasoning that keeps the runner's config out of the repository. A forum URL
/// in a worktree file would be a value an agent could edit, and redirecting the
/// forum is redirecting every post on it.
fn remote_file(h5i_root: &Path) -> PathBuf {
    h5i_root.join("forum").join("remote")
}

/// Host-side location of the chosen remote namespace.
fn namespace_file(h5i_root: &Path) -> PathBuf {
    h5i_root.join("forum").join("namespace")
}

fn read_namespace(h5i_root: &Path) -> String {
    match std::fs::read_to_string(namespace_file(h5i_root)) {
        Ok(raw) if raw.trim() == NS_BRANCH => NS_BRANCH.to_string(),
        _ => NS_CUSTOM.to_string(),
    }
}

/// Path of the local bare repository a forum falls back to.
fn default_remote_path(h5i_root: &Path) -> PathBuf {
    h5i_root.join("forum.git")
}

/// The configured remote, creating the local default if nothing is set.
pub fn remote(h5i_root: &Path) -> Result<Remote, H5iError> {
    if let Ok(raw) = std::fs::read_to_string(remote_file(h5i_root)) {
        let url = raw.trim().to_string();
        if !url.is_empty() {
            return Ok(Remote {
                url,
                is_default: false,
                namespace: read_namespace(h5i_root),
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
        namespace: read_namespace(h5i_root),
    })
}

/// Point this machine's forum at `url`. Human-only, host-side.
pub fn set_remote(h5i_root: &Path, url: &str) -> Result<(), H5iError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(H5iError::Metadata("a forum remote needs a URL".into()));
    }
    // A URL is handed to `git` as an argument, so a leading `-` would be read
    // as an option. `--end-of-options` covers the calls below, and refusing it
    // here covers everything that ever reads this file.
    if url.starts_with('-') {
        return Err(H5iError::Metadata(format!(
            "refusing a forum remote that starts with '-': {url:?}"
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

/// Choose whether threads are published as protectable branches.
pub fn set_namespace(h5i_root: &Path, branch_refs: bool) -> Result<(), H5iError> {
    let path = namespace_file(h5i_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
    }
    let ns = if branch_refs { NS_BRANCH } else { NS_CUSTOM };
    std::fs::write(&path, ns).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(())
}

/// What one sync moved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Refs whose local tip advanced because of something fetched.
    pub pulled: usize,
    /// Refs the remote was missing and was sent.
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

/// Bring the local forum and the remote into agreement.
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
        report.pulled += pull(repo, &dir, remote)?;
        // What the remote is missing, counted against the tips this round's
        // fetch staged. `git push` cannot say this itself — a push where
        // everything was already up to date succeeds exactly like one that
        // moved something — and a report that counted attempts rather than
        // movement kept an idle forum looking busy forever.
        let pending = pending_push(repo);
        if pending == 0 {
            return Ok(report);
        }
        match push(&dir, remote) {
            Ok(()) => {
                report.pushed += pending;
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
        "forum sync lost {MAX_ATTEMPTS} consecutive races against {} — \
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

/// Fetch the remote's forum and fold it into ours. Returns how many local tips
/// moved.
fn pull(repo: &Repository, dir: &Path, remote: &Remote) -> Result<usize, H5iError> {
    // Into a staging namespace, never straight onto the live refs: what comes
    // back was authored on a machine this one does not control, and it has to
    // be merged rather than adopted. `transfer.fsckObjects` is on for the same
    // reason the quarantine turns it on.
    let live_spec = format!("+{}/*:{INCOMING}/live/*", remote.namespace);
    let out = git(
        dir,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--prune",
            "--end-of-options",
            &remote.url,
            &live_spec,
        ],
    );
    match out {
        Ok(_) => {}
        // A remote with no forum yet has no matching refs, which git reports as
        // an error on some versions. That is an empty forum, not a failure —
        // but it also means `--prune` never ran, and the staging namespace may
        // still mirror a *previous* remote. Left in place, those tips would
        // tell `pending_push` the new remote already holds everything the old
        // one did, and the push that should seed it would be skipped forever.
        // An empty remote stages nothing, so make the staging say so.
        Err(e) if is_empty_remote(&e) => {
            clear_staging(repo);
            return Ok(0);
        }
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
                if repo.graph_descendant_of(local_oid, incoming_oid).unwrap_or(false) {
                    continue; // we already contain it
                }
                // The remote is simply ahead: take its tip. Merging here would
                // be *correct* — the union of a history with its own prefix —
                // but it would mint a commit, and that commit is news to the
                // next peer, whose own no-op merge is news back to us. Two
                // quiet clones then trade contentless merge commits forever.
                // Measured before this branch existed: one post followed by
                // six idle sync round-trips left the thread ref twelve commits
                // deep, all of them empty merges — and the resident tender
                // syncs every second. Fast-forward is not an optimisation; it
                // is what makes an idle forum go quiet.
                if repo.graph_descendant_of(incoming_oid, local_oid).unwrap_or(false) {
                    repo.reference(&local, incoming_oid, true, "h5i forum: fast-forward remote")?;
                    moved += 1;
                    continue;
                }
                // Both sides moved. Union-merge rather than pick a winner: the
                // log is append-only, so the union is the whole history and
                // neither side loses a post.
                let merged = if local.starts_with("refs/h5i/forum/meta") {
                    forum::union_merge_meta(repo, local_oid, incoming_oid)?
                } else {
                    forum::union_merge_thread(repo, local_oid, incoming_oid)?
                };
                repo.reference(&local, merged, true, "h5i forum: merge remote")?;
                moved += 1;
            }
            // A thread this machine has never seen.
            Err(_) => {
                repo.reference(&local, incoming_oid, false, "h5i forum: adopt remote")?;
                moved += 1;
            }
        }
    }
    Ok(moved)
}

/// Drop every staged incoming ref, so the staging namespace reflects a remote
/// that has nothing rather than whichever remote was fetched last.
fn clear_staging(repo: &Repository) {
    let Ok(refs) = repo.references_glob(&format!("{INCOMING}/live/*")) else {
        return;
    };
    let names: Vec<String> = refs
        .filter_map(|r| r.ok())
        .filter_map(|r| r.name().map(str::to_string))
        .collect();
    for name in names {
        if let Ok(mut r) = repo.find_reference(&name) {
            let _ = r.delete();
        }
    }
}

/// Map every staged incoming ref to the local ref it belongs to.
fn incoming_pairs(repo: &Repository) -> Result<Vec<(String, String)>, H5iError> {
    let mut out = Vec::new();
    for glob in [format!("{INCOMING}/live/*")] {
        let Ok(refs) = repo.references_glob(&glob) else {
            continue;
        };
        for r in refs.flatten() {
            let Some(name) = r.name() else { continue };
            let Some(leaf) = name.rsplit('/').next() else {
                continue;
            };
            // The leaf is a thread id or `meta`, both of which the forum's own
            // validator accepts. Anything else was not written by us.
            if leaf != "meta" && forum::validate_thread_id(leaf).is_err() {
                continue;
            }
            // `threads/<id>` loses its middle component through a `*` refspec,
            // so put it back.
            let local = if leaf == "meta" {
                forum::FORUM_META_REF.to_string()
            } else {
                forum::thread_ref(leaf)
            };
            out.push((name.to_string(), local));
        }
    }
    Ok(out)
}

/// Local forum refs the remote does not have yet, measured against the tips
/// the current round's fetch parked in the staging namespace. Zero means the
/// remote already holds everything we do, and the push can be skipped.
fn pending_push(repo: &Repository) -> usize {
    let Ok(refs) = repo.references_glob("refs/h5i/forum/*") else {
        return 0;
    };
    refs.filter_map(|r| r.ok())
        .filter_map(|r| Some((r.name()?.to_string(), r.target()?)))
        .filter(|(name, oid)| {
            let Some(rest) = name.strip_prefix("refs/h5i/forum/") else {
                return false;
            };
            repo.refname_to_id(&format!("{INCOMING}/live/{rest}")).ok() != Some(*oid)
        })
        .count()
}

/// Push the local forum. `Contended` when the remote moved under us.
fn push(dir: &Path, remote: &Remote) -> Result<(), SyncError> {
    // No `--force`: an append-only thread that has been merged onto the remote
    // tip is a fast-forward, so a rejection means someone else got there first
    // and the right answer is to merge again, never to overwrite them.
    let live_spec = format!("{LIVE}:{}/*", remote.namespace);
    match git(
        dir,
        &["push", "--quiet", "--end-of-options", &remote.url, &live_spec],
    ) {
        Ok(_) => Ok(()),
        Err(e) if is_rejected(&e) => Err(SyncError::Contended),
        // Nothing local to push is success, not failure.
        Err(e) if is_no_matching_ref(&e) => Ok(()),
        Err(e) => Err(SyncError::Fatal(e)),
    }
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
/// of the wire, because a forum's remote is a machine this one does not own.
///
/// Two pieces of the caller's environment are overridden, because this module
/// depends on things most git invocations do not:
///
/// - `LC_ALL=C`: the sync loop *classifies* git's stderr — a non-fast-forward
///   rejection is the compare-and-swap saying retry, not an error — and git
///   localises its messages. On a Japanese locale the marker text would never
///   match, and every contended push would surface as a failure instead of a
///   retry.
/// - `GIT_DIR` and its companions are dropped: they redirect git away from the
///   repository `current_dir` names, and a shell that exported them (a hook,
///   a script) would have this module fetching and pushing someone else's
///   refs. The repository is an argument here, never ambient state.
fn git(dir: &Path, args: &[&str]) -> Result<String, H5iError> {
    let mut cmd = Command::new("git");
    cmd.env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_NAMESPACE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("protocol.ext.allow=never")
        .arg("-c")
        .arg("transfer.fsckObjects=true")
        .arg("-c")
        .arg("fetch.fsckObjects=true")
        .args(args)
        .current_dir(dir);
    let label = args.first().copied().unwrap_or("");
    run_bounded(cmd, GIT_TIMEOUT, label)
}

/// Run `cmd` to completion, or kill it and error if it outruns `timeout`.
///
/// The output pipes are drained on their own threads: a child that fills a
/// pipe buffer and blocks on the write would never reach `try_wait`, so
/// polling the exit status without reading the pipes would deadlock on exactly
/// the hang this bound exists to prevent. A killed child leaves no corruption
/// here — git's writes are lockfile-and-rename — so the operation simply did
/// not land and the next sync retries.
fn run_bounded(mut cmd: Command, timeout: Duration, label: &str) -> Result<String, H5iError> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| H5iError::Metadata(format!("could not run git: {e}")))?;

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = stdout_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = stderr_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    // The remote is not answering. Kill the child and return at
                    // once, rather than holding a session's shutdown open on a
                    // dead peer. The reader threads are deliberately *not*
                    // joined here: git fetch/push spawns an `ssh` grandchild
                    // that inherits the output pipe, and killing git does not
                    // kill ssh — so a join would block on the pipe staying open
                    // until ssh itself gave up, which is the very wait this
                    // bound exists to cut. The readers detach and end when the
                    // grandchild does; their buffers are discarded.
                    let _ = child.kill();
                    let _ = child.wait();
                    drop(out_reader);
                    drop(err_reader);
                    return Err(H5iError::Metadata(format!(
                        "git {label} exceeded {}s against the remote and was stopped — \
                         the remote is unreachable or not responding",
                        timeout.as_secs()
                    )));
                }
                std::thread::sleep(GIT_POLL);
            }
            Err(e) => return Err(H5iError::Metadata(format!("git wait failed: {e}"))),
        }
    };

    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    if !status.success() {
        return Err(H5iError::Metadata(format!(
            "git {label} failed: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forum::{Author, NewPost, Role};

    /// A timeout must never read as contention. The sync loop retries a
    /// `Contended` push up to eight times; if a timed-out git were classified
    /// that way, a dead remote would turn one 45s wait into eight — six
    /// minutes — instead of failing once. So the timeout message must match
    /// none of the retry markers.
    #[test]
    fn a_timeout_is_fatal_not_contention() {
        let timeout = H5iError::Metadata(
            "git push exceeded 45s against the remote and was stopped — \
             the remote is unreachable or not responding"
                .into(),
        );
        assert!(!is_rejected(&timeout), "a timeout is not a non-fast-forward rejection");
        assert!(!is_no_matching_ref(&timeout), "a timeout is not an empty push");
        assert!(!is_empty_remote(&timeout), "a timeout is not an empty remote");
        // And the markers still classify what they should.
        let rejected = H5iError::Metadata("![rejected] (non-fast-forward)".into());
        assert!(is_rejected(&rejected));
    }

    /// A git that never answers is killed at the deadline rather than hanging
    /// the caller — which is what keeps a dead remote from wedging a box
    /// session's shutdown, since the tender's thread is joined on drop.
    #[test]
    fn a_hanging_git_is_bounded_not_waited_on_forever() {
        // `sh -c 'sleep 60'` stands in for a git that has connected to a dead
        // peer and is waiting on a socket that will never answer.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 60"]);
        let start = Instant::now();
        let r = run_bounded(cmd, Duration::from_millis(300), "fetch");
        let elapsed = start.elapsed();
        assert!(r.is_err(), "a child that outruns the timeout must error");
        assert!(
            r.unwrap_err().to_string().contains("unreachable or not responding"),
            "the error should name the cause"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "it must return near the deadline, not after the child would finish: {elapsed:?}"
        );
    }

    /// The bound does not truncate a healthy command's output.
    #[test]
    fn a_prompt_command_returns_its_full_output_within_the_bound() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf 'hello world'"]);
        let out = run_bounded(cmd, Duration::from_secs(5), "test").unwrap();
        assert_eq!(out, "hello world");
    }

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
            namespace: NS_CUSTOM.to_string(),
        };

        let a_dir = root.path().join("a");
        std::fs::create_dir_all(&a_dir).unwrap();
        let a = work_repo(&a_dir);
        let b_dir = root.path().join("b");
        std::fs::create_dir_all(&b_dir).unwrap();
        let b = work_repo(&b_dir);

        // A opens a thread and posts, then publishes.
        let h = forum::create_thread(&a, &human(), "cross-machine", None, None, None).unwrap();
        forum::append_post(&a, &agent("alice"), &h.id, post("FINDING", "from A")).unwrap();
        sync_with(&a, &remote).unwrap();

        // B has never heard of it; one sync and it has the whole thread.
        let r = sync_with(&b, &remote).unwrap();
        assert!(r.pulled > 0, "B should adopt the thread");
        let seen = forum::read_thread(&b, &h.id).unwrap();
        assert_eq!(seen.posts.len(), 1);
        assert_eq!(seen.posts[0].body, "from A");

        // B replies and publishes; A picks it up.
        forum::append_post(&b, &agent("bob"), &h.id, post("ACK", "from B")).unwrap();
        sync_with(&b, &remote).unwrap();
        sync_with(&a, &remote).unwrap();
        let back = forum::read_thread(&a, &h.id).unwrap();
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
            namespace: NS_CUSTOM.to_string(),
        };

        let a_dir = root.path().join("a");
        std::fs::create_dir_all(&a_dir).unwrap();
        let a = work_repo(&a_dir);
        let b_dir = root.path().join("b");
        std::fs::create_dir_all(&b_dir).unwrap();
        let b = work_repo(&b_dir);

        let h = forum::create_thread(&a, &human(), "race", None, None, None).unwrap();
        sync_with(&a, &remote).unwrap();
        sync_with(&b, &remote).unwrap();

        // Diverge: each side appends without seeing the other.
        forum::append_post(&a, &agent("alice"), &h.id, post("FINDING", "A only")).unwrap();
        forum::append_post(&b, &agent("bob"), &h.id, post("FINDING", "B only")).unwrap();

        sync_with(&a, &remote).unwrap();
        // B's push is a non-fast-forward, so its sync must fetch, merge and
        // retry rather than fail or overwrite.
        sync_with(&b, &remote).unwrap();
        sync_with(&a, &remote).unwrap();

        for (name, repo) in [("A", &a), ("B", &b)] {
            let t = forum::read_thread(repo, &h.id).unwrap();
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
            namespace: NS_CUSTOM.to_string(),
        };

        let a_dir = root.path().join("a");
        std::fs::create_dir_all(&a_dir).unwrap();
        let a = work_repo(&a_dir);
        let b_dir = root.path().join("b");
        std::fs::create_dir_all(&b_dir).unwrap();
        let b = work_repo(&b_dir);

        forum::put_roster_entry(
            &a,
            &human(),
            forum::RosterEntry {
                agent: "mallory".into(),
                box_id: None,
                role: Role::Worker,
                policy_digest: None,
                attached_at: forum::now_ts(),
                revoked_at: None,
                origin: None,
            },
        )
        .unwrap();
        sync_with(&a, &remote).unwrap();
        sync_with(&b, &remote).unwrap();

        // A revokes. B, which still thinks mallory is active, adds someone else
        // and syncs — the merge must not resurrect mallory.
        forum::revoke(&a, &human(), "mallory").unwrap();
        forum::put_roster_entry(
            &b,
            &human(),
            forum::RosterEntry {
                agent: "carol".into(),
                box_id: None,
                role: Role::Reviewer,
                policy_digest: None,
                attached_at: forum::now_ts(),
                revoked_at: None,
                origin: None,
            },
        )
        .unwrap();
        sync_with(&a, &remote).unwrap();
        sync_with(&b, &remote).unwrap();
        sync_with(&a, &remote).unwrap();

        for (name, repo) in [("A", &a), ("B", &b)] {
            let r = forum::read_roster(repo);
            assert!(
                !r.get("mallory").unwrap().is_active(),
                "{name} resurrected a revoked participant"
            );
            assert!(r.get("carol").is_some(), "{name} lost the other addition");
        }
    }

    /// An identity claimed on one clone is refused on another once the roster
    /// has travelled: the same guard that stops a local takeover stops the
    /// cross-machine name collision the sender-keyed design used to fold
    /// silently into one participant.
    #[test]
    fn a_name_taken_on_one_clone_cannot_be_attached_on_another() {
        let root = tempfile::tempdir().unwrap();
        let hub = root.path().join("hub.git");
        std::fs::create_dir_all(&hub).unwrap();
        git(&hub, &["init", "--bare", "--quiet"]).unwrap();
        let remote = Remote {
            url: hub.display().to_string(),
            is_default: false,
            namespace: NS_CUSTOM.to_string(),
        };
        let a_dir = root.path().join("a");
        std::fs::create_dir_all(&a_dir).unwrap();
        let a = work_repo(&a_dir);
        let b_dir = root.path().join("b");
        std::fs::create_dir_all(&b_dir).unwrap();
        let b = work_repo(&b_dir);

        let entry = |box_id: &str| forum::RosterEntry {
            agent: "claude-worker".into(),
            box_id: Some(box_id.into()),
            role: Role::Worker,
            policy_digest: None,
            attached_at: forum::now_ts(),
            revoked_at: None,
            origin: None,
        };
        forum::put_roster_entry(&a, &human(), entry("env/a/one")).unwrap();
        sync_with(&a, &remote).unwrap();
        sync_with(&b, &remote).unwrap();

        let err = forum::put_roster_entry(&b, &human(), entry("env/b/two")).unwrap_err();
        assert!(err.to_string().contains("already the identity"), "{err}");
        assert_eq!(
            forum::read_roster(&b).get("claude-worker").unwrap().box_id.as_deref(),
            Some("env/a/one"),
            "the travelled entry must keep its box"
        );
    }

    /// After the forum converges, syncing moves nothing and mints nothing.
    ///
    /// The regression this pins down: a clone that was merely *behind* used to
    /// union-merge instead of fast-forwarding, minting a contentless merge
    /// commit — which the other clone then merged, minting another. One post
    /// followed by six idle round-trips left the thread ref twelve commits
    /// deep, growing forever at one commit per tender pass.
    #[test]
    fn an_idle_forum_goes_quiet_instead_of_trading_empty_merges() {
        let root = tempfile::tempdir().unwrap();
        let hub = root.path().join("hub.git");
        std::fs::create_dir_all(&hub).unwrap();
        git(&hub, &["init", "--bare", "--quiet"]).unwrap();
        let remote = Remote {
            url: hub.display().to_string(),
            is_default: false,
            namespace: NS_CUSTOM.to_string(),
        };
        let a_dir = root.path().join("a");
        std::fs::create_dir_all(&a_dir).unwrap();
        let a = work_repo(&a_dir);
        let b_dir = root.path().join("b");
        std::fs::create_dir_all(&b_dir).unwrap();
        let b = work_repo(&b_dir);

        let h = forum::create_thread(&a, &human(), "quiet", None, None, None).unwrap();
        sync_with(&a, &remote).unwrap();
        sync_with(&b, &remote).unwrap();
        // One side posts; the other is now strictly behind.
        forum::append_post(&a, &agent("alice"), &h.id, post("FINDING", "news")).unwrap();
        sync_with(&a, &remote).unwrap();
        sync_with(&b, &remote).unwrap();
        sync_with(&a, &remote).unwrap();

        // Converged. From here, nothing may move.
        let refname = forum::thread_ref(&h.id);
        let tip_a = a.refname_to_id(&refname).unwrap();
        let tip_b = b.refname_to_id(&refname).unwrap();
        assert_eq!(tip_a, tip_b, "both clones should hold the same tip");
        for _ in 0..3 {
            assert!(sync_with(&a, &remote).unwrap().is_empty(), "idle sync must move nothing");
            assert!(sync_with(&b, &remote).unwrap().is_empty(), "idle sync must move nothing");
        }
        assert_eq!(a.refname_to_id(&refname).unwrap(), tip_a);
        assert_eq!(b.refname_to_id(&refname).unwrap(), tip_b);

        // And the whole exchange cost what it should: the create, the post,
        // and at most one real merge — not a commit per pass.
        let mut walk = a.revwalk().unwrap();
        walk.push(tip_a).unwrap();
        let depth = walk.count();
        assert!(depth <= 4, "an idle forum must not accrete commits, got {depth}");
    }

    /// Enrollments and the vote policy live on the meta ref, so they travel
    /// and merge exactly the way the roster does — including through a merge
    /// where both sides had touched their meta ref.
    #[test]
    fn enrollments_and_policy_travel_with_the_meta_ref() {
        use crate::forum::VoteRule;
        use crate::forum_identity;

        let root = tempfile::tempdir().unwrap();
        let hub = root.path().join("hub.git");
        std::fs::create_dir_all(&hub).unwrap();
        git(&hub, &["init", "--bare", "--quiet"]).unwrap();
        let remote = Remote {
            url: hub.display().to_string(),
            is_default: false,
            namespace: NS_CUSTOM.to_string(),
        };
        let a_dir = root.path().join("a");
        std::fs::create_dir_all(&a_dir).unwrap();
        let a = work_repo(&a_dir);
        let b_dir = root.path().join("b");
        std::fs::create_dir_all(&b_dir).unwrap();
        let b = work_repo(&b_dir);

        // B enrolls its machine and tightens the vote rule.
        forum_identity::put_enrollment(
            &b,
            &human(),
            forum_identity::Enrollment {
                version: forum_identity::ENROLLMENT_VERSION,
                principal: "github.com/user/12345678".into(),
                display_name: "hideakih".into(),
                origin: "machine-b".into(),
                ssh_pubkey: "ssh-ed25519 AAAATEST".into(),
                enrolled_at: forum::now_ts(),
                signature: "-----BEGIN SSH SIGNATURE-----\nx\n-----END SSH SIGNATURE-----\n"
                    .into(),
            },
        )
        .unwrap();
        forum_identity::set_vote_rule(&b, &human(), VoteRule::Principal).unwrap();

        // A, which has not heard any of that, changes its own meta ref too, so
        // the reconciliation is a real merge rather than an adoption.
        forum::put_roster_entry(
            &a,
            &human(),
            forum::RosterEntry {
                agent: "carol".into(),
                box_id: None,
                role: Role::Worker,
                policy_digest: None,
                attached_at: forum::now_ts(),
                revoked_at: None,
                origin: None,
            },
        )
        .unwrap();

        sync_with(&b, &remote).unwrap();
        sync_with(&a, &remote).unwrap();
        sync_with(&b, &remote).unwrap();

        for (name, repo) in [("A", &a), ("B", &b)] {
            let e = forum_identity::read_enrollments(repo);
            assert_eq!(
                e.by_origin.get("machine-b").map(|e| e.principal.as_str()),
                Some("github.com/user/12345678"),
                "{name} lost the enrollment"
            );
            assert_eq!(
                forum_identity::read_policy(repo).vote,
                VoteRule::Principal,
                "{name} lost the policy"
            );
            assert!(
                forum::read_roster(repo).get("carol").is_some(),
                "{name} lost the roster entry the merge crossed"
            );
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
            namespace: NS_CUSTOM.to_string(),
        };

        let a_dir = root.path().join("a");
        std::fs::create_dir_all(&a_dir).unwrap();
        let a = work_repo(&a_dir);
        let b_dir = root.path().join("b");
        std::fs::create_dir_all(&b_dir).unwrap();
        let b = work_repo(&b_dir);

        let h = forum::create_thread(&a, &human(), "vouch", None, None, None).unwrap();
        forum::append_post(
            &a,
            &agent("alice").from_host("machine-a"),
            &h.id,
            post("FINDING", "from A"),
        )
        .unwrap();
        sync_with(&a, &remote).unwrap();
        sync_with(&b, &remote).unwrap();
        forum::append_post(
            &b,
            &agent("bob").from_host("machine-b"),
            &h.id,
            post("ACK", "from B"),
        )
        .unwrap();
        sync_with(&b, &remote).unwrap();
        sync_with(&a, &remote).unwrap();

        let seen = forum::read_thread(&a, &h.id).unwrap();
        let by_body = |body: &str| {
            seen.posts
                .iter()
                .find(|p| p.body == body)
                .unwrap()
                .vouch("machine-a")
        };
        assert_eq!(by_body("from A"), forum::Vouch::Observed);
        assert_eq!(
            by_body("from B"),
            forum::Vouch::PeerClaimed {
                origin: "machine-b".into()
            }
        );

        // And the other way round on the other machine, from the same bytes.
        let seen_b = forum::read_thread(&b, &h.id).unwrap();
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
        let h = forum::create_thread(&repo, &human(), "t", None, None, None).unwrap();
        // `agent()` builds an author with no origin, the way a build from before
        // origins existed would.
        forum::append_post(&repo, &agent("old"), &h.id, post("FINDING", "x")).unwrap();
        let t = forum::read_thread(&repo, &h.id).unwrap();
        assert_eq!(t.posts[0].vouch("machine-a"), forum::Vouch::Unattributed);
        assert!(!t.posts[0].vouch("").is_observed(), "an empty identity matches nothing");
    }

    /// A human's close reaches the other machine.
    ///
    /// The version that moved a ref did not: the peer still held the live ref,
    /// pushed it back on its next sync, and the decision was silently undone.
    /// Closing is an append now, so it travels the way every other post does.
    #[test]
    fn a_close_survives_a_peer_that_had_not_heard_about_it() {
        let root = tempfile::tempdir().unwrap();
        let hub = root.path().join("hub.git");
        std::fs::create_dir_all(&hub).unwrap();
        git(&hub, &["init", "--bare", "--quiet"]).unwrap();
        let remote = Remote {
            url: hub.display().to_string(),
            is_default: false,
            namespace: NS_CUSTOM.to_string(),
        };
        let a_dir = root.path().join("a");
        std::fs::create_dir_all(&a_dir).unwrap();
        let a = work_repo(&a_dir);
        let b_dir = root.path().join("b");
        std::fs::create_dir_all(&b_dir).unwrap();
        let b = work_repo(&b_dir);

        let h = forum::create_thread(&a, &human(), "closing", None, None, None).unwrap();
        sync_with(&a, &remote).unwrap();
        sync_with(&b, &remote).unwrap();

        forum::close_thread(&a, &human(), &h.id).unwrap();
        sync_with(&a, &remote).unwrap();
        // B pushes its own (still open) view first, exactly as it would have
        // done before hearing about the close.
        sync_with(&b, &remote).unwrap();
        sync_with(&a, &remote).unwrap();

        for (name, repo) in [("A", &a), ("B", &b)] {
            assert!(
                forum::read_thread(repo, &h.id).unwrap().is_closed(),
                "{name} did not honour the close"
            );
        }
    }

    /// Deleting a thread ref from the remote destroys nothing.
    ///
    /// Anyone with push access can run `git push --delete`, and nothing stops
    /// them trying. Nothing on a forum depends on a ref being absent, though,
    /// so the next sync from any clone that still holds the thread puts it
    /// back: an attacker buys a window, never a loss.
    #[test]
    fn a_deleted_thread_ref_is_restored_by_any_clone_that_still_has_it() {
        let root = tempfile::tempdir().unwrap();
        let hub = root.path().join("hub.git");
        std::fs::create_dir_all(&hub).unwrap();
        git(&hub, &["init", "--bare", "--quiet"]).unwrap();
        let remote = Remote {
            url: hub.display().to_string(),
            is_default: false,
            namespace: NS_CUSTOM.to_string(),
        };
        let a_dir = root.path().join("a");
        std::fs::create_dir_all(&a_dir).unwrap();
        let a = work_repo(&a_dir);

        let h = forum::create_thread(&a, &human(), "durable", None, None, None).unwrap();
        forum::append_post(&a, &agent("alice"), &h.id, post("FINDING", "keep me")).unwrap();
        sync_with(&a, &remote).unwrap();

        // The attack, in one plain-git command.
        let dead = format!(":{}", forum::thread_ref(&h.id));
        git(&a_dir, &["push", "--end-of-options", &remote.url, &dead]).unwrap();
        let gone = git(
            &hub,
            &["for-each-ref", "--format=%(refname)", "refs/h5i/forum/threads/"],
        )
        .unwrap();
        assert!(gone.trim().is_empty(), "the delete really landed");

        sync_with(&a, &remote).unwrap();
        let back = git(
            &hub,
            &["for-each-ref", "--format=%(refname)", "refs/h5i/forum/threads/"],
        )
        .unwrap();
        assert!(back.contains(&h.id), "one sync should restore it:\n{back}");
    }

    /// A forum with no remote configured gets a local one, and it works — that
    /// is what makes solo use run the same code as a team.
    #[test]
    fn a_forum_with_no_configured_remote_still_syncs() {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("w");
        std::fs::create_dir_all(&work).unwrap();
        let repo = work_repo(&work);
        let h5i_root = root.path().join("h5i");
        std::fs::create_dir_all(&h5i_root).unwrap();

        let r = remote(&h5i_root).unwrap();
        assert!(r.is_default, "an unconfigured forum falls back to a local bare repo");
        assert!(default_remote_path(&h5i_root).join("HEAD").is_file());

        let h = forum::create_thread(&repo, &human(), "solo", None, None, None).unwrap();
        forum::append_post(&repo, &agent("solo"), &h.id, post("FINDING", "alone")).unwrap();
        sync(&repo, &h5i_root).unwrap();

        // It really landed on the remote, rather than the push being skipped.
        let listed = git(
            &default_remote_path(&h5i_root),
            &["for-each-ref", "--format=%(refname)", "refs/h5i/forum/"],
        )
        .unwrap();
        assert!(listed.contains(&h.id), "the thread should be on the remote:\n{listed}");
    }

    /// Publishing under branches is the same forum, and it is what lets a forge
    /// protect it: rulesets only reach `refs/heads/**`, so a custom namespace
    /// gets no server-side refusal of a delete or a force-push.
    #[test]
    fn a_forum_can_publish_under_branches_a_forge_can_protect() {
        let root = tempfile::tempdir().unwrap();
        let hub = root.path().join("hub.git");
        std::fs::create_dir_all(&hub).unwrap();
        git(&hub, &["init", "--bare", "--quiet"]).unwrap();
        let remote = Remote {
            url: hub.display().to_string(),
            is_default: false,
            namespace: NS_BRANCH.to_string(),
        };
        assert!(remote.is_branch_namespace());

        let a_dir = root.path().join("a");
        std::fs::create_dir_all(&a_dir).unwrap();
        let a = work_repo(&a_dir);
        let h = forum::create_thread(&a, &human(), "protected", None, None, None).unwrap();
        forum::append_post(&a, &agent("alice"), &h.id, post("FINDING", "hello")).unwrap();
        sync_with(&a, &remote).unwrap();

        // It really is a branch, which is the whole point.
        let heads = git(&hub, &["for-each-ref", "--format=%(refname)", "refs/heads/"]).unwrap();
        assert!(
            heads.contains(&format!("refs/heads/h5i-forum/threads/{}", h.id)),
            "the thread should be a branch:\n{heads}"
        );

        // And a second clone reads it back through the same namespace.
        let b_dir = root.path().join("b");
        std::fs::create_dir_all(&b_dir).unwrap();
        let b = work_repo(&b_dir);
        sync_with(&b, &remote).unwrap();
        let seen = forum::read_thread(&b, &h.id).unwrap();
        assert_eq!(seen.posts[0].body, "hello");
        // The local mirror keeps its own namespace either way, so nothing else
        // in the forum has to know which one is in use.
        assert!(b.refname_to_id(&forum::thread_ref(&h.id)).is_ok());
    }

    #[test]
    fn a_configured_remote_round_trips_and_a_dash_leading_url_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let h5i_root = root.path().to_path_buf();
        assert!(set_remote(&h5i_root, "  git@example.com:team/forum.git  ").is_ok());
        assert_eq!(remote(&h5i_root).unwrap().url, "git@example.com:team/forum.git");
        assert!(!remote(&h5i_root).unwrap().is_default);
        assert_eq!(remote(&h5i_root).unwrap().namespace, NS_CUSTOM);
        set_namespace(&h5i_root, true).unwrap();
        assert!(remote(&h5i_root).unwrap().is_branch_namespace());
        set_namespace(&h5i_root, false).unwrap();
        assert_eq!(remote(&h5i_root).unwrap().namespace, NS_CUSTOM);

        assert!(set_remote(&h5i_root, "--upload-pack=evil").is_err());
        assert!(set_remote(&h5i_root, "").is_err());

        clear_remote(&h5i_root);
        assert!(remote(&h5i_root).unwrap().is_default);
    }
}
