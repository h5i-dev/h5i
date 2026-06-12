//! Content-addressed object store for large raw outputs (token reduction).
//!
//! h5i's "associative" sidecar for *bulk* data. The problem it solves: an agent
//! that pipes a 4 MB `pytest` log or a giant JSON payload into its context burns
//! its window on noise. This module splits such an output into two halves,
//! borrowing the git-annex / git-lfs split between a *pointer* and the *blob*:
//!
//!   - **Raw blob** — the full bytes, content-addressed by sha256 and written to
//!     a local store under `.git/.h5i/objects/ab/cd/<sha256>`. This is the
//!     git-lfs-style "available locally" half. It is *not* pushed by a plain
//!     `git push`; it stays local until a remote backend is configured (future).
//!     Stored uncompressed today (`codec: "none"`); the layout reserves room for
//!     a `.zst` codec later without a new dependency now.
//!
//!   - **Manifest** — a small, durable JSON record carrying the *full* digest
//!     plus metadata (command, exit code, sizes, the filtered summary). Manifests
//!     are appended to the git ref `refs/h5i/objects` so they travel with
//!     `h5i share push`/`pull`. The summary is what an agent reads; the digest is
//!     how it rehydrates the raw on demand (`h5i recall object <id>`).
//!
//! Lifetime: manifests are immutable and kept forever (cheap, greppable history).
//! Only *local raw blobs* expire — [`gc`] evicts blobs that are unreferenced, or
//! (with a TTL) referenced-but-stale, unless pinned. Eviction never rewrites a
//! summary; a rehydrate of an evicted blob degrades gracefully with a clear
//! "absent" message.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use git2::{Repository, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::H5iError;
use crate::token_filter::{self, FilterConfig, OutputKind};

/// Git ref holding the append-only manifest log (one JSON object per line).
pub const OBJECTS_REF: &str = "refs/h5i/objects";
/// Top-level file inside the ref's tree holding the manifest log.
pub const MANIFESTS_FILE: &str = "manifests.jsonl";
/// Git ref holding shareable raw blobs (the optional [`GitRefStore`] backend):
/// a flat tree keyed by the full sha256 hex → a git blob of the raw bytes.
/// Content-addressed, so merging two sides is a set union. Pushed/pulled
/// on demand (raw output can be large) — see `h5i objects push`/`pull`.
pub const OBJECTS_DATA_REF: &str = "refs/h5i/objects-data";
/// The local content-addressed store directory name, under the h5i sidecar root.
pub const OBJECTS_DIR: &str = "objects";
/// File (under the store dir) listing pinned digests, one per line.
pub const PINS_FILE: &str = "pins";

/// Default capture threshold (bytes): output below this passes through unstored,
/// so wrapping a command is a no-op when there's nothing worth reducing. Shared
/// by `h5i capture run --min-bytes` and the `h5i_capture_run` MCP tool.
pub const DEFAULT_CAPTURE_MIN_BYTES: u64 = 2048;

/// Hard cap on a manifest's `summary` field (bytes). The filter already budgets
/// lines/tokens; this is a backstop so a pathological summary can never bloat
/// `refs/h5i/objects` (which is shared via `h5i push`). The full output is always
/// recoverable from the raw blob regardless.
pub const MAX_SUMMARY_BYTES: usize = 16 * 1024;
/// Hard cap on the number of `highlights` entries kept in a manifest.
pub const MAX_HIGHLIGHTS: usize = 20;
/// Hard cap on the length (bytes) of each `highlights` entry.
pub const MAX_HIGHLIGHT_BYTES: usize = 500;

const SUMMARY_TRUNC_MARK: &str = "\n… [summary truncated] …";

/// Truncate `s` to at most `max` bytes on a UTF-8 boundary, appending a marker
/// when truncated. Returns `(text, was_truncated)`.
fn clamp_text(s: String, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let budget = max.saturating_sub(SUMMARY_TRUNC_MARK.len());
    let mut end = budget.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push_str(SUMMARY_TRUNC_MARK);
    (out, true)
}

/// Cap the highlight list to [`MAX_HIGHLIGHTS`] entries and each entry to
/// [`MAX_HIGHLIGHT_BYTES`] bytes (UTF-8 safe).
fn clamp_highlights(mut hs: Vec<String>) -> Vec<String> {
    const ELLIPSIS: &str = "…"; // 3 bytes
    hs.truncate(MAX_HIGHLIGHTS);
    for h in hs.iter_mut() {
        if h.len() > MAX_HIGHLIGHT_BYTES {
            // Reserve room for the marker so the final string is <= the cap.
            let mut end = MAX_HIGHLIGHT_BYTES - ELLIPSIS.len();
            while end > 0 && !h.is_char_boundary(end) {
                end -= 1;
            }
            h.truncate(end);
            h.push_str(ELLIPSIS);
        }
    }
    hs
}

/// A git-tracked pointer to one stored raw output, plus its reduced summary.
///
/// This is the *only* thing that travels with the repo by default. It must carry
/// everything needed to (a) display a useful summary and (b) locate/verify the
/// raw bytes — hence the full digest, never a truncated one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Short, stable handle for the CLI (`h5i recall object <id>`): the first 16
    /// hex chars of the digest. Long enough to be unambiguous in practice.
    pub id: String,
    /// Logical kind of the payload: "tool-output", "log", "test", "json", …
    pub kind: String,
    /// The command that produced it, if captured via `h5i capture run`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    /// Working directory the command ran in (relative to the repo when possible).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Process exit code, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The HEAD tree the capture was taken against, for provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_tree: Option<String>,
    /// The git branch checked out when the capture was taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Files this capture is *about*: explicit `--file` args plus paths
    /// mentioned in the output (e.g. `src/x.rs:10` in an error). Repo-relative.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// The working-tree diff at capture time (changed/untracked files) — the
    /// "what I was working on" context. Repo-relative.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diff_files: Vec<String>,
    /// RFC3339 capture time (UTC, microsecond, lexically sortable).
    pub timestamp: String,
    /// Full content address of the raw blob, e.g. `sha256:<64 hex>`.
    pub raw_oid: String,
    /// Raw payload size in bytes.
    pub raw_size: u64,
    /// Raw payload line count.
    pub raw_lines: usize,
    /// Version of the filter algorithm that produced `summary`.
    pub filter_version: u32,
    /// The reduced text an agent reads instead of the raw output.
    pub summary: String,
    /// Highest-signal lines extracted from the raw (errors, hunk headers, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub highlights: Vec<String>,
    /// Which backend holds the raw bytes. Only "local" today.
    pub store: String,
    /// Compression codec of the stored blob. Only "none" today.
    pub codec: String,
    /// Best-effort token counts, for showing how much was saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_tokens: Option<usize>,
    /// Normalized structured result (the AI-friendly schema). Present for command
    /// captures; serde-skipped when absent so old manifests stay valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<crate::structured::ToolResult>,
    /// The h5i environment this capture is evidence for (`h5i env run`), e.g.
    /// `env/claude/fix-auth`. Absent for ordinary captures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_id: Option<String>,
    /// sha256 of the resolved policy (`policy.resolved.toml`) in force when an
    /// env capture was taken — what was *actually* enforced, not requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_digest: Option<String>,
    /// Trust/source lane for env evidence, e.g. "host-env-run",
    /// "tee-shim", or "inbox-capture".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_source: Option<String>,
    /// Summary + pointer for the env's egress decisions (supervisor tier;
    /// never an unbounded inline log). Absent until that phase ships.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<EgressSummary>,
    /// What was scrubbed from this capture (secret names, never values).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redactions: Vec<String>,
}

/// Counts + a bounded per-host breakdown of an env's network egress decisions —
/// the manifest holds only this summary (token-reduction principle, design §8).
/// Populated by the `isolation=container` allowlist proxy (the only tier that
/// observes egress today); `None` everywhere else.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressSummary {
    /// Requests the proxy permitted (on-allowlist host:port).
    pub allowed: u64,
    /// Requests the proxy refused with `403` (off-allowlist — a network
    /// boundary trip). The single highest-fidelity egress signal.
    pub denied: u64,
    /// Per `host:port` verdict counts, deduped and bounded ([`MAX_EGRESS_HOSTS`])
    /// so a probing loop can never bloat the shared `refs/h5i/objects` ref. This
    /// is what the dashboard reads directly — no raw rehydration needed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<EgressHost>,
    /// True when [`MAX_EGRESS_HOSTS`] was exceeded and the tail was dropped, so a
    /// reader never mistakes a clamped list for the whole picture.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hosts_truncated: bool,
    /// Object id (in this store) of the full `egress.jsonl`, when captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
}

/// One destination an env's traffic was steered at, with allow/deny tallies.
/// A host with `denied > 0` is a refused boundary attempt; the dashboard's NET
/// lane surfaces these as "Boundary blocked".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressHost {
    pub host: String,
    pub port: u16,
    pub allowed: u64,
    pub denied: u64,
}

/// Cap on distinct `host:port` entries kept in an [`EgressSummary`] — keeps the
/// shared manifest bounded regardless of how many hosts a run probes.
pub const MAX_EGRESS_HOSTS: usize = 64;

impl Manifest {
    /// The bare 64-char hex digest (no `sha256:` prefix).
    pub fn hex(&self) -> &str {
        self.raw_oid.strip_prefix("sha256:").unwrap_or(&self.raw_oid)
    }

    /// Token count of the **default agent-facing output** for this capture: the
    /// compact render of the structured result (what `capture run` emits by
    /// default), or the legacy text-summary token count for older /
    /// non-command captures that have no structured result. This is the honest
    /// denominator for "tokens kept out of context" — it matches what an agent
    /// actually sees, not the git-tracked `summary` field's size.
    pub fn agent_facing_tokens(&self) -> Option<usize> {
        match &self.structured {
            Some(s) => crate::token_filter::count_tokens(&crate::structured::render_compact(s)),
            None => self.summary_tokens,
        }
    }
}

/// A storage backend for raw blobs. Trait-shaped per the git-annex design so a
/// remote (S3 / HTTP / LFS-like) backend can be added later; only [`LocalStore`]
/// exists today.
pub trait Backend {
    fn name(&self) -> &str;
    fn has(&self, hex: &str) -> bool;
    fn put(&self, hex: &str, bytes: &[u8]) -> Result<(), H5iError>;
    fn get(&self, hex: &str) -> Result<Option<Vec<u8>>, H5iError>;
    fn remove(&self, hex: &str) -> Result<(), H5iError>;
}

/// The local filesystem backend: `<h5i_root>/objects/ab/cd/<sha256>`.
pub struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    pub fn new(h5i_root: &Path) -> LocalStore {
        LocalStore {
            root: h5i_root.join(OBJECTS_DIR),
        }
    }

    /// Sharded path for a digest: `objects/<a><b>/<c><d>/<full hex>`.
    pub fn blob_path(&self, hex: &str) -> PathBuf {
        // Defensive: callers validate, but never index out of range.
        if hex.len() < 4 {
            return self.root.join(hex);
        }
        self.root.join(&hex[0..2]).join(&hex[2..4]).join(hex)
    }

    /// Iterate every stored blob digest, with its on-disk size and mtime.
    pub fn iter_blobs(&self) -> Result<Vec<(String, u64, SystemTime)>, H5iError> {
        let mut out = Vec::new();
        if !self.root.exists() {
            return Ok(out);
        }
        for l1 in read_dir(&self.root)? {
            let l1 = l1.path();
            if !l1.is_dir() {
                continue;
            }
            for l2 in read_dir(&l1)? {
                let l2 = l2.path();
                if !l2.is_dir() {
                    continue;
                }
                for blob in read_dir(&l2)? {
                    let p = blob.path();
                    if !p.is_file() {
                        continue;
                    }
                    let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    if !is_hex64(name) {
                        continue;
                    }
                    let meta = std::fs::metadata(&p).map_err(|e| H5iError::with_path(e, &p))?;
                    let mtime = meta.modified().unwrap_or_else(|_| SystemTime::now());
                    out.push((name.to_string(), meta.len(), mtime));
                }
            }
        }
        Ok(out)
    }
}

impl Backend for LocalStore {
    fn name(&self) -> &str {
        "local"
    }

    fn has(&self, hex: &str) -> bool {
        self.blob_path(hex).is_file()
    }

    fn put(&self, hex: &str, bytes: &[u8]) -> Result<(), H5iError> {
        let path = self.blob_path(hex);
        if path.is_file() {
            return Ok(()); // content-addressed: identical content already stored
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
        }
        // Write to a temp file then rename for atomicity.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes).map_err(|e| H5iError::with_path(e, &tmp))?;
        std::fs::rename(&tmp, &path).map_err(|e| H5iError::with_path(e, &path))?;
        Ok(())
    }

