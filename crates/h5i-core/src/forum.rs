//! The forum: mediated collaboration between boxed agents.
//!
//! Boxed agents cannot reach each other. They post to a forum the host owns,
//! and the host decides what each box sees:
//!
//! > Agents can share information, never permissions.
//!
//! A post can change what a peer decides, never what its sandbox can do. The
//! forum carries text and content-addressed artifacts; apply, export and push
//! stay host commands a box cannot invoke.
//!
//! ## The two authority rules
//!
//! **The box writes what, never who.** `body`, `kind` and attachments are the
//! agent's claim. `sender`, `box_id`, `role` and `policy_digest` are stamped
//! host-side from the box's env binding, never read from the payload, so a box
//! asserting `"sender": "human"` changes nothing. [`Author`] is that stamp.
//!
//! **Only humans change trust boundaries.** Creating a thread, attaching a box,
//! revoking one and closing a thread are [`Role::Human`] operations. A box also
//! cannot reach these calls: the store lives outside every write grant it has.
//!
//! ## Layout: one ref per thread
//!
//! ```text
//! refs/h5i/forum/meta            roster.json, who is on the forum
//! refs/h5i/forum/threads/<id>    thread.json + posts.jsonl + attach-<digest>
//! ```
//!
//! Appending rewrites the blob it appends to, so one shared log would rewrite
//! the whole forum's history per post. Per-thread refs bound that to one
//! thread, localise compare-and-swap contention, and make the thread list a ref
//! enumeration ordered by tip timestamp. Git refs because [`crate::refstore`]
//! already does concurrent append and a ref travels with the repo, union-merge
//! included; `refs/h5i/*` is outside the default refspec, so a forum never
//! rides an ordinary `git push` to a forge.
//!
//! Nothing is ever deleted. Closing a thread is a `CLOSED` post ([`KIND_CLOSED`]
//! records why the ref-moving version died on a second machine), and
//! `posts.jsonl` stays strictly append-only so two clones reconcile by id.
//! Status is a projection over posts ([`Thread::status`]), never a mutated
//! field; `thread.json` holds only what creation fixed.
//!
//! ## Untrusted text
//!
//! Bodies keep their terminal control sequences and are neutralised at render,
//! as [`crate::redact`] prescribes, so print [`Post::display_body`] rather than
//! the field.
//!
//! Credentials go the other way, and the asymmetry is the point: an escape
//! sequence is dangerous when it reaches a terminal, but a credential is
//! dangerous once *stored*, since a git object is immutable, in every clone and
//! published if the forum has a remote. [`append_post`] scrubs body and
//! attachments before writing, recording which rules fired in
//! [`Post::redactions`].

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use git2::{Oid, Repository};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::H5iError;
use crate::refstore;

/// Ref holding the forum roster. A sibling of the `threads/` namespace, not a
/// parent of it: roster entries outlive any one thread.
pub const FORUM_META_REF: &str = "refs/h5i/forum/meta";
/// Prefix under which each thread gets its own ref.
pub const FORUM_THREADS_PREFIX: &str = "refs/h5i/forum/threads/";

const POSTS_FILE: &str = "posts.jsonl";
const THREAD_FILE: &str = "thread.json";
const ROSTER_FILE: &str = "roster.json";
/// Enrolled principal bindings, a sibling of the roster on the meta ref.
pub(crate) const ENROLLMENTS_FILE: &str = "enrollments.json";
/// Forum-wide policy, a sibling of the roster on the meta ref.
pub(crate) const POLICY_FILE: &str = "policy.json";
const ATTACH_PREFIX: &str = "attach-";

/// Wire-format version written by this build.
pub const PROTOCOL_VERSION: u32 = 1;

/// Attempts before an append gives up. Each retry is an object write and a ref
/// compare-and-swap; a writer would have to lose this many consecutive races to
/// fail, which bursty posting from a handful of boxes will not do.
const MAX_ATTEMPTS: usize = 64;

/// Largest single post body accepted, in bytes. A forum is for coordination;
/// anything larger belongs in an attachment, where it is content-addressed and
/// deduplicated.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Largest single attachment payload, in bytes.
pub const MAX_ATTACHMENT_BYTES: usize = 1024 * 1024;
/// Most attachments one post may carry.
pub const MAX_ATTACHMENTS: usize = 8;

// ── kinds ──────────────────────────────────────────────────────────────────

/// The post kinds this build writes.
///
/// Kept as strings rather than a closed enum so that a thread written by a
/// newer build still parses here: an unknown kind renders as itself instead of
/// failing the line. Writers are validated against this list; readers are not.
pub const KINDS: &[&str] = &[
    "TASK",
    "ASK",
    "FINDING",
    "RISK",
    "PROPOSAL",
    "CLAIM",
    "REVIEW_REQUEST",
    "HANDOFF",
    "DONE",
    "ACK",
    "BLOCKED",
    "CLOSED",
    "UPVOTE",
    "DOWNVOTE",
];

/// Kind of the post that opens a thread.
pub const KIND_TASK: &str = "TASK";
/// Kind that takes ownership of a thread.
pub const KIND_CLAIM: &str = "CLAIM";
/// Kind that asks for review of submitted work.
pub const KIND_REVIEW_REQUEST: &str = "REVIEW_REQUEST";
/// Kind that reports the thread's work finished.
pub const KIND_DONE: &str = "DONE";
/// Kind that reports the thread's work stuck.
pub const KIND_BLOCKED: &str = "BLOCKED";
/// Kind that agrees with another post.
///
/// A vote is a post, which is what makes it safe here: it is append-only,
/// host-stamped like everything else, and it merges across clones by the same
/// union that merges the conversation. Nothing new had to be trusted to add it.
///
/// Deliberately *not* karma. There is no reputation, no ranking of
/// participants, and no score that follows an agent between threads — a forum
/// where agents accumulate standing is a forum where an agent has a reason to
/// perform. What a vote says is narrower and more useful: **this is the post I
/// would act on.** A human scanning a long thread wants that, and so does an
/// agent deciding which of three proposals its peers converged on.
pub const KIND_UPVOTE: &str = "UPVOTE";
/// Kind that disagrees with another post.
pub const KIND_DOWNVOTE: &str = "DOWNVOTE";
/// Kind that closes a thread. Human only.
///
/// Closing is a post rather than a deleted ref, and that is not a detail. Every
/// other status here is a projection over an append-only log, and closing was
/// the one that was a mutation — so it was the one that did not survive
/// contact with a second machine. A peer that had not heard about the close
/// still held the live ref, pushed it back, and the human's decision was
/// silently undone. Measured, not theorised.
///
/// Making it an append fixes that and closes a second hole at the same time.
/// Nothing about a forum now depends on a ref being *absent*, so `git push
/// --delete` against the remote destroys nothing: the next sync from any clone
/// that still holds the thread puts it back. Deletion buys an attacker a
/// window, never a loss.
pub const KIND_CLOSED: &str = "CLOSED";

fn validate_kind(kind: &str) -> Result<(), H5iError> {
    if KINDS.contains(&kind) {
        return Ok(());
    }
    Err(H5iError::Metadata(format!(
        "unknown post kind {:?}: use one of {}",
        crate::redact::sanitize_display(kind),
        KINDS.join(", ")
    )))
}

// ── roles ──────────────────────────────────────────────────────────────────

/// What a participant may do on the forum.
///
/// A role is assigned by a human at attach time and recorded in the roster; it
/// is not something an agent can claim for itself. The gradient is deliberately
/// coarse — four roles, checked with three predicates — because a policy
/// language nobody can hold in their head is a policy nobody can audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Does the work: posts, claims threads, attaches patches.
    Worker,
    /// Reviews the work: posts and attaches, but never claims a thread.
    Reviewer,
    /// Reads only. An observer's presence changes no other participant's
    /// authority — ceilings are fixed per thread, not intersected per room.
    Observer,
    /// The person. The only role that may change the forum's membership or a
    /// thread's existence.
    Human,
}

impl Role {
    /// May this role add posts to a thread?
    pub fn can_post(self) -> bool {
        !matches!(self, Role::Observer)
    }

    /// May this role claim a thread, becoming its owner?
    pub fn can_claim(self) -> bool {
        matches!(self, Role::Worker | Role::Human)
    }

    /// May this role attach an artifact to a post?
    pub fn can_attach(self) -> bool {
        matches!(self, Role::Worker | Role::Reviewer | Role::Human)
    }

    /// May this role change who is on the forum, or whether a thread exists?
    ///
    /// Human only, always. This is the fifth invariant in one predicate.
    pub fn can_govern(self) -> bool {
        matches!(self, Role::Human)
    }

    /// The role's name as written in the roster and on a post.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Worker => "worker",
            Role::Reviewer => "reviewer",
            Role::Observer => "observer",
            Role::Human => "human",
        }
    }

    /// Parse a role name, rejecting anything unrecognised rather than
    /// defaulting: a typo that silently became `observer` would look like a
    /// broken agent, and one that silently became `worker` would be a hole.
    pub fn parse(s: &str) -> Result<Role, H5iError> {
        match s {
            "worker" => Ok(Role::Worker),
            "reviewer" => Ok(Role::Reviewer),
            "observer" => Ok(Role::Observer),
            "human" => Ok(Role::Human),
            other => Err(H5iError::Metadata(format!(
                "unknown role {:?}: use worker, reviewer, observer or human",
                crate::redact::sanitize_display(other)
            ))),
        }
    }
}

// ── the host's stamp ───────────────────────────────────────────────────────

/// Who the host says is posting.
///
/// Construction of this type is the moment authority is decided. Host-side code
/// builds it from what it already knows — the human at the terminal, or the env
/// directory a spooled record was found in — and never from the record's
/// contents. Every field here lands on the post; none of them is readable from
/// the box's own JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    /// Forum identity, e.g. `claude-worker`.
    pub sender: String,
    /// The box this identity is bound to, e.g. `env/claude/auth-race`. `None`
    /// for a human at the host terminal, who is in no box.
    pub box_id: Option<String>,
    /// What this participant may do.
    pub role: Role,
    /// Digest of the resolved policy the box was confined by, so a reader can
    /// tell whether two posts came from the same confinement.
    pub policy_digest: Option<String>,
    /// Which host stamped this. `None` writes an unattributed post, which only
    /// a build from before origins existed should produce.
    pub origin: Option<String>,
}

impl Author {
    /// Record which host is doing the stamping.
    ///
    /// Chained rather than a constructor argument so every existing call site
    /// keeps compiling and keeps meaning what it meant: an `Author` with no
    /// origin is one nobody claimed, which is exactly how [`Vouch`] reads it.
    pub fn from_host(mut self, origin: &str) -> Author {
        self.origin = Some(origin.to_string());
        self
    }
}

impl Author {
    /// The human at the host terminal.
    pub fn human(name: &str) -> Result<Author, H5iError> {
        validate_name(name)?;
        Ok(Author {
            sender: name.to_string(),
            box_id: None,
            role: Role::Human,
            policy_digest: None,
            origin: None,
        })
    }

    /// An agent in a box, as identified by the host from its env binding.
    pub fn agent(
        sender: &str,
        box_id: &str,
        role: Role,
        policy_digest: Option<String>,
    ) -> Result<Author, H5iError> {
        validate_name(sender)?;
        Ok(Author {
            sender: sender.to_string(),
            box_id: Some(box_id.to_string()),
            role,
            policy_digest,
            origin: None,
        })
    }

    fn require(&self, allowed: bool, what: &str) -> Result<(), H5iError> {
        if allowed {
            return Ok(());
        }
        Err(H5iError::Metadata(format!(
            "{} may not {what} (role: {})",
            crate::redact::sanitize_display(&self.sender),
            self.role.as_str()
        )))
    }

    /// Refuse unless this author may govern. The check the sibling modules —
    /// enrollment, policy — use for their own human-only writes.
    pub fn require_govern(&self, what: &str) -> Result<(), H5iError> {
        self.require(self.role.can_govern(), what)
    }
}

// ── posts ──────────────────────────────────────────────────────────────────

/// An artifact carried by a post.
///
/// The payload is a git blob in the thread's tree, addressed by the SHA-256 of
/// its bytes, so the same patch posted twice is stored once. The `kind` is an
/// allowlist rather than a MIME type: a forum that accepts arbitrary uploads is
/// a file-transfer channel between boxes, which is the thing this design exists
/// to not be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// `patch`, `test-report`, or `text`.
    pub kind: String,
    /// Lowercase hex SHA-256 of the payload; also its name in the tree.
    pub digest: String,
    /// Payload length in bytes.
    pub size: usize,
    /// Display name, e.g. `fix-refresh-race.diff`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Attachment kinds a post may carry.
pub const ATTACHMENT_KINDS: &[&str] = &["patch", "test-report", "text"];

fn validate_attachment_kind(kind: &str) -> Result<(), H5iError> {
    if ATTACHMENT_KINDS.contains(&kind) {
        return Ok(());
    }
    Err(H5iError::Metadata(format!(
        "unsupported attachment kind {:?}: use one of {}",
        crate::redact::sanitize_display(kind),
        ATTACHMENT_KINDS.join(", ")
    )))
}

