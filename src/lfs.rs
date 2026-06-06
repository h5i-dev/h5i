//! Native Git LFS Batch-API client — a shareable raw-blob backend that stores
//! objects on the remote's LFS server (content-addressed by sha256), so huge
//! tool output never bloats the git object database.
//!
//! This talks the LFS Batch protocol directly over HTTP (reusing the existing
//! `reqwest` dependency) — it does **not** shell out to the `git lfs` CLI and
//! does not use LFS's working-tree / pointer-file model. The manifest's
//! `raw_oid` (already in `refs/h5i/objects`) is the pointer; the bytes live in
//! LFS. Auth is resolved via `git credential fill` for the remote host.
//!
//! Only HTTP(S) remotes are supported natively; for SSH/other remotes the
//! caller falls back to the git-ref store ([`crate::objects::GitRefStore`]).
//!
//! NOTE: the actual transfer is exercised against a live LFS server; the pure
//! pieces (endpoint derivation, batch (de)serialization, content verification)
//! are unit-tested here.

use crate::error::H5iError;
use crate::objects::sha256_hex;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

const LFS_MIME: &str = "application/vnd.git-lfs+json";

/// Outcome of an LFS operation, split so callers can decide on fallback safely.
#[derive(Debug, thiserror::Error)]
pub enum LfsError {
    /// The remote clearly does not speak LFS (batch endpoint 404/501). The ONLY
    /// case where `--backend auto` may quietly fall back to the git-ref store.
    #[error("remote does not support LFS: {0}")]
    Unsupported(String),
    /// Auth/permission, content-address, malformed-response, or network/timeout
    /// failure — must be surfaced, NEVER silently routed to the git-ref store
    /// (that would defeat the reason LFS is the default for huge objects).
    #[error(transparent)]
    Fatal(#[from] H5iError),
}

impl LfsError {
    pub fn is_unsupported(&self) -> bool {
        matches!(self, LfsError::Unsupported(_))
    }
    pub fn fatal(msg: impl Into<String>) -> Self {
        LfsError::Fatal(H5iError::Internal(msg.into()))
    }
}

/// Classify a batch-endpoint HTTP status. Only a missing/not-implemented
/// endpoint counts as "LFS unsupported"; everything else (incl. 401/403) is
/// fatal so auto-fallback can't mask an auth/permission problem.
fn classify_batch_status(status: u16) -> LfsError {
    if status == 404 || status == 501 {
        LfsError::Unsupported(format!("batch endpoint returned HTTP {status}"))
    } else {
        LfsError::Fatal(H5iError::Internal(format!(
            "LFS batch returned HTTP {status} (auth/permission or server error)"
        )))
    }
}

/// Whether two URLs share scheme + host + port (so reusing the git host's Basic
/// credentials is safe — they must never go to a presigned third-party URL).
fn same_origin(a: &str, b: &str) -> bool {
    match (split_url(a), split_url(b)) {
        (Some((pa, ha, _)), Some((pb, hb, _))) => {
            pa.eq_ignore_ascii_case(&pb) && ha.eq_ignore_ascii_case(&hb)
        }
        _ => false,
    }
}

/// Derive the LFS API endpoint from a git remote URL, or `None` for non-HTTP(S)
/// remotes (which fall back to the git-ref store). GitHub/GitLab/Gitea all
/// expose `<repo>.git/info/lfs`.
pub fn endpoint_for_remote(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return None; // ssh:// , git@host:... , file:// → caller falls back
    }
    let base = url.strip_suffix(".git").unwrap_or(url);
    Some(format!("{base}.git/info/lfs"))
}

/// Split an `http(s)://host[:port]/path` URL into (protocol, host, path) for
/// `git credential`. Path has no leading slash (git credential convention).
fn split_url(url: &str) -> Option<(String, String, String)> {
    let (proto, rest) = url.split_once("://")?;
    let (host, path) = match rest.split_once('/') {
        Some((h, p)) => (h.to_string(), p.to_string()),
        None => (rest.to_string(), String::new()),
    };
    Some((proto.to_string(), host, path))
}

