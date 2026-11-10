//! Serving on an embassy-net TCP port (feature `embassy-net`).
//!
//! [`serve_tcp`] is one listener: accept, [`serve`] the connection, close,
//! repeat. Run several, each with its own buffers, to serve that many
//! connections at once (embassy tasks are not generic, so the task is the
//! firmware's):
//!
//! ```ignore
//! #[embassy_executor::task(pool_size = 2)]
//! async fn http(stack: Stack<'static>, app: &'static App) -> ! {
//!     let (mut srx, mut stx) = ([0u8; 1024], [0u8; 1024]);
//!     let (mut rx, mut tx) = ([0u8; 2048], [0u8; 1024]);
//!     serve_tcp(stack, 80, &Config::default(), &router(), app,
//!               &mut srx, &mut stx, &mut rx, &mut tx, &EmbassyBound).await
//! }
//! ```

use embassy_net::Stack;
use embassy_net::tcp::TcpSocket;

use crate::{Bound, Config, Router, Routes, serve};

/// Serve connections on `port`, one at a time, forever. `sock_rx` and
/// `sock_tx` are the socket's buffers, `rx` and `tx` the server's (see
/// [`serve`]); `bound` limits each request and response.
#[allow(clippy::too_many_arguments)]
pub async fn serve_tcp<S, R, B>(
    stack: Stack<'_>,
    port: u16,
    cfg: &Config,
    router: &Router<R>,
    state: &S,
    sock_rx: &mut [u8],
    sock_tx: &mut [u8],
    rx: &mut [u8],
    tx: &mut [u8],
    bound: &B,
) -> !
where
    R: Routes<S>,
    B: Bound,
{
    loop {
        let mut sock = TcpSocket::new(stack, sock_rx, sock_tx);
        if sock.accept(port).await.is_err() {
            continue;
        }
        let _ = serve(cfg, router, state, rx, tx, &mut sock, bound).await;
        // Send what is left and FIN; the bound keeps a peer that never
        // acknowledges from holding the listener.
        sock.close();
        if bound.within(sock.flush()).await.is_none() {
            sock.abort();
            let _ = bound.within(sock.flush()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::boxed::Box;
    use std::vec::Vec;

    use embassy_net::tcp::TcpSocket;
    use embassy_net::{Config as NetConfig, IpEndpoint, Ipv4Address, Ipv4Cidr, StackResources, StaticConfigV4};
    use embassy_net_driver_channel::driver::{HardwareAddress, LinkState};
    use embassy_net_driver_channel::{self as ch, RxRunner, TxRunner};
    use embedded_io_async::Write;

    use super::*;
    use crate::{NoBound, Request, Response};

    const MTU: usize = 1514;

    async fn hello(req: &Request<'_>, _: &(), out: &mut [u8]) -> Response {
        Response::bytes(out, 200, b"text/plain", req.path)
    }

    /// Frames from one end of the cable to the other.
    async fn wire(mut from: TxRunner<'_, MTU>, mut to: RxRunner<'_, MTU>) {
        let mut frame = [0u8; MTU];
        loop {
            let p = from.tx_buf().await;
            let n = p.len();
            frame[..n].copy_from_slice(p);
            from.tx_done();
            to.rx_buf().await[..n].copy_from_slice(&frame[..n]);
            to.rx_done(n);
        }
    }

    fn end(mac: u8, ip: u8) -> (Stack<'static>, embassy_net::Runner<'static, ch::Device<'static, MTU>>, ch::Runner<'static, MTU>) {
        let state = Box::leak(Box::new(ch::State::<MTU, 4, 4>::new()));
        let (runner, device) = ch::new(state, HardwareAddress::Ethernet([2, 0, 0, 0, 0, mac]));
        let cfg = NetConfig::ipv4_static(StaticConfigV4 {
            address: Ipv4Cidr::new(Ipv4Address::new(10, 0, 0, ip), 24),
            gateway: None,
            dns_servers: Default::default(),
        });
        let (stack, net) = embassy_net::new(device, cfg, Box::leak(Box::new(StackResources::<4>::new())), ip as u64);
        (stack, net, runner)
    }

    /// `serve_tcp` over real TCP (smoltcp), two connections in turn.
    #[test]
    fn serves_connections_over_embassy_net() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let (dev, mut dev_net, dev_link) = end(1, 1);
            let (cli, mut cli_net, cli_link) = end(2, 2);
            let (dev_state, dev_rx, dev_tx) = dev_link.split();
            let (cli_state, cli_rx, cli_tx) = cli_link.split();
            dev_state.set_link_state(LinkState::Up);
            cli_state.set_link_state(LinkState::Up);

            let router = Router::new().get(b"/hello", hello);
            let server = async {
                let (mut srx, mut stx) = ([0u8; 1024], [0u8; 1024]);
                let (mut rx, mut tx) = ([0u8; 2048], [0u8; 1024]);
                serve_tcp(dev, 80, &Config::default(), &router, &(), &mut srx, &mut stx, &mut rx, &mut tx, &NoBound).await
            };
            let client = async {
                let mut out = Vec::new();
                for path in ["/hello", "/hello/no"] {
                    let (mut srx, mut stx) = ([0u8; 1024], [0u8; 1024]);
                    let mut s = TcpSocket::new(cli, &mut srx, &mut stx);
                    s.connect(IpEndpoint::new(Ipv4Address::new(10, 0, 0, 1).into(), 80)).await.unwrap();
                    let req = std::format!("GET {path} HTTP/1.1\r\nHost: d\r\nConnection: close\r\n\r\n");
                    s.write_all(req.as_bytes()).await.unwrap();
                    let mut v = Vec::new();
                    let mut buf = [0u8; 256];
                    loop {
                        match s.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => v.extend_from_slice(&buf[..n]),
                        }
                    }
                    out.push(std::string::String::from_utf8(v).unwrap());
                }
                out
            };
            let out = tokio::select! {
                out = client => out,
                _ = server => unreachable!(),
                _ = dev_net.run() => unreachable!(),
                _ = cli_net.run() => unreachable!(),
                _ = wire(dev_tx, cli_rx) => unreachable!(),
                _ = wire(cli_tx, dev_rx) => unreachable!(),
            };
            assert_eq!(out[0], "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 6\r\nConnection: close\r\n\r\n/hello");
            assert!(out[1].starts_with("HTTP/1.1 404 Not Found\r\n"), "{}", out[1]);
        });
    }
}