/// One post. Lines in `posts.jsonl` are exactly this struct's JSON.
///
/// The total order is `(ts, id)`: `ts` is fixed-width RFC3339 UTC so it sorts
/// lexicographically, and `id` breaks ties deterministically. Fields added
/// after v1 must be `#[serde(default)]` and skipped when empty, so a thread
/// written by a newer build still reads here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Post {
    /// Stable id, 16 lowercase hex. Dedup key on merge, tie-break in the order.
    pub id: String,
    /// RFC3339 UTC, `YYYY-MM-DDThh:mm:ss.ffffffZ`.
    pub ts: String,
    /// Thread this post belongs to.
    ///
    /// Redundant with the ref the post is stored on, and carried anyway: a post
    /// delivered into a box's inbox arrives as a loose file with no ref around
    /// it, and a reader there still has to know which conversation it is part
    /// of.
    #[serde(default)]
    pub thread: String,
    /// Wire-format version.
    #[serde(default)]
    pub version: u32,
    /// One of [`KINDS`], or an unknown kind from a newer build.
    pub kind: String,
    /// Free text. Stored verbatim — render via [`Post::display_body`].
    pub body: String,
    /// Id of the post this replies to, when it is a reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// Artifacts carried by this post.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,

    // ── host-stamped below: never read from a box's payload ────────────────
    /// Forum identity the host attributes this post to.
    pub sender: String,
    /// Box the sender was bound to, absent for a human.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub box_id: Option<String>,
    /// The sender's role at the time of posting.
    pub role: String,
    /// Digest of the confinement the sending box ran under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_digest: Option<String>,
    /// Set when the host refused something on this post's behalf, e.g. an
    /// attachment over the cap or a revoked sender. A refusal is recorded, not
    /// silently dropped: a forum that quietly swallows what it rejects teaches
    /// its readers that nothing was rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denied: Option<String>,
    /// Which host stamped this post.
    ///
    /// **This field is attribution, not authentication.** Nothing signs it, so
    /// a hostile host can write any value here, including another host's. Its
    /// job is to let a reader see that two posts claim different origins and to
    /// let *this* host recognise its own work — which is the only part that is
    /// actually verifiable. See [`Post::vouch`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Secret-detector rules that fired while this post was being written.
    ///
    /// The body and every attachment are scrubbed unconditionally before they
    /// are stored; this names what was found, so a reviewer can see that an
    /// agent pasted a credential without the credential being on the forum.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redactions: Vec<String>,
}

/// How much this host actually knows about who wrote a post.
///
/// h5i already separates what a host *observed* from what a box *claimed*, and
/// gives a remote machine's report its own third tier rather than folding it
/// into either — `runner-observed` exists because "a signature from a machine
/// the threat model already sacrificed is not evidence".
///
/// A forum that crosses machines needs the same distinction, and needs it
/// badly, because without it the forum's central promise degrades in silence.
/// On one machine the line above a post is the host's knowledge: it stamped the
/// sender from the env directory it owns, and no agent could have written it.
/// Fetch that same post from a peer and the host observed *nothing* — it has
/// another machine's word for all of it — yet the line renders identically.
/// The sender quietly becomes a claim while still looking like a fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vouch {
    /// This host stamped it. The sender, box, role and policy digest are things
    /// it observed, not things it was told.
    Observed,
    /// It arrived over the remote. Everything about its author is the origin's
    /// account, including the origin. Verified: that it is in the log.
    PeerClaimed {
        /// The origin the post names. Unsigned, so a reader may not treat it as
        /// proof of anything — two posts naming different origins is the useful
        /// signal, not the string itself.
        origin: String,
    },
    /// It arrived over the remote naming no origin at all.
    Unattributed,
}

impl Vouch {
    /// Did this host see what it is reporting?
    pub fn is_observed(&self) -> bool {
        matches!(self, Vouch::Observed)
    }

    /// A short word for a dense surface. Never colour alone: a row that does not
    /// say which lane it is in is a row asserting more than h5i knows.
    pub fn as_str(&self) -> &'static str {
        match self {
            Vouch::Observed => "host-observed",
            Vouch::PeerClaimed { .. } => "peer-claimed",
            Vouch::Unattributed => "unattributed",
        }
    }
}

impl Post {
    /// What this host can say about who wrote this, given its own identity.
    ///
    /// The comparison is the whole mechanism, and its honesty comes from being
    /// one-sided: a host can be certain it *did* stamp something, and can be
    /// certain of nothing else. So `Observed` is a real guarantee and
    /// `PeerClaimed` is an explicit absence of one.
    pub fn vouch(&self, me: &str) -> Vouch {
        match self.origin.as_deref() {
            Some(o) if o == me => Vouch::Observed,
            Some(o) => Vouch::PeerClaimed {
                origin: o.to_string(),
            },
            None => Vouch::Unattributed,
        }
    }

    /// Total-order key.
    fn key(&self) -> (&str, &str) {
        (&self.ts, &self.id)
    }

    /// The body, made safe to print to a terminal, with its line breaks intact.
    ///
    /// A post is peer input: it was written by another agent, or arrived from
    /// another clone. Print this, not [`Post::body`].
    pub fn display_body(&self) -> String {
        crate::redact::sanitize_block(&self.body)
    }

    /// Was this post refused, in whole or in part, by the host?
    pub fn is_denied(&self) -> bool {
        self.denied.is_some()
    }
}

// ── counting votes ─────────────────────────────────────────────────────────

/// What one vote is one unit of.
///
/// The identity a forum can actually count is layered, and each layer buys a
/// different guarantee:
///
/// - a **sender** is a display name a worktree picked — free to mint, so it is
///   never the unit;
/// - an **origin** is the machine the host stamped — one per host, so a
///   hundred worktrees on one laptop are one opinion;
/// - a **principal** is the forge account an origin was enrolled under
///   ([`crate::forum_identity`]) — one per person, so two laptops belonging to
///   one account are still one opinion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VoteRule {
    /// One vote per host origin. The zero-setup default: nothing to enroll,
    /// and worktree multiplication buys nothing because every worktree on a
    /// machine posts through the same stamp. A pre-origin post falls back to
    /// its sender, which is the best that can be said about it.
    #[default]
    Origin,
    /// One vote per enrolled principal. Votes from an origin nobody enrolled
    /// count for nothing — not because they are hostile, but because a forum
    /// that promised "one account, one vote" and quietly counted anonymous
    /// machines would be lying about the promise.
    Principal,
}

impl VoteRule {
    /// The rule's name as stored in the policy and shown to a reader.
    pub fn as_str(self) -> &'static str {
        match self {
            VoteRule::Origin => "origin",
            VoteRule::Principal => "principal",
        }
    }

    /// Parse a rule name, refusing typos rather than defaulting: a policy that
    /// silently became `origin` would loosen a forum somebody meant to tighten.
    pub fn parse(s: &str) -> Result<VoteRule, H5iError> {
        match s {
            "origin" => Ok(VoteRule::Origin),
            "principal" => Ok(VoteRule::Principal),
            other => Err(H5iError::Metadata(format!(
                "unknown vote rule {:?}: use origin or principal",
                crate::redact::sanitize_display(other)
            ))),
        }
    }
}

/// Everything [`Thread::score_of_with`] needs to decide whose vote counts.
///
/// Built once per read from the forum's meta ref (see
/// [`crate::forum_identity::vote_context`]) rather than looked up per post, so
/// counting stays a pure function over the posts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VoteContext {
    /// The rule in force.
    pub rule: VoteRule,
    /// Enrolled bindings, `origin → principal`. Only consulted under
    /// [`VoteRule::Principal`].
    pub principals: BTreeMap<String, String>,
}

impl VoteContext {
    /// The dedup key this post's vote counts under, or `None` when it counts
    /// for nothing under the rule in force.
    ///
    /// Keys are prefixed by their layer so an origin can never collide with a
    /// sender that happens to spell the same.
    fn voter_key(&self, p: &Post) -> Option<String> {
        match self.rule {
            VoteRule::Origin => Some(match p.origin.as_deref() {
                Some(o) => format!("o:{o}"),
                None => format!("s:{}", p.sender),
            }),
            VoteRule::Principal => {
                let origin = p.origin.as_deref()?;
                let principal = self.principals.get(origin)?;
                Some(format!("p:{principal}"))
            }
        }
    }
}

// ── threads ────────────────────────────────────────────────────────────────

/// The authority ceiling a thread's work is bounded by.
///
/// Recorded at creation and checked when a box attaches: the box's resolved
/// profile must be a subset of the ceiling, or the attach is refused. Refused,
/// not downgraded — silently weakening a box's confinement to fit a room is how
/// a participant ends up less confined than its operator believes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ceiling {
    /// Profile name the ceiling is expressed as, e.g. `code-review`.
    pub profile: String,
    /// Digest of that profile as resolved when the thread was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

/// What is fixed about a thread at creation. Stored as `thread.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadHeader {
    /// Thread id, 16 lowercase hex; also its ref name.
    pub id: String,
    /// One-line title.
    pub title: String,
    /// RFC3339 UTC creation time.
    pub created_at: String,
    /// Who created it. A thread is always created by a human.
    pub created_by: String,
    /// Wire-format version.
    #[serde(default)]
    pub version: u32,
    /// Authority ceiling for work in this thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling: Option<Ceiling>,
    /// Git branch the thread's work lives on, when it has one. Metadata for
    /// display and filtering — the forum's layout does not depend on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

impl ThreadHeader {
    /// The title, made safe to print.
    pub fn display_title(&self) -> String {
        crate::redact::sanitize_display(&self.title)
    }
}

/// Where a thread has got to. Computed from its posts, never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadStatus {
    /// Nobody has claimed it.
    Open,
    /// Claimed and being worked.
    Claimed,
    /// Work submitted, review requested.
    Review,
    /// Reported finished.
    Done,
    /// Reported stuck.
    Blocked,
    /// Closed by a human. Out of the live list, out of every box's inbox, and
    /// still readable.
    Closed,
}

impl ThreadStatus {
    /// The status name as rendered.
    pub fn as_str(self) -> &'static str {
        match self {
            ThreadStatus::Open => "open",
            ThreadStatus::Claimed => "claimed",
            ThreadStatus::Review => "review",
            ThreadStatus::Done => "done",
            ThreadStatus::Blocked => "blocked",
            ThreadStatus::Closed => "closed",
        }
    }
}

/// A thread and its posts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// What was fixed at creation.
    pub header: ThreadHeader,
    /// Posts in `(ts, id)` order.
    pub posts: Vec<Post>,
}

impl Thread {
    /// The thread's status, projected from its posts.
    ///
    /// The projection walks in order and lets each signalling post move the
    /// state, so the last thing that happened wins: a `DONE` followed by a
    /// re-`CLAIM` is claimed again, which is what reopening looks like without
    /// a reopen verb.
    pub fn status(&self) -> ThreadStatus {
        let mut status = ThreadStatus::Open;
        let mut closed = false;
        for p in &self.posts {
            if p.is_denied() {
                continue; // a refused post moves nothing
            }
            if matches!(p.kind.as_str(), KIND_UPVOTE | KIND_DOWNVOTE) {
                continue; // agreeing with a post is not a move in the thread
            }
            if p.kind == KIND_CLOSED {
                closed = true;
                continue;
            }
            // Closing sticks until a human deliberately moves the thread on.
            // Last-writer-wins is right for the working statuses, where the
            // thread's owner is the authority; it is wrong here for two
            // reasons. An agent must not be able to undo a governance decision
            // just by claiming the thread. And "any later human post reopens
            // it" is too loose in a way that bites across machines: posts are
            // ordered by `(ts, id)`, so a post that merely *sorts* after the
            // close — one that arrived late from a peer, or one written while
            // the closing host's clock was ahead — would silently reopen a
            // thread nobody reopened. Only a human taking a status-moving
            // action counts.
            if closed {
                let reopening = p.role == Role::Human.as_str()
                    && matches!(
                        p.kind.as_str(),
                        KIND_CLAIM | KIND_REVIEW_REQUEST | KIND_DONE | KIND_BLOCKED
                    );
                if !reopening {
                    continue;
                }
                closed = false;
            }
            status = match p.kind.as_str() {
                KIND_CLAIM => ThreadStatus::Claimed,
                KIND_REVIEW_REQUEST => ThreadStatus::Review,
                KIND_DONE => ThreadStatus::Done,
                KIND_BLOCKED => ThreadStatus::Blocked,
                _ => status,
            };
        }
        if closed { ThreadStatus::Closed } else { status }
    }

    /// Has a human closed this thread?
    pub fn is_closed(&self) -> bool {
        self.status() == ThreadStatus::Closed
    }

    /// Net score of one post: agreements minus disagreements.
    ///
    /// One vote per participant per post, last one winning, so changing your
    /// mind is a second vote rather than an edit — which keeps the log
    /// append-only and keeps the change itself visible. A refused vote counts
    /// for nothing, exactly like a refused claim.
    ///
    /// "Participant" is decided by [`VoteContext`], and the default is the
    /// **host origin**, not the sender name. A sender name is free to mint: one
    /// person can open a hundred worktrees, attach each under a fresh name, and
    /// every one of them posts through the same host — so the origin the host
    /// stamps is the honest unit of opinion, and a hundred agents on one
    /// machine are one vote. The sender is only the key of last resort, for
    /// posts written before origins existed.
    pub fn score_of(&self, post_id: &str) -> i32 {
        self.score_of_with(post_id, &VoteContext::default())
    }

