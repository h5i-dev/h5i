//! The authority ceiling: what a box must be *under* to join a thread.
//!
//! A thread declares a ceiling profile at creation, and a box may attach only
//! if the confinement it actually runs under is a **subset** of that ceiling:
//!
//! > Agents can share information, never permissions.
//!
//! The forum shares the information. This module withholds the permissions, by
//! making the ceiling a property of the *room* rather than of the conversation.
//!
//! ## Why a static check and not a dynamic intersection
//!
//! Computing each participant's authority live, as the intersection of everyone
//! currently in the thread, is safe and unusable: a read-only observer joining
//! would strip write access from the agent doing the work, every join and leave
//! would move everyone's authority, and a long task would not be reproducible
//! from one hour to the next.
//!
//! So a human fixes the ceiling at thread creation and each box is checked once,
//! at attach. A box's resolved policy cannot change while it exists, so one that
//! passed at attach stays under the ceiling for its whole life, and joins and
//! leaves move nobody's authority.
//!
//! ## Refused, never downgraded
//!
//! A box exceeding the ceiling is refused rather than quietly re-confined.
//! Silently weakening its grants would leave the operator believing it has
//! authority it lost, and would make "attached successfully" stop meaning "runs
//! the way you configured it". Same choice `placement` makes for a capability a
//! runner lacks.
//!
//! ## What is compared
//!
//! The box side is its stored, digest-verified `policy.resolved.toml`, not a
//! profile resolved fresh from the worktree: what matters is the confinement
//! actually pinned, and `.h5i/env.toml` is a file an agent could have edited
//! since. The ceiling side is a profile resolved from the repository at check
//! time, which is the human's declaration.
//!
//! Every dimension below can widen what a box can *reach*. Tool allowlists and
//! resource caps are deliberately not compared: they bound what a box does with
//! authority it already has, and folding them in would fail ceilings for
//! reasons unrelated to reach.

use h5i_sandbox::sandbox_policy::{NetMode, Profile};

/// One way a box exceeds a ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The capability dimension, e.g. `net.egress`.
    pub dimension: String,
    /// What the box has that the ceiling does not permit.
    pub detail: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.dimension, self.detail)
    }
}

/// Compare a box's enforced profile against a ceiling profile.
///
/// The box side comes from `env::load_policy`, which verifies the stored
/// `policy.resolved.toml` against the digest pinned in the manifest before
/// handing it back — so what is compared here is confinement that was actually
/// enforced, not a profile re-resolved from a worktree an agent can edit.
///
/// Returns every violation rather than the first, so an operator fixing a
/// profile sees the whole gap in one pass instead of discovering it one attach
/// at a time. An empty result means the box is under the ceiling.
pub fn check(b: &Profile, ceiling: &Profile) -> Vec<Violation> {
    let mut out = Vec::new();

    // ── network ────────────────────────────────────────────────────────────
    if matches!(b.net_mode, NetMode::Host) && matches!(ceiling.net_mode, NetMode::Deny) {
        out.push(Violation {
            dimension: "net.mode".into(),
            detail: "box has network access, the ceiling denies it".into(),
        });
    }
    for host in &b.net_egress {
        if !ceiling.net_egress.iter().any(|c| c == host) {
            out.push(Violation {
                dimension: "net.egress".into(),
                detail: format!("box may reach {host}, the ceiling does not list it"),
            });
        }
    }

    // ── credentials ────────────────────────────────────────────────────────
    //
    // The sharpest dimension. A credential inside a box is authority the forum
    // can never take back, because the box can use it without asking h5i.
    for grant in &b.secret_grants {
        if !ceiling.secrets.iter().any(|c| c == &grant.name) {
            out.push(Violation {
                dimension: "secrets".into(),
                detail: format!(
                    "box is granted the secret {}, the ceiling does not list it",
                    grant.name
                ),
            });
        }
    }
    for grant in &b.auth {
        if !ceiling.auth.iter().any(|c| c.host == grant.host) {
            out.push(Violation {
                dimension: "auth".into(),
                detail: format!(
                    "box has authenticated egress to {}, the ceiling does not list it",
                    grant.host
                ),
            });
        }
    }

    // ── filesystem ─────────────────────────────────────────────────────────
    //
    // A write grant is checked against the ceiling's write grants only; a read
    // grant is satisfied by either, since anything writable is also readable.
    for path in &b.fs_write {
        if !covered_by(path, &ceiling.fs_write) {
            out.push(Violation {
                dimension: "fs.write".into(),
                detail: format!("box may write {path}, the ceiling does not cover it"),
            });
        }
    }
    let readable: Vec<String> = ceiling
        .fs_read
        .iter()
        .chain(ceiling.fs_write.iter())
        .cloned()
        .collect();
    for path in &b.fs_read {
        if !covered_by(path, &readable) {
            out.push(Violation {
                dimension: "fs.read".into(),
                detail: format!("box may read {path}, the ceiling does not cover it"),
            });
        }
    }

    // ── local reach ────────────────────────────────────────────────────────
    if b.unix_sockets && !ceiling.unix_sockets {
        out.push(Violation {
            dimension: "net.unix".into(),
            detail: "box may open AF_UNIX sockets, the ceiling does not allow it".into(),
        });
    }
    for port in &b.loopback_ports {
        if !ceiling.loopback_ports.contains(port) {
            out.push(Violation {
                dimension: "net.loopback".into(),
                detail: format!("box may reach loopback port {port}, the ceiling does not list it"),
            });
        }
    }

    // ── host-side execution ────────────────────────────────────────────────
    //
    // A command extractor runs on the host, outside the sandbox, to resolve a
    // secret. It is the one profile switch that executes code the box's
    // confinement does not apply to at all.
    if b.allow_command_extractors && !ceiling.allow_command_extractors {
        out.push(Violation {
            dimension: "secrets.command".into(),
            detail: "box may run host-side secret extractors, the ceiling does not allow it".into(),
        });
    }

    out
}

