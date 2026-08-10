//! Transport two: a Cloudflare quick tunnel, for a visitor who has no h5i.
//!
//! The peer-to-peer transport needs h5i on both ends, because a browser cannot
//! speak QUIC to an endpoint id. That rules out the person you most often want
//! clicking a prototype — a designer, a product manager, a customer — and no
//! amount of work on the P2P path reaches them. So this one trades the
//! property P2P has for the one it does not: anybody with the link can open it
//! in any browser.
//!
//! **What it costs, stated here and in the receipt rather than only in the
//! docs:**
//!
//! * **TLS terminates at Cloudflare.** This path is not end to end. Cloudflare
//!   can read the traffic between the visitor and this machine. For an
//!   agent-built prototype that is usually an acceptable trade and it is never
//!   ours to assume — [`crate::bridge::render_receipt`] writes it into the
//!   export so the decision is visible later.
//! * **`cloudflared` is somebody else's binary.** We neither ship it nor pin
//!   it. If it is not installed, the failure says so and names the alternative.
//! * **Quick tunnels are explicitly not a production service.** Cloudflare caps
//!   concurrency and does not support server-sent events on them. This is the
//!   no-install mode, not the default mode.
//!
//! What does **not** change is the bridge underneath. The URL carries a token,
//! the token is checked against the same grant table on every connection, live
//! connections are dropped when a grant is revoked, and the credential is
//! stripped before anything reaches the box. The capability degrades from "hold
//! the secret" to "hold the link"; it does not degrade to nothing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use h5i_error::H5iError;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::bridge::{Bridge, Path};
use crate::http_front::{self, Next};

/// How long to wait for `cloudflared` to publish a URL before giving up.
const URL_TIMEOUT: Duration = Duration::from_secs(45);
/// How often the watchdog asks whether the share still admits anyone.
const REVOKE_POLL: Duration = Duration::from_secs(1);

/// A running `cloudflared`, killed when this is dropped.
#[derive(Debug)]
pub struct Tunnel {
    child: tokio::process::Child,
    pub url: String,
}

impl Tunnel {
    /// The origin a visitor opens, with no token in it.
    pub fn origin(&self) -> &str {
        &self.url
    }

    /// Stop the tunnel. Called on the way out so a quick tunnel does not
    /// outlive the share that created it.
    pub async fn stop(&mut self) {
        let _ = self.child.kill().await;
    }
}

/// Pull a quick-tunnel URL out of a line of `cloudflared` logging.
///
/// Strict about what it will accept: the host must be a `trycloudflare.com`
/// subdomain made of the characters a hostname is allowed to have. This is a
/// URL we are about to print and tell someone to open, and `cloudflared`'s
/// output is a log format rather than an interface — a looser match would let a
/// change in its banner, or anything that got into its logs, choose what we
/// hand a person.
pub fn extract_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '|' || c == '"' || c == '<')
        .unwrap_or(rest.len());
    let url = rest[..end].trim_end_matches('/');
    let host = url.strip_prefix("https://")?;
    if !host.ends_with(".trycloudflare.com") {
        return None;
    }
    let label = host.strip_suffix(".trycloudflare.com")?;
    if label.is_empty()
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
    {
        return None;
    }
    Some(url.to_string())
}

