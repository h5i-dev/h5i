//! The thermostat on an STM32F407, serving HTTP/1.1 over USART2.
//!
//! The same `thermostat-app` router and `h5i-app-embedded` shell as the
//! host runner, over the verified `h5i-app-h1` parser. Three tasks share one
//! `DeviceEngine`:
//!
//! - the server, on USART2. A UART has no connections, so the line is
//!   framed (`h5i_app_embedded::link`); a gateway on the host
//!   (`scripts/embedded/uart-gateway.py`) puts TCP clients on it one at a
//!   time. Each connection gets an empty receive buffer and at most
//!   `CONNECTION_BOUND`.
//! - the sensor: a TMP108 on I2C1 (PB6 SCL, PB9 SDA, address 0x48), read
//!   every `SENSOR_PERIOD` and, when it changes, reported to the kernel as
//!   the `SENSOR` principal. A sensor unreadable twice in a row is reported
//!   as the interlock temperature, so the heater goes off. The read is blocking (about
//!   0.3 ms at 100 kHz), bounded by a 5 ms timeout.
//! - the actuator: the heater on PD12 and relays 0-3 on PD13, PD14, PD15,
//!   PD11 follow every committed state (`DeviceEngine::changed`).
//!
//! Committed states are kept in flash sectors 10 and 11 (`FlashStore`), so a
//! reset resumes from the last one. The boot id that signed requests are
//! bound to comes from the RNG.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_stm32::flash::{Blocking, Flash};
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::i2c::{self, I2c};
use embassy_stm32::rng::{self, Rng};
use embassy_stm32::usart::{BufferedInterruptHandler, BufferedUart, Config as UartConfig};
use embassy_stm32::{bind_interrupts, mode, peripherals};
use embassy_time::{Duration, Timer, with_timeout};
use h5i_app_embedded::Config;
use h5i_app_embedded::link::{Bound, Link, serve_link};
use h5i_app_embedded::store::FlashStore;
use panic_halt as _;
use static_cell::StaticCell;
use thermostat_app::{App, SENSOR, Thermostat, outputs, router, sensor_command};
use thermostat_kernel::Command;

bind_interrupts!(struct Irqs {
    USART2 => BufferedInterruptHandler<peripherals::USART2>;
    RNG => rng::InterruptHandler<peripherals::RNG>;
});

/// A demonstration key: provision a real one per device.
const KEY: &[u8] = b"dev-key";

/// Per-connection memory: fixed at compile time.
const RX: usize = 2048;
const TX: usize = 1024;

/// Flash sectors 10 and 11 (128 KiB each), past the firmware.
const STORE_BASE: u32 = 0x000C_0000;
const STORE_REGION: u32 = 0x0002_0000;

const TMP108: u8 = 0x48;
const SENSOR_PERIOD: Duration = Duration::from_millis(500);

/// The longest one connection may take, from its first byte to its last
/// response: a silent or slow client holds the line no longer than this.
const CONNECTION_BOUND: Duration = Duration::from_secs(5);

type Store = FlashStore<BlockingFlash, Thermostat, thermostat_app::Flash, 32>;
static APP: StaticCell<App<Store>> = StaticCell::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());

    let boot = Rng::new(p.RNG, Irqs).next_u32();
    let flash = BlockingFlash(Flash::new_blocking(p.FLASH));
    let app: &'static App<Store> = APP.init(App::with_store(KEY, boot, FlashStore::new(flash, STORE_BASE, STORE_REGION)));
    // A store that cannot be read starts from the initial state.
    let _ = app.engine.restore().await;

    // The temperature before the outputs: a restored state may be stale.
    let mut i2c_cfg = i2c::Config::default();
    // Blocking, so a dead sensor may stall the executor this long at most.
    i2c_cfg.timeout = Duration::from_millis(5);
    let mut sensor = I2c::new_blocking(p.I2C1, p.PB6, p.PB9, i2c_cfg);
    let last = report(app, &mut sensor, None).await;
    let pins = [
        Output::new(p.PD12, Level::Low, Speed::Low),
        Output::new(p.PD13, Level::Low, Speed::Low),
        Output::new(p.PD14, Level::Low, Speed::Low),
        Output::new(p.PD15, Level::Low, Speed::Low),
        Output::new(p.PD11, Level::Low, Speed::Low),
    ];
    spawner.spawn(actuate(app, pins).unwrap());
    spawner.spawn(sense(app, sensor, last).unwrap());

    let mut uart_tx = [0u8; 512];
    // Holds a pipelined burst while the device handles the request before
    // it; bytes past a full ring are lost. (512 was too few in Renode, which
    // delivers a burst at once.)
    let mut uart_rx = [0u8; 2048];
    let mut cfg = UartConfig::default();
    cfg.baudrate = 115_200;
    let Ok(uart) = BufferedUart::new(p.USART2, p.PA3, p.PA2, &mut uart_tx, &mut uart_rx, Irqs, cfg) else {
        // A configuration the peripheral refuses: nothing to serve on.
        loop {
            cortex_m::asm::wfi();
        }
    };
    let routes = router::<Store>();
    let mut rx = [0u8; RX];
    let mut tx = [0u8; TX];
    let mut link = Link::new(uart);
    loop {
        // Only a broken UART ends this; start over.
        let _ = serve_link(&Config::default(), &routes, app, &mut rx, &mut tx, &mut link, &Embassy).await;
    }
}

