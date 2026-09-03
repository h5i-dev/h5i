//! What a caller changes about a request before it goes out again.
//!
//! The point of owning the HTTP client is that nobody has to hand-maintain
//! `Content-Length`. An edit names a *part* of a request (a query parameter, a
//! header, a field inside a JSON body) and the engine rebuilds a well-formed
//! message around the change. String surgery on a raw request is the thing this
//! exists to replace.
//!
//! ```text
//! method=POST                     the verb
//! url=https://host/path?a=1       the whole target
//! path=/api/v2/users/456          the path, keeping the query
//! query.user_id=456               one query parameter
//! header.X-Forwarded-For=127.0.0.1
//! cookie.session=forged           one cookie, inside the Cookie header
//! json.user.role=admin            one field of a JSON body
//! form.username=admin             one field of a form body
//! body.raw=<bytes>                the whole body
//! ```
//!
//! Three rules are worth stating because they are what make an edit trustworthy.
//!
//! **Nothing is silently corrected.** A JSON edit against a body that is not
//! JSON is an error naming the content type that body actually has. An edit
//! whose path does not exist is an error unless the caller asked for it to be
//! created. A typo that quietly does nothing is a wrong answer wearing a right
//! answer's clothes, and it is the failure mode that wastes the most time in an
//! HTTP workbench, because the response looks plausible either way.
//!
//! **What was applied is reported.** [`Applied`] carries one line per edit, so
//! the record of an attempt is a record of what was sent rather than of what was
//! requested.
//!
//! **Values are text unless they are obviously not.** For `json.` targets the
//! value is parsed as JSON when it parses (`456` is a number, `true` a boolean,
//! `{"a":1}` an object) and is a string otherwise, which is what a caller means
//! nearly every time. `json.id="456"` is how to insist on the string.

use std::fmt;

use serde::{Deserialize, Serialize};
use url::Url;
use url::form_urlencoded;

/// The part of a request an edit names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Target {
    /// The request method.
    Method,
    /// The whole URL, replaced.
    Url,
    /// The path, keeping the query that was there.
    Path,
    /// One query parameter, by name.
    Query(String),
    /// One header, by name. Replaces every copy of that name.
    Header(String),
    /// One cookie, inside the `Cookie` header.
    Cookie(String),
    /// One field of a JSON body, by dotted path (`user.role`, `items.0.id`).
    Json(String),
    /// One field of a form-encoded body.
    Form(String),
    /// One part of a `multipart/form-data` body, and which of its three
    /// interesting pieces.
    Multipart { field: String, piece: Piece },
    /// The whole body.
    BodyRaw,
}

/// Which part of a multipart part an edit names.
///
/// Three, because a file upload has three separately-checked things in it and a
/// filter usually reads a different one from the one that decides where the
/// bytes land: the name the server stores it under, the type it claims to be,
/// and the content itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Piece {
    /// The bytes.
    Data,
    /// The declared filename. Where a traversal or a double extension goes.
    Filename,
    /// The declared `Content-Type`. What a type filter reads.
    ContentType,
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Method => write!(f, "method"),
            Target::Url => write!(f, "url"),
            Target::Path => write!(f, "path"),
            Target::Query(name) => write!(f, "query.{name}"),
            Target::Header(name) => write!(f, "header.{name}"),
            Target::Cookie(name) => write!(f, "cookie.{name}"),
            Target::Json(path) => write!(f, "json.{path}"),
            Target::Form(name) => write!(f, "form.{name}"),
            Target::Multipart { field, piece } => match piece {
                Piece::Data => write!(f, "multipart.{field}"),
                Piece::Filename => write!(f, "multipart.{field}.filename"),
                Piece::ContentType => write!(f, "multipart.{field}.content_type"),
            },
            Target::BodyRaw => write!(f, "body.raw"),
        }
    }
}

/// One change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    pub target: Target,
    /// `None` removes it. Only some targets can be removed, and the ones that
    /// cannot say so rather than treating a removal as setting an empty value:
    /// a request with no method is not a request, and a URL-less one has nowhere
    /// to go.
    pub value: Option<Vec<u8>>,
}

/// What went wrong, in the terms the caller used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditError {
    pub target: String,
    pub message: String,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.target, self.message)
    }
}

impl EditError {
    fn new(target: impl fmt::Display, message: impl Into<String>) -> Self {
        Self {
            target: target.to_string(),
            message: message.into(),
        }
    }
}

/// One edit, as it was applied.
///
/// `was` is what the request held before. It is what makes a replay's record
/// readable a week later, and what tells a caller that the parameter they
/// "changed" already had that value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    pub target: String,
    /// The new value, as text where it is text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// What was there before, when there was something and it was text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was: Option<String>,
    /// Set when the target did not exist and was created.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub created: bool,
}

