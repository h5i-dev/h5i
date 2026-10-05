//! `h5i websec oast`: a lab callback listener for this session.
//!
//! A blind bug answers somewhere other than the HTTP response. `serve` binds a
//! listener, `token` mints an id to embed, and `poll` reads the interactions
//! that id drew. Nothing is sent to an h5i-operated service. A hit is an
//! observation the agent cites. This verb does not write a finding.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context};
use serde::{Deserialize, Serialize};

const HEADER_CAP: usize = 64 * 1024;
const BODY_READ_CAP: usize = 64 * 1024;
const BODY_PREVIEW: usize = 4 * 1024;
const INTERACTIONS_CAP: u64 = 16 * 1024 * 1024;
const TOKEN_BYTES: usize = 16;

/// What `serve` writes while it is accepting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListenerInfo {
    pub bind: String,
    pub public_base: String,
    pub pid: u32,
    /// Seconds since the unix epoch.
    pub started: u64,
}

/// One minted correlation id and the URL a payload embeds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Minted {
    pub token: String,
    pub url: String,
    pub public_base: String,
}

/// One callback that named a minted token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Interaction {
    pub token: String,
    pub at: u64,
    pub method: String,
    pub path: String,
    pub host: String,
    pub peer: String,
    pub body_preview: String,
    pub body_truncated: bool,
}

#[derive(Debug, Deserialize)]
struct TokenLine {
    token: String,
}

/// `h5i websec oast`.
#[derive(clap::Subcommand)]
pub enum OastVerb {
    /// Bind the lab HTTP listener and wait. Default is localhost.
    ///
    /// A target that cannot reach this machine cannot call it back. Binding
    /// `0.0.0.0` is reachable from other hosts, so it also requires
    /// `--public-base`, the URL those hosts can actually dial.
    Serve {
        /// `addr:port`. Port `0` asks the kernel for a free port.
        #[arg(long, default_value = "127.0.0.1:0", value_name = "ADDR")]
        bind: String,
        /// The base to print in `token`, when it is not the bind address.
        ///
        /// Left exactly as given. A port of `0` is filled in only for the
        /// default base, which is `http://` plus the address that was bound.
        #[arg(long = "public-base", value_name = "URL")]
        public_base: Option<String>,
    },
    /// Mint a URL to embed. `serve` has to be running in this session.
    Token,
    /// Interactions that named this token.
    Poll {
        /// The id `token` printed.
        #[arg(value_name = "TOKEN")]
        token: String,
    },
}