    fn get(&self, hex: &str) -> Result<Option<Vec<u8>>, H5iError> {
        let path = self.blob_path(hex);
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(
            std::fs::read(&path).map_err(|e| H5iError::with_path(e, &path))?,
        ))
    }

    fn remove(&self, hex: &str) -> Result<(), H5iError> {
        let path = self.blob_path(hex);
        if path.is_file() {
            std::fs::remove_file(&path).map_err(|e| H5iError::with_path(e, &path))?;
        }
        Ok(())
    }
}

/// Compute the sha256 of `bytes` as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// What a freshly captured output produced: the manifest plus the raw stats.
pub struct CaptureOutcome {
    pub manifest: Manifest,
    /// True if this digest was already present locally (deduped).
    pub deduped: bool,
}

/// Options for [`capture`].
pub struct CaptureOptions {
    pub kind: OutputKind,
    pub cmd: Option<String>,
    pub cwd: Option<String>,
    pub exit_code: Option<i32>,
    pub git_tree: Option<String>,
    /// Files the caller explicitly associates with this capture (`--file`).
    /// The branch and the working-tree diff are detected automatically.
    pub files: Vec<String>,
    /// The command argv, used to pick a structured parser (pytest/cargo/…).
    /// Empty for non-command captures (`objects put`).
    pub cmd_argv: Vec<String>,
    pub filter: FilterConfig,
    /// Set by `h5i env run`: the env this capture is evidence for, and the
    /// digest of the policy enforced while producing it.
    pub env_id: Option<String>,
    pub policy_digest: Option<String>,
    pub evidence_source: Option<String>,
    /// Network egress verdicts observed while producing this capture (the
    /// `isolation=container` allowlist proxy populates it; `None` otherwise).
    pub egress: Option<EgressSummary>,
    /// Scrub secret-like spans from the payload BEFORE it is hashed and stored,
    /// so the content-addressed blob itself never carries a credential. Used by
    /// `h5i env run` (design §7). The detected rule ids land in
    /// `Manifest::redactions`.
    pub redact: bool,
}

/// Store `raw` in the local backend, build + persist a manifest, and return it.
///
/// This is the heart of the feature: it runs the deterministic filter, writes
/// the blob to the content-addressed store, and appends the manifest to
/// `refs/h5i/objects`. The caller then surfaces only `manifest.summary`.
pub fn capture(
    repo: &Repository,
    h5i_root: &Path,
    raw: &[u8],
    opts: CaptureOptions,
) -> Result<CaptureOutcome, H5iError> {
    let store = LocalStore::new(h5i_root);

    // Secret redaction (opt-in; e.g. `h5i env run`). Scrub BEFORE hashing and
    // storing so the content-addressed blob — which travels via `h5i objects
    // push` — can never carry a credential. Binary payloads are left untouched
    // (the scanner is line/text oriented); the redaction marker is recorded by
    // rule id, never the value.
    let mut redactions: Vec<String> = Vec::new();
    let redacted_holder;
    let raw: &[u8] = if opts.redact {
        match std::str::from_utf8(raw) {
            Ok(text) => {
                let findings = crate::secrets::scan_text(Path::new("<capture>"), text);
                if findings.is_empty() {
                    raw
                } else {
                    let mut ids: Vec<String> =
                        findings.iter().map(|f| f.rule_id.to_string()).collect();
                    ids.sort();
                    ids.dedup();
                    redactions = ids;
                    redacted_holder = crate::secrets::redact_text(text).into_bytes();
                    &redacted_holder[..]
                }
            }
            Err(_) => raw,
        }
    } else {
        raw
    };

    let hex = sha256_hex(raw);
    let deduped = store.has(&hex);
    store.put(&hex, raw)?;

    // Filter on the textual view of the payload. Non-UTF8 (e.g. screenshots)
    // still get stored; their "summary" notes that they're binary.
    let text = String::from_utf8_lossy(raw);
    let is_binary = raw.contains(&0);
    let filtered = if is_binary {
        token_filter::FilterResult {
            summary: format!("[binary payload · {} bytes]", raw.len()),
            kind: OutputKind::Generic,
            highlights: Vec::new(),
            raw_lines: 0,
            kept_lines: 0,
            raw_tokens: None,
            summary_tokens: None,
        }
    } else {
        token_filter::filter(&text, &opts.filter)
    };

    let kind = if opts.kind == OutputKind::Auto {
        // Prefer the classified kind, but tag command captures as "tool-output".
        if opts.cmd.is_some() {
            "tool-output".to_string()
        } else {
            filtered.kind.as_str().to_string()
        }
    } else {
        opts.kind.as_str().to_string()
    };

    // Associate the capture with the branch and the files it concerns: the
    // explicit `--file` set, paths mentioned in the output, and the working-tree
    // diff at capture time.
    let branch = current_branch(repo);
    let mut diff_files = working_diff_files(repo);
    dedup_sorted(&mut diff_files);
    let mut files = opts.files.clone();
    // Mine the summary + highlights for `path:line` references.
    let mut path_src = filtered.summary.clone();
    for h in &filtered.highlights {
        path_src.push('\n');
        path_src.push_str(h);
    }
    files.extend(extract_paths(&path_src));
    dedup_sorted(&mut files);

    // Backstop the git-tracked text fields. The filter already budgets lines and
    // tokens, but these manifests travel via `h5i push`, so a pathological
    // summary must never bloat the ref. (Path-mining above used the full text.)
    let raw_lines = filtered.raw_lines;
    let kept_lines = filtered.kept_lines;
    let (summary, summary_clamped) = clamp_text(filtered.summary, MAX_SUMMARY_BYTES);
    let highlights = clamp_highlights(filtered.highlights);
    // If we truncated the summary, the recorded token count must reflect it.
    let summary_tokens = if summary_clamped {
        token_filter::count_tokens(&summary)
    } else {
        filtered.summary_tokens
    };

    // Build the normalized structured result — but ONLY for command captures.
    // A non-command ingest (`objects put`) has no tool/exit semantics, so it gets
    // no structured record (keeps `recall --tool/--status` queries clean).
    let structured = if opts.cmd_argv.is_empty() {
        None
    } else {
        // A dedicated parser if one matches, else a generic envelope carrying the
        // reduced text as `body`. Never claims success it can't see.
        let mut s = if is_binary {
            let tool = opts.cmd_argv[0].rsplit('/').next().unwrap_or(&opts.cmd_argv[0]).to_string();
            let mut g = crate::structured::ToolResult::generic(&tool, opts.exit_code);
            g.body = Some(summary.clone());
            g
        } else {
            crate::structured::parse(&opts.cmd_argv, &text, opts.exit_code).unwrap_or_else(|| {
                let tool = opts.cmd_argv[0].rsplit('/').next().unwrap_or(&opts.cmd_argv[0]).to_string();
                let mut g = crate::structured::ToolResult::generic(&tool, opts.exit_code);
                g.body = Some(summary.clone());
                g
            })
        };
        s.raw_oid = Some(format!("sha256:{hex}"));
        // raw isn't fully represented if we dropped lines OR byte-clamped the summary.
        s.truncated.raw = raw_lines > kept_lines || summary_clamped;
        s.cap();
        Some(s)
    };

    // The command line can itself carry a credential (a secret passed as an
    // argument), so it is scrubbed too when redaction is on — otherwise the
    // payload would be clean but `cmd` would leak.
    let cmd = match (opts.redact, opts.cmd) {
        (true, Some(c)) => Some(crate::secrets::redact_text(&c)),
        (_, c) => c,
    };

    let manifest = Manifest {
        id: hex[..16].to_string(),
        kind,
        cmd,
        cwd: opts.cwd,
        exit_code: opts.exit_code,
        git_tree: opts.git_tree,
        branch,
        files,
        diff_files,
        timestamp: now_ts(),
        raw_oid: format!("sha256:{hex}"),
        raw_size: raw.len() as u64,
        raw_lines: filtered.raw_lines,
        filter_version: token_filter::FILTER_VERSION,
        summary,
        highlights,
        store: store.name().to_string(),
        codec: "none".to_string(),
        raw_tokens: filtered.raw_tokens,
        summary_tokens,
        structured,
        env_id: opts.env_id,
        policy_digest: opts.policy_digest,
        evidence_source: opts.evidence_source,
        egress: opts.egress,
        redactions,
    };

    append_manifest(repo, &manifest)?;
    Ok(CaptureOutcome { manifest, deduped })
}

/// Append `manifest` to `refs/h5i/objects` with compare-and-swap semantics,
/// mirroring the i5h message log: build a commit off the current tip, then move
/// the ref only if it hasn't moved under us. Retries on a lost race.
pub fn append_manifest(repo: &Repository, manifest: &Manifest) -> Result<(), H5iError> {
    const MAX_ATTEMPTS: usize = 64;
    let line = serde_json::to_string(manifest)?;
    let message = format!("h5i objects: {} ({})", manifest.id, manifest.kind);

    for _ in 0..MAX_ATTEMPTS {
        let tip = repo.refname_to_id(OBJECTS_REF).ok();
        let parent = match tip {
            Some(oid) => Some(repo.find_commit(oid)?),
            None => None,
        };
        let base_tree = parent.as_ref().and_then(|c| c.tree().ok());

        let mut log = read_blob_from_tree(repo, base_tree.as_ref(), MANIFESTS_FILE)
            .unwrap_or_default();
        if !log.is_empty() && !log.ends_with('\n') {
            log.push('\n');
        }
        log.push_str(&line);
        log.push('\n');

        let tree_oid = build_tree(repo, base_tree.as_ref(), &[(MANIFESTS_FILE, &log)])?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = signature(repo)?;
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let new_oid = repo.commit(None, &sig, &sig, &message, &tree, &parents)?;

        let cas_ok = match tip {
            None => repo.reference(OBJECTS_REF, new_oid, false, &message).is_ok(),
            Some(old) => repo
                .reference_matching(OBJECTS_REF, new_oid, true, old, &message)
                .is_ok(),
        };
        if cas_ok {
            return Ok(());
        }
    }
    Err(H5iError::Internal(format!(
        "h5i objects: manifest {} could not be appended after {MAX_ATTEMPTS} attempts",
        manifest.id
    )))
}

/// Reconcile two divergent `refs/h5i/objects` tips into one merge commit.
///
/// The manifest log is strictly append-only, so a divergence is just two
/// disjoint sets of appended manifests. We union them (deduping on the full
/// `(raw_oid, timestamp)` key so a manifest present on both sides appears once)
/// and re-sort by timestamp, then commit with both tips as parents (local
/// first, so the result stays a descendant of the local ref). Mirrors
/// [`crate::msg::union_merge_commits`] so `h5i pull` never drops a pointer.
pub fn union_merge_commits(
    repo: &Repository,
    local_oid: git2::Oid,
    incoming_oid: git2::Oid,
) -> Result<git2::Oid, H5iError> {
    let local_commit = repo.find_commit(local_oid)?;
    let incoming_commit = repo.find_commit(incoming_oid)?;

    let mut seen: HashSet<String> = HashSet::new();
    let mut merged: Vec<Manifest> = Vec::new();
    for oid in [local_oid, incoming_oid] {
        let raw = read_file_from_commit(repo, oid, MANIFESTS_FILE).unwrap_or_default();
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(m) = serde_json::from_str::<Manifest>(line) {
                let key = format!("{}|{}", m.raw_oid, m.timestamp);
                if seen.insert(key) {
                    merged.push(m);
                }
            }
        }
    }
    merged.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.id.cmp(&b.id)));

    let mut log = String::new();
    for m in &merged {
        log.push_str(&serde_json::to_string(m)?);
        log.push('\n');
    }

    let base_tree = local_commit.tree().ok();
    let tree_oid = build_tree(repo, base_tree.as_ref(), &[(MANIFESTS_FILE, &log)])?;
    let tree = repo.find_tree(tree_oid)?;
    let sig = signature(repo)?;
    let parents = [&local_commit, &incoming_commit];
    let oid = repo.commit(
        None,
        &sig,
        &sig,
        "h5i pull: union-merge of refs/h5i/objects",
        &tree,
        &parents,
    )?;
    Ok(oid)
}

fn read_file_from_commit(repo: &Repository, oid: git2::Oid, path: &str) -> Option<String> {
    let commit = repo.find_commit(oid).ok()?;
    let tree = commit.tree().ok()?;
    let entry = tree.get_path(Path::new(path)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    std::str::from_utf8(blob.content()).ok().map(str::to_owned)
}

/// Read every manifest from the ref, oldest-first.
pub fn read_manifests(repo: &Repository) -> Vec<Manifest> {
    let Some(raw) = read_ref_blob(repo, MANIFESTS_FILE) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Manifest>(l).ok())
        .collect()
}