/// Ask `git credential fill` for credentials for `url`. Returns
/// `(username, password)` or `None` if no helper/credentials are configured
/// (anonymous access — e.g. a public repo).
fn git_credential_fill(workdir: &Path, url: &str) -> Option<(String, String)> {
    let (protocol, host, path) = split_url(url)?;
    let input = format!("protocol={protocol}\nhost={host}\npath={path}\n\n");
    let out = std::process::Command::new("git")
        .args(["credential", "fill"])
        .current_dir(workdir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take()?.write_all(input.as_bytes()).ok()?;
            child.wait_with_output().ok()
        })?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut user = None;
    let mut pass = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("username=") {
            user = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("password=") {
            pass = Some(v.to_string());
        }
    }
    Some((user.unwrap_or_default(), pass?))
}

// ── Batch protocol types ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct BatchRequest<'a> {
    operation: &'a str,
    transfers: [&'a str; 1],
    objects: Vec<ObjId>,
    hash_algo: &'a str,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ObjId {
    pub oid: String,
    pub size: u64,
}

#[derive(Deserialize, Default)]
struct BatchResponse {
    #[serde(default)]
    objects: Vec<BatchObject>,
}

#[derive(Deserialize)]
struct BatchObject {
    oid: String,
    #[serde(default)]
    actions: Option<Actions>,
    #[serde(default)]
    error: Option<ObjError>,
}

#[derive(Deserialize, Default)]
struct Actions {
    #[serde(default)]
    upload: Option<Action>,
    #[serde(default)]
    download: Option<Action>,
}

#[derive(Deserialize)]
struct Action {
    href: String,
    #[serde(default)]
    header: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct ObjError {
    #[allow(dead_code)]
    code: i64,
    message: String,
}

/// A client bound to one remote's LFS endpoint.
pub struct LfsClient {
    endpoint: String,
    http: Client,
    /// Basic-auth `(username, password)` for the endpoint host, if resolved.
    auth: Option<(String, String)>,
}

impl LfsClient {
    /// Build a client for `remote_url`, or `None` if it isn't an HTTP(S) remote
    /// (the caller then uses the git-ref store). Resolves credentials eagerly.
    pub fn for_remote(workdir: &Path, remote_url: &str) -> Option<LfsClient> {
        let endpoint = endpoint_for_remote(remote_url)?;
        let http = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .ok()?;
        let auth = git_credential_fill(workdir, &endpoint);
        Some(LfsClient { endpoint, http, auth })
    }

    fn batch(&self, operation: &str, objects: Vec<ObjId>) -> Result<Vec<BatchObject>, LfsError> {
        if objects.is_empty() {
            return Ok(Vec::new());
        }
        let body = BatchRequest {
            operation,
            transfers: ["basic"],
            objects,
            hash_algo: "sha256",
        };
        let mut req = self
            .http
            .post(format!("{}/objects/batch", self.endpoint))
            .header("Accept", LFS_MIME)
            .header("Content-Type", LFS_MIME)
            .json(&body);
        if let Some((u, p)) = &self.auth {
            req = req.basic_auth(u, Some(p));
        }
        let resp = req.send().map_err(|e| {
            // A transport failure is fatal — do NOT let it trigger git-ref fallback.
            LfsError::Fatal(H5iError::Internal(format!("LFS batch request failed: {e}")))
        })?;
        if !resp.status().is_success() {
            return Err(classify_batch_status(resp.status().as_u16()));
        }
        let parsed: BatchResponse = resp.json().map_err(|e| {
            LfsError::Fatal(H5iError::Internal(format!("LFS batch response parse failed: {e}")))
        })?;
        Ok(parsed.objects)
    }

    /// Apply a transfer action's headers, plus Basic auth as a fallback when the
    /// action href targets our endpoint host and carried no Authorization.
    fn auth_for_action(
        &self,
        mut req: reqwest::blocking::RequestBuilder,
        action: &Action,
    ) -> reqwest::blocking::RequestBuilder {
        let mut had_authz = false;
        for (k, v) in &action.header {
            if k.eq_ignore_ascii_case("authorization") {
                had_authz = true;
            }
            req = req.header(k, v);
        }
        // Only reuse the git host's Basic credentials when the transfer URL is
        // the SAME ORIGIN as the LFS endpoint. LFS commonly returns presigned
        // third-party URLs (S3/GCS/Azure) — never leak git creds to those.
        if !had_authz && same_origin(&self.endpoint, &action.href) {
            if let Some((u, p)) = &self.auth {
                req = req.basic_auth(u, Some(p));
            }
        }
        req
    }

