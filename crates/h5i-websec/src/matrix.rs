//! `h5i websec matrix`: one request under several identities, the answers folded
//! into classes so "who can see what" is a table (design-websec.md W22).
//!
//! A missing boundary (user A reads user B's record, anon reaches an admin
//! route) only shows against a second data point: the same request under a
//! different identity. This groups the answers by shape and words
//! (`h5i-wire::triage`); two identities in one class saw the same thing. Whether
//! that boundary should have held is the agent's verdict, not this verb's.

use std::path::Path;
use std::process::Command;

use h5i_wire::message::StoredResponse;
use h5i_wire::read::{body_text, read_json};
use h5i_wire::triage::{By, Cluster, Fingerprint, Sample, cluster, text_digest};
use serde_json::{Value, json};

/// One identity's send: what it was, and what came back.
#[derive(Debug, Clone)]
pub struct Cell {
    /// The session the request was sent under.
    pub identity: String,
    /// The message id in that identity's store, `req_<n>`, or empty when the
    /// send never reached the wire.
    pub req: String,
    pub status: Option<u16>,
    pub bytes: Option<u64>,
    /// Why this identity has no answer, when it does not.
    pub error: Option<String>,
}

/// Build the report from the cells and their clusters. Pure, so the shape is
/// testable without sending: a cluster's `names` are identities, because that is
/// what each sample was named after.
pub fn assemble(request: &str, cells: &[Cell], clusters: &[Cluster]) -> Value {
    // identity -> the class label it landed in.
    let mut class_of: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    for group in clusters {
        for name in &group.names {
            class_of.insert(name.as_str(), group.label.as_str());
        }
    }

    let rows: Vec<Value> = cells
        .iter()
        .map(|cell| {
            json!({
                "identity": cell.identity,
                "req": (!cell.req.is_empty()).then(|| cell.req.clone()),
                "status": cell.status,
                "bytes": cell.bytes,
                // Two identities that share a class saw the same answer.
                "class": class_of.get(cell.identity.as_str()).copied(),
                "error": cell.error,
            })
        })
        .collect();

    let classes: Vec<Value> = clusters
        .iter()
        .map(|group| {
            json!({
                "class": group.label,
                "count": group.count,
                "representative": group.representative,
                // Members are identities here, not payload values.
                "identities": group.names,
            })
        })
        .collect();

    // `distinct` is the number an agent reads first; h5i does not judge it.
    let answered = cells.iter().filter(|c| c.status.is_some()).count();
    json!({
        "request": request,
        "identities": rows,
        "classes": classes,
        "distinct": clusters.len(),
        "sent": cells.len(),
        "answered": answered,
        "ok": answered == cells.len(),
    })
}

