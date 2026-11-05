# h5i-app on a microcontroller: the thermostat

A verified kernel served over a verified HTTP/1.1 parser, on a host and on an
STM32F407 emulated in [Renode](https://renode.io).

```
thermostat/kernel     the kernel (no_std, Aeneas subset), extracted to Lean
thermostat/proofs     its Lean proofs (`cargo app check examples/embedded/thermostat`)
thermostat/app        router and wire format, shared by host and firmware (no_std)
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

## Run it

```bash
# Host
cd examples/embedded && cargo run -p thermostat-host        # 127.0.0.1:8080
curl -s localhost:8080/state
curl -s -X PUT -H 'Authorization: Bearer dev-key' -d heat localhost:8080/mode
curl -s -X PUT -H 'Authorization: Bearer dev-key' -d on   localhost:8080/relay/7   # 404 no_such_relay

# Renode (UART bridged to TCP)
scripts/embedded/install-renode.sh
scripts/embedded/renode-smoke.sh
```

The firmware serves HTTP over USART2; `scripts/embedded/thermostat.resc`
connects the UART to a TCP port, so curl talks to the device byte for byte,
one connection at a time. After a response that closes the connection the
firmware drops input until the line is quiet for 50 ms, since a UART has no
connection to close. The smoke script checks reads, writes, the interlock,
pipelining, a chunked body, 13 rejections (CL+TE, duplicate CL, obs-fold,
space before the colon, bare LF, bad chunk sizes, oversized heads and
targets, ...) and that the device still serves afterwards.

`firmware/size.x` makes the link fail if the statics (including the
connection's buffers, which live in the executor's task) plus a 16 KiB stack
do not fit in RAM.

Ethernet (embassy-net over Renode's MAC model) was not attempted; the UART
transport is the design's second choice.