    /// Upload the objects the server is missing. `objs` carries `(oid, size)`
    /// only (cheap, from manifests); `load(oid)` is called to fetch the bytes
    /// **lazily, one at a time**, so huge blobs are never all held in memory.
    /// Already-present objects (no `upload` action in the batch) are skipped.
    /// Returns the number actually transferred. Idempotent.
    pub fn upload<F>(&self, objs: &[ObjId], load: F) -> Result<usize, LfsError>
    where
        F: Fn(&str) -> Result<Option<Vec<u8>>, H5iError>,
    {
        let resp = self.batch("upload", objs.to_vec())?;
        let mut uploaded = 0;
        for o in resp {
            if let Some(e) = &o.error {
                return Err(LfsError::Fatal(H5iError::Internal(format!(
                    "LFS upload of {} rejected: {}",
                    o.oid, e.message
                ))));
            }
            let Some(action) = o.actions.as_ref().and_then(|a| a.upload.as_ref()) else {
                continue; // no upload action ⇒ already present on the server
            };
            let Some(bytes) = load(&o.oid)? else { continue };
            if sha256_hex(&bytes) != o.oid {
                return Err(LfsError::Fatal(H5iError::Internal(format!(
                    "LFS upload of {} aborted: local bytes fail content-address check",
                    o.oid
                ))));
            }
            let req = self.http.put(&action.href).body(bytes);
            let req = self.auth_for_action(req, action);
            let r = req.send().map_err(|e| {
                LfsError::Fatal(H5iError::Internal(format!("LFS upload transfer failed: {e}")))
            })?;
            if !r.status().is_success() {
                return Err(LfsError::Fatal(H5iError::Internal(format!(
                    "LFS upload of {} returned HTTP {}",
                    o.oid,
                    r.status()
                ))));
            }
            uploaded += 1;
        }
        Ok(uploaded)
    }

    /// Download the requested objects, verifying each against its oid and handing
    /// it to `sink(oid, bytes)` **one blob at a time** (the whole set is never
    /// held in memory at once; each blob is buffered fully then handed off).
    /// Returns `(fetched, missing)` — `missing` counts requested objects the
    /// server reported an error for or couldn't transfer (so the caller can
    /// report it rather than silently succeeding with zero).
    pub fn download<F>(&self, want: &[ObjId], mut sink: F) -> Result<(usize, usize), LfsError>
    where
        F: FnMut(&str, &[u8]) -> Result<(), H5iError>,
    {
        let resp = self.batch("download", want.to_vec())?;
        let mut got = 0;
        let mut missing = 0;
        for o in resp {
            if o.error.is_some() {
                missing += 1; // per-object error (e.g. 404) — NOT "LFS unavailable"
                continue;
            }
            let Some(action) = o.actions.as_ref().and_then(|a| a.download.as_ref()) else {
                missing += 1;
                continue;
            };
            let req = self.http.get(&action.href);
            let req = self.auth_for_action(req, action);
            let r = req.send().map_err(|e| {
                LfsError::Fatal(H5iError::Internal(format!("LFS download transfer failed: {e}")))
            })?;
            if !r.status().is_success() {
                missing += 1;
                continue;
            }
            let bytes = r.bytes().map_err(|e| {
                LfsError::Fatal(H5iError::Internal(format!("LFS download read failed: {e}")))
            })?;
            // Content-address check — never trust the transferred bytes blindly.
            if sha256_hex(&bytes) != o.oid {
                return Err(LfsError::Fatal(H5iError::Internal(format!(
                    "LFS download of {} failed content-address check",
                    o.oid
                ))));
            }
            sink(&o.oid, &bytes)?;
            got += 1;
        }
        Ok((got, missing))
    }

