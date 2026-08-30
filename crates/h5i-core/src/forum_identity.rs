//! Who is behind a forum origin: enrollment, principals, and the vote policy.
//!
//! Three layers, each only as strong as what backs it. A **sender** is a display
//! name a worktree picked, free to mint. An **origin** is the machine stamp from
//! [`crate::forum::host_origin`], unforgeable locally but plain text on the
//! wire. A **principal** is a forge account bound to an origin by an enrollment.
//!
//! An enrollment is a signed record saying "this account operates this machine,
//! and here is the SSH public key that proves it". The key is the one the member
//! already pushes with, so the forge publishes it at
//! `https://github.com/<login>.keys` and nobody has to run a key server.
//!
//! It proves, offline, that the record was not altered and came from whoever
//! holds the key; tying that key to the account needs the forge, done once at
//! enrollment. It does **not** buy per-post authentication, since posts are
//! stamped rather than signed, so a hostile host can still write another host's
//! origin. What it narrows is the consequence: an unenrolled origin counts for
//! nothing under the principal vote rule.
//!
//! Both merges are deterministic in either direction, because two clones merging
//! each other must converge. Per origin, the same principal re-enrolling takes
//! the newer record (key rotation) and two different principals claiming one
//! origin keep the earlier, so an origin cannot be quietly re-bound by whoever
//! merges last. For policy, the newer `set_at` wins, and a tie goes to the
//! stricter rule.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use git2::Repository;
use serde::{Deserialize, Serialize};

use crate::error::H5iError;
use crate::forum::{self, Author, VoteContext, VoteRule};

/// Namespace for the SSH signature, so an enrollment signature can never be
/// replayed as a signature over anything else.
const SIG_NAMESPACE: &str = "h5i-forum-enroll";

/// Wire-format version written by this build.
pub const ENROLLMENT_VERSION: u32 = 1;

// ── the records ────────────────────────────────────────────────────────────

/// One origin's binding to a forge account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enrollment {
    /// Wire-format version.
    #[serde(default)]
    pub version: u32,
    /// The account, as a stable identifier: `github.com/user/12345678`. The
    /// numeric id, never the login — logins get renamed, ids do not.
    pub principal: String,
    /// The account's login at enrollment time. Display only, never a key.
    pub display_name: String,
    /// The host origin this account operates.
    pub origin: String,
    /// The SSH public key the record is signed with, pinned here so the
    /// binding stays verifiable offline and survives the account rotating its
    /// forge keys later.
    pub ssh_pubkey: String,
    /// RFC3339 UTC enrollment time.
    pub enrolled_at: String,
    /// Armored SSH signature over [`signing_payload`], namespace
    /// `h5i-forum-enroll`.
    pub signature: String,
}

/// Every enrollment on the forum, stored as `enrollments.json` on the meta ref.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enrollments {
    /// Bindings by origin. An origin belongs to at most one principal; one
    /// principal may enroll any number of origins (their laptop and their
    /// desktop are still one vote).
    #[serde(default)]
    pub by_origin: BTreeMap<String, Enrollment>,
}

impl Enrollments {
    /// The map the vote counter wants: `origin → principal`.
    pub fn principal_map(&self) -> BTreeMap<String, String> {
        self.by_origin
            .iter()
            .map(|(o, e)| (o.clone(), e.principal.clone()))
            .collect()
    }
}

/// Forum-wide policy, stored as `policy.json` on the meta ref.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumPolicy {
    /// What one vote is one unit of.
    #[serde(default)]
    pub vote: VoteRule,
    /// RFC3339 UTC time the policy was last set. Empty on a forum that never
    /// set one, and the merge's ordering key otherwise.
    #[serde(default)]
    pub set_at: String,
}

// ── reading and writing the meta ref ───────────────────────────────────────