/// Resolve a user-supplied handle to a manifest. Accepts the short id, the full
/// `sha256:<hex>` form, or any unambiguous hex prefix. Returns the most recent
/// match when an id repeats (same content captured twice).
pub fn find_manifest(repo: &Repository, handle: &str) -> Option<Manifest> {
    let needle = handle.strip_prefix("sha256:").unwrap_or(handle).to_lowercase();
    let manifests = read_manifests(repo);
    // Exact id, full hex, or prefix — search newest-first.
    manifests.into_iter().rev().find(|m| {
        m.id == needle || m.hex() == needle || (needle.len() >= 4 && m.hex().starts_with(&needle))
    })
}

/// Resolve a user handle to exactly one manifest, erroring on no match or an
/// **ambiguous** prefix (one that matches two or more *distinct* digests).
///
/// This is the strict resolver the CLI uses: unlike [`find_manifest`], which
/// quietly returns the newest match, it refuses to guess when a short prefix is
/// ambiguous. An exact id / full hex always wins outright (and the same content
/// captured twice shares one digest, so that is never "ambiguous").
pub fn resolve_manifest(repo: &Repository, handle: &str) -> Result<Manifest, H5iError> {
    let needle = handle.strip_prefix("sha256:").unwrap_or(handle).to_lowercase();
    let manifests = read_manifests(repo);

    // Exact id or full hex match wins (newest such, if duplicated).
    if let Some(m) = manifests
        .iter()
        .rev()
        .find(|m| m.id == needle || m.hex() == needle)
    {
        return Ok(m.clone());
    }

    if needle.len() < 4 {
        return Err(H5iError::Metadata(format!(
            "object handle '{handle}' is too short — use at least 4 hex chars, the full id, or sha256:<hex>"
        )));
    }

    // Prefix match: collect distinct digests that share the prefix.
    let mut distinct: Vec<Manifest> = Vec::new();
    for m in manifests.iter().rev() {
        if m.hex().starts_with(&needle) && !distinct.iter().any(|d| d.hex() == m.hex()) {
            distinct.push(m.clone());
        }
    }
    match distinct.len() {
        0 => Err(H5iError::Metadata(format!(
            "no object matches '{handle}' (try `h5i recall objects`)"
        ))),
        1 => Ok(distinct.into_iter().next().unwrap()),
        n => Err(H5iError::Metadata(format!(
            "object handle '{handle}' is ambiguous — it matches {n} distinct objects; use more characters or the full id"
        ))),
    }
}

/// Load the raw bytes for a manifest from the local backend. `Ok(None)` means
/// the manifest exists but the blob was evicted / never fetched ("absent").
pub fn load_raw(h5i_root: &Path, manifest: &Manifest) -> Result<Option<Vec<u8>>, H5iError> {
    LocalStore::new(h5i_root).get(manifest.hex())
}

/// Like [`load_raw`], but falls back to the [`GitRefStore`] when the blob is
/// absent locally (e.g. pulled metadata, evicted blob). A blob fetched from the
/// git-ref store is cached into the local store so subsequent reads are fast.
pub fn load_raw_with_remote(
    repo: &Repository,
    h5i_root: &Path,
    manifest: &Manifest,
) -> Result<Option<Vec<u8>>, H5iError> {
    let hex = manifest.hex();
    let local = LocalStore::new(h5i_root);
    if let Some(bytes) = local.get(hex)? {
        return Ok(Some(bytes));
    }
    // Already-fetched shared blobs (git-ref store).
    if let Some(bytes) = GitRefStore::new(repo).get(hex)? {
        let _ = local.put(hex, &bytes); // best-effort cache
        return Ok(Some(bytes));
    }
    // Lazy LFS fetch (network, best-effort): the whole point of LFS is to pull a
    // huge blob only when it's actually needed.
    if let Some(bytes) = try_lfs_fetch(repo, manifest) {
        let _ = local.put(hex, &bytes);
        return Ok(Some(bytes));
    }
    Ok(None)
}

/// Best-effort single-object fetch from the `origin` LFS server. Any failure
/// (non-HTTP remote, no LFS support, network/auth error) returns `None` so
/// `recall` falls through to its "absent" guidance rather than erroring.
fn try_lfs_fetch(repo: &Repository, manifest: &Manifest) -> Option<Vec<u8>> {
    let workdir = repo.workdir()?;
    let url = repo.find_remote("origin").ok()?.url()?.to_string();
    let client = crate::lfs::LfsClient::for_remote(workdir, &url)?;
    client.download_one(manifest.hex(), manifest.raw_size).ok().flatten()
}

// ── Pinning ──────────────────────────────────────────────────────────────────

fn pins_path(h5i_root: &Path) -> PathBuf {
    h5i_root.join(OBJECTS_DIR).join(PINS_FILE)
}

/// Read the set of pinned digests (full hex).
pub fn read_pins(h5i_root: &Path) -> HashSet<String> {
    let path = pins_path(h5i_root);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return HashSet::new();
    };
    raw.lines()
        .map(|l| l.trim().to_string())
        .filter(|l| is_hex64(l))
        .collect()
}

fn write_pins(h5i_root: &Path, pins: &HashSet<String>) -> Result<(), H5iError> {
    let path = pins_path(h5i_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
    }
    let mut sorted: Vec<&String> = pins.iter().collect();
    sorted.sort();
    let body = sorted.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
    std::fs::write(&path, body).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(())
}

/// Pin a blob so [`gc`] never evicts it. Returns the resolved hex.
pub fn pin(h5i_root: &Path, hex: &str) -> Result<(), H5iError> {
    let mut pins = read_pins(h5i_root);
    pins.insert(hex.to_string());
    write_pins(h5i_root, &pins)
}

/// Remove a pin.
pub fn unpin(h5i_root: &Path, hex: &str) -> Result<(), H5iError> {
    let mut pins = read_pins(h5i_root);
    pins.remove(hex);
    write_pins(h5i_root, &pins)
}

// ── Garbage collection ───────────────────────────────────────────────────────

/// One blob considered for eviction.
#[derive(Debug, Clone, Serialize)]
pub struct EvictedBlob {
    pub hex: String,
    pub size: u64,
    pub reason: String,
}

/// The result of a GC pass.
#[derive(Debug, Clone, Serialize)]
pub struct GcReport {
    pub dry_run: bool,
    pub evicted: Vec<EvictedBlob>,
    pub freed_bytes: u64,
    pub kept_referenced: usize,
    pub kept_pinned: usize,
    pub total_blobs: usize,
}

/// Evict local raw blobs to reclaim space, **never** touching manifests.
///
/// Policy:
///   - A blob with no manifest referencing it is an *orphan* → always evictable
///     (unless pinned). This is the safe default with no TTL.
///   - With `ttl`, a *referenced* blob older than the TTL is also evictable
///     (the git-annex "drop" — its summary stays, the raw becomes absent and can
///     be rehydrated later from a remote, once backends exist).
///   - Pinned blobs are never evicted.
pub fn gc(
    repo: &Repository,
    h5i_root: &Path,
    ttl: Option<Duration>,
    dry_run: bool,
) -> Result<GcReport, H5iError> {
    let store = LocalStore::new(h5i_root);
    let referenced: HashSet<String> = read_manifests(repo)
        .iter()
        .map(|m| m.hex().to_string())
        .collect();
    let pinned = read_pins(h5i_root);
    let now = SystemTime::now();

    let blobs = store.iter_blobs()?;
    let total_blobs = blobs.len();
    let mut evicted = Vec::new();
    let mut freed_bytes = 0u64;
    let mut kept_referenced = 0usize;
    let mut kept_pinned = 0usize;

    for (hex, size, mtime) in blobs {
        if pinned.contains(&hex) {
            kept_pinned += 1;
            continue;
        }
        let is_ref = referenced.contains(&hex);
        let age = now.duration_since(mtime).unwrap_or_default();

        let reason = if !is_ref {
            Some("orphan (no manifest)".to_string())
        } else if let Some(ttl) = ttl {
            if age >= ttl {
                Some(format!("referenced but older than TTL ({}s)", age.as_secs()))
            } else {
                None
            }
        } else {
            None
        };

        match reason {
            Some(reason) => {
                if !dry_run {
                    store.remove(&hex)?;
                }
                freed_bytes += size;
                evicted.push(EvictedBlob { hex, size, reason });
            }
            None => {
                if is_ref {
                    kept_referenced += 1;
                }
            }
        }
    }

    Ok(GcReport {
        dry_run,
        evicted,
        freed_bytes,
        kept_referenced,
        kept_pinned,
        total_blobs,
    })
}

/// One row of an fsck report: a manifest and whether its raw blob is present.
#[derive(Debug, Clone, Serialize)]
pub struct FsckRow {
    pub id: String,
    pub raw_oid: String,
    pub present: bool,
    pub pinned: bool,
}

/// Cross-check every manifest against the local store. Also reports orphan
/// blobs (present locally but referenced by no manifest).
#[derive(Debug, Clone, Serialize)]
pub struct FsckReport {
    pub rows: Vec<FsckRow>,
    pub absent: usize,
    pub orphans: Vec<String>,
}

/// Verify the integrity of the object store against the manifest log.
pub fn fsck(repo: &Repository, h5i_root: &Path) -> Result<FsckReport, H5iError> {
    let store = LocalStore::new(h5i_root);
    let pinned = read_pins(h5i_root);
    let manifests = read_manifests(repo);
    let referenced: HashSet<String> = manifests.iter().map(|m| m.hex().to_string()).collect();

    let mut rows = Vec::new();
    let mut absent = 0;
    for m in &manifests {
        let present = store.has(m.hex());
        if !present {
            absent += 1;
        }
        rows.push(FsckRow {
            id: m.id.clone(),
            raw_oid: m.raw_oid.clone(),
            present,
            pinned: pinned.contains(m.hex()),
        });
    }

    let orphans: Vec<String> = store
        .iter_blobs()?
        .into_iter()
        .map(|(hex, _, _)| hex)
        .filter(|hex| !referenced.contains(hex))
        .collect();

    Ok(FsckReport {
        rows,
        absent,
        orphans,
    })
}

// ── Duration parsing ─────────────────────────────────────────────────────────

/// Parse a human duration like `30d`, `12h`, `90m`, `45s`, `2w`. Bare numbers
/// are treated as seconds.
pub fn parse_duration(s: &str) -> Result<Duration, H5iError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(H5iError::Metadata("empty duration".into()));
    }
    let (num, unit) = s.split_at(
        s.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(s.len()),
    );
    let n: u64 = num
        .parse()
        .map_err(|_| H5iError::Metadata(format!("invalid duration: {s}")))?;
    let secs = match unit.trim() {
        "" | "s" | "sec" | "secs" => n,
        "m" | "min" | "mins" => n * 60,
        "h" | "hr" | "hrs" => n * 3600,
        "d" | "day" | "days" => n * 86_400,
        "w" | "wk" | "wks" => n * 604_800,
        other => {
            return Err(H5iError::Metadata(format!(
                "unknown duration unit '{other}' (use s/m/h/d/w)"
            )))
        }
    };
    Ok(Duration::from_secs(secs))
}

// ── git ref tree helpers (mirroring src/msg.rs) ──────────────────────────────

fn read_ref_blob(repo: &Repository, path: &str) -> Option<String> {
    let reference = repo.find_reference(OBJECTS_REF).ok()?;
    let commit = reference.peel_to_commit().ok()?;
    let tree = commit.tree().ok()?;
    let entry = tree.get_path(Path::new(path)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    std::str::from_utf8(blob.content()).ok().map(str::to_owned)
}

pub(crate) fn read_blob_from_tree(repo: &Repository, tree: Option<&git2::Tree>, path: &str) -> Option<String> {
    let entry = tree?.get_path(Path::new(path)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    std::str::from_utf8(blob.content()).ok().map(str::to_owned)
}

pub(crate) fn build_tree(
    repo: &Repository,
    base: Option<&git2::Tree>,
    files: &[(&str, &str)],
) -> Result<git2::Oid, H5iError> {
    let mut builder = repo.treebuilder(base)?;
    for (name, content) in files {
        let blob = repo.blob(content.as_bytes())?;
        builder.insert(name, blob, 0o100644)?;
    }
    Ok(builder.write()?)
}

pub(crate) fn signature(repo: &Repository) -> Result<Signature<'static>, H5iError> {
    repo.signature()
        .or_else(|_| Signature::now("h5i", "h5i@local"))
        .map_err(H5iError::Git)
}

// ── GitRefStore: shareable raw-blob backend (the git-ref store) ────────────────

/// A [`Backend`] that stores raw blobs in the [`OBJECTS_DATA_REF`] git ref as a
/// flat tree (`<full-hex>` → git blob). Reuses the repo's existing remote, auth,
/// and transfer (`git push`/`fetch`), so sharing raw output needs no extra
/// service and no new dependency. Because entries are content-addressed,
/// reconciling two sides is a plain set-union of tree entries.
pub struct GitRefStore<'a> {
    repo: &'a Repository,
}