/// Send `request` under each identity, read each from its own store, fold, and
/// report.
#[allow(clippy::too_many_arguments)]
pub fn run(
    root: &Path,
    selector: Option<&str>,
    request: &str,
    identities: &[String],
    set: &[String],
    rate: Option<f64>,
    create: bool,
    keep_credentials: bool,
    reset_budget: bool,
    json_out: bool,
    h5i: &std::ffi::OsStr,
) -> anyhow::Result<()> {
    if identities.len() < 2 {
        anyhow::bail!(
            "a matrix compares identities, so it needs at least two. Name them with \
             `--as anon,userB,admin`: each is a session you already hold, and h5i adds none"
        );
    }
    let seq = crate::sequence_of(request)?;
    // The request is read from the holder session; `--as` sends it under the
    // other identity's credentials, policy and store.
    let holder = selector;

    let mut cells: Vec<Cell> = Vec::with_capacity(identities.len());
    let mut samples: Vec<Sample> = Vec::with_capacity(identities.len());

    for identity in identities {
        let reply = send_as(
            h5i,
            &seq,
            identity,
            holder,
            set,
            rate,
            create,
            keep_credentials,
            reset_budget,
        )?;

        // A single resend comes back as a one-element `samples` list too.
        let sample = reply
            .get("samples")
            .and_then(Value::as_array)
            .and_then(|s| s.first());
        let sent_seq = sample.and_then(|s| s.get("seq")).and_then(Value::as_u64);
        let status = sample
            .and_then(|s| s.get("status"))
            .and_then(Value::as_u64)
            .map(|s| s as u16);
        let error = reply
            .get("response")
            .and_then(|r| r.get("error"))
            .filter(|e| !e.is_null())
            .or_else(|| reply.get("message").filter(|m| !m.is_null()))
            .and_then(Value::as_str)
            .map(str::to_string);

        let Some(sent_seq) = sent_seq else {
            // Refused before the wire: still an identity, so a row, not a gap.
            cells.push(Cell {
                identity: identity.clone(),
                req: String::new(),
                status: None,
                bytes: None,
                error: error.or_else(|| Some("the send did not reach the wire".to_string())),
            });
            continue;
        };

        // Read from this identity's store, not the caller's.
        let (_session, store) = crate::read::store_dir(root, Some(identity))?;
        let response: Option<StoredResponse> =
            read_json(&store.join(format!("{sent_seq}.response.json"))).ok();

        let (bytes, sample_row) = match &response {
            Some(response) => {
                let body = body_text(&store, &response.body);
                let path = url::Url::parse(&response.url)
                    .map(|u| u.path().to_string())
                    .unwrap_or_default();
                let content_type = header(response, "content-type").unwrap_or_default();
                let location = header(response, "location");
                let len = body.len().unwrap_or_default();
                (
                    Some(len as u64),
                    Some(Sample {
                        endpoint: path.clone(),
                        name: identity.clone(),
                        req: format!("req_{sent_seq}"),
                        fingerprint: Fingerprint::of(
                            response.status,
                            &content_type,
                            len,
                            location.as_deref(),
                        ),
                        skeleton: String::new(),
                        text: text_digest(body.as_str(), &path),
                    }),
                )
            }
            None => (None, None),
        };

        if let Some(sample) = sample_row {
            samples.push(sample);
        }
        cells.push(Cell {
            identity: identity.clone(),
            req: format!("req_{sent_seq}"),
            status,
            bytes,
            error,
        });
    }

    // "Same status, same size, different record" is the answer a matrix wants.
    let clusters = cluster(&samples, By::ShapeAndWords);
    let report = assemble(&format!("req_{seq}"), &cells, &clusters);

    if json_out {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        human(&report);
    }
    // A refused identity is a row, not a failure. Exit 2 only if none answered.
    if report.get("answered").and_then(Value::as_u64) == Some(0) {
        std::process::exit(2);
    }
    Ok(())
}

/// Send one identity's copy of the request. An identity equal to the holder is
/// sent without `--as`: there is no other session to borrow.
#[allow(clippy::too_many_arguments)]
fn send_as(
    h5i: &std::ffi::OsStr,
    seq: &str,
    identity: &str,
    holder: Option<&str>,
    set: &[String],
    rate: Option<f64>,
    create: bool,
    keep_credentials: bool,
    reset_budget: bool,
) -> anyhow::Result<Value> {
    let mut command = Command::new(h5i);
    command.arg("browser").arg("resend").arg(seq);
    for spec in set {
        command.arg("--set").arg(spec);
    }
    if create {
        command.arg("--create");
    }
    if reset_budget {
        command.arg("--reset-budget");
    }
    if keep_credentials {
        command.arg("--keep-credentials");
    }
    if let Some(rate) = rate {
        command.arg("--rate").arg(rate.to_string());
    }
    if holder != Some(identity) {
        command.arg("--as").arg(identity);
    }
    if let Some(holder) = holder {
        command.arg("--session").arg(holder);
    }
    command.arg("--json");

    let out = command
        .output()
        .map_err(|e| anyhow::anyhow!("could not run h5i for identity `{identity}`: {e}"))?;
    serde_json::from_slice(&out.stdout)
        .map_err(|e| anyhow::anyhow!("h5i answered oddly for identity `{identity}`: {e}"))
}

