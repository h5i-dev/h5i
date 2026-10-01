# DOM instrument end-to-end check

`pp-innerhtml.html` is a deliberately vulnerable page: it walks the query and
fragment into an object through `__proto__` (prototype pollution) and reflects
the fragment into `innerHTML` (a DOM-XSS sink).

This exercises the **proxy vehicle**, which injects the instrument into real
Chromium driven by `agent-browser`.

```sh
# 1. serve the fixture
python3 -m http.server 8000 --directory crates/h5i-websec/tests/fixtures/dom &

# 2. arm a proxy session with DOM instrumentation
h5i browser proxy http://localhost:8000/pp-innerhtml.html --dom-instrument -s dom

# 3. scan: this DRIVES the session's Chromium through the proxy for each probe,
#    then folds the beaconed reports into findings
h5i websec dom scan http://localhost:8000/pp-innerhtml.html -s dom
h5i websec finding list -s dom
```

Expect a finding for the `h5ipp` pollution canary and one for the
`hash → innerHTML` flow.

Notes:
- `scan` drives the browser itself via `h5i browser proxy-open`. Pass
  `--no-drive` to only collect reports from a browser you drive yourself.
- agent-browser is launched with `--ignore-https-errors` (it has no CA-trust
  flag) and `--proxy-bypass-list=<-loopback>` so Chromium does not bypass the
  proxy for a `localhost` target. Both are handled for you.
- If agent-browser cannot find Chrome, point it at one with
  `AGENT_BROWSER_EXECUTABLE_PATH=/path/to/chrome` in the environment.

To confirm capture stays **byte-faithful** when instrumentation is off, open the
same page through a plain `h5i browser proxy` (no `--dom-instrument`): the stored
response is identical to the origin, and a non-HTML response is never injected.