/// Run one `oast` verb against the session `selector` names.
pub fn run(
    root: &Path,
    selector: Option<&str>,
    what: &OastVerb,
    json_out: bool,
) -> anyhow::Result<()> {
    let session = crate::read::resolve_for_reading(root, selector)?;
    let dir = h5i_core::browser_session::dir(root, &session.id).join("oast");
    match what {
        OastVerb::Serve { bind, public_base } => {
            serve(&dir, &bind, public_base.as_deref(), None)?;
        }
        OastVerb::Token => {
            let minted = mint(&dir)?;
            if json_out {
                println!("{}", serde_json::to_string(&minted)?);
            } else {
                println!("{}", minted.url);
                println!("token {}", minted.token);
            }
        }
        OastVerb::Poll { token } => {
            let hits = poll(&dir, &token)?;
            if json_out {
                println!("{}", serde_json::to_string(&hits)?);
            } else if hits.is_empty() {
                println!("no interactions for {token}");
            } else {
                for hit in &hits {
                    println!(
                        "{} {} {} host {} peer {}{}",
                        hit.at,
                        hit.method,
                        hit.path,
                        hit.host,
                        hit.peer,
                        if hit.body_truncated {
                            " body truncated"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
    }
    Ok(())
}

/// Bind `bind` and answer HTTP until `stop` is set.
///
/// `stop` is for tests. A real `serve` passes `None` and blocks until the
/// process is killed. The files live in `dir`: `listener.json`, `tokens.jsonl`,
/// `interactions.jsonl`.
pub fn serve(
    dir: &Path,
    bind: &str,
    public_base: Option<&str>,
    stop: Option<Arc<AtomicBool>>,
) -> anyhow::Result<()> {
    let addr: SocketAddr = bind
        .parse()
        .with_context(|| format!("{bind:?} is not an addr:port to bind"))?;
    if addr.ip().is_unspecified() && public_base.is_none() {
        bail!(
            "binding {addr} is reachable from other hosts, and a target cannot dial {addr}. \
             Pass --public-base with the URL that target can reach"
        );
    }
    if listener_alive(dir) {
        bail!("oast serve is already running for this session");
    }
    private_dir(dir)?;
    let listener = TcpListener::bind(addr)
        .with_context(|| format!("could not bind the oast listener on {addr}"))?;
    let bound = listener
        .local_addr()
        .context("the oast listener has no local address")?;
    let public_base = match public_base {
        Some(base) => base.trim_end_matches('/').to_string(),
        None => format!("http://{bound}"),
    };
    let info = ListenerInfo {
        bind: bound.to_string(),
        public_base,
        pid: std::process::id(),
        started: now_secs(),
    };
    let path = listener_path(dir);
    // The file is the claim. A second serve that loses the race finds it and stops.
    write_new(&path, serde_json::to_string(&info)?.as_bytes()).or_else(|error| {
        if listener_alive(dir) {
            bail!("oast serve is already running for this session");
        }
        let _ = fs::remove_file(&path);
        write_new(&path, serde_json::to_string(&info)?.as_bytes()).map_err(|_| error)
    })?;
    let _clear = ClearFile(&path);
    loop {
        if stop.as_ref().is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            break;
        }
        let (stream, _) = listener.accept().context("the oast listener stopped accepting")?;
        handle_client(stream, dir);
    }
    Ok(())
}

/// Mint a token against the listener `dir` records, or say that it is down.
pub fn mint(dir: &Path) -> anyhow::Result<Minted> {
    let info = read_listener(dir)?;
    let token = fresh_token()?;
    let line = serde_json::to_string(&serde_json::json!({ "token": token, "at": now_secs() }))?;
    append_line(&tokens_path(dir), &line)?;
    Ok(Minted {
        url: format!("{}/{}", info.public_base.trim_end_matches('/'), token),
        public_base: info.public_base,
        token,
    })
}

/// Interactions whose recorded token is `token`. A missing log is no hits.
pub fn poll(dir: &Path, token: &str) -> anyhow::Result<Vec<Interaction>> {
    let text = match fs::read_to_string(interactions_path(dir)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("could not read oast interactions"),
    };
    let mut hits = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let Ok(hit) = serde_json::from_str::<Interaction>(line) else {
            continue;
        };
        if hit.token == token {
            hits.push(hit);
        }
    }
    Ok(hits)
}

fn handle_client(mut stream: TcpStream, dir: &Path) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let peer = stream
        .peer_addr()
        .map(|addr| addr.to_string())
        .unwrap_or_default();
    if let Some(request) = read_http(&mut stream) {
        // A full disk still gets an answer. The callback is the observation.
        let _ = record_matches(dir, &request, &peer);
    }
    let _ = write_ok(&mut stream);
}

struct RequestBits {
    method: String,
    target: String,
    host: String,
    header_values: Vec<String>,
    body: Vec<u8>,
    content_length: usize,
}

fn read_http(stream: &mut TcpStream) -> Option<RequestBits> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    let mut head_end = None;
    while buf.len() < HEADER_CAP {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(at) = find_header_end(&buf) {
                    head_end = Some(at);
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let end = head_end?;
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    if method.is_empty() || target.is_empty() {
        return None;
    }
    let mut host = String::new();
    let mut header_values = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("host") {
            host = value.to_string();
        }
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().unwrap_or(0);
        }
        header_values.push(value.to_string());
    }
    let mut body = buf[end..].to_vec();
    let want = content_length.min(BODY_READ_CAP);
    while body.len() < want {
        match stream.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = want - body.len();
                let n = n.min(room);
                body.extend_from_slice(&tmp[..n]);
            }
        }
    }
    body.truncate(want);
    Some(RequestBits {
        method,
        target,
        host,
        header_values,
        body,
        content_length,
    })
}

