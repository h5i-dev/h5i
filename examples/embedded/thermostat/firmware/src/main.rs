//! The thermostat on an STM32F407, serving HTTP/1.1 over USART2.
//!
//! The same `thermostat-app` router and `h5i-app-embedded` shell as the
//! host runner, over the verified `h5i-app-h1` parser. In Renode,
//! `scripts/embedded/thermostat.resc` connects USART2 to a TCP socket so
//! `curl http://127.0.0.1:8080/state` reaches it byte for byte.
//!
//! A UART has no connections, so the line is framed (`h5i_app_embedded::link`):
//! a gateway on the host (`scripts/embedded/uart-gateway.py`) marks where
//! each TCP client's bytes start and end and escapes them, and forwards to a
//! client only the device's bytes for that client. Each connection is served
//! with an empty receive buffer and at most `CONNECTION_BOUND`; a new
//! connection aborts the previous one, whose leftover bytes and unsent
//! responses are dropped.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_stm32::usart::{BufferedInterruptHandler, BufferedUart, Config as UartConfig};
use embassy_stm32::{bind_interrupts, peripherals};
use embassy_time::{Duration, with_timeout};
use h5i_app_embedded::Config;
use h5i_app_embedded::link::{Bound, Link, serve_link};
use panic_halt as _;
use thermostat_app::{App, router};

bind_interrupts!(struct Irqs {
    USART2 => BufferedInterruptHandler<peripherals::USART2>;
});

/// The device's state. A demonstration key: provision a real one per device.
static APP: App = App::new(b"dev-key");

/// Per-connection memory: fixed at compile time.
const RX: usize = 2048;
const TX: usize = 1024;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());
    let mut uart_tx = [0u8; 512];
    let mut uart_rx = [0u8; 512];
    let mut cfg = UartConfig::default();
    cfg.baudrate = 115_200;
    let Ok(uart) = BufferedUart::new(p.USART2, p.PA3, p.PA2, &mut uart_tx, &mut uart_rx, Irqs, cfg) else {
        // A configuration the peripheral refuses: nothing to serve on.
        loop {
            cortex_m::asm::wfi();
        }
    };
    let routes = router();
    let mut rx = [0u8; RX];
    let mut tx = [0u8; TX];
    let mut link = Link::new(uart);
    loop {
        // Only a broken UART ends this; start over.
        let _ = serve_link(&Config::default(), &routes, &APP, &mut rx, &mut tx, &mut link, &Embassy).await;
    }
}

/// The longest one connection may take, from its first byte to its last
/// response: a silent or slow client holds the line no longer than this.
const CONNECTION_BOUND: Duration = Duration::from_secs(5);

struct Embassy;

impl Bound for Embassy {
    async fn within<F: core::future::Future>(&self, f: F) -> Option<F::Output> {
        with_timeout(CONNECTION_BOUND, f).await.ok()
    }
}
