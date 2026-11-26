# Run an h5i kernel on a bare-metal device

Use `h5i-app-embedded` for an async, `no_std`, heap-free HTTP/1.1 shell around
the `h5i-app-h1` parser and framing core. Use
`examples/embedded/thermostat` as the reference layout. Its `kernel/` contains
the extractable decisions, `app/` shares routes and wire encodings,
`host/` runs the same server on Tokio, and the board crates supply hardware I/O.
Follow [app.md](app.md) for the extraction and proof gate.

## Wire the device shell

- Pass fixed receive and response buffers to `serve` over
  `embedded_io_async` I/O. Size the buffers for the supported requests and
  responses, and supply a `Bound` for request and response deadlines. Use
  `NoBound` when the transport supplies its own deadlines.
- Use `Router` and `Calls::call` to bind each route to its actor and command
  decoder. Keep sensor identities in the device task that reads the sensor.
  The `hmac` feature provides HMAC-SHA256 request authentication with a
  boot ID and increasing counter. Provision per-device keys and account
  separately for transport confidentiality and response authentication.
- Use `DeviceEngine` for a single tenant's snapshot. It saves writes through
  its `Store` before publishing the new state. Device tasks can use
  `changed` to observe committed state. Specify the semantics an actuator
  needs if several commits arrive before it polls.
- Use `FlashStore` with the `flash` feature for NOR persistence. Review the
  flash geometry, image layout, and recovery assumptions. The extracted
  `store_core.rs` and proofs under `crates/h5i-app-embedded/proofs` cover
  specific recovery properties under the modeled NOR operations.
- Use `serve_tcp` with the `embassy-net` feature for TCP, or
  `link::serve_link` for a framed serial connection. The sample
  `scripts/embedded/uart-gateway.py` bridges TCP clients to the serial link.

## Verify the target

From the repository root, start with the host tests and kernel proof gate.

```bash
cargo test --manifest-path examples/embedded/Cargo.toml --workspace --locked
h5i app prove examples/embedded/thermostat
```

The STM32F407 example in `thermostat/firmware` targets
`thumbv7em-none-eabihf`. With Renode installed, run
`scripts/embedded/renode-smoke.sh` to exercise its UART, sensor, GPIO, and
flash reset behavior. The separate `thermostat/xiao-rp2040` example targets
`thumbv6m-none-eabi` and uses USB CDC-ACM, an on-die sensor, and LEDs. It
starts from the initial snapshot on reset.

Report evidence at the layer exercised. Kernel and parser theorems apply
under their stated assumptions. Host tests and Renode cover the exercised
integration paths. Board timing, sensor and actuator behavior, and flash
driver behavior need evidence on the target. See
`crates/h5i-app-embedded/README.md` and `examples/embedded/README.md` for
the APIs and STM32 walkthrough, and [temporal.md](temporal.md) for properties
over device runs, crashes, and recovery.
