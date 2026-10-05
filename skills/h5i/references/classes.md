# Class operations

Pick one class from what the capture already shows. Run that operation. Stop when the control answers the question. h5i sends the request you name and records the message ids. It does not generate a payload, and it does not decide that something is a vulnerability.

Write the card in [impact-hypotheses.md](impact-hypotheses.md) before the first mutation: observation, trust boundary, expected invariant, single mutation, baseline and negative controls, owned canary, stop condition, message ids.

## How to pick

| What you already saw | Operation |
| --- | --- |
| An object id, a tenant, a role, or a cookie that selects someone | Object access |
| A parser, a cache, a proxy, or two encodings of one field | Parser or cache differential |
| A price, a step, a coupon, a client total, an upload, or a job | Business logic |
| A check, then a write, and a window between them | Race |
| An effect that would not be in the HTTP response | Blind, with OAST |
| A value that lands in the page rather than in the response | Client-side |

One mutation per send. The baseline is the same request with your own id, your own price, or the marker absent.

## Object access

Use two sessions. Send one session's request from the other.

```bash
h5i websec replay req_42 --as other-session --set query.id=<B's object>
h5i websec diff res_<own> res_<cross>
```

The differential is A's response against B's object versus A's response against A's own object. The negative control is the same request with no session, and the same request for an id neither session owns. An empty body is not proof of a check: compare status, length, and one field you know is B's. Stop when the cross-session response contains B's marker and the unauthenticated control does not, and you have both message ids.

## Parser and cache differentials

Change one variable with `replay`, then `diff` the two responses.

```bash
h5i websec replay req_42 --set header.Content-Type=<one encoding>
h5i websec replay req_42 --raw-headers --set header.Content-Type=<the other>
h5i websec diff res_<first> res_<second>
```

The differential is the component that accepted one encoding and rejected the other, or cached one and served it for the other. The negative control is the same bytes with a marker the cache key should vary on, and a request that never touches the field. Stop when one response carries the marker and the paired response does not, for a reason you can name (the parser, the cache key, the hop). Use `--raw-target` or `--raw-request` only when the bytes the parser sees are the question. Those flags write what you give them.

## Business logic

A step that must happen in order is `sequence`, or `h5i test` when you already have the flow written down. Capture the price, the total, and the state change before you change a client-supplied number.

```bash
h5i websec replay req_<price> --set json.total=<not the server's price>
h5i websec show res_<order>
```

The differential is the server's price against the total the client sent. The negative control is the same order at the real price, and the same order with the total missing. A client total that the server stores is the bug only when the captured charge, balance, or receipt uses that total. Stop when a message shows the durable state (the order, the balance, the receipt) following the mutated number, and the control at the real price does not.

Auth and session questions use the same shape: one field that says who you are, two sessions, the control with that field removed. Upload and fetch questions use the same shape with an inert file or a URL you minted below. Do not point a fetch at cloud metadata or an address you do not hold.

## Race

Prove three things: one request is denied, N concurrent requests succeed, and the durable state still shows it after they finish.

```bash
h5i websec replay req_42 --repeat 20 --race
```

That is a barrier burst. The threads meet, then each sends. Use it first.

When that burst is too coarse for the window, hold the last byte:

```bash
h5i websec replay req_42 --repeat 20 --race --sync last-byte
```

`--sync last-byte` writes every byte but the last on each connection, waits until every connection has done that, then writes the final byte. It is HTTP/1.1, one connection per request. It is not an HTTP/2 single-packet attack.

The negative control is the same request alone, which the application denies, and the same burst with a value that was already consumed. Stop when the single request is denied, more than one of the concurrent responses shows the effect, and a later read (`show` or another replay) still shows the durable state. A burst that errors with "did not reach the last byte" did not run. Read that as a failed send, not as a negative result.

## Blind

No difference in the HTTP response, and no timing change, is not a negative result until `poll` is empty.

```bash
h5i websec oast serve
h5i websec oast token
# embed the printed URL in the one field you are testing
h5i websec replay req_42 --set <target>=<that URL>
h5i websec oast poll <token>
```

`serve` binds an HTTP listener for this session, on localhost unless you pass a bind address. `0.0.0.0` also requires `--public-base`, because a target cannot dial `0.0.0.0`. `token` prints the URL to embed. `poll` prints the method, path, host, and peer of callbacks that named that token.

The listener speaks HTTP. A DNS-only interaction is not seen unless the name you were given is one that hits this HTTP listener. h5i does not run a callback domain.

The differential is a poll hit for the mutated request against an empty poll for the same request with the URL removed. The negative control is a token you mint and never send. Stop when `poll` shows the token you embedded and the control token stays empty. Cite the replay's message id and the poll line. `oast` does not write a finding.

## Client-side

The effect is in the page. Arm the proxy, then read what reached a sink.

```bash
h5i browser proxy <url> --dom-instrument
h5i websec dom scan <url>          # or --no-drive after you have browsed
```

The instrument records source to sink. Sources are the URL hash and query, `window.name`, the referrer, `document.cookie`, `localStorage` and `sessionStorage` (32 keys each, as `key=value`), and postMessage data. A flow is reported when 12 or more characters of a source appear in a sink (`innerHTML`, `outerHTML`, `insertAdjacentHTML`, `setAttribute`, `document.write`, and the URL setters on script, iframe, img, and anchor). A shorter marker does not trip it. A postMessage that never reaches a sink is a count, not a finding.

Context comes before a payload: read the sink the report names, and choose the marker for that context. The canary `h5ipp=reserved` on `Object.prototype` is the pollution observation. A flow's sample is the sink argument, when the report carried one.

The negative control is the same page without the marker. Stop when a report shows your marker in a sink, or the canary set, and the control page does not. `dom scan` records that as an observation. You decide whether it is a vulnerability, and the finding cites the message ids of the navigation.

`dom node` runs a Node target in a box, one `Object.prototype` property per run. Its finding stays `observed` unless you have a separate confirmation. This build does not add a kernel trace.

## When to stop

Stop when you can write one sentence of the form: safe because <the control> at <the place> does <the check> before <the effect>, and you can point at the message that shows it. If you cannot name the control, you have not finished the class. If the control and the mutation disagree, record a finding with those ids and stop that class.