/// Every enrollment on this clone's forum.
pub fn read_enrollments(repo: &Repository) -> Enrollments {
    forum::read_meta_file(repo, forum::ENROLLMENTS_FILE)
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// This clone's forum policy.
pub fn read_policy(repo: &Repository) -> ForumPolicy {
    forum::read_meta_file(repo, forum::POLICY_FILE)
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// The counting rule and bindings in force on this clone, ready for
/// [`crate::forum::Thread::score_of_with`].
///
/// Deliberately no signature check here: counting has to work on a host with
/// no `ssh-keygen`, and the merge rules — not per-read verification — are what
/// keep a binding from being quietly replaced. `verify_enrollment` is the
/// audit surface, run when a human asks.
pub fn vote_context(repo: &Repository) -> VoteContext {
    let policy = read_policy(repo);
    let principals = match policy.vote {
        VoteRule::Origin => BTreeMap::new(),
        VoteRule::Principal => read_enrollments(repo).principal_map(),
    };
    VoteContext {
        rule: policy.vote,
        principals,
    }
}

/// Record an enrollment. Human only — enrolling is a governance act on the
/// host being enrolled, and a box has no route here anyway.
pub fn put_enrollment(
    repo: &Repository,
    author: &Author,
    enrollment: Enrollment,
) -> Result<(), H5iError> {
    author.require_govern("enroll this host")?;
    forum::validate_name(&enrollment.origin)?;
    validate_principal(&enrollment.principal)?;
    if enrollment.signature.trim().is_empty() {
        return Err(H5iError::Metadata(
            "an enrollment must carry its signature".into(),
        ));
    }
    if key_blob(&enrollment.ssh_pubkey).is_none() {
        return Err(H5iError::Metadata(
            "an enrollment must pin an SSH public key (`<type> <base64>`)".into(),
        ));
    }
    let message = format!("h5i forum: enroll {}", enrollment.origin);
    forum::update_meta_file(repo, &message, forum::ENROLLMENTS_FILE, |raw| {
        let mut all: Enrollments = raw
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default();
        // Re-binding an origin to a *different* account is refused here rather
        // than accepted and lost: the cross-clone merge keeps an origin's
        // earliest binding (the anti-takeover direction), so a local rebind
        // would only survive until the next sync quietly reverted it. Refusing
        // at the door turns a silent revert into an explanation. Re-enrolling
        // the same account — key rotation — stays an ordinary update.
        if let Some(existing) = all.by_origin.get(&enrollment.origin)
            && existing.principal != enrollment.principal
        {
            return Err(H5iError::Metadata(format!(
                "{} is already enrolled as {} — an origin's first binding sticks \
                 across merges, so it cannot move to {}. To change accounts, give \
                 this machine a fresh origin (remove <h5i root>/forum/origin) and \
                 enroll that.",
                crate::redact::sanitize_display(&enrollment.origin),
                crate::redact::sanitize_display(&existing.principal),
                crate::redact::sanitize_display(&enrollment.principal),
            )));
        }
        all.by_origin
            .insert(enrollment.origin.clone(), enrollment.clone());
        Ok(serde_json::to_string_pretty(&all)?)
    })
}

/// Set the vote rule. Human only.
pub fn set_vote_rule(
    repo: &Repository,
    author: &Author,
    rule: VoteRule,
) -> Result<ForumPolicy, H5iError> {
    author.require_govern("change the forum's vote policy")?;
    let policy = ForumPolicy {
        vote: rule,
        set_at: forum::now_ts(),
    };
    let message = format!("h5i forum: vote policy {}", rule.as_str());
    let stored = policy.clone();
    forum::update_meta_file(repo, &message, forum::POLICY_FILE, move |_| {
        Ok(serde_json::to_string_pretty(&stored)?)
    })?;
    Ok(policy)
}

// ── merging across clones ──────────────────────────────────────────────────

/// Merge two `enrollments.json` blobs. `None` in means that side has no file;
/// `None` out means neither did, so the merge writes nothing.
pub fn merge_enrollments_raw(
    local: Option<&str>,
    incoming: Option<&str>,
) -> Result<Option<String>, H5iError> {
    if local.is_none() && incoming.is_none() {
        return Ok(None);
    }
    let parse = |raw: Option<&str>| -> Enrollments {
        raw.and_then(|r| serde_json::from_str(r).ok()).unwrap_or_default()
    };
    let mut merged = parse(local);
    for (origin, theirs) in parse(incoming).by_origin {
        match merged.by_origin.get(&origin) {
            None => {
                merged.by_origin.insert(origin, theirs);
            }
            Some(ours) if ours == &theirs => {}
            Some(ours) => {
                let winner = pick_enrollment(ours, &theirs).clone();
                merged.by_origin.insert(origin, winner);
            }
        }
    }
    Ok(Some(serde_json::to_string_pretty(&merged)?))
}

/// The deterministic winner when two clones disagree about one origin.
///
/// Same principal: the newer record wins, which is what key rotation looks
/// like. Different principals: the *earlier* record wins — an origin's first
/// binding sticks, so a later claim cannot take an origin over by merging.
/// Ties break on the serialized bytes, purely so both directions converge.
fn pick_enrollment<'a>(a: &'a Enrollment, b: &'a Enrollment) -> &'a Enrollment {
    let stable = |e: &Enrollment| serde_json::to_string(e).unwrap_or_default();
    if a.principal == b.principal {
        match a.enrolled_at.cmp(&b.enrolled_at) {
            std::cmp::Ordering::Less => b,
            std::cmp::Ordering::Greater => a,
            std::cmp::Ordering::Equal => {
                if stable(a) >= stable(b) { a } else { b }
            }
        }
    } else {
        match a.enrolled_at.cmp(&b.enrolled_at) {
            std::cmp::Ordering::Less => a,
            std::cmp::Ordering::Greater => b,
            std::cmp::Ordering::Equal => {
                if stable(a) <= stable(b) { a } else { b }
            }
        }
    }
}

/// Merge two `policy.json` blobs: the newer `set_at` wins, and a tie goes to
/// the stricter rule — two clones must converge, and when nothing else breaks
/// the tie, converging on the tighter reading is the only defensible pick.
pub fn merge_policy_raw(
    local: Option<&str>,
    incoming: Option<&str>,
) -> Result<Option<String>, H5iError> {
    if local.is_none() && incoming.is_none() {
        return Ok(None);
    }
    let parse = |raw: Option<&str>| -> ForumPolicy {
        raw.and_then(|r| serde_json::from_str(r).ok()).unwrap_or_default()
    };
    let a = parse(local);
    let b = parse(incoming);
    let winner = match a.set_at.cmp(&b.set_at) {
        std::cmp::Ordering::Greater => a,
        std::cmp::Ordering::Less => b,
        std::cmp::Ordering::Equal => {
            if a.vote == VoteRule::Principal || b.vote == VoteRule::Principal {
                ForumPolicy {
                    vote: VoteRule::Principal,
                    set_at: a.set_at,
                }
            } else {
                a
            }
        }
    };
    Ok(Some(serde_json::to_string_pretty(&winner)?))
}

// ── signing and verifying ──────────────────────────────────────────────────

/// The exact bytes an enrollment's signature covers: the record minus the
/// signature itself, as JSON with sorted keys, so every build canonicalizes
/// identically.
///
/// The sort is an explicit `BTreeMap`, not the accident of `serde_json`'s map
/// type: that type silently becomes insertion-ordered if any crate in the
/// build ever enables `preserve_order`, and a canonical form that depends on
/// feature unification is one that stops verifying across builds without
/// anyone touching this code. The record is flat, so sorting the top level is
/// sorting all of it.
pub fn signing_payload(e: &Enrollment) -> Result<String, H5iError> {
    let v = serde_json::to_value(e)?;
    let obj = v
        .as_object()
        .ok_or_else(|| H5iError::Internal("an enrollment serializes as an object".into()))?;
    let sorted: BTreeMap<&String, &serde_json::Value> =
        obj.iter().filter(|(k, _)| k.as_str() != "signature").collect();
    Ok(serde_json::to_string(&sorted)?)
}

/// Sign `enrollment` in place with the SSH private key at `key`.
///
/// Shells out to `ssh-keygen -Y sign`, which is in every OpenSSH install and
/// prompts on the tty if the key has a passphrase. The pinned public key is
/// read from `<key>.pub` and recorded before signing, so the signature covers
/// the key it must later verify against.
pub fn sign_enrollment(enrollment: &mut Enrollment, key: &Path) -> Result<(), H5iError> {
    enrollment.ssh_pubkey = read_pubkey(key)?;
    let payload = signing_payload(enrollment)?;

    let dir = tempfile::tempdir()
        .map_err(|e| H5iError::Metadata(format!("cannot create a signing workspace: {e}")))?;
    let payload_path = dir.path().join("enrollment");
    std::fs::write(&payload_path, &payload).map_err(|e| H5iError::with_path(e, &payload_path))?;

    let out = Command::new("ssh-keygen")
        .arg("-Y")
        .arg("sign")
        .arg("-f")
        .arg(key)
        .arg("-n")
        .arg(SIG_NAMESPACE)
        .arg(&payload_path)
        .output()
        .map_err(|e| H5iError::Metadata(format!("could not run ssh-keygen: {e}")))?;
    if !out.status.success() {
        return Err(H5iError::Metadata(format!(
            "ssh-keygen -Y sign failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let sig_path = dir.path().join("enrollment.sig");
    enrollment.signature =
        std::fs::read_to_string(&sig_path).map_err(|e| H5iError::with_path(e, &sig_path))?;

    // Never record a signature this build cannot verify: a signing quirk found
    // here is a typo; found on a peer it is a broken enrollment.
    verify_enrollment(enrollment)
}

/// Check an enrollment's signature against its own pinned key.
///
/// This is the offline half of the story: it proves the record is intact and
/// was written by whoever holds the pinned key. Whether that key belongs to
/// the named account is the forge's half — compare the pinned key against the
/// account's published keys ([`published_key_matches`]).
pub fn verify_enrollment(e: &Enrollment) -> Result<(), H5iError> {
    validate_principal(&e.principal)?;
    let payload = signing_payload(e)?;

    let dir = tempfile::tempdir()
        .map_err(|e| H5iError::Metadata(format!("cannot create a verify workspace: {e}")))?;
    // An allowed_signers line is `<identity> <keytype> <blob>`. The principal
    // has no whitespace (validated above), and the pinned key is re-parsed
    // down to its type and blob rather than written as the bytes a peer's
    // record happens to carry — this file has a line-oriented grammar, and a
    // key field with a newline in it would otherwise get to write lines of it.
    let pinned = key_blob(&e.ssh_pubkey).ok_or_else(|| {
        H5iError::Metadata(format!(
            "enrollment for {} pins something that is not an SSH public key",
            crate::redact::sanitize_display(&e.origin)
        ))
    })?;
    let allowed = format!("{} {pinned}\n", e.principal);
    let allowed_path = dir.path().join("allowed_signers");
    std::fs::write(&allowed_path, allowed).map_err(|err| H5iError::with_path(err, &allowed_path))?;
    let sig_path = dir.path().join("enrollment.sig");
    std::fs::write(&sig_path, &e.signature).map_err(|err| H5iError::with_path(err, &sig_path))?;

    let mut child = Command::new("ssh-keygen")
        .arg("-Y")
        .arg("verify")
        .arg("-f")
        .arg(&allowed_path)
        .arg("-I")
        .arg(&e.principal)
        .arg("-n")
        .arg(SIG_NAMESPACE)
        .arg("-s")
        .arg(&sig_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| H5iError::Metadata(format!("could not run ssh-keygen: {e}")))?;
    if let Some(stdin) = child.stdin.take() {
        use std::io::Write as _;
        let mut stdin = stdin;
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| H5iError::Metadata(format!("ssh-keygen did not finish: {e}")))?;
    if !out.status.success() {
        return Err(H5iError::Metadata(format!(
            "enrollment signature for {} did not verify: {}",
            crate::redact::sanitize_display(&e.origin),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// Read the public half of an SSH key: `<key>.pub`, first line, first two
/// fields (type and blob) — the comment is dropped because the forge drops it
/// too, and the pinned key must compare equal to the published one.
pub fn read_pubkey(key: &Path) -> Result<String, H5iError> {
    let pub_path = PathBuf::from(format!("{}.pub", key.display()));
    let raw = std::fs::read_to_string(&pub_path).map_err(|e| H5iError::with_path(e, &pub_path))?;
    key_blob(&raw).ok_or_else(|| {
        H5iError::Metadata(format!(
            "{} does not look like an SSH public key",
            pub_path.display()
        ))
    })
}

/// The comparable part of a public key line: `<type> <base64>`.
fn key_blob(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    let keytype = parts.next()?;
    let blob = parts.next()?;
    if !keytype.starts_with("ssh-") && !keytype.starts_with("ecdsa-") && !keytype.starts_with("sk-")
    {
        return None;
    }
    Some(format!("{keytype} {blob}"))
}

/// Is the pinned key among the keys the forge publishes for this account?
pub fn published_key_matches(published: &[String], pinned: &str) -> bool {
    let Some(pinned) = key_blob(pinned) else {
        return false;
    };
    published
        .iter()
        .filter_map(|k| key_blob(k))
        .any(|k| k == pinned)
}

/// The private key `enroll` uses when none is named: the first of the usual
/// suspects that exists.
pub fn default_ssh_key() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let ssh = PathBuf::from(home).join(".ssh");
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|n| ssh.join(n))
        .find(|p| p.is_file() && PathBuf::from(format!("{}.pub", p.display())).is_file())
}

// ── the forge ──────────────────────────────────────────────────────────────

/// The account behind the local `gh` login: `(principal, login)`.
///
/// The principal is built from the numeric id, so it survives the login being
/// renamed; the login rides along for display and for the key lookup.
pub fn github_principal() -> Result<(String, String), H5iError> {
    let raw = gh(&["api", "user"])?;
    let v: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| H5iError::Metadata(format!("gh api user returned malformed JSON: {e}")))?;
    let id = v
        .get("id")
        .and_then(|i| i.as_u64())
        .ok_or_else(|| H5iError::Metadata("gh api user returned no account id".into()))?;
    let login = v
        .get("login")
        .and_then(|l| l.as_str())
        .unwrap_or("")
        .to_string();
    Ok((format!("github.com/user/{id}"), login))
}

/// The SSH public keys GitHub publishes for `login`.
pub fn github_published_keys(login: &str) -> Result<Vec<String>, H5iError> {
    // The login is spliced into an API path, so hold it to the charset GitHub
    // itself allows rather than trusting whatever string reached us.
    if login.is_empty()
        || !login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(H5iError::Metadata(format!(
            "not a GitHub login: {:?}",
            crate::redact::sanitize_display(login)
        )));
    }
    // The API pages at 30 by default, and an account with more keys than that
    // would read as "key not published" purely because the match fell on a
    // later page. One page of 100 is the cheap fix; an account with more SSH
    // keys than that is not a case worth a pagination loop here.
    let raw = gh(&["api", &format!("users/{login}/keys?per_page=100")])?;
    let v: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| H5iError::Metadata(format!("gh returned malformed JSON: {e}")))?;
    Ok(v.as_array()
        .map(|keys| {
            keys.iter()
                .filter_map(|k| k.get("key").and_then(|s| s.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

fn gh(args: &[&str]) -> Result<String, H5iError> {
    let out = Command::new("gh")
        .args(args)
        .output()
        .map_err(|e| H5iError::Metadata(format!("could not run gh (is it installed?): {e}")))?;
    if !out.status.success() {
        return Err(H5iError::Metadata(format!(
            "gh {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ── validation ─────────────────────────────────────────────────────────────

/// A principal is stored, merged, rendered and used as a signature identity,
/// so the charset is conservative: forge-path characters, no whitespace, no
/// control bytes.
pub fn validate_principal(principal: &str) -> Result<(), H5iError> {
    if principal.is_empty() || principal.len() > 128 {
        return Err(H5iError::Metadata(
            "a principal must be 1 to 128 characters".into(),
        ));
    }
    if !principal
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':' | '@'))
    {
        return Err(H5iError::Metadata(format!(
            "invalid principal {:?}: use letters, digits, '.', '_', '-', '/', ':', '@' only",
            crate::redact::sanitize_display(principal)
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forum::{NewPost, Role};

    fn temp_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = Repository::init(dir.path()).expect("init");
        (dir, repo)
    }

    fn human() -> Author {
        Author::human("operator").expect("human author")
    }

    fn record(origin: &str, principal: &str, at: &str) -> Enrollment {
        Enrollment {
            version: ENROLLMENT_VERSION,
            principal: principal.into(),
            display_name: "someone".into(),
            origin: origin.into(),
            ssh_pubkey: "ssh-ed25519 AAAATEST".into(),
            enrolled_at: at.into(),
            signature: "-----BEGIN SSH SIGNATURE-----\ntest\n-----END SSH SIGNATURE-----\n".into(),
        }
    }

    /// Generate a throwaway key, or skip when the host has no ssh-keygen.
    fn throwaway_key(dir: &Path) -> Option<PathBuf> {
        let key = dir.join("id_test");
        let ok = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        ok.then_some(key)
    }

    #[test]
    fn an_enrollment_signs_and_verifies_and_tampering_is_caught() {
        let dir = tempfile::tempdir().unwrap();
        let Some(key) = throwaway_key(dir.path()) else {
            eprintln!("skipping: no ssh-keygen on this host");
            return;
        };
        let mut e = record("machine-a-00ff", "github.com/user/12345678", &forum::now_ts());
        sign_enrollment(&mut e, &key).unwrap();
        assert!(e.ssh_pubkey.starts_with("ssh-ed25519 "), "{}", e.ssh_pubkey);
        verify_enrollment(&e).unwrap();

        // Any field the signature covers is load-bearing.
        let mut forged = e.clone();
        forged.principal = "github.com/user/99999999".into();
        assert!(verify_enrollment(&forged).is_err(), "a re-bound principal must not verify");
        let mut forged = e.clone();
        forged.origin = "machine-b-1234".into();
        assert!(verify_enrollment(&forged).is_err(), "a re-bound origin must not verify");
    }

    #[test]
    fn the_signature_field_itself_is_outside_the_signed_payload() {
        let a = record("m", "github.com/user/1", "2026-01-01T00:00:00.000000Z");
        let mut b = a.clone();
        b.signature = "different".into();
        assert_eq!(signing_payload(&a).unwrap(), signing_payload(&b).unwrap());
        assert!(!signing_payload(&a).unwrap().contains("signature"));
    }

    #[test]
    fn enrollment_merge_is_deterministic_in_both_directions() {
        let early = record("m", "github.com/user/1", "2026-01-01T00:00:00.000000Z");
        let late_same = record("m", "github.com/user/1", "2026-02-01T00:00:00.000000Z");
        let late_other = record("m", "github.com/user/2", "2026-02-01T00:00:00.000000Z");

        let wrap = |e: &Enrollment| {
            serde_json::to_string(&Enrollments {
                by_origin: [(e.origin.clone(), e.clone())].into(),
            })
            .unwrap()
        };
        let unwrap = |raw: Option<String>| -> Enrollments {
            serde_json::from_str(&raw.unwrap()).unwrap()
        };

        // Same principal: rotation, the newer record wins either way round.
        for (a, b) in [(&early, &late_same), (&late_same, &early)] {
            let merged = unwrap(merge_enrollments_raw(Some(&wrap(a)), Some(&wrap(b))).unwrap());
            assert_eq!(merged.by_origin["m"].enrolled_at, late_same.enrolled_at);
        }
        // Different principals: the first binding sticks either way round.
        for (a, b) in [(&early, &late_other), (&late_other, &early)] {
            let merged = unwrap(merge_enrollments_raw(Some(&wrap(a)), Some(&wrap(b))).unwrap());
            assert_eq!(merged.by_origin["m"].principal, "github.com/user/1");
        }
        // A side with no file contributes nothing and loses nothing.
        assert!(merge_enrollments_raw(None, None).unwrap().is_none());
        let merged = unwrap(merge_enrollments_raw(None, Some(&wrap(&early))).unwrap());
        assert_eq!(merged.by_origin.len(), 1);
    }

    #[test]
    fn policy_merge_takes_the_newer_and_breaks_ties_strict() {
        let old = serde_json::to_string(&ForumPolicy {
            vote: VoteRule::Principal,
            set_at: "2026-01-01T00:00:00.000000Z".into(),
        })
        .unwrap();
        let new = serde_json::to_string(&ForumPolicy {
            vote: VoteRule::Origin,
            set_at: "2026-02-01T00:00:00.000000Z".into(),
        })
        .unwrap();
        for (a, b) in [(&old, &new), (&new, &old)] {
            let merged: ForumPolicy =
                serde_json::from_str(&merge_policy_raw(Some(a), Some(b)).unwrap().unwrap())
                    .unwrap();
            assert_eq!(merged.vote, VoteRule::Origin, "loosening later must be possible");
        }
        let tied = serde_json::to_string(&ForumPolicy {
            vote: VoteRule::Principal,
            set_at: "2026-02-01T00:00:00.000000Z".into(),
        })
        .unwrap();
        let merged: ForumPolicy =
            serde_json::from_str(&merge_policy_raw(Some(&new), Some(&tied)).unwrap().unwrap())
                .unwrap();
        assert_eq!(merged.vote, VoteRule::Principal, "a tie goes to the stricter rule");
        assert!(merge_policy_raw(None, None).unwrap().is_none());
    }

    #[test]
    fn enrollment_and_policy_round_trip_through_the_meta_ref() {
        let (_d, repo) = temp_repo();
        put_enrollment(&repo, &human(), record("machine-a", "github.com/user/7", &forum::now_ts()))
            .unwrap();
        let all = read_enrollments(&repo);
        assert_eq!(all.by_origin["machine-a"].principal, "github.com/user/7");

        assert_eq!(read_policy(&repo).vote, VoteRule::Origin, "the default is origin");
        set_vote_rule(&repo, &human(), VoteRule::Principal).unwrap();
        assert_eq!(read_policy(&repo).vote, VoteRule::Principal);

        // And the roster still lives beside them, untouched.
        forum::put_roster_entry(
            &repo,
            &human(),
            forum::RosterEntry {
                agent: "alice".into(),
                box_id: None,
                role: Role::Worker,
                policy_digest: None,
                attached_at: forum::now_ts(),
                revoked_at: None,
                origin: None,
            },
        )
        .unwrap();
        assert!(forum::read_roster(&repo).get("alice").is_some());
        assert_eq!(read_enrollments(&repo).by_origin.len(), 1);
    }

    #[test]
    fn only_a_human_enrolls_or_sets_policy() {
        let (_d, repo) = temp_repo();
        let w = Author::agent("alice", "env/a/1", Role::Worker, None).unwrap();
        assert!(put_enrollment(&repo, &w, record("m", "github.com/user/1", &forum::now_ts()))
            .is_err());
        assert!(set_vote_rule(&repo, &w, VoteRule::Principal).is_err());
    }

    #[test]
    fn an_unsigned_or_malformed_enrollment_is_refused_at_the_door() {
        let (_d, repo) = temp_repo();
        let mut e = record("m", "github.com/user/1", &forum::now_ts());
        e.signature = String::new();
        assert!(put_enrollment(&repo, &human(), e).is_err());
        let mut e = record("m", "bad principal with spaces", &forum::now_ts());
        e.principal = "has spaces".into();
        assert!(put_enrollment(&repo, &human(), e).is_err());
    }

    /// The merge keeps an origin's earliest binding, so a local rebind to a
    /// different account would only survive until the next sync reverted it.
    /// It is refused with an explanation instead; rotation is not.
    #[test]
    fn rebinding_an_origin_to_another_account_is_refused_rotation_is_not() {
        let (_d, repo) = temp_repo();
        put_enrollment(&repo, &human(), record("m", "github.com/user/1", &forum::now_ts()))
            .unwrap();
        let err = put_enrollment(&repo, &human(), record("m", "github.com/user/2", &forum::now_ts()))
            .unwrap_err();
        assert!(err.to_string().contains("first binding sticks"), "{err}");

        // Same account, new record: key rotation, allowed.
        let mut rotated = record("m", "github.com/user/1", &forum::now_ts());
        rotated.ssh_pubkey = "ssh-ed25519 AAAAROTATED".into();
        put_enrollment(&repo, &human(), rotated).unwrap();
        assert_eq!(
            read_enrollments(&repo).by_origin["m"].ssh_pubkey,
            "ssh-ed25519 AAAAROTATED"
        );
    }

    /// The canonical payload sorts its keys explicitly, so the signature does
    /// not depend on which map type `serde_json` was built with.
    #[test]
    fn the_signing_payload_is_key_sorted_and_signature_free() {
        let e = record("m", "github.com/user/1", "2026-01-01T00:00:00.000000Z");
        let payload = signing_payload(&e).unwrap();
        let keys: Vec<&str> = payload
            .split('"')
            .skip(1)
            .step_by(4)
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "keys must serialize in sorted order: {payload}");
        assert!(!payload.contains("signature"));
    }

    #[test]
    fn vote_context_reflects_the_policy_in_force() {
        let (_d, repo) = temp_repo();
        put_enrollment(&repo, &human(), record("machine-a", "github.com/user/7", &forum::now_ts()))
            .unwrap();

        let ctx = vote_context(&repo);
        assert_eq!(ctx.rule, VoteRule::Origin);
        assert!(ctx.principals.is_empty(), "origin counting needs no bindings");

        set_vote_rule(&repo, &human(), VoteRule::Principal).unwrap();
        let ctx = vote_context(&repo);
        assert_eq!(ctx.rule, VoteRule::Principal);
        assert_eq!(ctx.principals["machine-a"], "github.com/user/7");
    }

    /// The whole point, end to end: under the principal rule, only enrolled
    /// origins count, and every box on one enrolled machine is one vote.
    #[test]
    fn principal_rule_counts_enrolled_origins_once_and_others_not_at_all() {
        let (_d, repo) = temp_repo();
        let h = forum::create_thread(&repo, &human(), "t", None, None, None).unwrap();
        let target = forum::append_post(
            &repo,
            &human(),
            &h.id,
            NewPost {
                kind: "PROPOSAL".into(),
                body: "do it".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let vote = |name: &str, box_id: &str, origin: &str| {
            let author = Author::agent(name, box_id, Role::Worker, None)
                .unwrap()
                .from_host(origin);
            forum::append_post(
                &repo,
                &author,
                &h.id,
                NewPost {
                    kind: forum::KIND_UPVOTE.into(),
                    body: "+1".into(),
                    reply_to: Some(target.id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        };
        // Three worktrees on the enrolled machine, one on a stranger.
        vote("w1", "env/a/1", "machine-a");
        vote("w2", "env/a/2", "machine-a");
        vote("w3", "env/a/3", "machine-a");
        vote("mystery", "env/x/1", "machine-x");

        put_enrollment(&repo, &human(), record("machine-a", "github.com/user/7", &forum::now_ts()))
            .unwrap();
        set_vote_rule(&repo, &human(), VoteRule::Principal).unwrap();

        let t = forum::read_thread(&repo, &h.id).unwrap();
        assert_eq!(
            t.score_of_with(&target.id, &vote_context(&repo)),
            1,
            "three worktrees are one account, and the unenrolled machine counts for nothing"
        );
        assert_eq!(t.score_of(&target.id), 2, "the origin rule sees two machines");
    }
}
