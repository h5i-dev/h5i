//! The thermostat on an STM32F407, serving HTTP/1.1 over USART2.
//!
//! The same `thermostat-app` router and `h5i-app-embedded` shell as the
//! host runner, over the verified `h5i-app-h1` parser. In Renode,
//! `scripts/embedded/thermostat.resc` connects USART2 to a TCP socket so
//! `curl http://127.0.0.1:8080/state` reaches it byte for byte.
//!
//! A UART has no connections: after a response that closes the connection
//! (a rejection, `Connection: close`, HTTP/1.0) the firmware drops input
//! until the line has been quiet for 50 ms, so the rest of a refused
//! request is not read as the next one.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_stm32::usart::{BufferedInterruptHandler, BufferedUart, Config as UartConfig};
use embassy_stm32::{bind_interrupts, peripherals};
use embassy_time::{Duration, with_timeout};
use embedded_io_async::{Read, Write};
use h5i_app_embedded::{Config, serve};
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
    let Ok(mut uart) = BufferedUart::new(p.USART2, p.PA3, p.PA2, &mut uart_tx, &mut uart_rx, Irqs, cfg) else {
        // A configuration the peripheral refuses: nothing to serve on.
        loop {
            cortex_m::asm::wfi();
        }
    };
    let _ = uart.write_all(b"").await;
    let routes = router();
    let mut rx = [0u8; RX];
    let mut tx = [0u8; TX];
    loop {
        let _ = serve(&Config::default(), &routes, &APP, &mut rx, &mut tx, &mut uart).await;
        drain(&mut uart).await;
    }
}

/// Read and drop bytes until none arrive for 50 ms.
async fn drain<IO: Read>(io: &mut IO) {
    let mut junk = [0u8; 64];
    while let Ok(Ok(n)) = with_timeout(Duration::from_millis(50), io.read(&mut junk)).await {
        if n == 0 {
            return;
        }
    }
}