/// Read the sensor and report the reading, as the sensor principal, unless
/// it equals `last`; the reading.
async fn report(app: &App<Store>, sensor: &mut I2c<'static, mode::Blocking, i2c::Master>, last: Option<Command>) -> Option<Command> {
    // Point at the temperature register and read it; one retry, since a
    // transfer can fail once (a glitch, or Renode's I2C model after the
    // previous read) without the sensor being gone.
    let mut raw = [0u8; 2];
    let reading = (0..2).find_map(|_| sensor.blocking_write_read(TMP108, &[0x00], &mut raw).ok()).map(|_| raw);
    let cmd = sensor_command(reading);
    if last == Some(cmd) {
        return last;
    }
    // `ReportTemp` is accepted for any principal. If the store fails, the
    // state is unchanged and the next reading tries again.
    match app.engine.execute(&SENSOR, &cmd, None).await {
        Ok(_) => Some(cmd),
        Err(_) => None,
    }
}

#[embassy_executor::task]
async fn sense(app: &'static App<Store>, mut sensor: I2c<'static, mode::Blocking, i2c::Master>, mut last: Option<Command>) -> ! {
    loop {
        Timer::after(SENSOR_PERIOD).await;
        last = report(app, &mut sensor, last).await;
    }
}

#[embassy_executor::task]
async fn actuate(app: &'static App<Store>, mut pins: [Output<'static>; 5]) -> ! {
    let mut seen = app.engine.version();
    let mut s = app.engine.snapshot();
    loop {
        for (pin, on) in pins.iter_mut().zip(outputs(&s)) {
            pin.set_level(if on { Level::High } else { Level::Low });
        }
        s = app.engine.changed(&mut seen).await;
    }
}

struct Embassy;

impl Bound for Embassy {
    async fn within<F: core::future::Future>(&self, f: F) -> Option<F::Output> {
        with_timeout(CONNECTION_BOUND, f).await.ok()
    }
}

/// The blocking flash driver as an async one. Renode's flash controller
/// raises no interrupt, which embassy's async driver waits for. A sector
/// erase stalls the core (about a second on silicon); on hardware,
/// `Flash::new` (async) avoids that.
struct BlockingFlash(Flash<'static, Blocking>);

impl embedded_storage_async::nor_flash::ErrorType for BlockingFlash {
    type Error = embassy_stm32::flash::Error;
}

impl embedded_storage_async::nor_flash::ReadNorFlash for BlockingFlash {
    const READ_SIZE: usize = <Flash<'static, Blocking> as embedded_storage::nor_flash::ReadNorFlash>::READ_SIZE;
    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0.blocking_read(offset, bytes)
    }
    fn capacity(&self) -> usize {
        embedded_storage::nor_flash::ReadNorFlash::capacity(&self.0)
    }
}

impl embedded_storage_async::nor_flash::NorFlash for BlockingFlash {
    const WRITE_SIZE: usize = <Flash<'static, Blocking> as embedded_storage::nor_flash::NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <Flash<'static, Blocking> as embedded_storage::nor_flash::NorFlash>::ERASE_SIZE;
    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.0.blocking_erase(from, to)
    }
    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.0.blocking_write(offset, bytes)
    }
}
