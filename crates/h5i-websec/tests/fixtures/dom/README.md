# DOM instrument end-to-end check

`pp-innerhtml.html` is a deliberately vulnerable page: it walks the query and
fragment into an object through `__proto__` (prototype pollution) and reflects
the fragment into `innerHTML` (a DOM-XSS sink).

This exercises the **proxy vehicle**, which injects the instrument into real
Chromium. It needs `agent-browser` and (on Linux) `certutil`, so it is a manual
check, not a unit test.

```sh
# 1. serve the fixture
python3 -m http.server 8000 --directory crates/h5i-websec/tests/fixtures/dom &

# 2. arm a proxy session with DOM instrumentation
h5i browser proxy http://localhost:8000/pp-innerhtml.html --dom-instrument -s dom
# -> prints an agent-browser line that trusts the session CA

# 3. drive the probes through the proxied browser (pollution + a sink flow)
agent-browser --proxy <url> --ca-cert <pem> \
  open 'http://localhost:8000/pp-innerhtml.html?__proto__[h5ipp]=reserved#name=<b>x12345678</b>'

# 4. fold the beaconed reports into findings
h5i websec dom scan http://localhost:8000/pp-innerhtml.html -s dom
h5i websec finding list -s dom
```

Expect a finding for the `h5ipp` pollution canary and one for the
`hash → innerHTML` flow.

To confirm capture stays **byte-faithful** when instrumentation is off, open the
same page through a plain `h5i browser proxy` (no `--dom-instrument`) and diff
the stored response against the origin: it must be identical, and a non-HTML
response (JSON, image) must round-trip unchanged either way.