/// Start `cloudflared` pointed at a loopback port, and wait for its URL.
///
/// The argv is built here, never through a shell: the only value that varies is
/// a port number this process chose.
pub async fn start(local_port: u16) -> Result<Tunnel, H5iError> {
    let mut child = tokio::process::Command::new("cloudflared")
        .arg("tunnel")
        .arg("--no-autoupdate")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{local_port}"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                H5iError::Metadata(
                    "`cloudflared` is not installed, and `--tunnel` is a wrapper around it. \
                     Install it (https://developers.cloudflare.com/cloudflare-one/connections/\
                     connect-networks/downloads/), or share peer-to-peer with `h5i box share` \
                     and have the other side run `h5i join`."
                        .into(),
                )
            } else {
                H5iError::Metadata(format!("could not start `cloudflared`: {e}"))
            }
        })?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| H5iError::Metadata("cloudflared produced no output to read".into()))?;
    let mut lines = BufReader::new(stderr).lines();

    let found = tokio::time::timeout(URL_TIMEOUT, async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(url) = extract_url(&line) {
                return Some(url);
            }
        }
        None
    })
    .await;

    match found {
        Ok(Some(url)) => Ok(Tunnel { child, url }),
        Ok(None) => {
            let _ = child.kill().await;
            Err(H5iError::Metadata(
                "`cloudflared` exited without publishing a URL. Run it once by hand to see what \
                 it says: `cloudflared tunnel --url http://127.0.0.1:3000`."
                    .into(),
            ))
        }
        Err(_) => {
            let _ = child.kill().await;
            Err(H5iError::Metadata(format!(
                "`cloudflared` did not publish a URL within {}s. Quick tunnels need outbound \
                 network access to Cloudflare.",
                URL_TIMEOUT.as_secs()
            )))
        }
    }
}

/// The visitor-facing link: the origin plus the token that authorizes one grant.
pub fn invite_url(origin: &str, secret: &str) -> String {
    format!("{origin}/?{}={secret}", crate::gate::QUERY_PARAM)
}

/// Serve the tunnel's loopback side until the process stops.
///
/// The listener is bound on `127.0.0.1` and nothing else, on every path: what
/// reaches the internet is `cloudflared`'s outbound connection, not a port on
/// this machine.
pub async fn serve(bridge: Arc<Bridge>, listener: tokio::net::TcpListener) -> Result<(), H5iError> {
    // One entry per grant, because a tunnel genuinely cannot tell two browsers
    // apart — the peers it sees are Cloudflare's. Counting per grant is the
    // finest honest granularity, and the receipt says so rather than implying
    // a precision the transport does not have.
    let peers: Arc<Mutex<HashMap<String, crate::bridge::PeerId>>> = Default::default();
    let cancel = tokio::sync::broadcast::Sender::new(16);

    {
        // Revocation has to reach connections that are already open. A share
        // that could be revoked but kept serving the visitor already on it
        // would be a revoke that only worked on people who were not there.
        let bridge = bridge.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REVOKE_POLL).await;
                if bridge.is_spent() {
                    let _ = cancel.send(());
                }
            }
        });
    }

    loop {
        let Ok((sock, _)) = listener.accept().await else {
            continue;
        };
        let bridge = bridge.clone();
        let peers = peers.clone();
        let cancel = cancel.subscribe();
        tokio::spawn(async move {
            if let Err(e) = handle(bridge, peers, sock, cancel).await {
                eprintln!("share: {e}");
            }
        });
    }
}