    /// [`Thread::score_of`] under an explicit counting rule.
    pub fn score_of_with(&self, post_id: &str, ctx: &VoteContext) -> i32 {
        let mut by_voter: BTreeMap<String, i32> = BTreeMap::new();
        for p in &self.posts {
            if p.is_denied() || p.reply_to.as_deref() != Some(post_id) {
                continue;
            }
            let weight = match p.kind.as_str() {
                KIND_UPVOTE => 1,
                KIND_DOWNVOTE => -1,
                _ => continue,
            };
            let Some(voter) = ctx.voter_key(p) else {
                continue;
            };
            by_voter.insert(voter, weight);
        }
        by_voter.values().sum()
    }

    /// Posts that are not votes — the conversation, without its scoring.
    pub fn conversation(&self) -> impl Iterator<Item = &Post> {
        self.posts
            .iter()
            .filter(|p| !matches!(p.kind.as_str(), KIND_UPVOTE | KIND_DOWNVOTE))
    }

    /// The post that states the thread, when it has one.
    ///
    /// The first `TASK` post, which `create_thread` writes from the body it was
    /// given. A thread opened with a title alone has none, and a reader sees
    /// the title and the replies — which is fine, and is what every thread
    /// looked like before bodies existed.
    pub fn opening(&self) -> Option<&Post> {
        self.posts
            .iter()
            .find(|p| p.kind == KIND_TASK && !p.is_denied())
    }

    /// Everything after the opening post: what Reddit would call the comments.
    pub fn replies(&self) -> impl Iterator<Item = &Post> {
        let opening = self.opening().map(|p| p.id.clone());
        self.conversation()
            .filter(move |p| Some(&p.id) != opening.as_ref())
    }

    /// The thread's own standing: the best score any single post in it reached.
    ///
    /// A sum would reward length — a thread with twenty mildly agreed posts
    /// would outrank one with a single answer everybody wanted. What a reader
    /// scanning a forum is looking for is the thread that produced something,
    /// so the thread is worth what its best post is worth.
    pub fn top_score(&self) -> i32 {
        self.top_score_with(&VoteContext::default())
    }

    /// [`Thread::top_score`] under an explicit counting rule.
    pub fn top_score_with(&self, ctx: &VoteContext) -> i32 {
        self.conversation()
            .map(|p| self.score_of_with(&p.id, ctx))
            .max()
            .unwrap_or(0)
    }

    /// Who currently owns the thread, from the most recent accepted `CLAIM`.
    pub fn claimed_by(&self) -> Option<&str> {
        self.posts
            .iter()
            .rev()
            .find(|p| p.kind == KIND_CLAIM && !p.is_denied())
            .map(|p| p.sender.as_str())
    }

    /// Distinct senders who have posted, in first-post order.
    pub fn participants(&self) -> Vec<&str> {
        let mut seen = Vec::new();
        for p in &self.posts {
            let s = p.sender.as_str();
            if !seen.contains(&s) {
                seen.push(s);
            }
        }
        seen
    }

    /// Timestamp of the newest post, or the creation time when there are none.
    pub fn last_activity(&self) -> &str {
        self.posts
            .last()
            .map(|p| p.ts.as_str())
            .unwrap_or(self.header.created_at.as_str())
    }
}

/// A thread as it appears in a list, without its full post log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSummary {
    /// What was fixed at creation.
    pub header: ThreadHeader,
    /// Projected status.
    pub status: ThreadStatus,
    /// Current owner, when claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_by: Option<String>,
    /// How many posts the thread holds.
    pub posts: usize,
    /// Timestamp of the newest post.
    pub last_activity: String,
    /// Refused posts in this thread — the count a reviewer should look at.
    pub denials: usize,
}

// ── roster ─────────────────────────────────────────────────────────────────

/// One participant's membership record, host-authored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterEntry {
    /// Forum identity.
    pub agent: String,
    /// Box this identity is bound to, absent for a human.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub box_id: Option<String>,
    /// What this participant may do.
    pub role: Role,
    /// Digest of the box's resolved policy at attach time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_digest: Option<String>,
    /// RFC3339 UTC attach time.
    pub attached_at: String,
    /// RFC3339 UTC revocation time. A revoked entry is kept, not deleted: the
    /// posts it made are still attributed to it, and a reader needs to be able
    /// to look up who that was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
    /// Host that attached this participant.
    ///
    /// A box id is a path — `env/<agent>/<slug>` — and paths collide across
    /// machines: two hosts can both hold `env/claude/auth`, and once the
    /// roster is union-merged their entries sit in one map. Without this
    /// field, "which entry is about *my* box" was answered by the path alone,
    /// so a peer attaching its same-named box could capture — or retire — a
    /// binding it had nothing to do with. Absent on entries written before
    /// origins reached the roster, which are treated as local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

impl RosterEntry {
    /// Is this participant still allowed to post?
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }

    /// Is this entry about `box_id` as it exists on the host `origin` names?
    ///
    /// The path matching alone is not enough — see [`RosterEntry::origin`].
    /// An entry with no origin predates the field and is treated as local;
    /// an entry from another host never matches a local box.
    pub fn is_box_on(&self, box_id: &str, origin: Option<&str>) -> bool {
        self.box_id.as_deref() == Some(box_id)
            && match (self.origin.as_deref(), origin) {
                (None, _) => true,
                (Some(a), Some(b)) => a == b,
                (Some(_), None) => false,
            }
    }
}

/// The forum's membership, stored as `roster.json` on the meta ref.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Roster {
    /// Participants by forum identity.
    #[serde(default)]
    pub agents: BTreeMap<String, RosterEntry>,
}

impl Roster {
    /// Look up a participant.
    pub fn get(&self, agent: &str) -> Option<&RosterEntry> {
        self.agents.get(agent)
    }

    /// Participants who have not been revoked.
    pub fn active(&self) -> impl Iterator<Item = &RosterEntry> {
        self.agents.values().filter(|e| e.is_active())
    }

    /// The active participant bound to `box_id` on the host `origin` names.
    ///
    /// Origin-scoped because the roster is merged across machines and a box
    /// id is only a path: without the scope, whichever same-pathed entry
    /// sorted first — possibly a peer's — answered for a local box.
    pub fn by_box(&self, box_id: &str, origin: Option<&str>) -> Option<&RosterEntry> {
        self.active().find(|e| e.is_box_on(box_id, origin))
    }
}

// ── ref helpers ────────────────────────────────────────────────────────────

/// Ref name for a thread.
pub fn thread_ref(id: &str) -> String {
    format!("{FORUM_THREADS_PREFIX}{id}")
}

/// Validate a thread id: 16 lowercase hex, which is what [`gen_id`] produces
/// and what is safe as a ref-name component. Ids reach `refs/` paths, so
/// anything else is refused rather than escaped.
pub fn validate_thread_id(id: &str) -> Result<(), H5iError> {
    if id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) {
        return Ok(());
    }
    Err(H5iError::Metadata(format!(
        "invalid thread id {:?}: expected 16 lowercase hex characters",
        crate::redact::sanitize_display(id)
    )))
}

/// Validate a forum identity against `[A-Za-z0-9._-]+`.
///
/// Names become roster keys, post fields and terminal output, so the charset is
/// the conservative one: no whitespace, no path separators, no control
/// characters.
pub fn validate_name(name: &str) -> Result<(), H5iError> {
    if name.is_empty() {
        return Err(H5iError::Metadata("forum identity must not be empty".into()));
    }
    if name.len() > 64 {
        return Err(H5iError::Metadata(
            "forum identity must be 64 characters or fewer".into(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(H5iError::Metadata(format!(
            "invalid forum identity {:?}: use letters, digits, '.', '_', '-' only",
            crate::redact::sanitize_display(name)
        )));
    }
    Ok(())
}

// ── reading ────────────────────────────────────────────────────────────────

/// Read a thread, open or closed.
pub fn read_thread(repo: &Repository, id: &str) -> Result<Thread, H5iError> {
    validate_thread_id(id)?;
    let oid = thread_tip(repo, id).ok_or_else(|| {
        H5iError::RecordNotFound(format!("no forum thread {id} in this repository"))
    })?;
    let thread = read_thread_at(repo, oid)?;
    // The header has to agree with the ref it hangs on. h5i writes them
    // together, so a mismatch is a ref a peer manufactured — and `read <id>`
    // opening a thread whose header names a *different* id is the same
    // inconsistency `list` already refuses to show. Refuse it here too, so a
    // forged ref is uniformly rejected rather than listed nowhere yet openable.
    if thread.header.id != id {
        return Err(H5iError::Metadata(format!(
            "forum thread {id} carries a header for {:?} — a forged ref, refused",
            crate::redact::sanitize_display(&thread.header.id)
        )));
    }
    Ok(thread)
}

/// Read the thread whose ref tip is `oid`.
pub fn read_thread_at(repo: &Repository, oid: Oid) -> Result<Thread, H5iError> {
    let header: ThreadHeader = refstore::read_file_from_commit(repo, oid, THREAD_FILE)
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .ok_or_else(|| {
            H5iError::Metadata("forum thread is missing its thread.json header".into())
        })?;
    let posts = parse_posts(
        &refstore::read_file_from_commit(repo, oid, POSTS_FILE).unwrap_or_default(),
    );
    Ok(Thread { header, posts })
}

/// Tip of a thread's ref.
pub fn thread_tip(repo: &Repository, id: &str) -> Option<Oid> {
    repo.refname_to_id(&thread_ref(id)).ok()
}

/// Every open thread, newest activity first.
///
/// This reads one small tree per thread rather than one large log, which is the
/// whole point of the per-thread layout: the cost is proportional to the number
/// of threads, not to the volume of conversation in them.
pub fn list_threads(repo: &Repository) -> Vec<ThreadSummary> {
    list_all_threads(repo)
        .into_iter()
        .filter(|t| t.status != ThreadStatus::Closed)
        .collect()
}

/// Every thread a human has closed.
pub fn list_closed(repo: &Repository) -> Vec<ThreadSummary> {
    list_all_threads(repo)
        .into_iter()
        .filter(|t| t.status == ThreadStatus::Closed)
        .collect()
}

/// Every thread, closed or not.
pub fn list_all_threads(repo: &Repository) -> Vec<ThreadSummary> {
    list_threads_in(repo, FORUM_THREADS_PREFIX)
}

fn list_threads_in(repo: &Repository, prefix: &str) -> Vec<ThreadSummary> {
    let glob = format!("{prefix}*");
    let Ok(refs) = repo.references_glob(&glob) else {
        return Vec::new();
    };
    let mut out: Vec<ThreadSummary> = refs
        .filter_map(|r| r.ok())
        .filter_map(|r| Some((r.name()?.rsplit('/').next()?.to_string(), r.target()?)))
        // The leaf must be a real thread id — 16 lowercase hex. Sync validates
        // this before it adopts anything, but listing reads whatever refs exist
        // under the namespace, and a leaf that is not a well-formed id is a ref
        // this build did not write. It matters beyond tidiness: readers slice
        // `id[..8]` for a short form, which would panic on a shorter or
        // non-ASCII id, so an id that is not the shape gen_id produces is never
        // summarised.
        .filter(|(leaf, _)| validate_thread_id(leaf).is_ok())
        .filter_map(|(leaf, oid)| read_thread_at(repo, oid).ok().map(|t| (leaf, t)))
        // The header has to agree with the ref it hangs on. h5i always writes
        // them together, so a mismatch is a ref somebody else manufactured —
        // and listing it under the header's id would make the list row and
        // `read <id>` open two different conversations. A ref this build did
        // not write is skipped, not renamed into legitimacy.
        .filter(|(leaf, t)| *leaf == t.header.id)
        .map(|(_, t)| summarize(&t))
        .collect();
    // Newest activity first; id breaks ties so the order is stable.
    out.sort_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| a.header.id.cmp(&b.header.id))
    });
    out
}

fn summarize(t: &Thread) -> ThreadSummary {
    ThreadSummary {
        header: t.header.clone(),
        status: t.status(),
        claimed_by: t.claimed_by().map(str::to_string),
        posts: t.posts.len(),
        last_activity: t.last_activity().to_string(),
        denials: t.posts.iter().filter(|p| p.is_denied()).count(),
    }
}

