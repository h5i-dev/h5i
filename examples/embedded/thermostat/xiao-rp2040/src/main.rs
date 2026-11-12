//! The thermostat on a bare Seeed XIAO RP2040: no extension board, no wiring.
//!
//! The same `thermostat-app` router and `h5i-app-embedded` shell as the host
//! runner and the STM32 firmware, over the verified `h5i-app-h1` parser.
//!
//! - HTTP over the USB-C port (CDC-ACM, `/dev/ttyACM0`), framed by
//!   `h5i_app_embedded::link` like the STM32's UART; `uart-gateway.py` on the
//!   host puts TCP clients on it one at a time.
//! - The temperature is the RP2040's on-die sensor (ADC channel 4), reported
//!   as the `SENSOR` principal when it changes. An ADC error is reported as
//!   the interlock temperature, so the heater goes off.
//! - The heater is the red LED, relays 0 and 1 the green and blue ones.
//!   Relays 2 and 3 exist only in the state.
//!
//! No flash store: a reset starts from `INITIAL`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_rp::adc::{self, Adc, Channel};
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{self, Driver};
use embassy_rp::bind_interrupts;
use embassy_time::{Duration, Timer, with_timeout};
use embassy_usb::UsbDevice;
use embassy_usb::class::cdc_acm::{BufferedReceiver, CdcAcmClass, CdcAcmError, Sender, State};
use h5i_app_embedded::Config;
use h5i_app_embedded::link::{Bound, Link, serve_link};
use panic_halt as _;
use static_cell::StaticCell;
use thermostat_app::{App, SENSOR, outputs, router};
use thermostat_kernel::{Command, MAX_TEMP_DC};

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => usb::InterruptHandler<USB>;
    ADC_IRQ_FIFO => adc::InterruptHandler;
});

/// A demonstration key: provision a real one per device.
const KEY: &[u8] = b"dev-key";

/// Per-connection memory: fixed at compile time.
const RX: usize = 2048;
const TX: usize = 1024;

const SENSOR_PERIOD: Duration = Duration::from_millis(500);

/// The longest one connection may take, from its first byte to its last
/// response: a silent or slow client holds the line no longer than this.
const CONNECTION_BOUND: Duration = Duration::from_secs(5);

type UsbDriver = Driver<'static, USB>;

static APP: StaticCell<App> = StaticCell::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    // The RP2040 has no TRNG; the ring oscillator is random enough for a
    // boot id, which only has to differ between boots.
    let boot = RoscRng.next_u32();
    let app: &'static App = APP.init(App::new(KEY, boot));

    // The temperature before the outputs.
    let mut adc = Adc::new(p.ADC, Irqs, adc::Config::default());
    let mut die = Channel::new_temp_sensor(p.ADC_TEMP_SENSOR);
    let last = report(app, &mut adc, &mut die, None).await;
    // The XIAO's RGB LED is active low.
    let pins = [
        Output::new(p.PIN_17, Level::High),
        Output::new(p.PIN_16, Level::High),
        Output::new(p.PIN_25, Level::High),
    ];
    spawner.spawn(actuate(app, pins).unwrap());
    spawner.spawn(sense(app, adc, die, last).unwrap());

    // USB CDC-ACM.
    let mut config = embassy_usb::Config::new(0x1209, 0x0001);
    config.manufacturer = Some("h5i");
    config.product = Some("thermostat (XIAO RP2040)");
    config.max_power = 100;
    static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    static STATE: StaticCell<State> = StaticCell::new();
    static PACKET: StaticCell<[u8; 64]> = StaticCell::new();
    let mut builder = embassy_usb::Builder::new(
        Driver::new(p.USB, Irqs),
        config,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        &mut [],
        CONTROL_BUF.init([0; 64]),
    );
    let class = CdcAcmClass::new(&mut builder, STATE.init(State::new()), 64);
    spawner.spawn(usb_task(builder.build()).unwrap());

    let (tx, rx) = class.split();
    let mut port = Port { tx, rx: rx.into_buffered(PACKET.init([0; 64])) };
    let routes = router();
    let mut rx = [0u8; RX];
    let mut tx = [0u8; TX];
    loop {
        // Until a host opens the port; a cable pulled ends `serve_link`.
        port.rx.wait_connection().await;
        let mut link = Link::new(&mut port);
        let _ = serve_link(&Config::default(), &routes, app, &mut rx, &mut tx, &mut link, &Embassy).await;
    }
}

/// The die temperature in tenths of a degree from a 12-bit ADC reading
/// (RP2040 datasheet 4.9.5: 27 °C at 0.706 V, -1.721 mV/°C, 3.3 V reference),
/// or the interlock temperature for an ADC error, so it fails safe.
fn temp_command(raw: Option<u16>) -> Command {
    let dc = match raw {
        Some(raw) => {
            let uv = raw as i64 * 3_300_000 / 4096;
            (270 - (uv - 706_000) * 10 / 1721).clamp(i16::MIN as i64, i16::MAX as i64) as i16
        }
        None => MAX_TEMP_DC,
    };
    Command::ReportTemp { dc }
}

/// Read the sensor (8 samples averaged) and report it, as the sensor
/// principal, unless it equals `last`; the reading.
async fn report(app: &App, adc: &mut Adc<'static, adc::Async>, die: &mut Channel<'static>, last: Option<Command>) -> Option<Command> {
    let mut sum = 0u32;
    let mut ok = true;
    for _ in 0..8 {
        match adc.read(die).await {
            Ok(v) => sum += v as u32,
            Err(_) => ok = false,
        }
    }
    let cmd = temp_command(ok.then_some((sum / 8) as u16));
    if last == Some(cmd) {
        return last;
    }
    match app.engine.execute(&SENSOR, &cmd, None).await {
        Ok(_) => Some(cmd),
        Err(_) => None,
    }
}

#[embassy_executor::task]
async fn sense(app: &'static App, mut adc: Adc<'static, adc::Async>, mut die: Channel<'static>, mut last: Option<Command>) -> ! {
    loop {
        Timer::after(SENSOR_PERIOD).await;
        last = report(app, &mut adc, &mut die, last).await;
    }
}

#[embassy_executor::task]
async fn actuate(app: &'static App, mut pins: [Output<'static>; 3]) -> ! {
    let mut seen = app.engine.version();
    let mut s = app.engine.snapshot();
    loop {
        for (pin, on) in pins.iter_mut().zip(outputs(&s)) {
            pin.set_level(if on { Level::Low } else { Level::High });
        }
        s = app.engine.changed(&mut seen).await;
    }
}

#[embassy_executor::task]
async fn usb_task(mut usb: UsbDevice<'static, UsbDriver>) -> ! {
    usb.run().await
}

/// The CDC-ACM halves as one byte stream for `Link`.
struct Port {
    tx: Sender<'static, UsbDriver>,
    rx: BufferedReceiver<'static, UsbDriver>,
}

impl embedded_io_async::ErrorType for Port {
    type Error = CdcAcmError;
}

impl embedded_io_async::Read for Port {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, CdcAcmError> {
        self.rx.read(buf).await
    }
}

impl embedded_io_async::Write for Port {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, CdcAcmError> {
        self.tx.write(buf).await
    }
    async fn flush(&mut self) -> Result<(), CdcAcmError> {
        self.tx.flush().await
    }
}

struct Embassy;

impl Bound for Embassy {
    async fn within<F: core::future::Future>(&self, f: F) -> Option<F::Output> {
        with_timeout(CONNECTION_BOUND, f).await.ok()
    }
}