/// A request, in the parts an edit acts on.
///
/// Deliberately not [`crate::broker::Fetch`]: this is the editable shape, and a
/// `Fetch` is what the broker is handed once editing is done. Keeping them
/// apart means an edit cannot accidentally set a field the broker treats as its
/// own.
#[derive(Debug, Clone, PartialEq)]
pub struct Editable {
    pub method: String,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Editable {
    /// The `Content-Type`, which decides what a `json.` or `form.` edit means.
    pub fn content_type(&self) -> Option<&str> {
        self.headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.as_str())
    }

    fn set_header(&mut self, name: &str, value: &str) -> Option<String> {
        let mut previous = None;
        let mut placed = false;
        // Rewritten where it sits, rather than removed and appended: header
        // order is observable on the wire, and a workbench that quietly moved a
        // header to the end would be reproducing something other than the
        // request it was given. Later copies of the same name are dropped, since
        // the caller asked for one value.
        self.headers.retain_mut(|(existing, current)| {
            if !existing.eq_ignore_ascii_case(name) {
                return true;
            }
            if previous.is_none() {
                previous = Some(current.clone());
                *current = value.to_string();
                placed = true;
                return true;
            }
            false
        });
        if !placed {
            self.headers.push((name.to_string(), value.to_string()));
        }
        previous
    }