/// Digest of a ceiling profile, for pinning in a thread header.
///
/// The same canonical TOML serialization the box policy digest uses, so the two
/// are comparable and a ceiling that was edited after a thread was opened is
/// visible as a changed digest rather than as silence. `Profile`'s field order
/// is its canonical order by construction, which is what makes this stable.
pub fn profile_digest(p: &Profile) -> String {
    let canonical = toml::to_string(p).unwrap_or_default();
    crate::refstore::sha256_hex(canonical.as_bytes())
}

/// Is `path` equal to, or inside, one of `grants`?
///
/// Compared as normalised path strings rather than canonicalised paths: a
/// profile grant may name a directory that does not exist yet on this host, and
/// canonicalising would turn "not created yet" into "not covered".
///
/// A `..` component defeats that comparison — `/work/../.ssh` string-matches
/// under `/work` while reaching somewhere else entirely, and unlike persona
/// sources and private paths, an fs grant is not refused for carrying one at
/// profile load. So a path with a parent component is never covered and never
/// covers: the check fails closed, and the violation names the path so the
/// operator can write the plain form.
fn covered_by(path: &str, grants: &[String]) -> bool {
    if has_parent_component(path) {
        return false;
    }
    let p = normalise(path);
    grants.iter().any(|g| {
        if has_parent_component(g) {
            return false;
        }
        let g = normalise(g);
        p == g || p.starts_with(&format!("{g}/"))
    })
}

fn has_parent_component(p: &str) -> bool {
    p.split('/').any(|c| c == "..")
}