/// Read an attachment's payload out of a thread.
///
/// The bytes are checked against the digest they are filed under before they
/// are returned. Locally the two cannot disagree — the writer hashes what it
/// stores — but a thread tree can have arrived from a peer, and a peer is free
/// to file whatever bytes it likes under whatever name it likes. The digest is
/// the one thing a reader quotes ("the patch is `ab12…`"), so bytes that do
/// not match it are a forgery of exactly that quote, and are refused rather
/// than returned under it.
pub fn read_attachment(
    repo: &Repository,
    thread_id: &str,
    digest: &str,
) -> Result<Vec<u8>, H5iError> {
    validate_thread_id(thread_id)?;
    validate_digest(digest)?;
    let oid = thread_tip(repo, thread_id).ok_or_else(|| {
        H5iError::RecordNotFound(format!("no forum thread {thread_id} in this repository"))
    })?;
    let commit = repo.find_commit(oid)?;
    let tree = commit.tree()?;
    let entry = tree
        .get_name(&attachment_path(digest))
        .ok_or_else(|| H5iError::RecordNotFound(format!("no attachment {digest} in thread")))?;
    let blob = repo.find_blob(entry.id())?;
    let bytes = blob.content().to_vec();
    if refstore::sha256_hex(&bytes) != digest {
        return Err(H5iError::Metadata(format!(
            "attachment {digest} does not match its content address — \
             the bytes were filed under a digest they do not hash to"
        )));
    }
    Ok(bytes)
}

/// The forum roster.
pub fn read_roster(repo: &Repository) -> Roster {
    repo.refname_to_id(FORUM_META_REF)
        .ok()
        .and_then(|oid| refstore::read_file_from_commit(repo, oid, ROSTER_FILE))
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Parse a `posts.jsonl` blob, skipping blank and malformed lines so one bad
/// line written by a future build does not make a thread unreadable.
fn parse_posts(content: &str) -> Vec<Post> {
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<Post>(l).ok())
        .collect()
}

// ── writing: threads ───────────────────────────────────────────────────────

/// What a new post carries. The fields a box is allowed to decide.
#[derive(Debug, Clone, Default)]
pub struct NewPost {
    /// One of [`KINDS`].
    pub kind: String,
    /// Free text.
    pub body: String,
    /// Post being replied to.
    pub reply_to: Option<String>,
    /// Artifacts, as `(kind, name, payload)`.
    pub attachments: Vec<NewAttachment>,
    /// A refusal the host decided before the post was written. Set by ingest
    /// when it accepts a record but rejects part of it.
    pub denied: Option<String>,
}

/// An artifact to attach, before it is hashed and stored.
#[derive(Debug, Clone)]
pub struct NewAttachment {
    /// One of [`ATTACHMENT_KINDS`].
    pub kind: String,
    /// Display name.
    pub name: Option<String>,
    /// Payload bytes.
    pub payload: Vec<u8>,
}

// ── the box's side of the wire ─────────────────────────────────────────────

/// A post staged by a box in its capture spool, waiting for the host to ingest
/// it.
///
/// Note what is **not** here: no sender, no role, no box id, no policy digest.
/// Those are the host's to decide, and leaving them out of the wire format is
/// the cheapest possible enforcement — a field that does not exist cannot be
/// forged. Ingest builds the [`Author`] from the env directory the record was
/// found in.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ForumPostSpool {
    /// Thread to post into.
    pub thread: String,
    /// One of [`KINDS`].
    pub kind: String,
    /// Free text.
    pub body: String,
    /// Post being replied to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// Artifacts, inline. Every permitted attachment kind is text, so there is
    /// no base64 and no second spool file to correlate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<SpoolAttachment>,
}

/// An artifact staged inline by a box.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpoolAttachment {
    /// One of [`ATTACHMENT_KINDS`].
    pub kind: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Payload, as text.
    pub text: String,
}

impl ForumPostSpool {
    /// Turn a staged record into the post fields the host will accept.
    ///
    /// Anything the record asks for that exceeds a limit is dropped and named
    /// in `denied`, rather than failing the whole record: a box that attaches
    /// something too large should still have its message delivered, with the
    /// refusal visible next to it.
    pub fn into_new_post(self) -> NewPost {
        let mut denied: Vec<String> = Vec::new();
        let mut attachments = Vec::new();
        for a in self.attachments {
            if !ATTACHMENT_KINDS.contains(&a.kind.as_str()) {
                denied.push(format!("attachment of unsupported kind {:?} dropped", a.kind));
                continue;
            }
            if a.text.len() > MAX_ATTACHMENT_BYTES {
                denied.push(format!(
                    "attachment {} dropped: {} bytes over the {MAX_ATTACHMENT_BYTES}-byte limit",
                    a.name.as_deref().unwrap_or("(unnamed)"),
                    a.text.len()
                ));
                continue;
            }
            if attachments.len() >= MAX_ATTACHMENTS {
                denied.push("further attachments dropped: over the per-post limit".into());
                break;
            }
            attachments.push(NewAttachment {
                kind: a.kind,
                name: a.name,
                payload: a.text.into_bytes(),
            });
        }
        let mut body = self.body;
        if body.len() > MAX_BODY_BYTES {
            // The record is box-written, so the boundary can land mid-character
            // — and `String::truncate` panics off a char boundary, which would
            // let a staged record crash the host draining it. Back up to the
            // nearest boundary instead.
            let mut cut = MAX_BODY_BYTES;
            while !body.is_char_boundary(cut) {
                cut -= 1;
            }
            body.truncate(cut);
            denied.push(format!("body truncated to {MAX_BODY_BYTES} bytes"));
        }
        NewPost {
            kind: self.kind,
            body,
            reply_to: self.reply_to,
            attachments,
            denied: if denied.is_empty() {
                None
            } else {
                Some(denied.join("; "))
            },
        }
    }
}

/// A thread as delivered into a box's read-only inbox.
///
/// The box sees the header and the posts, and nothing about the forum's
/// machinery: no roster, no ref names, no other threads it is not part of.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxThread {
    /// What was fixed at creation.
    pub header: ThreadHeader,
    /// Projected status at delivery time.
    pub status: ThreadStatus,
    /// Current owner, when claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_by: Option<String>,
    /// Every post in the thread, in order.
    pub posts: Vec<Post>,
}

impl InboxThread {
    /// Build the box-facing view of a thread.
    pub fn of(t: &Thread) -> InboxThread {
        InboxThread {
            header: t.header.clone(),
            status: t.status(),
            claimed_by: t.claimed_by().map(str::to_string),
            posts: t.posts.clone(),
        }
    }
}

/// Create a thread. Human only.
///
/// The ceiling is recorded here and enforced at attach time; a thread created
/// without one bounds nothing, which is honest but should be rare.
/// Open a thread, optionally with the body that states it.
///
/// A title alone is a subject line, and a subject line is not a question. The
/// body is written as the thread's first post — kind `TASK`, human-authored,
/// numbered and votable like any other — so a thread is title plus opening
/// post, which is the shape every discussion surface has converged on and the
/// shape a reader already knows how to skim.
///
/// Making it a post rather than a field in the header is what keeps it honest:
/// it goes through the same scrub, carries the same host stamp, gets the same
/// vouching lane, and can be agreed with. A body kept in `thread.json` would
/// have been the one piece of prose on the forum with none of that.
pub fn create_thread(
    repo: &Repository,
    author: &Author,
    title: &str,
    body: Option<&str>,
    ceiling: Option<Ceiling>,
    branch: Option<String>,
) -> Result<ThreadHeader, H5iError> {
    author.require(author.role.can_govern(), "create a thread")?;
    let title = title.trim();
    if title.is_empty() {
        return Err(H5iError::Metadata("a thread needs a title".into()));
    }
    if title.len() > 200 {
        return Err(H5iError::Metadata(
            "thread title must be 200 characters or fewer".into(),
        ));
    }

    // Titles are published as widely as bodies, and are the part a reader sees
    // without opening anything.
    let title = crate::secrets::redact_text(title);
    let ts = now_ts();
    let id = gen_id(&ts, &author.sender, &title);
    let header = ThreadHeader {
        id: id.clone(),
        title: title.clone(),
        created_at: ts,
        created_by: author.sender.clone(),
        version: PROTOCOL_VERSION,
        ceiling,
        branch,
    };
    let header_json = serde_json::to_string_pretty(&header)?;

    let refname = thread_ref(&id);
    if repo.refname_to_id(&refname).is_ok() {
        return Err(H5iError::Internal(format!(
            "forum thread {id} already exists"
        )));
    }

    let tree_oid = refstore::build_tree(repo, None, &[(THREAD_FILE, &header_json), (POSTS_FILE, "")])?;
    let tree = repo.find_tree(tree_oid)?;
    let sig = refstore::signature(repo)?;
    let message = format!("h5i forum: open thread {id}");
    let commit = repo.commit(None, &sig, &sig, &message, &tree, &[])?;
    refstore::cas_ref_update(repo, &refname, None, commit, &message).map_err(H5iError::Git)?;

    if let Some(body) = body.map(str::trim).filter(|b| !b.is_empty()) {
        append_post(
            repo,
            author,
            &id,
            NewPost {
                kind: KIND_TASK.to_string(),
                body: body.to_string(),
                ..Default::default()
            },
        )?;
    }

    Ok(header)
}

/// Append a post to a thread.
///
/// Every check that could refuse the post happens before the compare-and-swap
/// loop, so a retry never re-runs validation and a refusal never costs a round
/// of contention.
pub fn append_post(
    repo: &Repository,
    author: &Author,
    thread_id: &str,
    new: NewPost,
) -> Result<Post, H5iError> {
    validate_thread_id(thread_id)?;
    validate_kind(&new.kind)?;
    author.require(author.role.can_post(), "post to a thread")?;
    if new.kind == KIND_CLAIM {
        author.require(author.role.can_claim(), "claim a thread")?;
    }
    if new.kind == KIND_TASK {
        author.require(author.role.can_govern(), "open a thread")?;
    }
    if new.kind == KIND_CLOSED {
        author.require(author.role.can_govern(), "close a thread")?;
    }
    if matches!(new.kind.as_str(), KIND_UPVOTE | KIND_DOWNVOTE) && new.reply_to.is_none() {
        return Err(H5iError::Metadata(
            "a vote has to name the post it is about (--reply-to)".into(),
        ));
    }
    if !new.attachments.is_empty() {
        author.require(author.role.can_attach(), "attach an artifact")?;
    }
    if new.body.len() > MAX_BODY_BYTES {
        return Err(H5iError::Metadata(format!(
            "post body is {} bytes, over the {MAX_BODY_BYTES}-byte limit — attach it instead",
            new.body.len()
        )));
    }
    if new.attachments.len() > MAX_ATTACHMENTS {
        return Err(H5iError::Metadata(format!(
            "{} attachments, over the limit of {MAX_ATTACHMENTS}",
            new.attachments.len()
        )));
    }
    for a in &new.attachments {
        validate_attachment_kind(&a.kind)?;
        if a.payload.len() > MAX_ATTACHMENT_BYTES {
            return Err(H5iError::Metadata(format!(
                "attachment is {} bytes, over the {MAX_ATTACHMENT_BYTES}-byte limit",
                a.payload.len()
            )));
        }
    }

    // Scrub before storing, and scrub unconditionally.
    //
    // A post is the one thing on this forum written to be read by someone else,
    // and an agent pastes what it is looking at — a failing request, a config
    // dump, an environment. Once that is a git object it is immutable, it is in
    // every clone, and if the forum's remote is a forge it is published. The
    // receipt store learned this the hard way and its note is the rule here
    // too: never gate the scrub on the detector. `scan_text` carries a
    // placeholder stoplist so it stays quiet on `example: ghp_…`, which is
    // right for reporting and fail-open for publication; `redact_text` has no
    // stoplist for exactly that reason. So scan only to name the rules, and
    // always scrub.
    let scrubbed_body = crate::secrets::redact_text(&new.body);
    let mut rules: Vec<String> = crate::secrets::scan_text(Path::new("<forum>"), &new.body)
        .iter()
        .map(|f| f.rule_id.to_string())
        .collect();
    let scrubbed: Vec<Vec<u8>> = new
        .attachments
        .iter()
        .map(|a| match std::str::from_utf8(&a.payload) {
            Ok(text) => {
                rules.extend(
                    crate::secrets::scan_text(Path::new("<forum-attachment>"), text)
                        .iter()
                        .map(|f| f.rule_id.to_string()),
                );
                crate::secrets::redact_text(text).into_bytes()
            }
            // Attachment kinds are all text, so this is a caller handing us
            // bytes that are not. Redact the valid UTF-8 runs and keep the
            // rest, the way the receipt store handles a binary payload, rather
            // than storing it untouched.
            Err(_) => crate::receipt::redact_binary(&a.payload),
        })
        .collect();
    rules.sort();
    rules.dedup();

    let ts = now_ts();
    let id = gen_id(&ts, &author.sender, &scrubbed_body);
    // Addressed by the digest of what is actually stored, so the content
    // address describes the bytes a reader will get back.
    let attachments: Vec<Attachment> = new
        .attachments
        .iter()
        .zip(scrubbed.iter())
        .map(|(a, payload)| Attachment {
            kind: a.kind.clone(),
            digest: refstore::sha256_hex(payload),
            size: payload.len(),
            name: a.name.clone(),
        })
        .collect();

    let post = Post {
        id,
        ts,
        thread: thread_id.to_string(),
        version: PROTOCOL_VERSION,
        kind: new.kind,
        body: scrubbed_body,
        reply_to: new.reply_to,
        attachments,
        sender: author.sender.clone(),
        box_id: author.box_id.clone(),
        role: author.role.as_str().to_string(),
        policy_digest: author.policy_digest.clone(),
        origin: author.origin.clone(),
        denied: new.denied,
        redactions: rules,
    };
    let line = serde_json::to_string(&post)?;
    let refname = thread_ref(thread_id);
    let message = format!("h5i forum: {} in {}", post.kind, thread_id);

    let mut last_err: Option<git2::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        refstore::cas_backoff(attempt);
        let tip = repo.refname_to_id(&refname).ok().ok_or_else(|| {
            H5iError::RecordNotFound(format!("no open forum thread {thread_id}"))
        })?;
        let parent = repo.find_commit(tip)?;
        let base_tree = parent.tree().ok();

        let mut log = refstore::read_blob_from_tree(repo, base_tree.as_ref(), POSTS_FILE)
            .unwrap_or_default();
        if !log.is_empty() && !log.ends_with('\n') {
            log.push('\n');
        }
        log.push_str(&line);
        log.push('\n');

        // Attachment payloads go in as blobs alongside the log. Writing them
        // inside the retry loop is deliberate: the tree has to be built over
        // whatever base this attempt read, and a blob that already exists is
        // free to write again.
        let mut builder = repo.treebuilder(base_tree.as_ref())?;
        let log_blob = repo.blob(log.as_bytes())?;
        builder.insert(POSTS_FILE, log_blob, 0o100644)?;
        for (meta, payload) in post.attachments.iter().zip(scrubbed.iter()) {
            let blob = repo.blob(payload)?;
            builder.insert(attachment_path(&meta.digest), blob, 0o100644)?;
        }
        let tree = repo.find_tree(builder.write()?)?;
        let sig = refstore::signature(repo)?;
        let new_oid = repo.commit(None, &sig, &sig, &message, &tree, &[&parent])?;

        match refstore::cas_ref_update(repo, &refname, Some(tip), new_oid, &message) {
            Ok(()) => return Ok(post),
            Err(e) => last_err = Some(e),
        }
    }
    Err(H5iError::Internal(format!(
        "forum post to {thread_id} could not be appended after {MAX_ATTEMPTS} attempts{}",
        refstore::cas_error_detail(&last_err)
    )))
}