impl<'a> GitRefStore<'a> {
    pub fn new(repo: &'a Repository) -> Self {
        GitRefStore { repo }
    }

    fn tip_tree(&self) -> Option<git2::Tree<'a>> {
        let reference = self.repo.find_reference(OBJECTS_DATA_REF).ok()?;
        reference.peel_to_commit().ok()?.tree().ok()
    }

    /// The blob oid stored under `hex` ONLY IF its content hashes to `hex`.
    /// A corrupt/tampered entry (name present, bytes wrong) returns `None` —
    /// this is the content-validated notion of "present" used by [`Self::has`].
    fn valid_blob_oid(&self, hex: &str) -> Option<git2::Oid> {
        let tree = self.tip_tree()?;
        let entry = tree.get_name(hex)?;
        let blob = self.repo.find_blob(entry.id()).ok()?;
        (sha256_hex(blob.content()) == hex).then(|| entry.id())
    }

    /// Insert/remove one entry and commit the new tree (CAS-retried).
    fn mutate(&self, hex: &str, blob: Option<git2::Oid>) -> Result<(), H5iError> {
        const MAX_ATTEMPTS: usize = 64;
        let msg = match blob {
            Some(_) => format!("h5i objects-data: + {hex}"),
            None => format!("h5i objects-data: - {hex}"),
        };
        for _ in 0..MAX_ATTEMPTS {
            let tip = self.repo.refname_to_id(OBJECTS_DATA_REF).ok();
            let parent = match tip {
                Some(oid) => Some(self.repo.find_commit(oid)?),
                None => None,
            };
            let base_tree = parent.as_ref().and_then(|c| c.tree().ok());
            let mut builder = self.repo.treebuilder(base_tree.as_ref())?;
            match blob {
                Some(oid) => builder.insert(hex, oid, 0o100644).map(|_| ())?,
                None => {
                    if builder.get(hex)?.is_some() {
                        builder.remove(hex)?;
                    }
                }
            }
            let tree = self.repo.find_tree(builder.write()?)?;
            let sig = signature(self.repo)?;
            let parents: Vec<&git2::Commit> = parent.iter().collect();
            let new_oid = self.repo.commit(None, &sig, &sig, &msg, &tree, &parents)?;
            let cas_ok = match tip {
                None => self.repo.reference(OBJECTS_DATA_REF, new_oid, false, &msg).is_ok(),
                Some(old) => self
                    .repo
                    .reference_matching(OBJECTS_DATA_REF, new_oid, true, old, &msg)
                    .is_ok(),
            };
            if cas_ok {
                return Ok(());
            }
        }
        Err(H5iError::Internal(format!(
            "h5i objects-data: could not update {hex} after {MAX_ATTEMPTS} attempts"
        )))
    }
}

impl Backend for GitRefStore<'_> {
    fn name(&self) -> &str {
        "git-ref"
    }

    fn has(&self, hex: &str) -> bool {
        // "Present" means VALID content exists — a corrupt entry doesn't count,
        // so callers (put/mirror) can heal it instead of being blocked by it.
        self.valid_blob_oid(hex).is_some()
    }

    fn put(&self, hex: &str, bytes: &[u8]) -> Result<(), H5iError> {
        // Enforce the content address: never store bytes under a digest that
        // isn't their sha256 (a corrupt store would poison `recall`).
        let actual = sha256_hex(bytes);
        if actual != hex {
            return Err(H5iError::Internal(format!(
                "objects-data put: content hash {actual} != key {hex}"
            )));
        }
        if self.has(hex) {
            return Ok(()); // a VALID entry already exists → idempotent
        }
        // Either absent or corrupt: insert overwrites the entry name, so this
        // also REPAIRS a tampered entry with the correct bytes.
        let blob = self.repo.blob(bytes)?;
        self.mutate(hex, Some(blob))
    }

    fn get(&self, hex: &str) -> Result<Option<Vec<u8>>, H5iError> {
        let Some(tree) = self.tip_tree() else {
            return Ok(None);
        };
        let Some(entry) = tree.get_name(hex) else {
            return Ok(None);
        };
        let blob = self.repo.find_blob(entry.id())?;
        let bytes = blob.content().to_vec();
        // Verify before returning — a tampered ref must never yield bytes that
        // don't match the requested digest (and must not be cached downstream).
        if sha256_hex(&bytes) != hex {
            return Err(H5iError::Internal(format!(
                "objects-data get: stored bytes for {hex} fail content-address check (corrupt ref)"
            )));
        }
        Ok(Some(bytes))
    }

    fn remove(&self, hex: &str) -> Result<(), H5iError> {
        if !self.has(hex) {
            return Ok(());
        }
        self.mutate(hex, None)
    }
}

/// Copy every locally-stored blob referenced by a manifest into the git-ref
/// store, so the next `git push` of [`OBJECTS_DATA_REF`] shares them. Returns
/// the number newly mirrored (already-present blobs are skipped). This is the
/// "stage blobs for sharing" step behind `h5i objects push`.
pub fn mirror_local_to_gitref(repo: &Repository, h5i_root: &Path) -> Result<usize, H5iError> {
    let local = LocalStore::new(h5i_root);
    let remote = GitRefStore::new(repo);
    let mut seen: HashSet<String> = HashSet::new();
    let mut mirrored = 0;
    for m in read_manifests(repo) {
        let hex = m.hex().to_string();
        if !seen.insert(hex.clone()) || remote.has(&hex) {
            continue;
        }
        if let Some(bytes) = local.get(&hex)? {
            remote.put(&hex, &bytes)?;
            mirrored += 1;
        }
    }
    Ok(mirrored)
}

/// Copy every blob in the git-ref store into the local store (so `recall` is
/// fast and works offline). Returns `(written, skipped_corrupt)` — an entry
/// whose bytes don't hash to its name is skipped, never cached under the
/// trusted digest path.
pub fn mirror_gitref_to_local(repo: &Repository, h5i_root: &Path) -> Result<(usize, usize), H5iError> {
    let local = LocalStore::new(h5i_root);
    let Some(tree) = GitRefStore::new(repo).tip_tree() else {
        return Ok((0, 0));
    };
    let (mut written, mut skipped) = (0, 0);
    for entry in tree.iter() {
        let Some(hex) = entry.name() else { continue };
        if !is_hex64(hex) || local.has(hex) {
            continue;
        }
        if let Ok(blob) = repo.find_blob(entry.id()) {
            if sha256_hex(blob.content()) != hex {
                skipped += 1; // corrupt/tampered entry — do not cache
                continue;
            }
            local.put(hex, blob.content())?;
            written += 1;
        }
    }
    Ok((written, skipped))
}

/// Union two divergent [`OBJECTS_DATA_REF`] tips: blobs are content-addressed,
/// so the merge is just the set union of tree entries (both sides agree on any
/// shared key). Returns the merge commit oid. Mirrors [`union_merge_commits`].
pub fn union_merge_data_commits(
    repo: &Repository,
    local_oid: git2::Oid,
    incoming_oid: git2::Oid,
) -> Result<git2::Oid, H5iError> {
    let local_commit = repo.find_commit(local_oid)?;
    let incoming_commit = repo.find_commit(incoming_oid)?;
    // Rebuild from scratch keeping only VALID entries from EITHER side — so a
    // corrupt local entry can't win over a valid incoming one (or vice versa),
    // and an entry corrupt on both sides is dropped entirely.
    let trees: Vec<git2::Tree> = [local_commit.tree(), incoming_commit.tree()]
        .into_iter()
        .filter_map(Result::ok)
        .collect();
    let tree_oid = rebuild_valid_data_tree(repo, &trees)?;
    let tree = repo.find_tree(tree_oid)?;
    let sig = signature(repo)?;
    let oid = repo.commit(
        None,
        &sig,
        &sig,
        "h5i pull: union-merge of refs/h5i/objects-data",
        &tree,
        &[&local_commit, &incoming_commit],
    )?;
    Ok(oid)
}

/// Build a fresh `objects-data` commit from `incoming` keeping only entries
/// whose bytes hash to their name — used when installing a remote data ref where
/// no local one exists, so corrupt/tampered entries are truly rejected (not just
/// skipped during local caching). The result descends from `incoming` so a later
/// push fast-forwards.
pub fn sanitize_data_commit(repo: &Repository, incoming_oid: git2::Oid) -> Result<git2::Oid, H5iError> {
    let incoming_commit = repo.find_commit(incoming_oid)?;
    let trees: Vec<git2::Tree> = incoming_commit.tree().into_iter().collect();
    let tree_oid = rebuild_valid_data_tree(repo, &trees)?;
    let tree = repo.find_tree(tree_oid)?;
    let sig = signature(repo)?;
    let oid = repo.commit(
        None,
        &sig,
        &sig,
        "h5i objects pull: sanitize refs/h5i/objects-data",
        &tree,
        &[&incoming_commit],
    )?;
    Ok(oid)
}

/// Rebuild a flat tree from `trees` (in priority order) keeping only entries
/// whose name is a sha256 hex AND whose blob content hashes to that name. The
/// first valid occurrence of a name wins; corrupt occurrences are skipped.
fn rebuild_valid_data_tree(repo: &Repository, trees: &[git2::Tree]) -> Result<git2::Oid, H5iError> {
    let mut builder = repo.treebuilder(None)?;
    for tree in trees {
        for entry in tree.iter() {
            let Some(name) = entry.name() else { continue };
            if !is_hex64(name) || builder.get(name)?.is_some() {
                continue;
            }
            if let Ok(blob) = repo.find_blob(entry.id()) {
                if sha256_hex(blob.content()) == name {
                    builder.insert(name, entry.id(), 0o100644)?;
                }
            }
        }
    }
    Ok(builder.write()?)
}

fn now_ts() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6fZ")
        .to_string()
}

/// The checked-out branch name, or `None` when detached / on an unborn HEAD.
fn current_branch(repo: &Repository) -> Option<String> {
    let head = repo.head().ok()?;
    if !head.is_branch() {
        return None;
    }
    head.shorthand().map(str::to_owned)
}

/// Repo-relative paths of files changed in the working tree (modified, staged,
/// or untracked) at the moment of capture — the "diff" the work belongs to.
pub fn working_diff_files(repo: &Repository) -> Vec<String> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    let Ok(statuses) = repo.statuses(Some(&mut opts)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in statuses.iter() {
        // Skip purely-ignored / unmodified entries.
        if e.status().is_empty() || e.status() == git2::Status::IGNORED {
            continue;
        }
        if let Some(p) = e.path() {
            out.push(p.to_string());
        }
    }
    out
}

/// Extract `path:line` file references from text (e.g. `src/auth.rs:55:9`),
/// returning the path portion. Used to tag a capture with the files its output
/// points at, even when they weren't in the diff.
fn extract_paths(text: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        // A path with an extension, followed by :<line>. Conservative to avoid
        // matching timestamps / URLs.
        regex::Regex::new(r"([A-Za-z0-9_][A-Za-z0-9_./\-]*\.[A-Za-z0-9]+):\d+").unwrap()
    });
    let mut out = Vec::new();
    for cap in re.captures_iter(text) {
        let p = cap[1].trim_start_matches("./");
        if !p.is_empty() {
            out.push(p.to_string());
        }
    }
    out
}

fn dedup_sorted(v: &mut Vec<String>) {
    v.sort();
    v.dedup();
    const CAP: usize = 50;
    if v.len() > CAP {
        v.truncate(CAP);
    }
}

fn read_dir(p: &Path) -> Result<Vec<std::fs::DirEntry>, H5iError> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(p).map_err(|e| H5iError::with_path(e, p))? {
        out.push(e.map_err(|e| H5iError::with_path(e, p))?);
    }
    Ok(out)
}

// ── Search over captured objects ─────────────────────────────────────────────
//
// `objects list` filters at the *manifest* level — which captures match some
// metadata. Search goes one level deeper: it queries the normalized
// `structured::ToolResult` findings — message text, rule, severity, kind,
// location path, fingerprint — uniformly across every captured tool. The
// fingerprint query in particular enables "has this exact failure happened
// before?" recurrence tracking that raw logs can't answer.
//
// The matcher is a pure function over already-loaded manifests, so it is cheap
// (manifests are small) and fully unit-testable without a repo or a clock.