fn normalise(p: &str) -> String {
    let t = p.trim().trim_end_matches('/');
    if t.is_empty() { "/".to_string() } else { t.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use h5i_sandbox::sandbox_policy::{AuthGrant, IsolationClaim, SecretGrant};

    /// A confined built-in profile: network denied, nothing granted beyond the
    /// defaults every process-tier box gets.
    fn profile() -> Profile {
        Profile::builtin("test", IsolationClaim::Process)
    }

    fn secret(name: &str) -> SecretGrant {
        SecretGrant {
            name: name.to_string(),
            source: None,
            inject: None,
            ttl: None,
        }
    }

    fn auth(host: &str) -> AuthGrant {
        AuthGrant {
            host: host.to_string(),
            credential_env: "GH_TOKEN".into(),
            base_url_var: "GH_HOST".into(),
            token_var: "GH_TOKEN".into(),
        }
    }

    #[test]
    fn an_identical_profile_is_under_its_own_ceiling() {
        let mut p = profile();
        p.fs_read.push("/usr/share".into());
        p.net_mode = NetMode::Host;
        p.net_egress.push("crates.io".into());
        assert!(check(&p, &p).is_empty());
    }

    #[test]
    fn a_box_with_network_under_a_deny_ceiling_is_refused() {
        let mut b = profile();
        b.net_mode = NetMode::Host;
        let v = check(&b, &profile());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].dimension, "net.mode");
    }

    #[test]
    fn egress_must_be_listed_by_the_ceiling() {
        let mut b = profile();
        b.net_mode = NetMode::Host;
        b.net_egress = vec!["crates.io".into(), "evil.example".into()];
        let mut c = profile();
        c.net_mode = NetMode::Host;
        c.net_egress = vec!["crates.io".into()];

        let v = check(&b, &c);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].dimension, "net.egress");
        assert!(v[0].detail.contains("evil.example"));
    }

    #[test]
    fn an_empty_ceiling_egress_list_permits_nothing() {
        let mut b = profile();
        b.net_egress = vec!["crates.io".into()];
        let v = check(&b, &profile());
        assert_eq!(v.len(), 1, "a ceiling that lists no host allows no host");
    }

    #[test]
    fn a_credential_the_ceiling_does_not_name_is_refused() {
        let mut b = profile();
        b.secret_grants.push(secret("GITHUB_TOKEN"));
        let v = check(&b, &profile());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].dimension, "secrets");
        assert!(v[0].detail.contains("GITHUB_TOKEN"));
    }

    #[test]
    fn authenticated_egress_must_be_named_by_the_ceiling() {
        let mut b = profile();
        b.auth.push(auth("api.github.com"));
        let v = check(&b, &profile());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].dimension, "auth");

        // and passes once the ceiling names the same host
        let mut c = profile();
        c.auth.push(auth("api.github.com"));
        assert!(check(&b, &c).is_empty());
    }

    #[test]
    fn a_write_grant_outside_the_ceiling_is_refused() {
        let mut b = profile();
        b.fs_write.push("/home/me/.ssh".into());
        let mut c = profile();
        c.fs_write.push("/home/me/work".into());
        let v = check(&b, &c);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].dimension, "fs.write");
        assert!(v[0].detail.contains(".ssh"));
    }

    #[test]
    fn a_grant_inside_a_ceiling_directory_is_covered() {
        let mut b = profile();
        b.fs_write.push("/home/me/work/project".into());
        b.fs_read.push("/home/me/work".into());
        let mut c = profile();
        c.fs_write.push("/home/me/work/".into()); // trailing slash must not matter
        c.fs_read = b.fs_read.clone();
        assert!(check(&b, &c).is_empty());
    }

    #[test]
    fn a_ceiling_write_grant_also_satisfies_a_read_grant() {
        let mut b = profile();
        b.fs_read = vec!["/srv/data".into()];
        b.fs_write = Vec::new();
        let mut c = profile();
        c.fs_read = Vec::new();
        c.fs_write = vec!["/srv/data".into()];
        assert!(check(&b, &c).is_empty(), "anything writable is readable");
    }

    /// `/work/../.ssh` string-matches under `/work` while reaching `~/.ssh`.
    /// A grant with a parent component must fail the check, in both roles.
    #[test]
    fn a_parent_component_never_covers_and_is_never_covered() {
        let mut b = profile();
        b.fs_write = vec!["/home/me/work/../.ssh".into()];
        let mut c = profile();
        c.fs_write = vec!["/home/me/work".into()];
        let v = check(&b, &c);
        assert_eq!(v.len(), 1, "traversal must not hide under the ceiling: {v:?}");
        assert_eq!(v[0].dimension, "fs.write");

        // And a ceiling written with `..` covers nothing, rather than covering
        // whatever the string happens to prefix.
        let mut b = profile();
        b.fs_write = vec!["/home/me/work/../.ssh/keys".into()];
        let mut c = profile();
        c.fs_write = vec!["/home/me/work/../.ssh".into()];
        assert_eq!(check(&b, &c).len(), 1);
    }

    #[test]
    fn a_prefix_that_is_not_a_path_component_does_not_cover() {
        let mut b = profile();
        b.fs_read = vec!["/home/me/work-secrets".into()];
        b.fs_write = Vec::new();
        let mut c = profile();
        c.fs_read = vec!["/home/me/work".into()];
        c.fs_write = Vec::new();
        assert_eq!(
            check(&b, &c).len(),
            1,
            "/home/me/work must not cover /home/me/work-secrets"
        );
    }

    #[test]
    fn unix_sockets_and_host_extractors_need_the_ceilings_permission() {
        let mut b = profile();
        b.unix_sockets = true;
        b.allow_command_extractors = true;
        let mut c = profile();
        c.unix_sockets = false;
        c.allow_command_extractors = false;
        let v = check(&b, &c);
        let dims: Vec<&str> = v.iter().map(|x| x.dimension.as_str()).collect();
        assert!(dims.contains(&"net.unix"));
        assert!(dims.contains(&"secrets.command"));
    }

    #[test]
    fn every_violation_is_reported_not_just_the_first() {
        let mut b = profile();
        b.net_mode = NetMode::Host;
        b.unix_sockets = true;
        b.fs_write.push("/etc".into());
        let mut c = profile();
        c.unix_sockets = false;
        let v = check(&b, &c);
        assert_eq!(v.len(), 3, "got: {v:?}");
    }
}