fn record_matches(dir: &Path, request: &RequestBits, peer: &str) -> anyhow::Result<()> {
    let tokens = load_tokens(dir);
    if tokens.is_empty() {
        return Ok(());
    }
    let path_bytes = request.target.as_bytes();
    let host_bytes = request.host.as_bytes();
    let matched: Vec<&str> = tokens
        .iter()
        .map(String::as_str)
        .filter(|token| {
            contains(path_bytes, token)
                || contains(host_bytes, token)
                || request
                    .header_values
                    .iter()
                    .any(|value| value.contains(token))
                || contains(&request.body, token)
        })
        .collect();
    if matched.is_empty() {
        return Ok(());
    }
    let path = interactions_path(dir);
    if fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0) >= INTERACTIONS_CAP {
        return Ok(());
    }
    let preview_end = request.body.len().min(BODY_PREVIEW);
    let truncated = request.content_length > BODY_PREVIEW || request.body.len() > BODY_PREVIEW;
    let preview = String::from_utf8_lossy(&request.body[..preview_end]).into_owned();
    let at = now_secs();
    for token in matched {
        let hit = Interaction {
            token: token.to_string(),
            at,
            method: clip(&request.method, 32),
            path: clip(&request.target, 512),
            host: clip(&request.host, 256),
            peer: peer.to_string(),
            body_preview: preview.clone(),
            body_truncated: truncated,
        };
        append_line(&path, &serde_json::to_string(&hit)?)?;
    }
    Ok(())
}

fn load_tokens(dir: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(tokens_path(dir)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<TokenLine>(line).ok())
        .map(|line| line.token)
        .filter(|token| !token.is_empty())
        .collect()
}

fn read_listener(dir: &Path) -> anyhow::Result<ListenerInfo> {
    let path = listener_path(dir);
    let text = fs::read_to_string(&path).map_err(|_| down())?;
    let info: ListenerInfo = serde_json::from_str(&text).map_err(|_| down())?;
    if !pid_alive(info.pid) {
        return Err(down());
    }
    Ok(info)
}

fn listener_alive(dir: &Path) -> bool {
    read_listener(dir).is_ok()
}

fn down() -> anyhow::Error {
    anyhow!(
        "oast serve is not running in this session. Start it with `h5i websec oast serve`, then \
         mint a token"
    )
}

fn fresh_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; TOKEN_BYTES];
    let mut file = fs::File::open("/dev/urandom")
        .context("could not read /dev/urandom to mint an oast token")?;
    file.read_exact(&mut bytes)
        .context("could not read /dev/urandom to mint an oast token")?;
    let mut token = String::with_capacity(TOKEN_BYTES * 2);
    for byte in bytes {
        token.push_str(&format!("{byte:02x}"));
    }
    Ok(token)
}

fn write_ok(stream: &mut TcpStream) -> std::io::Result<()> {
    // Fixed body. The token the client sent is never written back.
    let body = "ok\n";
    let message = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(message.as_bytes())?;
    let _ = stream.shutdown(std::net::Shutdown::Both);
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn contains(hay: &[u8], token: &str) -> bool {
    let needle = token.as_bytes();
    !needle.is_empty() && hay.windows(needle.len()).any(|window| window == needle)
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at + 4)
}

fn listener_path(dir: &Path) -> PathBuf {
    dir.join("listener.json")
}

fn tokens_path(dir: &Path) -> PathBuf {
    dir.join("tokens.jsonl")
}

fn interactions_path(dir: &Path) -> PathBuf {
    dir.join("interactions.jsonl")
}

fn private_dir(dir: &Path) -> anyhow::Result<()> {
    if dir.exists() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
    }
    #[cfg(not(unix))]
    {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(bytes)?;
    }
    Ok(())
}

fn append_line(path: &Path, line: &str) -> anyhow::Result<()> {
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?
    };
    #[cfg(not(unix))]
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    if !line.ends_with('\n') {
        file.write_all(b"\n")?;
    }
    Ok(())
}

fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if Path::new("/proc/1").exists() {
        return Path::new(&format!("/proc/{pid}")).exists();
    }
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

struct ClearFile<'a>(&'a Path);