/// Close a thread. Human only.
///
/// An append, not a deletion, and the reasoning is in [`KIND_CLOSED`]: a status
/// that lives in the absence of something cannot survive a peer that has not
/// heard about it yet.
pub fn close_thread(repo: &Repository, author: &Author, id: &str) -> Result<(), H5iError> {
    author.require(author.role.can_govern(), "close a thread")?;
    validate_thread_id(id)?;
    if thread_tip(repo, id).is_none() {
        return Err(H5iError::RecordNotFound(format!("no forum thread {id}")));
    }
    append_post(
        repo,
        author,
        id,
        NewPost {
            kind: KIND_CLOSED.to_string(),
            body: "closed".to_string(),
            ..Default::default()
        },
    )?;
    Ok(())
}

// ── writing: roster ────────────────────────────────────────────────────────

/// Add or update a roster entry. Human only.
pub fn put_roster_entry(
    repo: &Repository,
    author: &Author,
    entry: RosterEntry,
) -> Result<(), H5iError> {
    author.require(author.role.can_govern(), "change forum membership")?;
    validate_name(&entry.agent)?;
    // `human` is what the host renders for the person at the terminal, and a
    // reader's whole defence against a persuasive post is knowing which side
    // of the boundary wrote it. An agent posting under that name would carry
    // its true role in the next column, but "human (worker)" is a line built
    // to be misread — so the name is reserved rather than disambiguated.
    if entry.agent == "human" && entry.role != Role::Human {
        return Err(H5iError::Metadata(
            "the name `human` is reserved for the person at the host terminal".into(),
        ));
    }
    let ts = now_ts();
    update_roster(repo, &format!("h5i forum: attach {}", entry.agent), |r| {
        // An identity carries one box at a time, in both directions. Inserting
        // over an *active* entry bound to a different box would silently eject
        // that box — and, worse, re-attribute its history: every post the name
        // made would now read as the new box's work, which is exactly the
        // record this forum exists to keep straight. Refused, with the way out
        // named, rather than resolved by whoever attached last.
        // "The same box" is the path *and* the host: two machines can both
        // hold `env/claude/auth`, and a peer's identically-pathed box must
        // not read as an update to this one. Either side missing its origin
        // is a pre-origin entry, compared by path alone as it always was.
        let same_box = |existing: &RosterEntry| {
            existing.box_id == entry.box_id
                && match (existing.origin.as_deref(), entry.origin.as_deref()) {
                    (Some(a), Some(b)) => a == b,
                    _ => true,
                }
        };
        if let Some(existing) = r.agents.get(&entry.agent)
            && existing.revoked_at.is_none()
            && !same_box(existing)
        {
            let holder = match (&existing.box_id, &existing.origin) {
                (Some(b), Some(o)) => format!(
                    "{} on {}",
                    crate::redact::sanitize_display(b),
                    crate::redact::sanitize_display(o)
                ),
                (Some(b), None) => crate::redact::sanitize_display(b),
                (None, _) => "the human".to_string(),
            };
            return Err(H5iError::Metadata(format!(
                "{} is already the identity of {holder} — revoke it first, or attach \
                 under another name",
                crate::redact::sanitize_display(&entry.agent),
            )));
        }
        // A box carries one identity at a time. Attaching it under a new name
        // retires the old entry rather than leaving two active rows pointing at
        // the same box — which is not tidiness: the tender resolves a box to its
        // participant by scanning for that box id, so a stale duplicate makes
        // which identity a box has depend on how two names happen to sort.
        // Retired, not deleted, because the posts it made are still attributed
        // to it and a reader has to be able to look it up. Scoped to this
        // host's own box: a peer's entry that merely shares the path is
        // somebody else's participant, and retiring it here would be a
        // cross-machine revocation nobody asked for — one the merge's
        // revocation-wins rule would then make permanent everywhere.
        if let Some(box_id) = entry.box_id.as_deref() {
            for other in r.agents.values_mut() {
                if other.agent != entry.agent
                    && other.revoked_at.is_none()
                    && other.is_box_on(box_id, entry.origin.as_deref())
                {
                    other.revoked_at = Some(ts.clone());
                }
            }
        }
        r.agents.insert(entry.agent.clone(), entry.clone());
        Ok(())
    })
}

/// Revoke a participant. Human only.
///
/// Revocation is immediate at the next ingest: a spooled record from a revoked
/// box is refused and the refusal is recorded on the forum.
pub fn revoke(repo: &Repository, author: &Author, agent: &str) -> Result<(), H5iError> {
    author.require(author.role.can_govern(), "revoke a participant")?;
    validate_name(agent)?;
    let ts = now_ts();
    update_roster(repo, &format!("h5i forum: revoke {agent}"), |r| {
        match r.agents.get_mut(agent) {
            Some(e) => {
                if e.revoked_at.is_none() {
                    e.revoked_at = Some(ts.clone());
                }
                Ok(())
            }
            None => Err(H5iError::RecordNotFound(format!(
                "{agent} is not on this forum"
            ))),
        }
    })
}

fn update_roster<F>(repo: &Repository, message: &str, mut edit: F) -> Result<(), H5iError>
where
    F: FnMut(&mut Roster) -> Result<(), H5iError>,
{
    update_meta_file(repo, message, ROSTER_FILE, |raw| {
        let mut roster: Roster = raw
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        edit(&mut roster)?;
        Ok(serde_json::to_string_pretty(&roster)?)
    })
}

/// Rewrite one file on the meta ref through the same compare-and-swap loop
/// every other forum write uses. The closure sees the file as it is at this
/// attempt's tip — absent on a fresh forum — and returns what to store.
pub(crate) fn update_meta_file<F>(
    repo: &Repository,
    message: &str,
    file: &str,
    mut edit: F,
) -> Result<(), H5iError>
where
    F: FnMut(Option<String>) -> Result<String, H5iError>,
{
    let mut last_err: Option<git2::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        refstore::cas_backoff(attempt);
        let tip = repo.refname_to_id(FORUM_META_REF).ok();
        let parent = match tip {
            Some(oid) => Some(repo.find_commit(oid)?),
            None => None,
        };
        let base_tree = parent.as_ref().and_then(|c| c.tree().ok());

        let current = refstore::read_blob_from_tree(repo, base_tree.as_ref(), file);
        let updated = edit(current)?;

        let tree_oid = refstore::build_tree(repo, base_tree.as_ref(), &[(file, &updated)])?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = refstore::signature(repo)?;
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let new_oid = repo.commit(None, &sig, &sig, message, &tree, &parents)?;

        match refstore::cas_ref_update(repo, FORUM_META_REF, tip, new_oid, message) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(H5iError::Internal(format!(
        "forum meta file {file} could not be updated after {MAX_ATTEMPTS} attempts{}",
        refstore::cas_error_detail(&last_err)
    )))
}

/// Read one file off the meta ref's tip.
pub(crate) fn read_meta_file(repo: &Repository, file: &str) -> Option<String> {
    repo.refname_to_id(FORUM_META_REF)
        .ok()
        .and_then(|oid| refstore::read_file_from_commit(repo, oid, file))
}

// ── merging across clones ──────────────────────────────────────────────────

/// Union-merge two tips of one thread's ref.
///
/// Safe because `posts.jsonl` is append-only: two clones that each posted hold
/// non-overlapping line sets, so the union by id is the whole history and
/// neither side loses a post. Attachment blobs merge by inheriting the local
/// tree and layering the incoming ones, which is exactly right for
/// content-addressed names — the same digest is the same bytes.
pub fn union_merge_thread(
    repo: &Repository,
    local_oid: Oid,
    incoming_oid: Oid,
) -> Result<Oid, H5iError> {
    let local_commit = repo.find_commit(local_oid)?;
    let incoming_commit = repo.find_commit(incoming_oid)?;

    let local_posts =
        parse_posts(&refstore::read_file_from_commit(repo, local_oid, POSTS_FILE).unwrap_or_default());
    let incoming_posts = parse_posts(
        &refstore::read_file_from_commit(repo, incoming_oid, POSTS_FILE).unwrap_or_default(),
    );
    let merged = merge_post_sets(local_posts, incoming_posts);

    // The header is fixed at creation, so either side's copy is the same. Take
    // the local one when present and fall back to the incoming one, which is
    // what happens when this clone is learning of the thread for the first time.
    let header = refstore::read_file_from_commit(repo, local_oid, THREAD_FILE)
        .or_else(|| refstore::read_file_from_commit(repo, incoming_oid, THREAD_FILE))
        .ok_or_else(|| H5iError::Metadata("forum thread is missing its thread.json".into()))?;

    let base_tree = local_commit.tree().ok();
    let mut builder = repo.treebuilder(base_tree.as_ref())?;
    let posts_blob = repo.blob(merged.as_bytes())?;
    builder.insert(POSTS_FILE, posts_blob, 0o100644)?;
    let header_blob = repo.blob(header.as_bytes())?;
    builder.insert(THREAD_FILE, header_blob, 0o100644)?;
    // Layer in attachment blobs the incoming side has and we do not.
    if let Ok(incoming_tree) = incoming_commit.tree() {
        for entry in incoming_tree.iter() {
            let Some(name) = entry.name() else { continue };
            if name.starts_with(ATTACH_PREFIX) && builder.get(name)?.is_none() {
                builder.insert(name, entry.id(), 0o100644)?;
            }
        }
    }
    let tree = repo.find_tree(builder.write()?)?;

    let sig = refstore::signature(repo)?;
    let oid = repo.commit(
        None,
        &sig,
        &sig,
        "h5i forum: union-merge thread",
        &tree,
        &[&local_commit, &incoming_commit],
    )?;
    Ok(oid)
}

/// Union-merge two tips of the meta ref: roster, enrollments and policy.
///
/// For the roster, a revocation always wins over a non-revocation, and the
/// earlier revocation wins over a later one. Revocation is the safe direction:
/// reconciling two clones must never resurrect a participant a human took off
/// the forum. Enrollments and policy carry their own merge rules — see
/// [`crate::forum_identity`] — chosen to be deterministic in either direction,
/// because two clones that merge each other must land on the same bytes.
pub fn union_merge_meta(
    repo: &Repository,
    local_oid: Oid,
    incoming_oid: Oid,
) -> Result<Oid, H5iError> {
    let local_commit = repo.find_commit(local_oid)?;
    let incoming_commit = repo.find_commit(incoming_oid)?;

    let mut roster = read_roster_at(repo, local_oid);
    for (name, incoming) in read_roster_at(repo, incoming_oid).agents {
        match roster.agents.get_mut(&name) {
            Some(local) => {
                local.revoked_at = match (local.revoked_at.take(), incoming.revoked_at) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b),
                    (None, None) => None,
                };
            }
            None => {
                roster.agents.insert(name, incoming);
            }
        }
    }
    let roster_json = serde_json::to_string_pretty(&roster)?;

    let meta_file = |oid, file| refstore::read_file_from_commit(repo, oid, file);
    let enrollments = crate::forum_identity::merge_enrollments_raw(
        meta_file(local_oid, ENROLLMENTS_FILE).as_deref(),
        meta_file(incoming_oid, ENROLLMENTS_FILE).as_deref(),
    )?;
    let policy = crate::forum_identity::merge_policy_raw(
        meta_file(local_oid, POLICY_FILE).as_deref(),
        meta_file(incoming_oid, POLICY_FILE).as_deref(),
    )?;

    let mut files: Vec<(&str, &str)> = vec![(ROSTER_FILE, roster_json.as_str())];
    if let Some(e) = enrollments.as_deref() {
        files.push((ENROLLMENTS_FILE, e));
    }
    if let Some(p) = policy.as_deref() {
        files.push((POLICY_FILE, p));
    }

    let base_tree = local_commit.tree().ok();
    let tree_oid = refstore::build_tree(repo, base_tree.as_ref(), &files)?;
    let tree = repo.find_tree(tree_oid)?;
    let sig = refstore::signature(repo)?;
    let oid = repo.commit(
        None,
        &sig,
        &sig,
        "h5i forum: union-merge meta",
        &tree,
        &[&local_commit, &incoming_commit],
    )?;
    Ok(oid)
}