/// Criteria for [`search_manifests`]. All set fields are ANDed together.
/// Finding-level fields (`severity`/`kind`/`rule`/`path`/`fingerprint`) require
/// at least one matching finding in a capture; manifest-level fields
/// (`branch`/`status`/`tool`/`since`) gate the whole capture.
#[derive(Debug, Default, Clone)]
pub struct SearchFilters {
    /// Case-insensitive free text. Matches a finding's message/rule/id/detail/
    /// expected/actual/location, or — for captures with no matching structured
    /// finding — the summary/highlights/command.
    pub query: Option<String>,
    /// Finding severity (`error` | `warning` | `failure`).
    pub severity: Option<String>,
    /// Finding kind (`test_failure`|`diagnostic`|`build_error`|`panic`|`generic`).
    pub kind: Option<String>,
    /// Finding rule / error code, matched case-insensitively and exactly (e.g. `TS2322`).
    pub rule: Option<String>,
    /// Path fragment matched against a finding's location(s) (suffix/equality).
    pub path: Option<String>,
    /// Finding fingerprint; matched by prefix so a short handle works.
    pub fingerprint: Option<String>,
    /// Only captures taken on this git branch.
    pub branch: Option<String>,
    /// Only captures with this structured status (`passed`|`ok`|`failed`|`error`|`unknown`).
    pub status: Option<String>,
    /// Only captures from this tool (e.g. `pytest`, `cargo`).
    pub tool: Option<String>,
    /// Only captures taken inside this environment (`env run`). Matched against
    /// the manifest's `env_id` via [`env_id_matches`] (full id / `<agent>/<slug>`
    /// / bare `<slug>`).
    pub env: Option<String>,
    /// RFC3339 cutoff in the manifest timestamp format; keep captures at or after
    /// it. Compared lexically, which is correct for the zero-padded UTC format.
    pub since: Option<String>,
}

/// Match a capture's stored `env_id` (the full `env/<agent>/<slug>`) against a
/// user-supplied env name, accepting the full id, the `<agent>/<slug>` form, or
/// a bare `<slug>`. Mirrors `env::find`'s name resolution so the same names work
/// for `env …` and `recall objects/search --env …` — and it still resolves
/// captures of a since-removed env (no on-disk manifest needed). Unlike `find`,
/// a bare slug shared by two agents matches both (this is a filter, not a
/// unique selection).
pub fn env_id_matches(env_id: Option<&str>, query: &str) -> bool {
    let Some(id) = env_id else { return false };
    let q = query.trim().trim_matches('/');
    id == q || id == format!("env/{q}") || id.rsplit('/').next() == Some(q)
}

impl SearchFilters {
    fn has_finding_constraints(&self) -> bool {
        self.severity.is_some()
            || self.kind.is_some()
            || self.rule.is_some()
            || self.path.is_some()
            || self.fingerprint.is_some()
    }
}

/// One capture that matched, with the specific findings that matched. `findings`
/// is empty when the capture matched only at the text/metadata level (an older
/// manifest with no structured findings, or a manifest-filter-only query).
pub struct SearchHit<'a> {
    pub manifest: &'a Manifest,
    pub findings: Vec<&'a crate::structured::Finding>,
}

/// Serialize a `#[serde(rename_all = "snake_case")]` enum to its string form.
fn enum_str<T: serde::Serialize>(v: &T) -> Option<String> {
    serde_json::to_value(v)
        .ok()
        .and_then(|x| x.as_str().map(str::to_string))
}

/// `needle` matches `path` if they are equal or either is a suffix of the other
/// — the same lenient rule `objects list --file` uses, so `auth.rs` finds
/// `src/auth.rs` and vice-versa.
fn path_match(path: &str, needle: &str) -> bool {
    path == needle || path.ends_with(needle) || needle.ends_with(path)
}

fn finding_paths(f: &crate::structured::Finding) -> impl Iterator<Item = &str> {
    f.location
        .iter()
        .chain(f.locations.iter())
        .map(|l| l.path.as_str())
}

/// Does the finding's free text contain `q` (already lowercased)?
fn finding_text_matches(f: &crate::structured::Finding, q: &str) -> bool {
    let fields = [
        Some(f.message.as_str()),
        f.rule.as_deref(),
        f.id.as_deref(),
        f.detail.as_deref(),
        f.expected.as_deref(),
        f.actual.as_deref(),
    ];
    if fields
        .into_iter()
        .flatten()
        .any(|s| s.to_lowercase().contains(q))
    {
        return true;
    }
    finding_paths(f).any(|p| p.to_lowercase().contains(q))
}

fn finding_matches(f: &crate::structured::Finding, flt: &SearchFilters, q: Option<&str>) -> bool {
    if let Some(want) = &flt.severity {
        if enum_str(&f.severity).as_deref() != Some(want.as_str()) {
            return false;
        }
    }
    if let Some(want) = &flt.kind {
        if enum_str(&f.kind).as_deref() != Some(want.as_str()) {
            return false;
        }
    }
    if let Some(want) = &flt.rule {
        match &f.rule {
            Some(r) if r.eq_ignore_ascii_case(want) => {}
            _ => return false,
        }
    }
    if let Some(want) = &flt.path {
        if !finding_paths(f).any(|p| path_match(p, want)) {
            return false;
        }
    }
    if let Some(want) = &flt.fingerprint {
        if !f.fingerprint.starts_with(want.as_str()) {
            return false;
        }
    }
    if let Some(q) = q {
        if !finding_text_matches(f, q) {
            return false;
        }
    }
    true
}

/// Capture-level text match (summary/highlights/command) for manifests without a
/// matching structured finding.
fn manifest_text_matches(m: &Manifest, q: &str) -> bool {
    m.summary.to_lowercase().contains(q)
        || m.highlights.iter().any(|h| h.to_lowercase().contains(q))
        || m.cmd.as_deref().is_some_and(|c| c.to_lowercase().contains(q))
}