async fn handle(
    bridge: Arc<Bridge>,
    peers: Arc<Mutex<HashMap<String, crate::bridge::PeerId>>>,
    mut sock: tokio::net::TcpStream,
    mut cancel: tokio::sync::broadcast::Receiver<()>,
) -> Result<(), H5iError> {
    let Some((head, rest)) = http_front::read_head(&mut sock).await else {
        return Ok(());
    };

    // Resolved once, here, so the decision and the accounting agree about which
    // grant let this connection in.
    let mut grant = None;
    let next = http_front::decide(
        &head,
        |token| match bridge.authorize(token) {
            Ok(g) => {
                grant = Some(g);
                true
            }
            Err(_) => false,
        },
        // The visitor's origin is https, because Cloudflare terminates it.
        true,
    );

    let (head, _upgrade) = match next {
        Next::Respond(body) => {
            http_front::respond(&mut sock, &body).await;
            return Ok(());
        }
        Next::Proxy { head, upgrade } => (head, upgrade),
    };
    let Some(grant) = grant else {
        return Ok(());
    };

    // On a blocking pool because it is blocking: the dialer waits for its
    // helper to hand back a connected socket. A runtime worker parked on that
    // syscall is a worker not serving the other requests of the same page.
    let upstream = {
        let bridge = bridge.clone();
        tokio::task::spawn_blocking(move || bridge.open_upstream())
            .await
            .map_err(|e| H5iError::Metadata(format!("the box dialer panicked: {e}")))??
    };
    upstream.set_nonblocking(true)?;
    let upstream = tokio::net::TcpStream::from_std(upstream)?;

    let id = {
        let mut map = peers.lock().expect("peer map");
        *map.entry(grant.id.clone()).or_insert_with(|| {
            bridge.peer_joined(
                "a browser (the tunnel cannot tell two apart)".into(),
                &grant,
                Path::Tunnel,
            )
        })
    };
    bridge.peer_connection(id);

    let (peer_r, peer_w) = sock.into_split();
    let (up_r, mut up_w) = upstream.into_split();
    {
        use tokio::io::AsyncWriteExt as _;
        up_w.write_all(head.as_bytes()).await?;
        // Whatever arrived in the same packet as the head — a form body, most
        // often. Dropping it would break every POST on the shared app.
        if !rest.is_empty() {
            up_w.write_all(&rest).await?;
        }
    }

    let from_peer = AtomicU64::new(head.len() as u64 + rest.len() as u64);
    let to_peer = AtomicU64::new(0);
    tokio::select! {
        _ = crate::pump::duplex(peer_r, peer_w, up_r, up_w, &from_peer, &to_peer) => {}
        _ = cancel.recv() => {}
    }
    bridge.peer_bytes(
        id,
        to_peer.load(Ordering::Relaxed),
        from_peer.load(Ordering::Relaxed),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_is_read_out_of_cloudflareds_banner() {
        let line = "2026-08-10T10:00:00Z INF |  https://odd-cat-1234.trycloudflare.com  |";
        assert_eq!(
            extract_url(line).as_deref(),
            Some("https://odd-cat-1234.trycloudflare.com")
        );
    }

    #[test]
    fn only_a_quick_tunnel_host_is_accepted_as_one() {
        // `cloudflared`'s log format is not an interface, and its output is not
        // all ours. A looser match would let a banner change — or anything that
        // got into its logs — choose the URL we hand a person to open.
        for line in [
            "INF Visit https://developers.cloudflare.com/argo-tunnel for docs",
            "INF see https://example.test/",
            "INF https://evil.test/?x=https://a.trycloudflare.com",
            "INF https://.trycloudflare.com",
            "INF https://a b.trycloudflare.com",
            "nothing here at all",
        ] {
            assert_eq!(extract_url(line), None, "accepted a URL from: {line}");
        }
    }

    #[test]
    fn a_trailing_slash_or_quote_does_not_end_up_in_the_url() {
        assert_eq!(
            extract_url(r#"INF url="https://a-b-c.trycloudflare.com/""#).as_deref(),
            Some("https://a-b-c.trycloudflare.com")
        );
    }

    #[test]
    fn the_invite_link_carries_the_token_and_nothing_else() {
        let url = invite_url("https://odd-cat.trycloudflare.com", "abc123");
        assert_eq!(url, "https://odd-cat.trycloudflare.com/?h5i=abc123");
    }

    #[tokio::test]
    async fn a_missing_cloudflared_says_what_to_install_and_what_else_to_try() {
        // Only meaningful when cloudflared really is absent; where it is
        // installed this asserts nothing, which is the right trade for not
        // making the suite depend on a third-party binary.
        if which_cloudflared() {
            return;
        }
        let err = start(3000).await.expect_err("no cloudflared");
        let msg = format!("{err}");
        assert!(msg.contains("cloudflared"), "{msg}");
        assert!(msg.contains("h5i join"), "{msg}");
    }

    fn which_cloudflared() -> bool {
        std::process::Command::new("cloudflared")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }
}
