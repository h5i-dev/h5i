# h5i-app on a microcontroller: the thermostat

A verified kernel served over a verified HTTP/1.1 parser, on a host and on an
STM32F407 emulated in [Renode](https://renode.io).

```
thermostat/kernel     the kernel (no_std, Aeneas subset), extracted to Lean
thermostat/proofs     its Lean proofs (`cargo app check examples/embedded/thermostat`)
thermostat/app        routes, wire format, sensor and flash encodings, shared by host and firmware (no_std)
thermostat/host       the same server over tokio, for curl and tests
thermostat/firmware   the Cortex-M binary (own workspace, thumbv7em-none-eabihf)
```

## Proven about the kernel (`thermostat/proofs`)

- `transition_total`: no command panics on any state, for any actor.
- `set_relay_refused`: a relay index of 4 or more is refused before any
  array access.
- `inv_reachable`, `interlock`: in every state reached by any run of commands
  from `INITIAL`, the heater is off at or above 35.0 °C and the setpoint is in
  5.0..=30.0 °C.
- `ReadOnly.sound`: `Read` writes nothing.

The mutants in `h5i-app.toml` (interlock boundary, relay bound, setpoint
bound) each break a proof.

## From the kernel to the hardware

The interlock is about the reported temperature and the committed state, so
the firmware keeps both on the device:

- The temperature comes only from a TMP108 on I2C1, read by a sensor task
  and reported as the `SENSOR` principal when it changes. No HTTP route
  reports one. A sensor that fails twice in a row is reported as 35.0 °C,
  so the interlock turns the heater off.
- An actuator task sets the heater (PD12) and relays (PD13, PD14, PD15,
  PD11) from every committed state (`DeviceEngine::changed`). The pin
  mapping (`thermostat_app::outputs`) is trusted.
- Committed states are kept in flash sectors 10 and 11 (`FlashStore`); a
  reset resumes from the last one. The sensor is read before the outputs
  are first set, so a restored state is not acted on with a stale
  temperature.

Writes are signed with HMAC-SHA256 (`h5i_app_embedded::hmac`), bound to a
boot id from the RNG and an increasing counter. The demonstration key is
`dev-key`; provision a real one per device.

## Run it

```bash
# Host (POST /temp stands in for the sensor)
cd examples/embedded && cargo run -p thermostat-host        # 127.0.0.1:8080
curl -s localhost:8080/state
scripts/embedded/h5i-hmac.py --key dev-key PUT http://127.0.0.1:8080/mode heat
scripts/embedded/h5i-hmac.py --key dev-key PUT http://127.0.0.1:8080/relay/7 on   # 404 no_such_relay

# Renode (UART bridged to TCP)
scripts/embedded/install-renode.sh
scripts/embedded/renode-smoke.sh
```

The firmware serves HTTP over USART2; `scripts/embedded/thermostat.resc`
connects the UART to a TCP port and puts a TMP108 model on I2C1 (set its
temperature from the monitor: `i2c1.tmp108 Temperature 36`).
`scripts/embedded/uart-gateway.py` puts TCP clients on that line one at a
time with explicit connection markers (`h5i_app_embedded::link`), so a
partial request from one client can never be completed by the next one's
bytes, a silent client holds the line for at most 5 s, and no client
receives another's response. Point clients at the gateway's port; Renode's
UART port must be reachable only by the gateway.

The smoke script checks reads and signed writes (unsigned, bearer and
replayed requests are 401), the sensor driving the interlock, the GPIO
outputs, that the state survives a reset and the boot id changes, pipelining,
a chunked body, 13 rejections (CL+TE, duplicate CL, obs-fold, space before
the colon, bare LF, bad chunk sizes, oversized heads and targets, ...), that
the device still serves afterwards, and the shared-line attacks: a hijack
attempt, a silent client, and stale responses.

`firmware/size.x` makes the link fail if the statics (including the
connection's buffers, which live in the executor's task) plus a 16 KiB stack
do not fit in RAM.

## Limits

- The firmware uses the blocking flash driver, because Renode's flash
  controller raises no interrupt. A sector erase then stalls the core (about
  a second on silicon); bytes that arrive on the UART meanwhile are lost. A
  damaged signed write fails to verify; a damaged read may be answered or
  refused. On hardware, use embassy's async `Flash`.
- The UART receive ring (2048 bytes) must hold whatever arrives while the
  device handles the request before it.
- Not run on hardware. A network transport exists (`serve_tcp`, tested on
  two embassy-net stacks on a host); no board firmware uses it yet.