/// Search captured objects by their normalized findings (and metadata),
/// preserving the input ordering (callers pass newest-first). Pure and
/// clock-free: `--since` is applied by lexical timestamp comparison.
pub fn search_manifests<'a>(manifests: &'a [Manifest], flt: &SearchFilters) -> Vec<SearchHit<'a>> {
    let q = flt.query.as_ref().map(|s| s.to_lowercase());
    let has_fc = flt.has_finding_constraints();
    let mut hits = Vec::new();

    for m in manifests {
        // ── manifest-level gates ──
        if let Some(b) = &flt.branch {
            if m.branch.as_deref() != Some(b.as_str()) {
                continue;
            }
        }
        if let Some(want) = &flt.status {
            let got = m.structured.as_ref().and_then(|s| enum_str(&s.status));
            if got.as_deref() != Some(want.as_str()) {
                continue;
            }
        }
        if let Some(want) = &flt.tool {
            if m.structured.as_ref().map(|s| s.tool.as_str()) != Some(want.as_str()) {
                continue;
            }
        }
        if let Some(want) = &flt.env {
            if !env_id_matches(m.env_id.as_deref(), want) {
                continue;
            }
        }
        if let Some(since) = &flt.since {
            if m.timestamp.as_str() < since.as_str() {
                continue;
            }
        }

        // ── finding-level matches ──
        let matched: Vec<&crate::structured::Finding> = m
            .structured
            .as_ref()
            .map(|s| {
                s.findings
                    .iter()
                    .filter(|f| finding_matches(f, flt, q.as_deref()))
                    .collect()
            })
            .unwrap_or_default();

        if !matched.is_empty() {
            hits.push(SearchHit { manifest: m, findings: matched });
        } else if has_fc {
            // A finding-level constraint was required but nothing matched.
            continue;
        } else if let Some(q) = &q {
            // No finding constraints, free-text query: allow a capture-level match.
            if manifest_text_matches(m, q) {
                hits.push(SearchHit { manifest: m, findings: Vec::new() });
            }
        } else {
            // Only manifest-level gates (or none): the capture itself is the hit.
            hits.push(SearchHit { manifest: m, findings: Vec::new() });
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup() -> (tempfile::TempDir, Repository, PathBuf) {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let h5i_root = dir.path().join(".git").join(".h5i");
        std::fs::create_dir_all(&h5i_root).unwrap();
        (dir, repo, h5i_root)
    }

    fn opts() -> CaptureOptions {
        CaptureOptions {
            kind: OutputKind::Auto,
            cmd: Some("pytest -q".into()),
            cwd: None,
            exit_code: Some(1),
            git_tree: None,
            files: Vec::new(),
            cmd_argv: vec!["pytest".into(), "-q".into()],
            filter: FilterConfig::default(),
            env_id: None,
            policy_digest: None,
            evidence_source: None,
            egress: None,
            redact: false,
        }
    }

    #[test]
    fn sha256_is_stable() {
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn redact_flag_scrubs_payload_and_command_before_storage() {
        let (_d, repo, h5i_root) = setup();
        let secret = "ghp_0123456789012345678901234567890123ab";
        let raw = format!("auth token={secret}\nok\n");
        let mut o = opts();
        o.redact = true;
        o.cmd = Some(format!("deploy --token {secret}"));
        o.cmd_argv = vec!["deploy".into()];
        let outcome = capture(&repo, &h5i_root, raw.as_bytes(), o).unwrap();
        let m = &outcome.manifest;
        // The detected rule id is recorded; the value never is.
        assert!(m.redactions.contains(&"GITHUB_PAT".to_string()), "{:?}", m.redactions);
        assert!(!m.summary.contains(secret));
        assert!(!m.cmd.as_ref().unwrap().contains(secret), "cmd leaked the secret");
        // The content-addressed blob (what `objects push` shares) is scrubbed,
        // so the hash is of the REDACTED bytes — the raw secret is unrecoverable.
        let stored = load_raw(&h5i_root, m).unwrap().unwrap();
        assert!(!String::from_utf8_lossy(&stored).contains(secret), "raw blob leaked the secret");
    }

    #[test]
    fn redact_flag_off_leaves_payload_intact() {
        let (_d, repo, h5i_root) = setup();
        let secret = "ghp_0123456789012345678901234567890123ab";
        let raw = format!("token={secret}\n");
        let mut o = opts();
        o.redact = false;
        let outcome = capture(&repo, &h5i_root, raw.as_bytes(), o).unwrap();
        assert!(outcome.manifest.redactions.is_empty());
        let stored = load_raw(&h5i_root, &outcome.manifest).unwrap().unwrap();
        assert!(String::from_utf8_lossy(&stored).contains(secret));
    }

    #[test]
    fn blob_path_is_sharded() {
        let dir = tempdir().unwrap();
        let store = LocalStore::new(dir.path());
        let p = store.blob_path("abcdef0000000000000000000000000000000000000000000000000000000000");
        assert!(p.ends_with(
            "ab/cd/abcdef0000000000000000000000000000000000000000000000000000000000"
        ));
    }

    #[test]
    fn capture_stores_raw_and_restores_exact_bytes() {
        let (_d, repo, h5i_root) = setup();
        let mut raw = String::new();
        for i in 0..1000 {
            raw.push_str(&format!("log line {i}\n"));
        }
        raw.push_str("error: boom at src/x.rs:9\n");
        let out = capture(&repo, &h5i_root, raw.as_bytes(), opts()).unwrap();
        assert!(!out.deduped);

        // Manifest carries the full digest and a much smaller summary.
        assert!(out.manifest.raw_oid.starts_with("sha256:"));
        assert_eq!(out.manifest.hex().len(), 64);
        assert!(out.manifest.summary.len() < raw.len());
        assert!(out.manifest.summary.contains("error: boom"));

        // get restores exact bytes.
        let restored = load_raw(&h5i_root, &out.manifest).unwrap().unwrap();
        assert_eq!(restored, raw.as_bytes());

        // Manifest is in the ref log and resolvable by id / prefix.
        let found = find_manifest(&repo, &out.manifest.id).unwrap();
        assert_eq!(found.raw_oid, out.manifest.raw_oid);
        let by_prefix = find_manifest(&repo, &out.manifest.hex()[..8]).unwrap();
        assert_eq!(by_prefix.raw_oid, out.manifest.raw_oid);
    }

    #[test]
    fn identical_content_dedupes() {
        let (_d, repo, h5i_root) = setup();
        let raw = b"same content";
        let a = capture(&repo, &h5i_root, raw, opts()).unwrap();
        let b = capture(&repo, &h5i_root, raw, opts()).unwrap();
        assert!(!a.deduped);
        assert!(b.deduped);
        assert_eq!(a.manifest.raw_oid, b.manifest.raw_oid);
        // Two manifests, one blob.
        assert_eq!(read_manifests(&repo).len(), 2);
        let store = LocalStore::new(&h5i_root);
        assert_eq!(store.iter_blobs().unwrap().len(), 1);
    }

    #[test]
    fn absent_blob_degrades_gracefully() {
        let (_d, repo, h5i_root) = setup();
        let out = capture(&repo, &h5i_root, b"hello world", opts()).unwrap();
        // Manually remove the blob to simulate eviction.
        LocalStore::new(&h5i_root).remove(out.manifest.hex()).unwrap();
        let restored = load_raw(&h5i_root, &out.manifest).unwrap();
        assert!(restored.is_none(), "evicted blob should read as absent");
        // Manifest (summary) is still intact.
        assert!(find_manifest(&repo, &out.manifest.id).is_some());
    }

    #[test]
    fn gc_removes_orphans_but_keeps_referenced() {
        let (_d, repo, h5i_root) = setup();
        let out = capture(&repo, &h5i_root, b"referenced", opts()).unwrap();
        // Write an orphan blob with no manifest.
        let store = LocalStore::new(&h5i_root);
        let orphan_hex = sha256_hex(b"orphan");
        store.put(&orphan_hex, b"orphan").unwrap();

        let report = gc(&repo, &h5i_root, None, false).unwrap();
        assert_eq!(report.evicted.len(), 1);
        assert_eq!(report.evicted[0].hex, orphan_hex);
        assert_eq!(report.kept_referenced, 1);
        assert!(store.has(out.manifest.hex()));
        assert!(!store.has(&orphan_hex));
    }

    #[test]
    fn gc_dry_run_evicts_nothing() {
        let (_d, repo, h5i_root) = setup();
        let store = LocalStore::new(&h5i_root);
        let orphan = sha256_hex(b"orphan2");
        store.put(&orphan, b"orphan2").unwrap();
        let report = gc(&repo, &h5i_root, None, true).unwrap();
        assert!(report.dry_run);
        assert_eq!(report.evicted.len(), 1);
        assert!(store.has(&orphan), "dry-run must not delete");
    }

    #[test]
    fn pinned_referenced_blob_survives_ttl_gc() {
        let (_d, repo, h5i_root) = setup();
        let out = capture(&repo, &h5i_root, b"pin me", opts()).unwrap();
        pin(&h5i_root, out.manifest.hex()).unwrap();
        // TTL of 0 would normally evict every referenced blob.
        let report = gc(&repo, &h5i_root, Some(Duration::from_secs(0)), false).unwrap();
        assert_eq!(report.kept_pinned, 1);
        assert!(report.evicted.is_empty());
        assert!(LocalStore::new(&h5i_root).has(out.manifest.hex()));
    }

    #[test]
    fn ttl_gc_evicts_referenced_but_stale() {
        let (_d, repo, h5i_root) = setup();
        let out = capture(&repo, &h5i_root, b"stale", opts()).unwrap();
        // TTL of 0s: any referenced blob is "older than TTL".
        let report = gc(&repo, &h5i_root, Some(Duration::from_secs(0)), false).unwrap();
        assert_eq!(report.evicted.len(), 1);
        assert!(!LocalStore::new(&h5i_root).has(out.manifest.hex()));
        // Summary survives eviction.
        assert!(find_manifest(&repo, &out.manifest.id).is_some());
    }

    #[test]
    fn fsck_flags_absent_and_orphans() {
        let (_d, repo, h5i_root) = setup();
        let out = capture(&repo, &h5i_root, b"present", opts()).unwrap();
        let store = LocalStore::new(&h5i_root);
        store.put(&sha256_hex(b"orphan3"), b"orphan3").unwrap();
        store.remove(out.manifest.hex()).unwrap(); // make the referenced one absent

        let report = fsck(&repo, &h5i_root).unwrap();
        assert_eq!(report.absent, 1);
        assert_eq!(report.orphans.len(), 1);
    }

    #[test]
    fn binary_payload_is_stored_with_marker_summary() {
        let (_d, repo, h5i_root) = setup();
        let raw = vec![0u8, 1, 2, 3, 0, 255];
        let out = capture(&repo, &h5i_root, &raw, opts()).unwrap();
        assert!(out.manifest.summary.contains("binary"));
        assert_eq!(load_raw(&h5i_root, &out.manifest).unwrap().unwrap(), raw);
    }

    fn manifest_with_oid(hex: &str) -> Manifest {
        Manifest {
            id: hex[..16].to_string(),
            kind: "test".into(),
            cmd: None,
            cwd: None,
            exit_code: None,
            git_tree: None,
            branch: None,
            files: Vec::new(),
            diff_files: Vec::new(),
            timestamp: now_ts(),
            raw_oid: format!("sha256:{hex}"),
            raw_size: 0,
            raw_lines: 0,
            filter_version: 1,
            summary: String::new(),
            highlights: Vec::new(),
            store: "local".into(),
            codec: "none".into(),
            raw_tokens: None,
            summary_tokens: None,
            structured: None,
            env_id: None,
            policy_digest: None,
            evidence_source: None,
            egress: None,
            redactions: Vec::new(),
        }
    }

    #[test]
    fn resolve_manifest_errors_on_ambiguous_prefix() {
        let (_d, repo, _h5i_root) = setup();
        // Two distinct digests sharing the "abcd" prefix.
        let a = format!("abcd{}", "0".repeat(60));
        let b = format!("abcd{}", "1".repeat(60));
        append_manifest(&repo, &manifest_with_oid(&a)).unwrap();
        append_manifest(&repo, &manifest_with_oid(&b)).unwrap();

        // Ambiguous prefix → error.
        let err = resolve_manifest(&repo, "abcd").unwrap_err();
        assert!(err.to_string().contains("ambiguous"), "{err}");

        // Full hex (and sha256: form) resolve to exactly one.
        assert_eq!(resolve_manifest(&repo, &a).unwrap().hex(), a);
        assert_eq!(
            resolve_manifest(&repo, &format!("sha256:{b}")).unwrap().hex(),
            b
        );

        // A prefix unique to one digest resolves; an unknown one errors.
        assert_eq!(resolve_manifest(&repo, "abcd0000").unwrap().hex(), a);
        assert!(resolve_manifest(&repo, "ffff").unwrap_err().to_string().contains("no object"));

        // Too-short handles are rejected outright.
        assert!(resolve_manifest(&repo, "ab")
            .unwrap_err()
            .to_string()
            .contains("too short"));
    }

    #[test]
    fn clamp_text_truncates_on_char_boundary() {
        let (short, t) = clamp_text("hello".into(), 100);
        assert_eq!(short, "hello");
        assert!(!t);
        // Multi-byte chars must not be split.
        let s = "é".repeat(20_000); // 2 bytes each → 40 KB
        let (out, t) = clamp_text(s, MAX_SUMMARY_BYTES);
        assert!(t);
        assert!(out.len() <= MAX_SUMMARY_BYTES);
        assert!(out.ends_with("truncated] …"));
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn clamp_highlights_caps_count_and_length() {
        let hs: Vec<String> = (0..50).map(|i| format!("h{i} ") + &"x".repeat(1000)).collect();
        let out = clamp_highlights(hs);
        assert_eq!(out.len(), MAX_HIGHLIGHTS);
        for h in &out {
            assert!(h.len() <= MAX_HIGHLIGHT_BYTES, "highlight too long: {}", h.len());
        }
    }

    #[test]
    fn capture_clamps_oversized_summary_and_highlights() {
        let (_d, repo, h5i_root) = setup();
        // 40 distinct long error lines → kept verbatim (high-signal), ~40 KB summary.
        let mut raw = String::new();
        for i in 0..40 {
            raw.push_str(&format!("error[E{i:04}]: {}\n", "x".repeat(1200)));
        }
        let out = capture(&repo, &h5i_root, raw.as_bytes(), opts()).unwrap();
        assert!(
            out.manifest.summary.len() <= MAX_SUMMARY_BYTES,
            "summary not clamped: {} bytes",
            out.manifest.summary.len()
        );
        assert!(out.manifest.highlights.len() <= MAX_HIGHLIGHTS);
        for h in &out.manifest.highlights {
            assert!(h.len() <= MAX_HIGHLIGHT_BYTES);
        }
        // The raw is still fully recoverable despite the clamped summary.
        assert_eq!(load_raw(&h5i_root, &out.manifest).unwrap().unwrap(), raw.as_bytes());
    }

    #[test]
    fn capture_populates_structured_result() {
        let (_d, repo, h5i_root) = setup();
        // opts() uses cmd_argv ["pytest","-q"]; give it pytest-shaped output.
        let raw = "=== FAILURES ===\nFAILED tests/t.py::test_x - assert 0 == 1\n=== 1 failed, 9 passed in 0.5s ===\n";
        let out = capture(&repo, &h5i_root, raw.as_bytes(), opts()).unwrap();
        let s = out.manifest.structured.as_ref().expect("structured present");
        assert_eq!(s.tool, "pytest");
        assert_eq!(s.parser_confidence, crate::structured::ParserConfidence::Parsed);
        assert_eq!(s.status, crate::structured::Status::Failed);
        assert_eq!(s.findings.len(), 1);
        assert_eq!(s.raw_oid.as_deref(), Some(out.manifest.raw_oid.as_str()));
    }

    #[test]
    fn non_command_capture_has_no_structured() {
        // `objects put` (empty cmd_argv) must not produce a structured record,
        // so recall --tool/--status stays clean of manual ingests.
        let (_d, repo, h5i_root) = setup();
        let mut o = opts();
        o.cmd_argv = Vec::new();
        let out = capture(&repo, &h5i_root, b"some pasted log content here", o).unwrap();
        assert!(out.manifest.structured.is_none());
    }

    #[test]
    fn capture_without_parser_falls_back_to_generic_structured() {
        let (_d, repo, h5i_root) = setup();
        let mut o = opts();
        o.cmd_argv = vec!["make".into(), "all".into()];
        o.exit_code = Some(0);
        let raw = "gcc -c a.c\ngcc -c b.c\nlinking\n";
        let out = capture(&repo, &h5i_root, raw.as_bytes(), o).unwrap();
        let s = out.manifest.structured.as_ref().unwrap();
        assert_eq!(s.tool, "make");
        assert_eq!(s.parser_confidence, crate::structured::ParserConfidence::Generic);
        assert_eq!(s.status, crate::structured::Status::Ok); // exit 0, non-test
        assert!(s.body.is_some());
    }

    #[test]
    fn extract_paths_finds_file_line_refs() {
        let text = "thread panicked at src/auth.rs:55:9:\nsee tests/x.py:10 and ./lib/util.rs:3\nno match here 12:30:00";
        let mut got = extract_paths(text);
        got.sort();
        assert_eq!(got, vec!["lib/util.rs", "src/auth.rs", "tests/x.py"]);
    }

    #[test]
    fn capture_records_branch_and_mentioned_files() {
        let (dir, repo, h5i_root) = setup();
        // Make a branch + a commit so HEAD is born and on a named branch.
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let tree_id = {
            let mut idx = repo.index().unwrap();
            idx.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        let oid = repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
        let commit = repo.find_commit(oid).unwrap();
        repo.branch("feature-x", &commit, true).unwrap();
        repo.set_head("refs/heads/feature-x").unwrap();

        let raw = b"running\nerror at src/widget.rs:42: boom\n";
        let mut o = opts();
        o.files = vec!["src/explicit.rs".to_string()];
        let out = capture(&repo, &h5i_root, raw, o).unwrap();

        assert_eq!(out.manifest.branch.as_deref(), Some("feature-x"));
        // explicit + mentioned (from the error line) both recorded.
        assert!(out.manifest.files.contains(&"src/explicit.rs".to_string()));
        assert!(out.manifest.files.contains(&"src/widget.rs".to_string()));
        let _ = dir;
    }

    #[test]
    fn parse_duration_units() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86_400));
        assert_eq!(parse_duration("1w").unwrap(), Duration::from_secs(604_800));
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        assert!(parse_duration("5y").is_err());
    }

    #[test]
    fn git_ref_store_round_trips() {
        let (_dir, repo, _root) = setup();
        let store = GitRefStore::new(&repo);
        let hex = sha256_hex(b"shared raw output");
        assert!(!store.has(&hex));
        assert!(store.get(&hex).unwrap().is_none());
        store.put(&hex, b"shared raw output").unwrap();
        assert!(store.has(&hex));
        assert_eq!(store.get(&hex).unwrap().unwrap(), b"shared raw output");
        // Idempotent: a second put of identical content is a no-op.
        store.put(&hex, b"shared raw output").unwrap();
        assert!(store.has(&hex));
        store.remove(&hex).unwrap();
        assert!(!store.has(&hex));
    }

    #[test]
    fn git_ref_store_put_rejects_mismatched_digest() {
        let (_dir, repo, _root) = setup();
        let store = GitRefStore::new(&repo);
        // The key isn't sha256(bytes) → must be refused, never stored.
        let err = store.put(&"a".repeat(64), b"some bytes").unwrap_err();
        assert!(format!("{err}").contains("content hash"), "{err}");
        assert!(!store.has(&"a".repeat(64)));
    }

    // Build a tampered objects-data ref: a valid-looking hex name whose bytes
    // do NOT hash to it.
    fn craft_corrupt_data_ref(repo: &Repository, name: &str, bytes: &[u8]) {
        let blob = repo.blob(bytes).unwrap();
        let mut b = repo.treebuilder(None).unwrap();
        b.insert(name, blob, 0o100644).unwrap();
        let tree = repo.find_tree(b.write().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let oid = repo.commit(None, &sig, &sig, "tamper", &tree, &[]).unwrap();
        repo.reference(OBJECTS_DATA_REF, oid, true, "tamper").unwrap();
    }

    #[test]
    fn git_ref_store_get_rejects_corrupt_entry() {
        let (_dir, repo, _root) = setup();
        let name = "0".repeat(64); // valid hex64, but not the hash of the bytes
        craft_corrupt_data_ref(&repo, &name, b"tampered bytes");
        let err = GitRefStore::new(&repo).get(&name).unwrap_err();
        assert!(format!("{err}").contains("content-address"), "{err}");
    }

    #[test]
    fn union_merge_drops_corrupt_incoming_entries() {
        let (_dir, repo, _root) = setup();
        // local: one valid blob.
        let good = sha256_hex(b"good");
        GitRefStore::new(&repo).put(&good, b"good").unwrap();
        let local = repo.refname_to_id(OBJECTS_DATA_REF).unwrap();
        // incoming: a separate commit carrying only a corrupt (name != hash) entry.
        let bad = "f".repeat(64);
        let blob = repo.blob(b"evil").unwrap();
        let mut b = repo.treebuilder(None).unwrap();
        b.insert(&bad, blob, 0o100644).unwrap();
        let tree = repo.find_tree(b.write().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let incoming = repo.commit(None, &sig, &sig, "evil", &tree, &[]).unwrap();

        let merged = union_merge_data_commits(&repo, local, incoming).unwrap();
        let mtree = repo.find_commit(merged).unwrap().tree().unwrap();
        assert!(mtree.get_name(&good).is_some(), "valid entry must be kept");
        assert!(mtree.get_name(&bad).is_none(), "corrupt entry must be dropped");
    }

    #[test]
    fn put_repairs_a_corrupt_existing_entry() {
        let (_dir, repo, _root) = setup();
        let good = sha256_hex(b"payload");
        craft_corrupt_data_ref(&repo, &good, b"WRONG bytes"); // right name, wrong content
        let store = GitRefStore::new(&repo);
        assert!(!store.has(&good), "corrupt entry must not count as present");
        store.put(&good, b"payload").unwrap(); // correct bytes → repair
        assert!(store.has(&good), "put must heal the corrupt entry");
        assert_eq!(store.get(&good).unwrap().unwrap(), b"payload");
    }

    #[test]
    fn mirror_local_heals_a_corrupt_gitref_entry() {
        let (_dir, repo, root) = setup();
        let m = capture(&repo, &root, b"big payload\n".repeat(40).as_slice(), opts())
            .unwrap()
            .manifest;
        let hex = m.hex().to_string();
        craft_corrupt_data_ref(&repo, &hex, b"corrupt"); // plant a bad entry under the name
        assert!(!GitRefStore::new(&repo).has(&hex));
        // mirror has the correct local blob → repairs rather than skipping.
        assert_eq!(mirror_local_to_gitref(&repo, &root).unwrap(), 1);
        assert!(GitRefStore::new(&repo).has(&hex), "mirror healed the corrupt entry");
    }

    #[test]
    fn merge_prefers_valid_incoming_over_corrupt_local() {
        let (_dir, repo, _root) = setup();
        let hex = sha256_hex(b"V");
        craft_corrupt_data_ref(&repo, &hex, b"corrupt-local"); // local corrupt under hex
        let local = repo.refname_to_id(OBJECTS_DATA_REF).unwrap();
        // incoming: the VALID entry for the same key.
        let blob = repo.blob(b"V").unwrap();
        let mut b = repo.treebuilder(None).unwrap();
        b.insert(&hex, blob, 0o100644).unwrap();
        let tree = repo.find_tree(b.write().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let incoming = repo.commit(None, &sig, &sig, "valid", &tree, &[]).unwrap();

        let merged = union_merge_data_commits(&repo, local, incoming).unwrap();
        repo.reference(OBJECTS_DATA_REF, merged, true, "m").unwrap();
        assert!(GitRefStore::new(&repo).has(&hex), "valid incoming must beat corrupt local");
        assert_eq!(GitRefStore::new(&repo).get(&hex).unwrap().unwrap(), b"V");
    }

    #[test]
    fn pull_install_sanitizes_corrupt_only_incoming() {
        let (_dir, repo, _root) = setup();
        let name = "1".repeat(64);
        craft_corrupt_data_ref(&repo, &name, b"corrupt"); // incoming = corrupt-only commit
        let incoming = repo.refname_to_id(OBJECTS_DATA_REF).unwrap();
        let clean = sanitize_data_commit(&repo, incoming).unwrap();
        repo.reference(OBJECTS_DATA_REF, clean, true, "clean").unwrap();
        assert!(
            !GitRefStore::new(&repo).has(&name),
            "a corrupt-only pull must not leave the key has()==true"
        );
        let ctree = repo.find_commit(clean).unwrap().tree().unwrap();
        assert!(ctree.get_name(&name).is_none(), "corrupt entry dropped from sanitized tree");
    }

    #[test]
    fn mirror_local_to_gitref_then_recover_after_local_eviction() {
        let (_dir, repo, root) = setup();
        // Capture stores the raw locally + writes a manifest.
        let m = capture(&repo, &root, b"a huge log\n".repeat(50).as_slice(), opts())
            .unwrap()
            .manifest;
        let hex = m.hex().to_string();
        // Stage into the git-ref store (what `objects push` mirrors).
        assert_eq!(mirror_local_to_gitref(&repo, &root).unwrap(), 1);
        assert!(GitRefStore::new(&repo).has(&hex));
        // Evict the local blob → load_raw can't find it…
        LocalStore::new(&root).remove(&hex).unwrap();
        assert!(load_raw(&root, &m).unwrap().is_none());
        // …but the remote-aware path recovers it from the git-ref store and caches.
        let bytes = load_raw_with_remote(&repo, &root, &m).unwrap().unwrap();
        assert_eq!(bytes, b"a huge log\n".repeat(50));
        assert!(LocalStore::new(&root).has(&hex), "blob should be cached back locally");
    }

    // ── search_manifests ────────────────────────────────────────────────────

    use crate::structured::{
        Finding, FindingKind, Location, ParserConfidence, ResultKind, Severity, Status, ToolResult,
        SCHEMA_VERSION,
    };

    fn hex64(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    fn mk_finding(
        kind: FindingKind,
        sev: Severity,
        rule: Option<&str>,
        msg: &str,
        path: Option<&str>,
        fp: &str,
    ) -> Finding {
        Finding {
            kind,
            severity: sev,
            id: None,
            rule: rule.map(str::to_string),
            message: msg.to_string(),
            location: path.map(|p| Location {
                path: p.to_string(),
                line: Some(42),
                column: None,
                end_line: None,
                end_column: None,
            }),
            locations: Vec::new(),
            expected: None,
            actual: None,
            detail: None,
            fixable: false,
            suggestions: Vec::new(),
            fingerprint: fp.to_string(),
        }
    }

    /// Build a manifest carrying a structured result with the given findings.
    fn mk_manifest(
        fill: char,
        tool: &str,
        status: Status,
        branch: Option<&str>,
        findings: Vec<Finding>,
    ) -> Manifest {
        let mut m = manifest_with_oid(&hex64(fill));
        m.branch = branch.map(str::to_string);
        m.structured = Some(ToolResult {
            schema_version: SCHEMA_VERSION,
            tool: tool.to_string(),
            kind: ResultKind::Test,
            status,
            exit_code: Some(1),
            duration_ms: None,
            counts: Default::default(),
            parser_confidence: ParserConfidence::Parsed,
            raw_oid: None,
            findings,
            suppressed: Vec::new(),
            truncated: Default::default(),
            body: None,
            extra: Default::default(),
        });
        m
    }

    fn one_finding_manifest() -> Manifest {
        mk_manifest(
            'a',
            "pytest",
            Status::Failed,
            Some("feature/x"),
            vec![mk_finding(
                FindingKind::TestFailure,
                Severity::Failure,
                Some("AssertionError"),
                "expected 200 but got 500 in handler",
                Some("src/api/auth.rs"),
                "fp-aaaa-1111",
            )],
        )
    }

    #[test]
    fn search_query_matches_message_rule_and_path() {
        let ms = vec![one_finding_manifest()];

        // Message substring (case-insensitive).
        let hits = search_manifests(&ms, &SearchFilters { query: Some("HANDLER".into()), ..Default::default() });
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].findings.len(), 1);

        // Rule text.
        let hits = search_manifests(&ms, &SearchFilters { query: Some("assertion".into()), ..Default::default() });
        assert_eq!(hits.len(), 1);

        // Location path.
        let hits = search_manifests(&ms, &SearchFilters { query: Some("auth.rs".into()), ..Default::default() });
        assert_eq!(hits.len(), 1);

        // A term that appears nowhere → no hit.
        let hits = search_manifests(&ms, &SearchFilters { query: Some("nonexistent".into()), ..Default::default() });
        assert!(hits.is_empty());
    }

    #[test]
    fn search_filters_severity_and_kind() {
        let ms = vec![mk_manifest(
            'b',
            "cargo",
            Status::Failed,
            None,
            vec![
                mk_finding(FindingKind::BuildError, Severity::Error, Some("E0599"), "no method", Some("src/a.rs"), "fp1"),
                mk_finding(FindingKind::Diagnostic, Severity::Warning, Some("dead_code"), "unused", Some("src/b.rs"), "fp2"),
            ],
        )];

        let hits = search_manifests(&ms, &SearchFilters { severity: Some("error".into()), ..Default::default() });
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].findings.len(), 1, "only the error-severity finding matches");
        assert_eq!(hits[0].findings[0].rule.as_deref(), Some("E0599"));

        let hits = search_manifests(&ms, &SearchFilters { kind: Some("diagnostic".into()), ..Default::default() });
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].findings[0].rule.as_deref(), Some("dead_code"));
    }

    #[test]
    fn search_rule_is_exact_case_insensitive() {
        let ms = vec![one_finding_manifest()]; // rule "AssertionError"
        // Exact, case-insensitive → match.
        let hits = search_manifests(&ms, &SearchFilters { rule: Some("assertionerror".into()), ..Default::default() });
        assert_eq!(hits.len(), 1);
        // Partial rule must NOT match (rule is exact, unlike free-text query).
        let hits = search_manifests(&ms, &SearchFilters { rule: Some("assertion".into()), ..Default::default() });
        assert!(hits.is_empty());
    }

    #[test]
    fn search_path_matches_by_suffix_either_direction() {
        let ms = vec![one_finding_manifest()]; // path "src/api/auth.rs"
        for needle in ["auth.rs", "api/auth.rs", "src/api/auth.rs"] {
            let hits = search_manifests(&ms, &SearchFilters { path: Some(needle.into()), ..Default::default() });
            assert_eq!(hits.len(), 1, "path filter '{needle}' should match");
        }
        let hits = search_manifests(&ms, &SearchFilters { path: Some("other.rs".into()), ..Default::default() });
        assert!(hits.is_empty());
    }

    #[test]
    fn search_fingerprint_matches_by_prefix() {
        let ms = vec![one_finding_manifest()]; // fingerprint "fp-aaaa-1111"
        let hits = search_manifests(&ms, &SearchFilters { fingerprint: Some("fp-aaaa".into()), ..Default::default() });
        assert_eq!(hits.len(), 1);
        let hits = search_manifests(&ms, &SearchFilters { fingerprint: Some("fp-aaaa-1111".into()), ..Default::default() });
        assert_eq!(hits.len(), 1);
        let hits = search_manifests(&ms, &SearchFilters { fingerprint: Some("fp-bbbb".into()), ..Default::default() });
        assert!(hits.is_empty());
    }

    #[test]
    fn search_manifest_level_branch_status_tool() {
        let ms = vec![
            mk_manifest('a', "pytest", Status::Failed, Some("main"), vec![mk_finding(FindingKind::TestFailure, Severity::Failure, None, "m", None, "f1")]),
            mk_manifest('b', "cargo", Status::Passed, Some("dev"), vec![mk_finding(FindingKind::TestFailure, Severity::Failure, None, "m", None, "f2")]),
        ];

        assert_eq!(search_manifests(&ms, &SearchFilters { branch: Some("dev".into()), ..Default::default() }).len(), 1);
        assert_eq!(search_manifests(&ms, &SearchFilters { status: Some("passed".into()), ..Default::default() }).len(), 1);
        assert_eq!(search_manifests(&ms, &SearchFilters { tool: Some("pytest".into()), ..Default::default() }).len(), 1);
        // A branch nobody is on.
        assert!(search_manifests(&ms, &SearchFilters { branch: Some("nope".into()), ..Default::default() }).is_empty());
    }

    #[test]
    fn env_id_matches_full_agentslug_and_bare_slug() {
        let id = Some("env/claude/fix-auth");
        assert!(env_id_matches(id, "env/claude/fix-auth")); // full id
        assert!(env_id_matches(id, "claude/fix-auth")); // <agent>/<slug>
        assert!(env_id_matches(id, "fix-auth")); // bare slug
        assert!(env_id_matches(id, "/fix-auth/")); // stray slashes trimmed
        // Non-matches.
        assert!(!env_id_matches(id, "fix")); // partial slug, not a component
        assert!(!env_id_matches(id, "claude")); // agent alone is not the slug
        assert!(!env_id_matches(id, "codex/fix-auth")); // wrong agent
        assert!(!env_id_matches(None, "fix-auth")); // capture has no env_id
    }

    #[test]
    fn search_filters_by_env_id() {
        let mut alpha = mk_manifest('a', "pytest", Status::Failed, Some("main"),
            vec![mk_finding(FindingKind::TestFailure, Severity::Failure, None, "boom", None, "f1")]);
        alpha.env_id = Some("env/tester/alpha".into());
        let mut beta = mk_manifest('b', "cargo", Status::Failed, Some("main"),
            vec![mk_finding(FindingKind::TestFailure, Severity::Failure, None, "boom", None, "f2")]);
        beta.env_id = Some("env/tester/beta".into());
        let plain = mk_manifest('c', "npm", Status::Failed, Some("main"),
            vec![mk_finding(FindingKind::TestFailure, Severity::Failure, None, "boom", None, "f3")]);
        let ms = vec![alpha, beta, plain];

        // Bare slug selects exactly its env's capture.
        let hits = search_manifests(&ms, &SearchFilters { env: Some("alpha".into()), ..Default::default() });
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].manifest.env_id.as_deref(), Some("env/tester/alpha"));
        // Full id works too; a non-env capture is never matched by --env.
        assert_eq!(search_manifests(&ms, &SearchFilters { env: Some("env/tester/beta".into()), ..Default::default() }).len(), 1);
        assert!(search_manifests(&ms, &SearchFilters { env: Some("ghost".into()), ..Default::default() }).is_empty());
        // --env composes with the free-text query.
        assert_eq!(search_manifests(&ms, &SearchFilters { env: Some("alpha".into()), query: Some("boom".into()), ..Default::default() }).len(), 1);
    }

    #[test]
    fn search_since_is_lexical_cutoff() {
        let mut old = one_finding_manifest();
        old.timestamp = "2026-01-01T00:00:00.000000Z".into();
        let mut recent = mk_manifest('c', "pytest", Status::Failed, None, vec![mk_finding(FindingKind::TestFailure, Severity::Failure, None, "m", None, "f9")]);
        recent.timestamp = "2026-06-01T00:00:00.000000Z".into();
        let ms = vec![old, recent];

        let hits = search_manifests(&ms, &SearchFilters { since: Some("2026-03-01T00:00:00.000000Z".into()), ..Default::default() });
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].manifest.timestamp, "2026-06-01T00:00:00.000000Z");
    }

    #[test]
    fn search_and_semantics_apply_per_finding() {
        // severity=error on finding A, query "bar" on finding B → no single
        // finding satisfies both, so the capture is excluded.
        let ms = vec![mk_manifest(
            'd',
            "cargo",
            Status::Failed,
            None,
            vec![
                mk_finding(FindingKind::BuildError, Severity::Error, None, "foo", None, "fa"),
                mk_finding(FindingKind::Diagnostic, Severity::Warning, None, "bar", None, "fb"),
            ],
        )];
        let hits = search_manifests(&ms, &SearchFilters { severity: Some("error".into()), query: Some("bar".into()), ..Default::default() });
        assert!(hits.is_empty(), "no finding is both error-severity and matches 'bar'");

        // Same filters, but one finding satisfies both → hit.
        let ms = vec![mk_manifest('e', "cargo", Status::Failed, None, vec![mk_finding(FindingKind::BuildError, Severity::Error, None, "bar boom", None, "fc")])];
        let hits = search_manifests(&ms, &SearchFilters { severity: Some("error".into()), query: Some("bar".into()), ..Default::default() });
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn search_falls_back_to_summary_when_no_structured() {
        // Older capture: no structured result, query lives in the summary.
        let mut m = manifest_with_oid(&hex64('f'));
        m.summary = "Traceback: boom in module".into();
        m.structured = None;
        let ms = vec![m];

        let hits = search_manifests(&ms, &SearchFilters { query: Some("boom".into()), ..Default::default() });
        assert_eq!(hits.len(), 1);
        assert!(hits[0].findings.is_empty(), "capture-level match carries no findings");

        // A finding-level constraint can't be satisfied without structure → excluded.
        let hits = search_manifests(&ms, &SearchFilters { severity: Some("error".into()), ..Default::default() });
        assert!(hits.is_empty());
    }

    #[test]
    fn search_query_only_in_summary_when_structured_findings_dont_match() {
        // Structured findings exist but don't mention the term; the summary does.
        let mut m = one_finding_manifest(); // finding msg has no "flaky"
        m.summary = "note: this test is flaky".into();
        let ms = vec![m];
        let hits = search_manifests(&ms, &SearchFilters { query: Some("flaky".into()), ..Default::default() });
        assert_eq!(hits.len(), 1);
        assert!(hits[0].findings.is_empty(), "matched on summary, not a finding");
    }

    #[test]
    fn search_no_criteria_returns_every_capture() {
        let ms = vec![
            one_finding_manifest(),
            mk_manifest('1', "cargo", Status::Ok, None, vec![]),
        ];
        let hits = search_manifests(&ms, &SearchFilters::default());
        assert_eq!(hits.len(), 2);
        // The capture with a finding reports it; the empty one is a capture-level hit.
        assert_eq!(hits[0].findings.len(), 1);
        assert_eq!(hits[1].findings.len(), 0);
    }

    #[test]
    fn search_finding_constraint_with_no_match_excludes_capture() {
        // A capture whose only finding is a warning, filtered for errors → gone,
        // even though the capture exists.
        let ms = vec![mk_manifest('2', "ruff", Status::Failed, None, vec![mk_finding(FindingKind::Diagnostic, Severity::Warning, Some("E501"), "line too long", Some("a.py"), "fp")])];
        let hits = search_manifests(&ms, &SearchFilters { severity: Some("error".into()), ..Default::default() });
        assert!(hits.is_empty());
    }

    #[test]
    fn search_matches_paths_in_the_locations_vec() {
        // A finding with no singular `location` but several `locations` (rustc-style
        // multi-span) must still match on path, via both query and --path.
        let mut f = mk_finding(FindingKind::BuildError, Severity::Error, None, "borrow", None, "fp");
        f.locations = vec![
            Location { path: "src/lib.rs".into(), line: Some(1), column: None, end_line: None, end_column: None },
            Location { path: "src/borrow.rs".into(), line: Some(9), column: None, end_line: None, end_column: None },
        ];
        let ms = vec![mk_manifest('a', "cargo", Status::Failed, None, vec![f])];

        assert_eq!(search_manifests(&ms, &SearchFilters { path: Some("borrow.rs".into()), ..Default::default() }).len(), 1);
        assert_eq!(search_manifests(&ms, &SearchFilters { query: Some("lib.rs".into()), ..Default::default() }).len(), 1);
        assert!(search_manifests(&ms, &SearchFilters { path: Some("missing.rs".into()), ..Default::default() }).is_empty());
    }

    #[test]
    fn search_returns_every_matching_finding_in_a_capture() {
        let ms = vec![mk_manifest(
            'b',
            "cargo",
            Status::Failed,
            None,
            vec![
                mk_finding(FindingKind::BuildError, Severity::Error, None, "e1", Some("a.rs"), "f1"),
                mk_finding(FindingKind::BuildError, Severity::Error, None, "e2", Some("b.rs"), "f2"),
                mk_finding(FindingKind::Diagnostic, Severity::Warning, None, "w1", Some("c.rs"), "f3"),
            ],
        )];
        let hits = search_manifests(&ms, &SearchFilters { severity: Some("error".into()), ..Default::default() });
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].findings.len(), 2, "both error findings should be reported");
    }

    #[test]
    fn search_combines_manifest_and_finding_level_filters() {
        let ms = vec![
            // Right branch, but its finding is a warning.
            mk_manifest('a', "cargo", Status::Failed, Some("dev"), vec![mk_finding(FindingKind::Diagnostic, Severity::Warning, None, "w", Some("a.rs"), "f1")]),
            // Right branch AND an error finding → the only hit.
            mk_manifest('b', "cargo", Status::Failed, Some("dev"), vec![mk_finding(FindingKind::BuildError, Severity::Error, None, "e", Some("b.rs"), "f2")]),
            // Error finding but wrong branch.
            mk_manifest('c', "cargo", Status::Failed, Some("main"), vec![mk_finding(FindingKind::BuildError, Severity::Error, None, "e", Some("c.rs"), "f3")]),
        ];
        let hits = search_manifests(&ms, &SearchFilters { branch: Some("dev".into()), severity: Some("error".into()), ..Default::default() });
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].findings[0].message, "e");
    }

    #[test]
    fn search_since_boundary_is_inclusive() {
        let cutoff = "2026-03-01T00:00:00.000000Z";
        let mut exact = one_finding_manifest();
        exact.timestamp = cutoff.into();
        let ms = vec![exact];
        // since uses `<` to exclude, so a timestamp equal to the cutoff is kept.
        let hits = search_manifests(&ms, &SearchFilters { since: Some(cutoff.into()), ..Default::default() });
        assert_eq!(hits.len(), 1, "an exact-boundary timestamp must be included");
    }

    #[test]
    fn search_preserves_input_order() {
        let ms = vec![
            mk_manifest('1', "t", Status::Failed, None, vec![mk_finding(FindingKind::TestFailure, Severity::Failure, None, "boom", None, "f1")]),
            mk_manifest('2', "t", Status::Failed, None, vec![mk_finding(FindingKind::TestFailure, Severity::Failure, None, "boom", None, "f2")]),
            mk_manifest('3', "t", Status::Failed, None, vec![mk_finding(FindingKind::TestFailure, Severity::Failure, None, "boom", None, "f3")]),
        ];
        let hits = search_manifests(&ms, &SearchFilters { query: Some("boom".into()), ..Default::default() });
        let order: Vec<&str> = hits.iter().map(|h| h.findings[0].fingerprint.as_str()).collect();
        assert_eq!(order, vec!["f1", "f2", "f3"], "hits keep the caller's ordering");
    }

    #[test]
    fn search_query_matches_detail_expected_actual_and_id() {
        let mut f = mk_finding(FindingKind::TestFailure, Severity::Failure, None, "plain message", None, "fp");
        f.id = Some("tests::case_alpha".into());
        f.detail = Some("traceback line referencing widget".into());
        f.expected = Some("HTTP 200".into());
        f.actual = Some("HTTP 503".into());
        let ms = vec![mk_manifest('a', "pytest", Status::Failed, None, vec![f])];

        for term in ["case_alpha", "widget", "200", "503"] {
            let hits = search_manifests(&ms, &SearchFilters { query: Some(term.into()), ..Default::default() });
            assert_eq!(hits.len(), 1, "query '{term}' should match a finding field");
        }
    }

    #[test]
    fn search_empty_query_matches_all_findings() {
        // An empty query string is a no-op filter (every finding "contains" "").
        let ms = vec![one_finding_manifest()];
        let hits = search_manifests(&ms, &SearchFilters { query: Some(String::new()), ..Default::default() });
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].findings.len(), 1);
    }

    #[test]
    fn search_status_and_tool_exclude_captures_without_structure() {
        // Manifest with no structured result can't satisfy a structured gate.
        let bare = manifest_with_oid(&hex64('e'));
        assert!(bare.structured.is_none());
        let ms = vec![bare];
        assert!(search_manifests(&ms, &SearchFilters { status: Some("failed".into()), ..Default::default() }).is_empty());
        assert!(search_manifests(&ms, &SearchFilters { tool: Some("pytest".into()), ..Default::default() }).is_empty());
    }

    #[test]
    fn search_fingerprint_prefix_is_case_sensitive() {
        let ms = vec![one_finding_manifest()]; // fingerprint "fp-aaaa-1111"
        assert_eq!(search_manifests(&ms, &SearchFilters { fingerprint: Some("fp-a".into()), ..Default::default() }).len(), 1);
        // Fingerprints are lowercase hex-ish tokens; matching is exact-prefix, not folded.
        assert!(search_manifests(&ms, &SearchFilters { fingerprint: Some("FP-A".into()), ..Default::default() }).is_empty());
    }
}