impl Drop for ClearFile<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn serve_for(dir: &Path, public_base: Option<&str>) -> (std::thread::JoinHandle<()>, Arc<AtomicBool>, ListenerInfo) {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thr = Arc::clone(&stop);
        let dir_thr = dir.to_path_buf();
        let base = public_base.map(str::to_string);
        let handle = std::thread::spawn(move || {
            serve(&dir_thr, "127.0.0.1:0", base.as_deref(), Some(stop_thr)).unwrap();
        });
        let info = wait_listener(dir);
        (handle, stop, info)
    }

    fn wait_listener(dir: &Path) -> ListenerInfo {
        for _ in 0..100 {
            if let Ok(info) = read_listener(dir) {
                return info;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("listener.json did not appear in {}", dir.display());
    }

    fn stop(dir: &Path, info: &ListenerInfo, stop: &AtomicBool, handle: std::thread::JoinHandle<()>) {
        stop.store(true, Ordering::SeqCst);
        if let Ok(mut stream) = TcpStream::connect(&info.bind) {
            let _ = stream.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        }
        handle.join().expect("serve returned");
        assert!(
            !listener_path(dir).exists(),
            "a stopped listener drops its claim"
        );
    }

    fn exchange(addr: &str, request: &str) -> String {
        let mut stream = TcpStream::connect(addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response
    }

    #[test]
    fn an_unspecified_bind_without_a_public_base_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let error = serve(dir.path(), "0.0.0.0:0", None, None).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("public-base"), "{message}");
        assert!(!listener_path(dir.path()).exists());
    }

    #[test]
    fn a_token_is_recorded_from_the_path_the_host_and_the_query() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("oast");
        let (handle, stop_flag, info) = serve_for(&dir, None);
        let bound: SocketAddr = info.bind.parse().unwrap();
        assert_eq!(
            info.public_base,
            format!("http://{bound}"),
            "port 0 is replaced with the bound address"
        );
        let minted = mint(&dir).unwrap();
        assert!(minted.url.starts_with(&info.public_base), "{}", minted.url);
        assert!(minted.url.ends_with(&minted.token));

        let path_response = exchange(
            &info.bind,
            &format!("GET /{} HTTP/1.1\r\nHost: lab\r\nConnection: close\r\n\r\n", minted.token),
        );
        assert!(path_response.contains("\r\n\r\nok\n"), "{path_response}");
        assert!(!path_response.contains(&minted.token), "{path_response}");

        exchange(
            &info.bind,
            &format!(
                "GET /cb?x={} HTTP/1.1\r\nHost: lab\r\nConnection: close\r\n\r\n",
                minted.token
            ),
        );
        let other = mint(&dir).unwrap();
        exchange(
            &info.bind,
            &format!(
                "GET / HTTP/1.1\r\nHost: {}.example\r\nConnection: close\r\n\r\n",
                other.token
            ),
        );
        exchange(
            &info.bind,
            "GET /scanner HTTP/1.1\r\nHost: lab\r\nConnection: close\r\n\r\n",
        );

        let path_hits = poll(&dir, &minted.token).unwrap();
        assert!(
            path_hits.iter().any(|hit| hit.path.contains(&minted.token)),
            "{path_hits:?}"
        );
        assert!(
            path_hits.iter().any(|hit| hit.path.contains("x=")),
            "{path_hits:?}"
        );
        assert!(
            path_hits.iter().all(|hit| hit.token == minted.token),
            "poll keeps the other token out: {path_hits:?}"
        );
        let host_hits = poll(&dir, &other.token).unwrap();
        assert_eq!(host_hits.len(), 1, "{host_hits:?}");
        assert!(host_hits[0].host.starts_with(&other.token), "{host_hits:?}");
        let logged = fs::read_to_string(interactions_path(&dir)).unwrap();
        assert!(!logged.contains("/scanner"), "{logged}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(tokens_path(&dir)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the token file is owner-only");
            let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "the oast directory is owner-only");
        }

        let again = serve(&dir, "127.0.0.1:0", None, None).unwrap_err();
        assert!(
            again.to_string().contains("already running"),
            "{}",
            again
        );
        stop(&dir, &info, &stop_flag, handle);
    }

    #[test]
    fn a_caller_public_base_is_kept_and_a_long_body_is_clipped() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("oast");
        let (handle, stop_flag, info) = serve_for(&dir, Some("http://lab.test/hook"));
        assert_eq!(info.public_base, "http://lab.test/hook");
        let minted = mint(&dir).unwrap();
        assert_eq!(minted.url, format!("http://lab.test/hook/{}", minted.token));
        let body = "A".repeat(20_000);
        exchange(
            &info.bind,
            &format!(
                "POST /{} HTTP/1.1\r\nHost: lab\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                minted.token,
                body.len(),
                body
            ),
        );
        let hits = poll(&dir, &minted.token).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(hits[0].body_truncated, "{hits:?}");
        assert!(hits[0].body_preview.len() <= BODY_PREVIEW, "{}", hits[0].body_preview.len());
        assert!(hits[0].body_preview.chars().all(|c| c == 'A'));
        let gone = tempfile::tempdir().unwrap();
        let error = mint(gone.path()).unwrap_err();
        assert!(error.to_string().contains("not running"), "{error}");
        stop(&dir, &info, &stop_flag, handle);
    }
}