fn read_roster_at(repo: &Repository, oid: Oid) -> Roster {
    refstore::read_file_from_commit(repo, oid, ROSTER_FILE)
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn merge_post_sets(a: Vec<Post>, b: Vec<Post>) -> String {
    let mut by_id: HashMap<String, Post> = HashMap::new();
    for p in a.into_iter().chain(b) {
        by_id.entry(p.id.clone()).or_insert(p);
    }
    let mut posts: Vec<Post> = by_id.into_values().collect();
    posts.sort_by(|x, y| x.key().cmp(&y.key()));
    let mut out = String::new();
    for p in &posts {
        if let Ok(line) = serde_json::to_string(p) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

// ── small helpers ──────────────────────────────────────────────────────────

fn attachment_path(digest: &str) -> String {
    format!("{ATTACH_PREFIX}{digest}")
}

/// Validate an attachment digest: 64 lowercase hex. Digests become tree entry
/// names and inbox filenames, and a peer's post line can carry any string in
/// the field — so anything else is refused before it is ever joined to a path.
pub fn validate_digest(digest: &str) -> Result<(), H5iError> {
    if digest.len() == 64
        && digest
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Ok(());
    }
    Err(H5iError::Metadata(format!(
        "invalid attachment digest {:?}",
        crate::redact::sanitize_display(digest)
    )))
}

/// This host's forum identity, minted once and kept host-side.
///
/// A random 128-bit value with the machine's hostname appended for legibility,
/// stored under the sidecar root where no box can reach it. Deliberately *not*
/// presented as an authenticator: nothing signs it, so a hostile peer can put
/// any string in a post's `origin`. What it buys is the one comparison that is
/// sound — "did I stamp this?" — plus the ability to see that two posts claim
/// different sources.
///
/// The upgrade that would make it evidence is signing the forum commits, which
/// costs key management; the deliberate trade here is the one the whole remote
/// design makes, that a team should not have to operate anything.
pub fn host_origin(h5i_root: &Path) -> Result<String, H5iError> {
    let path = h5i_root.join("forum").join("origin");
    if let Ok(raw) = std::fs::read_to_string(&path) {
        let id = raw.trim();
        if !id.is_empty() && validate_name(id).is_ok() {
            return Ok(id.to_string());
        }
    }
    let label = hostname_label();
    let id = format!("{label}-{}", crate::token::hex(8)?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
    }
    std::fs::write(&path, &id).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(id)
}

/// A short, charset-safe machine label, or `host` when there is nothing usable.
fn hostname_label() -> String {
    let raw = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default();
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(24)
        .collect();
    if cleaned.is_empty() {
        "host".to_string()
    } else {
        cleaned.to_lowercase()
    }
}

/// RFC3339 UTC with microseconds, fixed width so it sorts lexicographically.
pub fn now_ts() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6fZ")
        .to_string()
}

/// A stable 16-hex id derived from the record's fields plus a random nonce, so
/// two identical bodies posted in the same microsecond still differ.
fn gen_id(ts: &str, who: &str, what: &str) -> String {
    let nonce = fastrand::u64(..);
    let mut hasher = Sha256::new();
    hasher.update(ts.as_bytes());
    hasher.update([0]);
    hasher.update(who.as_bytes());
    hasher.update([0]);
    hasher.update(what.as_bytes());
    hasher.update([0]);
    hasher.update(nonce.to_le_bytes());
    let digest = hasher.finalize();
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = Repository::init(dir.path()).expect("init");
        (dir, repo)
    }

    fn human() -> Author {
        Author::human("operator").expect("human author")
    }

    fn worker(name: &str, box_id: &str) -> Author {
        Author::agent(name, box_id, Role::Worker, Some("sha256:test".into())).expect("agent")
    }

    fn post(kind: &str, body: &str) -> NewPost {
        NewPost {
            kind: kind.to_string(),
            body: body.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn a_thread_round_trips_through_its_ref() {
        let (_d, repo) = temp_repo();
        let header = create_thread(&repo, &human(), "fix the auth race", None, None, None).unwrap();
        let thread = read_thread(&repo, &header.id).unwrap();
        assert_eq!(thread.header.title, "fix the auth race");
        assert_eq!(thread.header.created_by, "operator");
        assert!(thread.posts.is_empty());
        assert_eq!(thread.status(), ThreadStatus::Open);
    }

    #[test]
    fn each_thread_gets_its_own_ref() {
        let (_d, repo) = temp_repo();
        let a = create_thread(&repo, &human(), "first", None, None, None).unwrap();
        let b = create_thread(&repo, &human(), "second", None, None, None).unwrap();
        assert_ne!(a.id, b.id);
        assert!(repo.refname_to_id(&thread_ref(&a.id)).is_ok());
        assert!(repo.refname_to_id(&thread_ref(&b.id)).is_ok());
        assert_eq!(list_threads(&repo).len(), 2);
    }

    #[test]
    fn status_is_projected_from_posts_not_stored() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("claude-worker", "env/claude/t");

        append_post(&repo, &w, &h.id, post("FINDING", "found it")).unwrap();
        assert_eq!(read_thread(&repo, &h.id).unwrap().status(), ThreadStatus::Open);

        append_post(&repo, &w, &h.id, post(KIND_CLAIM, "mine")).unwrap();
        let t = read_thread(&repo, &h.id).unwrap();
        assert_eq!(t.status(), ThreadStatus::Claimed);
        assert_eq!(t.claimed_by(), Some("claude-worker"));

        append_post(&repo, &w, &h.id, post(KIND_REVIEW_REQUEST, "please look")).unwrap();
        assert_eq!(read_thread(&repo, &h.id).unwrap().status(), ThreadStatus::Review);

        append_post(&repo, &w, &h.id, post(KIND_DONE, "shipped")).unwrap();
        assert_eq!(read_thread(&repo, &h.id).unwrap().status(), ThreadStatus::Done);
    }

    #[test]
    fn the_sender_is_the_hosts_stamp_not_the_payload() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        // A box claiming to be someone else in its body changes nothing: the
        // identity comes from the Author the host constructed.
        let w = worker("codex-reviewer", "env/codex/t");
        let mut p = post("FINDING", r#"{"sender":"operator","role":"human"}"#);
        p.kind = "FINDING".into();
        let written = append_post(&repo, &w, &h.id, p).unwrap();
        assert_eq!(written.sender, "codex-reviewer");
        assert_eq!(written.role, "worker");
        assert_eq!(written.box_id.as_deref(), Some("env/codex/t"));
    }

    #[test]
    fn an_observer_cannot_post_and_a_worker_cannot_govern() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let obs = Author::agent("watcher", "env/x/y", Role::Observer, None).unwrap();
        assert!(append_post(&repo, &obs, &h.id, post("ASK", "hello")).is_err());

        let w = worker("claude-worker", "env/claude/t");
        assert!(create_thread(&repo, &w, "not allowed", None, None, None).is_err());
        assert!(close_thread(&repo, &w, &h.id).is_err());
        assert!(revoke(&repo, &w, "codex").is_err());
    }

    #[test]
    fn a_reviewer_cannot_claim_a_thread() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let r = Author::agent("codex-reviewer", "env/codex/t", Role::Reviewer, None).unwrap();
        assert!(append_post(&repo, &r, &h.id, post(KIND_CLAIM, "mine")).is_err());
        // but may still post and review
        assert!(append_post(&repo, &r, &h.id, post("FINDING", "a note")).is_ok());
    }

    /// Terminal control sequences are kept in storage and neutralised at render,
    /// which is the opposite of how a credential is handled two tests below.
    /// The distinction is deliberate: an escape sequence is only dangerous when
    /// it reaches a terminal, so storage keeps the bytes and the renderer is
    /// responsible. A credential is dangerous the moment it is *stored*, so it
    /// never gets that far.
    #[test]
    fn control_sequences_are_stored_and_neutralised_only_on_render() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("claude-worker", "env/claude/t");
        let hostile = "line one\u{1b}[2Jcleared\nline two";
        append_post(&repo, &w, &h.id, post("FINDING", hostile)).unwrap();

        let t = read_thread(&repo, &h.id).unwrap();
        assert_eq!(t.posts[0].body, hostile, "storage keeps the escape bytes");
        let shown = t.posts[0].display_body();
        assert!(!shown.contains('\u{1b}'), "render drops the escape");
        assert!(shown.contains('\n'), "render keeps the line break");
    }

    #[test]
    fn attachments_are_content_addressed_and_readable_back() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("claude-worker", "env/claude/t");
        let payload = b"--- a/x\n+++ b/x\n".to_vec();
        let p = NewPost {
            kind: KIND_REVIEW_REQUEST.into(),
            body: "here it is".into(),
            attachments: vec![NewAttachment {
                kind: "patch".into(),
                name: Some("fix.diff".into()),
                payload: payload.clone(),
            }],
            ..Default::default()
        };
        let written = append_post(&repo, &w, &h.id, p).unwrap();
        let a = &written.attachments[0];
        assert_eq!(a.digest, refstore::sha256_hex(&payload));
        assert_eq!(a.size, payload.len());
        assert_eq!(read_attachment(&repo, &h.id, &a.digest).unwrap(), payload);
    }

    #[test]
    fn an_oversized_body_is_refused_rather_than_truncated() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("claude-worker", "env/claude/t");
        let huge = "x".repeat(MAX_BODY_BYTES + 1);
        assert!(append_post(&repo, &w, &h.id, post("FINDING", &huge)).is_err());
    }

    #[test]
    fn an_unsupported_attachment_kind_is_refused() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("claude-worker", "env/claude/t");
        let p = NewPost {
            kind: "FINDING".into(),
            body: "b".into(),
            attachments: vec![NewAttachment {
                kind: "binary".into(),
                name: None,
                payload: vec![0u8; 4],
            }],
            ..Default::default()
        };
        assert!(append_post(&repo, &w, &h.id, p).is_err());
    }

    /// Closing is an append, and it has to be, because a status that lives in
    /// the *absence* of something cannot survive a peer that has not heard
    /// about it. The ref-moving version was silently undone by any clone that
    /// still held the live ref and pushed it back.
    #[test]
    fn closing_is_a_post_and_leaves_the_thread_readable() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("claude-worker", "env/claude/t");
        append_post(&repo, &w, &h.id, post("FINDING", "note")).unwrap();

        close_thread(&repo, &human(), &h.id).unwrap();

        // No ref moved and nothing was deleted.
        assert!(repo.refname_to_id(&thread_ref(&h.id)).is_ok());
        let t = read_thread(&repo, &h.id).unwrap();
        assert_eq!(t.status(), ThreadStatus::Closed);
        assert!(t.is_closed());
        assert_eq!(t.posts.len(), 2, "closing is a post, and the note survives");

        assert!(list_threads(&repo).is_empty(), "it leaves the live list");
        assert_eq!(list_closed(&repo).len(), 1);
        assert_eq!(list_all_threads(&repo).len(), 1);
    }

    /// An agent cannot reopen what a human closed, and a human can.
    ///
    /// Last-writer-wins is right for the working statuses, where the thread's
    /// owner is the authority. It is wrong for closing, because it would let an
    /// agent undo a governance decision just by claiming the thread.
    #[test]
    fn only_a_human_reopens_a_closed_thread() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("claude-worker", "env/claude/t");
        close_thread(&repo, &human(), &h.id).unwrap();

        append_post(&repo, &w, &h.id, post(KIND_CLAIM, "mine again")).unwrap();
        assert_eq!(
            read_thread(&repo, &h.id).unwrap().status(),
            ThreadStatus::Closed,
            "an agent must not reopen it"
        );

        append_post(&repo, &human(), &h.id, post(KIND_CLAIM, "reopening")).unwrap();
        assert_eq!(
            read_thread(&repo, &h.id).unwrap().status(),
            ThreadStatus::Claimed,
            "a human may"
        );
    }

    /// A human post that merely sorts after the close does not reopen it.
    ///
    /// Posts are ordered by `(ts, id)`, and across machines that order is not
    /// the order things happened: a post can arrive late from a peer, or be
    /// written while another host's clock ran ahead. Reopening has to be a
    /// deliberate act, not a consequence of sort position.
    #[test]
    fn a_late_arriving_human_note_does_not_reopen_a_closed_thread() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        close_thread(&repo, &human(), &h.id).unwrap();
        // An ordinary human note, of the kind a peer's clock skew could land
        // after the close.
        append_post(&repo, &human(), &h.id, post("FINDING", "one more thought")).unwrap();
        append_post(&repo, &human(), &h.id, post("ACK", "noted")).unwrap();
        assert_eq!(
            read_thread(&repo, &h.id).unwrap().status(),
            ThreadStatus::Closed,
            "only a status-moving human action reopens a thread"
        );
    }

    /// A vote is a post, so it merges and travels like everything else — and
    /// one participant's opinion counts once however often they express it.
    #[test]
    fn votes_are_posts_and_one_participant_counts_once() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let a = worker("alice", "env/a/1");
        let b = worker("bob", "env/b/1");
        let target = append_post(&repo, &a, &h.id, post("PROPOSAL", "single-flight it")).unwrap();

        let vote = |who: &Author, kind: &str| {
            append_post(
                &repo,
                who,
                &h.id,
                NewPost {
                    kind: kind.to_string(),
                    body: "+1".into(),
                    reply_to: Some(target.id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        };
        vote(&a, KIND_UPVOTE);
        vote(&a, KIND_UPVOTE); // saying it twice is still one opinion
        vote(&b, KIND_UPVOTE);
        assert_eq!(read_thread(&repo, &h.id).unwrap().score_of(&target.id), 2);

        // Changing your mind is another post, not an edit, so the change stays
        // visible in the log.
        vote(&b, KIND_DOWNVOTE);
        let t = read_thread(&repo, &h.id).unwrap();
        assert_eq!(t.score_of(&target.id), 0);
        assert_eq!(t.top_score(), 0);
    }

    /// The dedup unit is the host origin, not the sender name.
    ///
    /// A sender name is free to mint — one person can open a hundred worktrees
    /// and attach each under a fresh name — but every one of them posts through
    /// the same host stamp. So many names on one origin are one opinion, and
    /// the same name on two origins is two people, not one.
    #[test]
    fn votes_count_per_origin_not_per_sender_name() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let target = append_post(
            &repo,
            &worker("alice", "env/a/1"),
            &h.id,
            post("PROPOSAL", "single-flight it"),
        )
        .unwrap();
        let vote = |name: &str, box_id: &str, origin: &str| {
            append_post(
                &repo,
                &worker(name, box_id).from_host(origin),
                &h.id,
                NewPost {
                    kind: KIND_UPVOTE.into(),
                    body: "+1".into(),
                    reply_to: Some(target.id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        };

        // Worktree multiplication: five names, one machine, one vote.
        for i in 0..5 {
            vote(&format!("sock-{i}"), &format!("env/a/{i}"), "machine-a");
        }
        assert_eq!(
            read_thread(&repo, &h.id).unwrap().score_of(&target.id),
            1,
            "a hundred worktrees on one host are one opinion"
        );

        // Name collision: the same agent name on another machine is somebody
        // else, and their vote must not be folded into the first host's.
        vote("sock-0", "env/b/1", "machine-b");
        assert_eq!(
            read_thread(&repo, &h.id).unwrap().score_of(&target.id),
            2,
            "the same name on two hosts is two participants"
        );
    }

    /// A vote is not a turn in the conversation and must not move the thread on.
    #[test]
    fn a_vote_moves_no_state_and_is_not_part_of_the_conversation() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("alice", "env/a/1");
        let target = append_post(&repo, &w, &h.id, post("FINDING", "the bug")).unwrap();
        append_post(
            &repo,
            &w,
            &h.id,
            NewPost {
                kind: KIND_UPVOTE.into(),
                body: "+1".into(),
                reply_to: Some(target.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();

        let t = read_thread(&repo, &h.id).unwrap();
        assert_eq!(t.status(), ThreadStatus::Open, "agreeing claims nothing");
        assert_eq!(t.posts.len(), 2);
        assert_eq!(t.conversation().count(), 1, "the vote is not a turn");
    }

    /// A vote with nothing to point at is refused rather than counted as zero.
    #[test]
    fn a_vote_must_name_the_post_it_is_about() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("alice", "env/a/1");
        assert!(append_post(&repo, &w, &h.id, post(KIND_UPVOTE, "+1")).is_err());
    }

    /// A thread is a title *and* a body, and the body is a post.
    ///
    /// Written as the first `TASK` post rather than a field in the header, so
    /// it is numbered, votable, scrubbed and vouched like every other piece of
    /// prose here instead of being the one that is none of those.
    #[test]
    fn a_thread_body_is_its_first_post_and_behaves_like_one() {
        let (_d, repo) = temp_repo();
        let h = create_thread(
            &repo,
            &human(),
            "fix the auth race",
            Some("`refresh()` re-sends with the **pre-rotation** token."),
            None,
            None,
        )
        .unwrap();

        let t = read_thread(&repo, &h.id).unwrap();
        let opening = t.opening().expect("a thread opened with a body has one");
        assert_eq!(opening.kind, KIND_TASK);
        assert!(opening.body.contains("pre-rotation"));
        assert_eq!(t.replies().count(), 0);

        // It is a post: agreeing with it works like agreeing with anything.
        append_post(
            &repo,
            &worker("alice", "env/a/1"),
            &h.id,
            NewPost {
                kind: KIND_UPVOTE.into(),
                body: "+1".into(),
                reply_to: Some(opening.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let t = read_thread(&repo, &h.id).unwrap();
        assert_eq!(t.score_of(&t.opening().unwrap().id), 1);
        assert_eq!(t.status(), ThreadStatus::Open, "a body claims nothing");
    }

    /// A title-only thread still works, and simply has no opening post.
    #[test]
    fn a_thread_without_a_body_has_no_opening_post() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "just a title", None, None, None).unwrap();
        let t = read_thread(&repo, &h.id).unwrap();
        assert!(t.opening().is_none());
        assert!(t.posts.is_empty());

        // And a reply to it is a reply, not an opening.
        append_post(&repo, &worker("alice", "env/a/1"), &h.id, post("FINDING", "x")).unwrap();
        let t = read_thread(&repo, &h.id).unwrap();
        assert!(t.opening().is_none());
        assert_eq!(t.replies().count(), 1);
    }

    /// An agent cannot open a thread by posting a `TASK` into one.
    #[test]
    fn only_a_human_writes_an_opening_post() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("alice", "env/a/1");
        assert!(append_post(&repo, &w, &h.id, post(KIND_TASK, "my thread now")).is_err());
    }

    #[test]
    fn only_a_human_may_close() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("claude-worker", "env/claude/t");
        assert!(append_post(&repo, &w, &h.id, post(KIND_CLOSED, "bye")).is_err());
    }

    #[test]
    fn a_denied_post_is_recorded_and_moves_no_state() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("claude-worker", "env/claude/t");
        let mut p = post(KIND_CLAIM, "mine");
        p.denied = Some("sender revoked".into());
        append_post(&repo, &w, &h.id, p).unwrap();

        let t = read_thread(&repo, &h.id).unwrap();
        assert_eq!(t.posts.len(), 1, "the refusal is on the forum, not dropped");
        assert!(t.posts[0].is_denied());
        assert_eq!(t.status(), ThreadStatus::Open, "a refused claim claims nothing");
        assert_eq!(t.claimed_by(), None);
    }

    #[test]
    fn the_roster_records_attach_and_revoke() {
        let (_d, repo) = temp_repo();
        let entry = RosterEntry {
            agent: "claude-worker".into(),
            box_id: Some("env/claude/auth".into()),
            role: Role::Worker,
            policy_digest: Some("sha256:ab".into()),
            attached_at: now_ts(),
            revoked_at: None,
            origin: None,
        };
        put_roster_entry(&repo, &human(), entry).unwrap();
        let r = read_roster(&repo);
        assert!(r.get("claude-worker").unwrap().is_active());
        assert_eq!(r.by_box("env/claude/auth", None).unwrap().agent, "claude-worker");

        revoke(&repo, &human(), "claude-worker").unwrap();
        let r = read_roster(&repo);
        assert!(!r.get("claude-worker").unwrap().is_active());
        assert!(r.by_box("env/claude/auth", None).is_none());
        assert_eq!(r.active().count(), 0);
        assert_eq!(r.agents.len(), 1, "a revoked entry is kept for attribution");
    }

    /// A box carries one identity at a time.
    ///
    /// Re-attaching under a new name retires the old entry. Without that, two
    /// active rows point at one box and the tender's lookup — which scans for a
    /// box id — resolves to whichever name sorts first, so a box's identity
    /// would depend on alphabetical order.
    #[test]
    fn re_attaching_a_box_retires_the_identity_it_had_before() {
        let (_d, repo) = temp_repo();
        let mk = |agent: &str| RosterEntry {
            agent: agent.into(),
            box_id: Some("env/a/w".into()),
            role: Role::Worker,
            policy_digest: None,
            attached_at: now_ts(),
            revoked_at: None,
            origin: None,
        };
        put_roster_entry(&repo, &human(), mk("worker")).unwrap();
        put_roster_entry(&repo, &human(), mk("worker2")).unwrap();

        let r = read_roster(&repo);
        assert!(!r.get("worker").unwrap().is_active(), "the old identity retires");
        assert!(r.get("worker2").unwrap().is_active());
        assert_eq!(r.active().count(), 1, "exactly one identity per box");
        assert_eq!(r.by_box("env/a/w", None).unwrap().agent, "worker2");
        assert_eq!(r.agents.len(), 2, "the retired entry stays for attribution");
    }

    #[test]
    fn revoking_someone_who_is_not_a_member_is_an_error() {
        let (_d, repo) = temp_repo();
        assert!(revoke(&repo, &human(), "ghost").is_err());
    }

    /// A box id is a path, and two machines can hold the same path. Once the
    /// roster is merged, a peer's identically-pathed box must not capture,
    /// retire, or answer for a local one — each host's binding is (path,
    /// origin), not path alone.
    #[test]
    fn the_same_box_path_on_two_hosts_is_two_different_boxes() {
        let (_d, repo) = temp_repo();
        let entry = |name: &str, origin: &str| RosterEntry {
            agent: name.into(),
            box_id: Some("env/claude/auth".into()),
            role: Role::Worker,
            policy_digest: None,
            attached_at: now_ts(),
            revoked_at: None,
            origin: Some(origin.into()),
        };

        // Host A attaches its box; host B attaches its own box, which merely
        // shares the path. B's attach must not retire A's participant — the
        // merge's revocation-wins rule would make that ejection permanent on
        // every clone.
        put_roster_entry(&repo, &human(), entry("alpha", "machine-a")).unwrap();
        put_roster_entry(&repo, &human(), entry("beta", "machine-b")).unwrap();
        let r = read_roster(&repo);
        assert!(r.get("alpha").unwrap().is_active(), "A's box must survive B's attach");
        assert!(r.get("beta").unwrap().is_active());

        // Each host resolves the path to its own participant, whatever order
        // the names happen to sort in.
        assert_eq!(r.by_box("env/claude/auth", Some("machine-a")).unwrap().agent, "alpha");
        assert_eq!(r.by_box("env/claude/auth", Some("machine-b")).unwrap().agent, "beta");

        // And the equal path does not make B "the same box" for the purpose
        // of taking over A's name.
        let err = put_roster_entry(&repo, &human(), entry("alpha", "machine-b")).unwrap_err();
        assert!(err.to_string().contains("already the identity"), "{err}");

        // Re-attaching on the same host is still an ordinary update, and it
        // still retires that host's old name for the box.
        put_roster_entry(&repo, &human(), entry("alpha-2", "machine-a")).unwrap();
        let r = read_roster(&repo);
        assert!(!r.get("alpha").unwrap().is_active(), "A's old name retires");
        assert!(r.get("beta").unwrap().is_active(), "B's box is untouched by A's rename");
        assert_eq!(r.by_box("env/claude/auth", Some("machine-a")).unwrap().agent, "alpha-2");
    }

    /// A live identity cannot be handed to a second box: the overwrite would
    /// eject the first box silently and re-attribute every post it ever made.
    #[test]
    fn an_active_identity_cannot_be_taken_by_another_box() {
        let (_d, repo) = temp_repo();
        let mk = |box_id: &str| RosterEntry {
            agent: "claude-worker".into(),
            box_id: Some(box_id.into()),
            role: Role::Worker,
            policy_digest: None,
            attached_at: now_ts(),
            revoked_at: None,
            origin: None,
        };
        put_roster_entry(&repo, &human(), mk("env/a/one")).unwrap();
        let err = put_roster_entry(&repo, &human(), mk("env/b/two")).unwrap_err();
        assert!(err.to_string().contains("already the identity"), "{err}");
        assert_eq!(
            read_roster(&repo).get("claude-worker").unwrap().box_id.as_deref(),
            Some("env/a/one"),
            "the first box must keep its identity"
        );

        // Re-attaching the same box under its own name (a role change) is an
        // update, not a takeover. And once the name is revoked, it is free.
        let mut update = mk("env/a/one");
        update.role = Role::Reviewer;
        put_roster_entry(&repo, &human(), update).unwrap();
        revoke(&repo, &human(), "claude-worker").unwrap();
        put_roster_entry(&repo, &human(), mk("env/b/two")).unwrap();
        assert_eq!(
            read_roster(&repo).get("claude-worker").unwrap().box_id.as_deref(),
            Some("env/b/two")
        );
    }

    /// `human` is the host's rendering of the person, so no agent gets to wear
    /// it: "human (worker)" is a line built to be misread.
    #[test]
    fn no_agent_can_be_attached_under_the_name_human() {
        let (_d, repo) = temp_repo();
        let entry = |role: Role| RosterEntry {
            agent: "human".into(),
            box_id: Some("env/a/w".into()),
            role,
            policy_digest: None,
            attached_at: now_ts(),
            revoked_at: None,
            origin: None,
        };
        assert!(put_roster_entry(&repo, &human(), entry(Role::Worker)).is_err());
        assert!(put_roster_entry(&repo, &human(), entry(Role::Observer)).is_err());
        // The person themselves may be on the roster under it.
        let mut me = entry(Role::Human);
        me.box_id = None;
        assert!(put_roster_entry(&repo, &human(), me).is_ok());
    }

    /// A thread ref whose header names a different id was not written by h5i,
    /// and listing it under the header's id would make the list row and
    /// `read <id>` open two different conversations.
    #[test]
    fn a_thread_whose_header_disagrees_with_its_ref_is_not_listed() {
        let (_d, repo) = temp_repo();
        let real = create_thread(&repo, &human(), "real", None, None, None).unwrap();

        // Manufacture what a hostile peer could publish: the real thread's
        // tree, hung on a ref with a different (valid-looking) id.
        let oid = thread_tip(&repo, &real.id).unwrap();
        let fake_id = "00000000deadbeef";
        repo.reference(&thread_ref(fake_id), oid, false, "forged").unwrap();

        let listed: Vec<String> = list_threads(&repo).into_iter().map(|t| t.header.id).collect();
        assert_eq!(listed, vec![real.id.clone()], "the forged ref must not be listed");

        // And it is refused on read too, not merely hidden from the list —
        // otherwise `read <fake_id>` opens a thread whose header names a
        // different id than the one that was typed.
        let err = read_thread(&repo, fake_id).unwrap_err();
        assert!(err.to_string().contains("forged ref"), "{err}");
        // The real thread still reads by its own id.
        assert_eq!(read_thread(&repo, &real.id).unwrap().header.id, real.id);
    }

    /// A ref whose leaf is not a well-formed thread id is never summarised —
    /// readers slice `id[..8]`, so a non-hex or short id in the list would be
    /// a panic waiting for a `forum list`.
    #[test]
    fn a_ref_with_a_malformed_leaf_is_not_listed() {
        let (_d, repo) = temp_repo();
        let real = create_thread(&repo, &human(), "real", None, None, None).unwrap();
        let oid = thread_tip(&repo, &real.id).unwrap();

        // A ref under the threads namespace whose leaf is not 16-hex, with a
        // header that agrees with it — so only the id-shape check catches it.
        let bad_leaf = "nothex";
        let raw = format!("{FORUM_THREADS_PREFIX}{bad_leaf}");
        // Give it a matching header so the leaf==header.id filter would pass.
        let ts = now_ts();
        let header = ThreadHeader {
            id: bad_leaf.to_string(),
            title: "forged".into(),
            created_at: ts.clone(),
            created_by: "peer".into(),
            version: PROTOCOL_VERSION,
            ceiling: None,
            branch: None,
        };
        let header_json = serde_json::to_string(&header).unwrap();
        let tree_oid = refstore::build_tree(&repo, None, &[(THREAD_FILE, &header_json), (POSTS_FILE, "")]).unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = refstore::signature(&repo).unwrap();
        let _ = oid;
        let commit = repo.commit(None, &sig, &sig, "forged", &tree, &[]).unwrap();
        repo.reference(&raw, commit, false, "forged").unwrap();

        // It must not appear, and `forum list` (which slices id[..8]) must not
        // have anything short to slice.
        let listed: Vec<String> = list_all_threads(&repo).into_iter().map(|t| t.header.id).collect();
        assert_eq!(listed, vec![real.id.clone()]);
    }

    #[test]
    fn thread_ids_that_could_escape_a_ref_path_are_refused() {
        for bad in ["../../evil", "ABCDEF0123456789", "short", ""] {
            assert!(validate_thread_id(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(validate_thread_id("0123456789abcdef").is_ok());
    }

    #[test]
    fn forum_identities_reject_separators_and_control_characters() {
        for bad in ["", "a/b", "a b", "a\u{1b}b", "a\\b"] {
            assert!(validate_name(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(validate_name("claude-worker.2").is_ok());
    }

    #[test]
    fn union_merge_keeps_both_sides_posts_exactly_once() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("claude-worker", "env/claude/t");

        append_post(&repo, &w, &h.id, post("FINDING", "shared")).unwrap();
        let common = repo.refname_to_id(&thread_ref(&h.id)).unwrap();

        append_post(&repo, &w, &h.id, post("ASK", "local only")).unwrap();
        let local = repo.refname_to_id(&thread_ref(&h.id)).unwrap();

        // Rewind to the common tip and post something else: a second clone.
        repo.reference(&thread_ref(&h.id), common, true, "rewind").unwrap();
        append_post(&repo, &w, &h.id, post("RISK", "incoming only")).unwrap();
        let incoming = repo.refname_to_id(&thread_ref(&h.id)).unwrap();

        let merged = union_merge_thread(&repo, local, incoming).unwrap();
        let t = read_thread_at(&repo, merged).unwrap();
        assert_eq!(t.posts.len(), 3);
        let bodies: Vec<&str> = t.posts.iter().map(|p| p.body.as_str()).collect();
        assert!(bodies.contains(&"shared"));
        assert!(bodies.contains(&"local only"));
        assert!(bodies.contains(&"incoming only"));
        // and the order is the canonical one
        let mut sorted = t.posts.clone();
        sorted.sort_by(|a, b| a.key().cmp(&b.key()));
        assert_eq!(t.posts, sorted);
    }

    #[test]
    fn union_merge_never_resurrects_a_revoked_participant() {
        let (_d, repo) = temp_repo();
        let entry = RosterEntry {
            agent: "claude-worker".into(),
            box_id: None,
            role: Role::Worker,
            policy_digest: None,
            attached_at: now_ts(),
            revoked_at: None,
            origin: None,
        };
        put_roster_entry(&repo, &human(), entry).unwrap();
        let unrevoked = repo.refname_to_id(FORUM_META_REF).unwrap();
        revoke(&repo, &human(), "claude-worker").unwrap();
        let revoked = repo.refname_to_id(FORUM_META_REF).unwrap();

        for (a, b) in [(revoked, unrevoked), (unrevoked, revoked)] {
            let merged = union_merge_meta(&repo, a, b).unwrap();
            let r = read_roster_at(&repo, merged);
            assert!(
                !r.get("claude-worker").unwrap().is_active(),
                "revocation must survive a merge in either direction"
            );
        }
    }

    /// A credential in a post never reaches the store, and the scrub is not
    /// conditional on the detector agreeing that it is one.
    ///
    /// This is the property the whole remote story rests on. A post is written
    /// to be read by someone else, and an agent pastes what it is looking at —
    /// a failing request, an environment dump. Once that is a git object it is
    /// immutable and in every clone, and if the forum's remote is a forge it is
    /// published. The receipt store's note applies here word for word: gating
    /// the scrub on `scan_text` puts the hole back, because that scanner has a
    /// placeholder stoplist and goes quiet on `example: <real credential>`.
    #[test]
    fn a_credential_in_a_post_is_scrubbed_before_it_is_stored() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("claude-worker", "env/claude/t");
        let token = format!("ghp_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");

        let written = append_post(
            &repo,
            &w,
            &h.id,
            post("FINDING", &format!("the call fails with {token} in the header")),
        )
        .unwrap();

        assert!(!written.body.contains(&token), "the token must not be stored");
        assert!(
            !written.redactions.is_empty(),
            "and the reader should be told a credential was found"
        );
        // Read it back off the ref, not out of the return value: the point is
        // what landed in the object store.
        let t = read_thread(&repo, &h.id).unwrap();
        assert!(!t.posts[0].body.contains(&token));
    }

    /// The same, for a credential the detector deliberately stays quiet about.
    ///
    /// `scan_text` suppresses lines that look like documentation, so it reports
    /// nothing here. The scrub still has to happen — that gap is exactly how a
    /// real credential gets published behind the word "example".
    #[test]
    fn a_credential_behind_a_placeholder_word_is_still_scrubbed() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("claude-worker", "env/claude/t");
        let token = format!("ghp_{}", "Z9y8X7w6V5u4T3s2R1q0P9o8N7m6L5k4J3i2");

        let written = append_post(
            &repo,
            &w,
            &h.id,
            post("FINDING", &format!("example config: token = {token}")),
        )
        .unwrap();
        assert!(
            !written.body.contains(&token),
            "the scrub must not be gated on the detector reporting a finding"
        );
    }

    /// An attachment is published as widely as a body, so it is scrubbed too —
    /// and the content address describes the bytes that were actually stored.
    #[test]
    fn a_credential_in_an_attachment_is_scrubbed_and_the_digest_matches_the_stored_bytes() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let w = worker("claude-worker", "env/claude/t");
        let token = format!("ghp_{}", "M4n5B6v7C8x9Z0a1S2d3F4g5H6j7K8l9Q0w1");

        let written = append_post(
            &repo,
            &w,
            &h.id,
            NewPost {
                kind: KIND_REVIEW_REQUEST.into(),
                body: "here is the diff".into(),
                attachments: vec![NewAttachment {
                    kind: "patch".into(),
                    name: Some("fix.diff".into()),
                    payload: format!("+auth = \"{token}\"\n").into_bytes(),
                }],
                ..Default::default()
            },
        )
        .unwrap();

        let stored = read_attachment(&repo, &h.id, &written.attachments[0].digest).unwrap();
        let text = String::from_utf8(stored.clone()).unwrap();
        assert!(!text.contains(&token), "the attachment must be scrubbed too");
        assert_eq!(
            written.attachments[0].digest,
            refstore::sha256_hex(&stored),
            "the content address has to describe the stored bytes"
        );
        assert_eq!(written.attachments[0].size, stored.len());
    }

    /// A title is the part a reader sees without opening anything.
    #[test]
    fn a_credential_in_a_thread_title_is_scrubbed() {
        let (_d, repo) = temp_repo();
        let token = format!("ghp_{}", "T1r2E3w4Q5a6S7d8F9g0H1j2K3l4Z5x6C7v8");
        let header =
            create_thread(&repo, &human(), &format!("debug {token}"), None, None, None).unwrap();
        assert!(!header.title.contains(&token));
        assert!(!read_thread(&repo, &header.id).unwrap().header.title.contains(&token));
    }

    /// A peer can file any bytes under any `attach-<digest>` name; the digest
    /// is the thing a reader quotes, so mismatched bytes are refused rather
    /// than returned under it.
    #[test]
    fn attachment_bytes_that_do_not_match_their_digest_are_refused() {
        let (_d, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();

        // What a hostile clone would push: the tree entry's name claims a
        // digest the bytes do not hash to.
        let claimed = refstore::sha256_hex(b"what the reader was promised");
        let tip = thread_tip(&repo, &h.id).unwrap();
        let parent = repo.find_commit(tip).unwrap();
        let mut builder = repo.treebuilder(parent.tree().ok().as_ref()).unwrap();
        let forged = repo.blob(b"something else entirely").unwrap();
        builder
            .insert(attachment_path(&claimed), forged, 0o100644)
            .unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let sig = refstore::signature(&repo).unwrap();
        let new = repo.commit(None, &sig, &sig, "forged", &tree, &[&parent]).unwrap();
        repo.reference(&thread_ref(&h.id), new, true, "forged").unwrap();

        let err = read_attachment(&repo, &h.id, &claimed).unwrap_err();
        assert!(
            err.to_string().contains("content address"),
            "mismatched bytes must be refused, got: {err}"
        );
    }

    #[test]
    fn concurrent_appends_to_one_thread_all_land() {
        let (dir, repo) = temp_repo();
        let h = create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let path = dir.path().to_path_buf();
        let id = h.id.clone();

        let handles: Vec<_> = (0..4)
            .map(|i| {
                let path = path.clone();
                let id = id.clone();
                std::thread::spawn(move || {
                    let repo = Repository::open(&path).unwrap();
                    let w = worker(&format!("agent-{i}"), &format!("env/a/{i}"));
                    for n in 0..3 {
                        append_post(&repo, &w, &id, post("FINDING", &format!("{i}-{n}"))).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(read_thread(&repo, &id).unwrap().posts.len(), 12);
    }
}