    /// Convenience: download a single object's bytes (used by lazy `recall`).
    pub fn download_one(&self, oid: &str, size: u64) -> Result<Option<Vec<u8>>, LfsError> {
        let mut found = None;
        self.download(&[ObjId { oid: oid.to_string(), size }], |_, b| {
            found = Some(b.to_vec());
            Ok(())
        })?;
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_derivation() {
        assert_eq!(
            endpoint_for_remote("https://github.com/o/r.git").unwrap(),
            "https://github.com/o/r.git/info/lfs"
        );
        assert_eq!(
            endpoint_for_remote("https://github.com/o/r").unwrap(),
            "https://github.com/o/r.git/info/lfs"
        );
        assert_eq!(
            endpoint_for_remote("https://gitlab.com/o/r/").unwrap(),
            "https://gitlab.com/o/r.git/info/lfs"
        );
        // Non-HTTP remotes → no native LFS (fall back to git-ref store).
        assert!(endpoint_for_remote("git@github.com:o/r.git").is_none());
        assert!(endpoint_for_remote("ssh://git@host/o/r.git").is_none());
        assert!(endpoint_for_remote("/srv/git/r.git").is_none());
    }

    #[test]
    fn split_url_parts() {
        assert_eq!(
            split_url("https://github.com/o/r.git/info/lfs").unwrap(),
            ("https".into(), "github.com".into(), "o/r.git/info/lfs".into())
        );
        assert_eq!(
            split_url("http://host:8080/x").unwrap(),
            ("http".into(), "host:8080".into(), "x".into())
        );
    }

    #[test]
    fn batch_request_serializes_to_lfs_shape() {
        let body = BatchRequest {
            operation: "upload",
            transfers: ["basic"],
            objects: vec![ObjId { oid: "abc".into(), size: 12 }],
            hash_algo: "sha256",
        };
        let j = serde_json::to_value(&body).unwrap();
        assert_eq!(j["operation"], "upload");
        assert_eq!(j["transfers"][0], "basic");
        assert_eq!(j["hash_algo"], "sha256");
        assert_eq!(j["objects"][0]["oid"], "abc");
        assert_eq!(j["objects"][0]["size"], 12);
    }

    fn client_with_auth(endpoint: &str) -> LfsClient {
        LfsClient {
            endpoint: endpoint.to_string(),
            http: Client::new(),
            auth: Some(("u".into(), "p".into())),
        }
    }

    fn authz_of(req: reqwest::blocking::RequestBuilder) -> Option<String> {
        let r = req.build().unwrap();
        r.headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_string())
    }

    fn action(href: &str, hdr: &[(&str, &str)]) -> Action {
        Action {
            href: href.to_string(),
            header: hdr.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    #[test]
    fn basic_auth_only_leaks_to_same_origin() {
        let c = client_with_auth("https://git.example.com/o/r.git/info/lfs");

        // Same origin as the endpoint → reuse git Basic creds.
        let a = action("https://git.example.com/transfer/abc", &[]);
        let req = c.auth_for_action(c.http.get(&a.href), &a);
        assert!(authz_of(req).unwrap().starts_with("Basic "));

        // Presigned third-party URL → MUST NOT carry the git credentials.
        let a = action("https://s3.amazonaws.com/bucket/abc?sig=x", &[]);
        let req = c.auth_for_action(c.http.get(&a.href), &a);
        assert!(authz_of(req).is_none(), "git creds leaked to a third-party URL");

        // Action carries its own Authorization → use it, don't add Basic.
        let a = action("https://s3.amazonaws.com/bucket/abc", &[("Authorization", "Bearer t123")]);
        let req = c.auth_for_action(c.http.get(&a.href), &a);
        assert_eq!(authz_of(req).unwrap(), "Bearer t123");
    }

    #[test]
    fn same_origin_compares_scheme_host_port() {
        assert!(same_origin("https://h/x", "https://h/y"));
        assert!(!same_origin("https://h/x", "http://h/y")); // scheme
        assert!(!same_origin("https://h:1/x", "https://h:2/y")); // port (host carries it)
        assert!(!same_origin("https://a/x", "https://b/y")); // host
    }

    #[test]
    fn only_404_501_are_unsupported() {
        assert!(classify_batch_status(404).is_unsupported());
        assert!(classify_batch_status(501).is_unsupported());
        for s in [400u16, 401, 403, 409, 500, 503] {
            assert!(!classify_batch_status(s).is_unsupported(), "HTTP {s} must be fatal");
        }
    }

    #[test]
    fn batch_response_parses_actions_and_errors() {
        let raw = r#"{"transfer":"basic","objects":[
            {"oid":"a","size":1,"actions":{"download":{"href":"https://x/a","header":{"Authorization":"Bearer t"}}}},
            {"oid":"b","size":2,"error":{"code":404,"message":"missing"}}
        ]}"#;
        let parsed: BatchResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.objects.len(), 2);
        assert!(parsed.objects[0].actions.as_ref().unwrap().download.is_some());
        assert_eq!(
            parsed.objects[0].actions.as_ref().unwrap().download.as_ref().unwrap().header
                .get("Authorization").unwrap(),
            "Bearer t"
        );
        assert_eq!(parsed.objects[1].error.as_ref().unwrap().message, "missing");
    }
}