    fn remove_header(&mut self, name: &str) -> Option<String> {
        let mut previous = None;
        self.headers.retain(|(existing, current)| {
            if existing.eq_ignore_ascii_case(name) {
                if previous.is_none() {
                    previous = Some(current.clone());
                }
                return false;
            }
            true
        });
        previous
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Parse `target=value` as one edit.
///
/// The value is everything after the first `=`, so a value containing `=` (a
/// base64 payload, a JWT, a SQL clause) needs no escaping. That matters more
/// than it looks: the payloads this is for are exactly the strings that are
/// awkward in a shell.
pub fn parse_set(spec: &str) -> Result<Edit, EditError> {
    let (target, value) = spec
        .split_once('=')
        .ok_or_else(|| EditError::new(spec, "an edit is `target=value`, and this has no `=`"))?;
    Ok(Edit {
        target: parse_target(target)?,
        value: Some(value.as_bytes().to_vec()),
    })
}

/// Parse a bare target, for a removal.
pub fn parse_unset(spec: &str) -> Result<Edit, EditError> {
    Ok(Edit {
        target: parse_target(spec)?,
        value: None,
    })
}

fn parse_target(spec: &str) -> Result<Target, EditError> {
    let spec = spec.trim();
    if let Some((kind, name)) = spec.split_once('.') {
        let name = name.trim();
        if name.is_empty() {
            return Err(EditError::new(spec, "names the kind but not which one"));
        }
        return match kind.trim().to_ascii_lowercase().as_str() {
            "query" => Ok(Target::Query(name.to_string())),
            "header" => Ok(Target::Header(name.to_string())),
            "cookie" => Ok(Target::Cookie(name.to_string())),
            "json" => Ok(Target::Json(name.trim_start_matches("$.").to_string())),
            "form" => Ok(Target::Form(name.to_string())),
            "body" if name.eq_ignore_ascii_case("raw") => Ok(Target::BodyRaw),
            "body" => Err(EditError::new(
                spec,
                "the only body target is `body.raw`; to change one field use `json.` or `form.`",
            )),
            "multipart" => {
                // `multipart.avatar`, `multipart.avatar.filename`,
                // `multipart.avatar.content_type`. The field name comes first
                // because that is how a person says it out loud.
                let (field, piece) = match name.rsplit_once('.') {
                    Some((field, "filename")) => (field, Piece::Filename),
                    Some((field, "content_type")) | Some((field, "content-type")) => {
                        (field, Piece::ContentType)
                    }
                    _ => (name, Piece::Data),
                };
                if field.is_empty() {
                    return Err(EditError::new(spec, "names a piece but not which field"));
                }
                Ok(Target::Multipart {
                    field: field.to_string(),
                    piece,
                })
            }
            other => Err(EditError::new(
                spec,
                format!(
                    "`{other}` is not something this engine can edit. \
                     Try method, url, path, query., header., cookie., json., form. or body.raw"
                ),
            )),
        };
    }
    match spec.to_ascii_lowercase().as_str() {
        "method" => Ok(Target::Method),
        "url" => Ok(Target::Url),
        "path" => Ok(Target::Path),
        other => Err(EditError::new(
            spec,
            format!(
                "`{other}` is not something this engine can edit. \
                 Try method, url, path, query., header., cookie., json., form. or body.raw"
            ),
        )),
    }
}

/// Is this path segment a dot segment, in any of the spellings a URL parser
/// treats as one?
///
/// The URL standard resolves `.` and `..` away, and percent-decodes before
/// deciding — so `.%2e`, `%2e.` and `%2e%2e` are all "..", which is exactly what
/// the Apache traversal CVEs of 2021 are written with.
fn is_a_dot_segment(segment: &str) -> bool {
    let decoded = segment
        .replace("%2e", ".")
        .replace("%2E", ".")
        .replace("%2f", "/")
        .replace("%2F", "/");
    decoded == "." || decoded == ".."
}

/// Refuse a path the URL parser would resolve rather than send.
///
/// A workbench that quietly straightened `/cgi-bin/.%2e/.%2e/etc/passwd` into
/// `/etc/passwd` would send a request nobody asked for and report its 404 as
/// evidence about the request that was asked for. That is worse than not being
/// able to send it: a false negative that looks like a finding.
///
/// So this says what happened and stops. h5i builds every request from a parsed
/// `Url`, and the URL standard resolves dot segments before the bytes exist —
/// there is no layer here where the original spelling survives. Sending a
/// request-target the parser would rewrite is a capability this engine does not
/// have yet; see `docs/design/design-websec.md`.
fn refuse_a_resolved_traversal(
    target: &impl fmt::Display,
    asked: &str,
    resolved: &str,
) -> Result<(), EditError> {
    // Whatever was named — a path or a whole URL — only its path can hold a
    // dot segment.
    let asked = match asked.split_once("://") {
        Some((_, rest)) => match rest.find('/') {
            Some(at) => &rest[at..],
            None => "/",
        },
        None => asked,
    };
    let asked_path = asked.split(['?', '#']).next().unwrap_or(asked);
    if !asked_path.split('/').any(is_a_dot_segment) {
        return Ok(());
    }
    Err(EditError::new(
        target,
        format!(
            "{asked_path:?} contains a dot segment, and the URL standard resolves those \
             before a request exists: this would have gone out as {resolved:?}. h5i has no \
             way to send a request-target its URL parser would rewrite, so it refuses rather \
             than testing a different request and reporting the answer as yours."
        ),
    ))
}

/// Apply every edit, in order, and say what each one did.
///
/// Order is the caller's, and it is applied rather than optimised: setting a
/// body and then a field of it is a different instruction from the reverse, and
/// an engine that reordered them would answer a question nobody asked.
///
/// `create` allows an edit to add a target that is not there. Off by default
/// because a query parameter that does not exist is usually a typo, and a typo
/// that silently succeeds costs an agent a whole turn spent reading a response
/// that was never going to differ.
pub fn apply(
    request: &mut Editable,
    edits: &[Edit],
    create: bool,
) -> Result<Vec<Applied>, EditError> {
    let mut applied = Vec::with_capacity(edits.len());
    for edit in edits {
        applied.push(apply_one(request, edit, create)?);
    }
    Ok(applied)
}

fn text(value: &[u8]) -> String {
    String::from_utf8_lossy(value).into_owned()
}

fn apply_one(request: &mut Editable, edit: &Edit, create: bool) -> Result<Applied, EditError> {
    let target = edit.target.to_string();
    let removing = edit.value.is_none();
    let value = edit.value.clone().unwrap_or_default();

    match &edit.target {
        Target::Method => {
            if removing {
                return Err(EditError::new(&target, "a request without a method is not one"));
            }
            let was = std::mem::replace(&mut request.method, text(&value).trim().to_ascii_uppercase());
            Ok(Applied {
                target,
                value: Some(request.method.clone()),
                was: Some(was),
                created: false,
            })
        }

        Target::Url => {
            if removing {
                return Err(EditError::new(&target, "a request without a URL has nowhere to go"));
            }
            let raw = text(&value);
            let parsed = Url::parse(raw.trim())
                .map_err(|e| EditError::new(&target, format!("{raw:?} is not a URL: {e}")))?;
            refuse_a_resolved_traversal(&target, raw.trim(), parsed.path())?;
            let was = std::mem::replace(&mut request.url, parsed);
            Ok(Applied {
                target,
                value: Some(request.url.to_string()),
                was: Some(was.to_string()),
                created: false,
            })
        }

        Target::Path => {
            if removing {
                return Err(EditError::new(&target, "every URL has a path; set it to `/` instead"));
            }
            let asked = text(&value);
            let asked = asked.trim();
            let was = request.url.path().to_string();
            let mut candidate = request.url.clone();
            candidate.set_path(asked);
            refuse_a_resolved_traversal(&target, asked, candidate.path())?;
            request.url = candidate;
            Ok(Applied {
                target,
                value: Some(request.url.path().to_string()),
                was: Some(was),
                created: false,
            })
        }

        Target::Query(name) => {
            let pairs: Vec<(String, String)> = request
                .url
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let was = pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone());
            if was.is_none() && !create && !removing {
                return Err(missing(&target, "query parameter", &pairs));
            }
            let mut replaced = false;
            let mut next: Vec<(String, String)> = Vec::with_capacity(pairs.len() + 1);
            for (k, v) in pairs {
                if &k == name {
                    if removing {
                        continue;
                    }
                    // Only the first copy is replaced; a repeated parameter is a
                    // list, and collapsing it would change the request in a way
                    // nobody asked for.
                    if !replaced {
                        next.push((k, text(&value)));
                        replaced = true;
                        continue;
                    }
                }
                next.push((k, v));
            }
            if !replaced && !removing {
                next.push((name.clone(), text(&value)));
            }
            if next.is_empty() {
                request.url.set_query(None);
            } else {
                let mut serializer = form_urlencoded::Serializer::new(String::new());
                for (k, v) in &next {
                    serializer.append_pair(k, v);
                }
                request.url.set_query(Some(&serializer.finish()));
            }
            Ok(Applied {
                target,
                value: (!removing).then(|| text(&value)),
                created: was.is_none() && !removing,
                was,
            })
        }

        Target::Header(name) => {
            if crate::net::header_is_the_clients(name) {
                return Err(EditError::new(
                    &target,
                    "this header frames the message and is the client's to compute. \
                     Setting it would describe a request other than the one that goes out",
                ));
            }
            if removing {
                let was = request.remove_header(name);
                return Ok(Applied {
                    target,
                    value: None,
                    was,
                    created: false,
                });
            }
            let was = request.set_header(name, &text(&value));
            Ok(Applied {
                target,
                value: Some(text(&value)),
                created: was.is_none(),
                was,
            })
        }

        Target::Cookie(name) => {
            let current = request.header("cookie").unwrap_or_default().to_string();
            let mut pairs: Vec<(String, String)> = current
                .split(';')
                .filter_map(|pair| pair.split_once('='))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect();
            let was = pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
            if was.is_none() && !create && !removing {
                return Err(missing(&target, "cookie", &pairs));
            }
            if removing {
                pairs.retain(|(k, _)| k != name);
            } else if let Some(slot) = pairs.iter_mut().find(|(k, _)| k == name) {
                slot.1 = text(&value);
            } else {
                pairs.push((name.clone(), text(&value)));
            }
            if pairs.is_empty() {
                request.remove_header("cookie");
            } else {
                let joined = pairs
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("; ");
                request.set_header("Cookie", &joined);
            }
            Ok(Applied {
                target,
                value: (!removing).then(|| text(&value)),
                created: was.is_none() && !removing,
                was,
            })
        }

        Target::Json(path) => {
            // An empty body with `--set-create` is a body to *build*, the way a
            // multipart one is. A request that carries no JSON yet is the
            // ordinary starting point for an API call the page never makes, and
            // refusing it sent callers to `body.raw` to hand-write the JSON that
            // this edit exists to maintain.
            if request.body.is_empty() && create {
                request.body = b"{}".to_vec();
                if request.content_type().is_none() {
                    request.set_header("Content-Type", "application/json");
                }
            }
            let kind = request.content_type().unwrap_or("").to_string();
            let mut document: serde_json::Value = serde_json::from_slice(&request.body)
                .map_err(|e| {
                    EditError::new(
                        &target,
                        format!(
                            "this request's body is not JSON ({e}); its Content-Type is {}. \
                             Use `form.` for a form body, or `body.raw` for anything else",
                            if kind.is_empty() { "unset" } else { &kind }
                        ),
                    )
                })?;
            let was = json_at(&document, path).map(render_json);
            if was.is_none() && !create && !removing {
                return Err(EditError::new(
                    &target,
                    format!(
                        "this body has no `{path}`. Pass --set-create to add it, \
                         or check the field name against the stored request"
                    ),
                ));
            }
            if removing {
                json_remove(&mut document, path)
                    .map_err(|why| EditError::new(&target, why))?;
            } else {
                let parsed = serde_json::from_slice::<serde_json::Value>(&value)
                    .unwrap_or_else(|_| serde_json::Value::String(text(&value)));
                json_set(&mut document, path, parsed)
                    .map_err(|why| EditError::new(&target, why))?;
            }
            request.body = serde_json::to_vec(&document)
                .map_err(|e| EditError::new(&target, format!("could not rebuild the body: {e}")))?;
            Ok(Applied {
                target,
                value: (!removing).then(|| text(&value)),
                created: was.is_none() && !removing,
                was,
            })
        }

        Target::Form(name) => {
            let mut pairs: Vec<(String, String)> =
                form_urlencoded::parse(&request.body).into_owned().collect();
            let was = pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
            if was.is_none() && !create && !removing {
                return Err(missing(&target, "form field", &pairs));
            }
            if removing {
                pairs.retain(|(k, _)| k != name);
            } else if let Some(slot) = pairs.iter_mut().find(|(k, _)| k == name) {
                slot.1 = text(&value);
            } else {
                pairs.push((name.clone(), text(&value)));
            }
            let mut serializer = form_urlencoded::Serializer::new(String::new());
            for (k, v) in &pairs {
                serializer.append_pair(k, v);
            }
            request.body = serializer.finish().into_bytes();
            if request.content_type().is_none() {
                request.set_header("Content-Type", "application/x-www-form-urlencoded");
            }
            Ok(Applied {
                target,
                value: (!removing).then(|| text(&value)),
                created: was.is_none() && !removing,
                was,
            })
        }

        Target::Multipart { field, piece } => {
            let declared = request.content_type().unwrap_or("").to_string();
            let mut parts = match crate::multipart::boundary_of(&declared) {
                // Already multipart: take it apart, or say it is not the shape
                // it claims rather than quietly replacing it.
                Some(boundary) => crate::multipart::parse(&request.body, &boundary)
                    .ok_or_else(|| {
                        EditError::new(
                            &target,
                            "this request says it is multipart and its body does not match \
                             the boundary it declares, so there is nothing to edit safely",
                        )
                    })?,
                None if request.body.is_empty() || create => {
                    // Not multipart yet. With `--set-create` this *builds* an
                    // upload, which is the common case: a file-upload test
                    // usually has no recorded upload to start from, because the
                    // engine does not post files itself.
                    Vec::new()
                }
                None => {
                    return Err(EditError::new(
                        &target,
                        format!(
                            "this request's body is not multipart; its Content-Type is {}. \
                             Pass --set-create to build a multipart body instead",
                            if declared.is_empty() { "unset" } else { &declared }
                        ),
                    ));
                }
            };

            let at = parts.iter().position(|part| &part.name == field);
            let was = at.and_then(|at| match piece {
                Piece::Data => Some(String::from_utf8_lossy(&parts[at].data).into_owned()),
                Piece::Filename => parts[at].filename.clone(),
                Piece::ContentType => parts[at].content_type.clone(),
            });
            if at.is_none() && !create && !removing {
                let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
                return Err(EditError::new(
                    &target,
                    format!(
                        "no such part, so nothing would change. This request has: {}. \
                         Pass --set-create to add it",
                        if names.is_empty() {
                            "none".to_string()
                        } else {
                            names.join(", ")
                        }
                    ),
                ));
            }

            if removing {
                if let Some(at) = at {
                    match piece {
                        Piece::Data => {
                            parts.remove(at);
                        }
                        Piece::Filename => parts[at].filename = None,
                        Piece::ContentType => parts[at].content_type = None,
                    }
                }
            } else {
                let at = match at {
                    Some(at) => at,
                    None => {
                        parts.push(crate::multipart::Part {
                            name: field.clone(),
                            ..Default::default()
                        });
                        parts.len() - 1
                    }
                };
                match piece {
                    Piece::Data => parts[at].data = value.clone(),
                    Piece::Filename => parts[at].filename = Some(text(&value)),
                    Piece::ContentType => parts[at].content_type = Some(text(&value)),
                }
            }

            // The engine's own boundary, always. Re-using the one that arrived
            // means a caller who puts that string in a part's data splits the
            // message, and sends something other than what was asked for.
            let boundary = crate::multipart::fresh_boundary();
            request.body = crate::multipart::serialize(&parts, &boundary);
            request.set_header(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            );
            Ok(Applied {
                target,
                value: (!removing).then(|| text(&value)),
                created: was.is_none() && !removing,
                was,
            })
        }

        Target::BodyRaw => {
            let was = request.body.len();
            request.body = value.clone();
            Ok(Applied {
                target,
                value: Some(format!("{} bytes", request.body.len())),
                was: Some(format!("{was} bytes")),
                created: false,
            })
        }
    }
}

/// The error for a target that is not there, carrying what is.
///
/// The names, not a bare refusal: nine times out of ten the caller is one
/// spelling away, and the list is the answer. Values are never included, since
/// the thing being listed may be a session cookie.
fn missing(target: &str, kind: &str, have: &[(String, String)]) -> EditError {
    let names: Vec<&str> = have.iter().map(|(k, _)| k.as_str()).collect();
    let known = if names.is_empty() {
        "this request has none".to_string()
    } else {
        format!("this request has: {}", names.join(", "))
    };
    EditError::new(
        target,
        format!("no such {kind}, so nothing would change. {known}. Pass --set-create to add it"),
    )
}

/// Render a JSON value the way a person reads it: strings bare, the rest as JSON.
fn render_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Walk a dotted path. A numeric segment indexes an array.
fn json_at<'a>(document: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut at = document;
    for segment in path.split('.').filter(|s| !s.is_empty()) {
        at = match at {
            serde_json::Value::Object(map) => map.get(segment)?,
            serde_json::Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(at)
}

fn json_set(
    document: &mut serde_json::Value,
    path: &str,
    value: serde_json::Value,
) -> Result<(), String> {
    let segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    let Some((last, parents)) = segments.split_last() else {
        *document = value;
        return Ok(());
    };
    let mut at = document;
    for segment in parents {
        at = match at {
            serde_json::Value::Object(map) => map
                .entry(segment.to_string())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new())),
            serde_json::Value::Array(items) => {
                let index: usize = segment
                    .parse()
                    .map_err(|_| format!("`{segment}` is not an index, and that step is an array"))?;
                items
                    .get_mut(index)
                    .ok_or_else(|| format!("this body's array has no index {index}"))?
            }
            _ => return Err(format!("`{segment}` is a scalar, so it has no fields")),
        };
    }
    match at {
        serde_json::Value::Object(map) => {
            map.insert(last.to_string(), value);
            Ok(())
        }
        serde_json::Value::Array(items) => {
            let index: usize = last
                .parse()
                .map_err(|_| format!("`{last}` is not an index, and that step is an array"))?;
            match items.get_mut(index) {
                Some(slot) => {
                    *slot = value;
                    Ok(())
                }
                None => Err(format!("this body's array has no index {index}")),
            }
        }
        _ => Err(format!("`{last}` sits under a scalar, which has no fields")),
    }
}