fn header(response: &StoredResponse, name: &str) -> Option<String> {
    response
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

fn human(report: &Value) {
    println!("  request : {}", report["request"].as_str().unwrap_or_default());
    println!(
        "  distinct: {} answer(s) across {} identities",
        report["distinct"].as_u64().unwrap_or_default(),
        report["sent"].as_u64().unwrap_or_default()
    );
    for row in report["identities"].as_array().into_iter().flatten() {
        let identity = row["identity"].as_str().unwrap_or_default();
        match row["error"].as_str() {
            Some(why) => println!("  {identity:<16} refused: {why}"),
            None => println!(
                "  {identity:<16} {} {} bytes  [{}]  {}",
                row["status"].as_u64().unwrap_or_default(),
                row["bytes"].as_u64().unwrap_or_default(),
                row["class"].as_str().unwrap_or("?"),
                row["req"].as_str().unwrap_or_default(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(identity: &str, seq: u64, status: u16, len: u64, body: &str) -> Sample {
        Sample {
            endpoint: "/api/record".to_string(),
            name: identity.to_string(),
            req: format!("req_{seq}"),
            fingerprint: Fingerprint::of(Some(status), "application/json", len, None),
            skeleton: String::new(),
            text: text_digest(body, "/api/record"),
        }
    }

    fn cell(identity: &str, seq: u64, status: u16, bytes: u64) -> Cell {
        Cell {
            identity: identity.to_string(),
            req: format!("req_{seq}"),
            status: Some(status),
            bytes: Some(bytes),
            error: None,
        }
    }

    #[test]
    fn identities_that_see_the_same_record_share_a_class() {
        // A and B get the same record (boundary absent), admin a different one.
        let samples = vec![
            sample("userA", 10, 200, 42, r#"{"owner":"userA","secret":1}"#),
            sample("userB", 11, 200, 42, r#"{"owner":"userA","secret":1}"#),
            sample("admin", 12, 200, 55, r#"{"owner":"admin","secret":9,"all":true}"#),
        ];
        let cells = vec![cell("userA", 10, 200, 42), cell("userB", 11, 200, 42), cell("admin", 12, 200, 55)];
        let clusters = cluster(&samples, By::ShapeAndWords);

        let report = assemble("req_9", &cells, &clusters);
        assert_eq!(report["distinct"].as_u64(), Some(2), "two answers: {report}");

        let rows = report["identities"].as_array().unwrap();
        let class = |who: &str| -> String {
            rows.iter()
                .find(|r| r["identity"] == who)
                .and_then(|r| r["class"].as_str())
                .unwrap_or_default()
                .to_string()
        };
        assert_eq!(class("userA"), class("userB"), "A and B saw the same record");
        assert_ne!(class("userA"), class("admin"), "admin did not");
        assert_eq!(report["ok"].as_bool(), Some(true));
    }

    #[test]
    fn a_refused_identity_is_a_row_not_a_gap() {
        let samples = vec![sample("userA", 10, 200, 42, "{}")];
        let cells = vec![
            cell("userA", 10, 200, 42),
            Cell {
                identity: "anon".to_string(),
                req: String::new(),
                status: None,
                bytes: None,
                error: Some("the send did not reach the wire".to_string()),
            },
        ];
        let clusters = cluster(&samples, By::ShapeAndWords);
        let report = assemble("req_9", &cells, &clusters);

        assert_eq!(report["sent"].as_u64(), Some(2));
        assert_eq!(report["answered"].as_u64(), Some(1));
        assert_eq!(report["ok"].as_bool(), Some(false));
        let anon = report["identities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["identity"] == "anon")
            .unwrap();
        assert!(anon["class"].is_null(), "a send that never landed is in no class");
        assert!(anon["error"].is_string());
    }
}
