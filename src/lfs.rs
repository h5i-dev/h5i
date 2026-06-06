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

    fn batch(&self, operation: &str, objects: Vec<ObjId>) -> Result<Vec<BatchObject>, H5iError> {
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
        let resp = req
            .send()
            .map_err(|e| H5iError::Internal(format!("LFS batch request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(H5iError::Internal(format!(
                "LFS batch {operation} returned HTTP {} (check remote LFS support / credentials)",
                resp.status()
            )));
        }
        let parsed: BatchResponse = resp
            .json()
            .map_err(|e| H5iError::Internal(format!("LFS batch response parse failed: {e}")))?;
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
        if !had_authz {
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
    pub fn upload<F>(&self, objs: &[ObjId], load: F) -> Result<usize, H5iError>
    where
        F: Fn(&str) -> Result<Option<Vec<u8>>, H5iError>,
    {
        let resp = self.batch("upload", objs.to_vec())?;
        let mut uploaded = 0;
        for o in resp {
            if let Some(e) = &o.error {
                return Err(H5iError::Internal(format!(
                    "LFS upload of {} rejected: {}",
                    o.oid, e.message
                )));
            }
            let Some(action) = o.actions.as_ref().and_then(|a| a.upload.as_ref()) else {
                continue; // no upload action ⇒ already present on the server
            };
            let Some(bytes) = load(&o.oid)? else { continue };
            if sha256_hex(&bytes) != o.oid {
                return Err(H5iError::Internal(format!(
                    "LFS upload of {} aborted: local bytes fail content-address check",
                    o.oid
                )));
            }
            let req = self.http.put(&action.href).body(bytes);
            let req = self.auth_for_action(req, action);
            let r = req
                .send()
                .map_err(|e| H5iError::Internal(format!("LFS upload transfer failed: {e}")))?;
            if !r.status().is_success() {
                return Err(H5iError::Internal(format!(
                    "LFS upload of {} returned HTTP {}",
                    o.oid,
                    r.status()
                )));
            }
            uploaded += 1;
        }
        Ok(uploaded)
    }

    /// Download the requested objects, verifying each against its oid and handing
    /// it to `sink(oid, bytes)` **one at a time** (so huge blobs stream straight
    /// to disk). Missing/errored objects are omitted. Returns the count handed
    /// to the sink.
    pub fn download<F>(&self, want: &[ObjId], mut sink: F) -> Result<usize, H5iError>
    where
        F: FnMut(&str, &[u8]) -> Result<(), H5iError>,
    {
        let resp = self.batch("download", want.to_vec())?;
        let mut got = 0;
        for o in resp {
            if o.error.is_some() {
                continue;
            }
            let Some(action) = o.actions.as_ref().and_then(|a| a.download.as_ref()) else {
                continue;
            };
            let req = self.http.get(&action.href);
            let req = self.auth_for_action(req, action);
            let r = req
                .send()
                .map_err(|e| H5iError::Internal(format!("LFS download transfer failed: {e}")))?;
            if !r.status().is_success() {
                continue;
            }
            let bytes = r
                .bytes()
                .map_err(|e| H5iError::Internal(format!("LFS download read failed: {e}")))?;
            // Content-address check — never trust the transferred bytes blindly.
            if sha256_hex(&bytes) != o.oid {
                return Err(H5iError::Internal(format!(
                    "LFS download of {} failed content-address check",
                    o.oid
                )));
            }
            sink(&o.oid, &bytes)?;
            got += 1;
        }
        Ok(got)
    }

    /// Convenience: download a single object's bytes (used by lazy `recall`).
    pub fn download_one(&self, oid: &str, size: u64) -> Result<Option<Vec<u8>>, H5iError> {
        let mut found = None;
        self.download(&[ObjId { oid: oid.to_string(), size }], |_, b| {
            found = Some(b.to_vec());
            Ok(())
        })?;
        Ok(found)
    }

    /// True if `endpoint` is reachable for a single object download (used to
    /// detect whether the remote actually supports LFS before relying on it).
    pub fn has(&self, oid: &str, size: u64) -> bool {
        self.batch("download", vec![ObjId { oid: oid.to_string(), size }])
            .map(|objs| objs.iter().any(|o| o.oid == oid && o.error.is_none()))
            .unwrap_or(false)
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