fn json_remove(document: &mut serde_json::Value, path: &str) -> Result<(), String> {
    let segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    let Some((last, parents)) = segments.split_last() else {
        return Err("there is nothing to remove without a path".to_string());
    };
    let mut at = document;
    for segment in parents {
        at = match at {
            serde_json::Value::Object(map) => map
                .get_mut(*segment)
                .ok_or_else(|| format!("this body has no `{segment}`"))?,
            serde_json::Value::Array(items) => {
                let index: usize = segment
                    .parse()
                    .map_err(|_| format!("`{segment}` is not an index, and that step is an array"))?;
                items
                    .get_mut(index)
                    .ok_or_else(|| format!("this body's array has no index {index}"))?
            }
            _ => return Err(format!("`{segment}` is a scalar, so it has no fields")),
        };
    }
    match at {
        serde_json::Value::Object(map) => {
            map.remove(*last);
            Ok(())
        }
        serde_json::Value::Array(items) => {
            let index: usize = last
                .parse()
                .map_err(|_| format!("`{last}` is not an index, and that step is an array"))?;
            if index < items.len() {
                items.remove(index);
            }
            Ok(())
        }
        _ => Err(format!("`{last}` sits under a scalar, which has no fields")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Editable {
        Editable {
            method: "GET".to_string(),
            url: Url::parse("https://app.test/api/users?user_id=123&page=2").unwrap(),
            headers: vec![
                ("Cookie".to_string(), "session=abc; theme=dark".to_string()),
                ("Accept".to_string(), "application/json".to_string()),
            ],
            body: Vec::new(),
        }
    }

    fn set(spec: &str) -> Edit {
        parse_set(spec).expect("parses")
    }

    #[test]
    fn a_query_parameter_is_replaced_in_place() {
        let mut request = request();
        let applied = apply(&mut request, &[set("query.user_id=456")], false).expect("applies");
        assert_eq!(request.url.as_str(), "https://app.test/api/users?user_id=456&page=2");
        assert_eq!(applied[0].was.as_deref(), Some("123"));
        assert!(!applied[0].created);
    }

    /// The failure that costs the most time: a typo that changes nothing and
    /// returns a response that looks like an answer.
    #[test]
    fn a_parameter_that_is_not_there_is_an_error_naming_the_ones_that_are() {
        let mut refused = request();
        let error = apply(&mut refused, &[set("query.userid=456")], false).expect_err("refused");
        assert!(error.message.contains("user_id"), "{}", error.message);
        assert!(error.message.contains("page"), "{}", error.message);
        // ...and it is a different request only when the caller says so.
        let mut created = request();
        let applied = apply(&mut created, &[set("query.userid=456")], true).expect("created");
        assert!(applied[0].created);
        assert!(created.url.as_str().contains("userid=456"));
    }

    #[test]
    fn one_cookie_changes_and_the_others_stay() {
        let mut request = request();
        apply(&mut request, &[set("cookie.session=forged")], false).expect("applies");
        assert_eq!(
            request.header("cookie"),
            Some("session=forged; theme=dark"),
            "the rest of the jar's header survives"
        );
    }

    #[test]
    fn a_json_field_is_typed_the_way_it_reads() {
        let mut request = Editable {
            method: "POST".to_string(),
            url: Url::parse("https://app.test/api/user").unwrap(),
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: br#"{"user":{"id":1,"role":"user"},"active":false}"#.to_vec(),
        };
        apply(
            &mut request,
            &[set("json.user.role=admin"), set("json.user.id=99"), set("json.active=true")],
            false,
        )
        .expect("applies");
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["user"]["role"], "admin", "bare text stays text");
        assert_eq!(body["user"]["id"], 99, "a number reads as a number");
        assert_eq!(body["active"], true);
    }

    /// The API call a page never makes has to be composable, not hand-written.
    #[test]
    fn a_json_body_can_be_built_where_there_was_none() {
        let mut request = Editable {
            method: "GET".to_string(),
            url: Url::parse("https://app.test/check").unwrap(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        apply(
            &mut request,
            &[set("method=POST"), set("json.service_name=-t custom \"id\"")],
            true,
        )
        .expect("builds");
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.content_type(),
            Some("application/json"),
            "and it declares what it now is"
        );
        let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json");
        assert_eq!(body["service_name"], "-t custom \"id\"");
    }

    /// Without `--set-create` an empty body is still an empty body: the caller
    /// did not ask for one to be invented.
    #[test]
    fn an_empty_body_is_not_quietly_turned_into_json() {
        let mut request = Editable {
            method: "POST".to_string(),
            url: Url::parse("https://app.test/check").unwrap(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        assert!(apply(&mut request, &[set("json.a=1")], false).is_err());
    }

    #[test]
    fn a_json_edit_against_a_body_that_is_not_json_names_what_it_is() {
        let mut request = Editable {
            method: "POST".to_string(),
            url: Url::parse("https://app.test/upload").unwrap(),
            headers: vec![("Content-Type".to_string(), "text/plain".to_string())],
            body: b"hello".to_vec(),
        };
        let error = apply(&mut request, &[set("json.a=1")], false).expect_err("refused");
        assert!(error.message.contains("text/plain"), "{}", error.message);
    }

    #[test]
    fn a_form_field_round_trips_through_encoding() {
        let mut request = Editable {
            method: "POST".to_string(),
            url: Url::parse("https://app.test/login").unwrap(),
            headers: vec![(
                "Content-Type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            )],
            body: b"username=alice&password=hunter2".to_vec(),
        };
        apply(&mut request, &[set("form.username=admin' OR '1'='1")], false).expect("applies");
        let pairs: Vec<(String, String)> =
            form_urlencoded::parse(&request.body).into_owned().collect();
        assert_eq!(pairs[0].1, "admin' OR '1'='1", "the payload survives encoding");
        assert_eq!(pairs[1].1, "hunter2");
    }

    /// The file-upload test, built from nothing: the engine never posts a file
    /// itself, so there is usually no recorded upload to start from.
    #[test]
    fn an_upload_can_be_built_where_there_was_no_body() {
        let mut request = Editable {
            method: "POST".to_string(),
            url: Url::parse("https://app.test/upload").unwrap(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        apply(
            &mut request,
            &[
                set("multipart.file=<?php system($_GET[0]); ?>"),
                set("multipart.file.filename=shell.php.png"),
                set("multipart.file.content_type=image/png"),
            ],
            true,
        )
        .expect("builds");

        let kind = request.content_type().expect("a content type was set");
        let boundary = crate::multipart::boundary_of(kind).expect("with a boundary");
        let parts = crate::multipart::parse(&request.body, &boundary).expect("well formed");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].name, "file");
        assert_eq!(parts[0].filename.as_deref(), Some("shell.php.png"));
        assert_eq!(parts[0].content_type.as_deref(), Some("image/png"));
        assert_eq!(parts[0].data, b"<?php system($_GET[0]); ?>");
    }

    /// The three pieces are edited apart, because a server checks them apart.
    #[test]
    fn one_piece_of_an_upload_changes_and_the_rest_stays() {
        let boundary = "----abc";
        let original = crate::multipart::serialize(
            &[
                crate::multipart::Part {
                    name: "csrf".to_string(),
                    data: b"tok_1".to_vec(),
                    ..Default::default()
                },
                crate::multipart::Part {
                    name: "avatar".to_string(),
                    filename: Some("cat.png".to_string()),
                    content_type: Some("image/png".to_string()),
                    data: b"\x89PNG data".to_vec(),
                    ..Default::default()
                },
            ],
            boundary,
        );
        let mut request = Editable {
            method: "POST".to_string(),
            url: Url::parse("https://app.test/upload").unwrap(),
            headers: vec![(
                "Content-Type".to_string(),
                format!("multipart/form-data; boundary={boundary}"),
            )],
            body: original,
        };

        let applied = apply(
            &mut request,
            &[set("multipart.avatar.filename=../../etc/passwd")],
            false,
        )
        .expect("applies");
        assert_eq!(applied[0].was.as_deref(), Some("cat.png"));

        let kind = request.content_type().unwrap().to_string();
        let boundary = crate::multipart::boundary_of(&kind).unwrap();
        let parts = crate::multipart::parse(&request.body, &boundary).unwrap();
        assert_eq!(parts[0].data, b"tok_1", "the other field is untouched");
        assert_eq!(parts[1].filename.as_deref(), Some("../../etc/passwd"));
        assert_eq!(parts[1].content_type.as_deref(), Some("image/png"));
        assert_eq!(parts[1].data, b"\x89PNG data", "the file itself is untouched");
    }

    /// `.name` is not `.filename`, and the difference has to be visible.
    #[test]
    fn a_near_miss_on_a_piece_name_is_caught_by_the_create_rule() {
        let boundary = "----abc";
        let mut request = Editable {
            method: "POST".to_string(),
            url: Url::parse("https://app.test/upload").unwrap(),
            headers: vec![(
                "Content-Type".to_string(),
                format!("multipart/form-data; boundary={boundary}"),
            )],
            body: crate::multipart::serialize(
                &[crate::multipart::Part {
                    name: "file".to_string(),
                    filename: Some("cat.png".to_string()),
                    data: b"data".to_vec(),
                    ..Default::default()
                }],
                boundary,
            ),
        };
        let error = apply(&mut request, &[set("multipart.file.name=shell.php")], false)
            .expect_err("refused");
        assert!(
            error.message.contains("This request has: file"),
            "the error names the part that does exist: {}",
            error.message
        );
    }

    #[test]
    fn a_multipart_edit_on_a_body_that_is_not_one_says_so() {
        let mut request = Editable {
            method: "POST".to_string(),
            url: Url::parse("https://app.test/upload").unwrap(),
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: b"{}".to_vec(),
        };
        let error = apply(&mut request, &[set("multipart.file=x")], false).expect_err("refused");
        assert!(error.message.contains("application/json"), "{}", error.message);
    }

    #[test]
    fn a_framing_header_is_refused_here_too() {
        let mut request = request();
        let error = apply(&mut request, &[set("header.Content-Length=999")], false)
            .expect_err("refused");
        assert!(error.message.contains("client's to compute"), "{}", error.message);
    }

    #[test]
    fn a_value_may_contain_the_separator() {
        let edit = set("query.q=a=b=c");
        assert_eq!(edit.value.as_deref(), Some(b"a=b=c".as_slice()));
    }

    #[test]
    fn an_unknown_target_says_what_the_targets_are() {
        let error = parse_set("headers.X=1").expect_err("refused");
        assert!(error.message.contains("header."), "{}", error.message);
        // `multipart.<field>` where the field itself contains a dot is legal, so
        // only `filename` and `content_type` are read as pieces. A near miss
        // like `.name` is a field name, and the create rule is what catches it:
        // see `a_near_miss_on_a_piece_name_is_caught_by_the_create_rule`.
        let edit = parse_set("multipart.file.name=x").expect("parses as a field");
        assert_eq!(
            edit.target,
            Target::Multipart {
                field: "file.name".to_string(),
                piece: Piece::Data
            }
        );
    }

    #[test]
    fn edits_apply_in_the_order_they_were_given() {
        let mut request = Editable {
            method: "POST".to_string(),
            url: Url::parse("https://app.test/api").unwrap(),
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: b"{}".to_vec(),
        };
        apply(
            &mut request,
            &[
                Edit {
                    target: Target::BodyRaw,
                    value: Some(br#"{"role":"user"}"#.to_vec()),
                },
                set("json.role=admin"),
            ],
            false,
        )
        .expect("applies");
        assert_eq!(request.body, br#"{"role":"admin"}"#.to_vec());
    }

    /// A traversal payload that the URL parser resolves is a request nobody
    /// asked for. Sending it and reporting its answer would be a false
    /// negative wearing the shape of evidence.
    #[test]
    fn a_path_the_url_parser_would_resolve_is_refused_rather_than_straightened() {
        for spelling in [
            "path=/cgi-bin/.%2e/.%2e/etc/passwd",
            "path=/cgi-bin/%2e%2e/%2e%2e/etc/passwd",
            "path=/a/../etc/passwd",
            "path=/a/./b",
            "url=https://app.test/cgi-bin/.%2E/.%2E/etc/passwd",
        ] {
            let mut request = request();
            let error = apply(&mut request, &[set(spelling)], false)
                .expect_err("`{spelling}` should be refused");
            assert!(
                error.to_string().contains("dot segment"),
                "{spelling}: {error}"
            );
            assert_eq!(
                request.url.as_str(),
                "https://app.test/api/users?user_id=123&page=2",
                "a refused edit leaves the request alone"
            );
        }
    }

    /// And an ordinary path still goes through, dots in a filename included.
    #[test]
    fn a_path_with_no_dot_segment_is_set_as_asked() {
        let mut request = request();
        apply(&mut request, &[set("path=/static/app.min.js")], false).expect("applies");
        assert_eq!(request.url.path(), "/static/app.min.js");
    }
}
