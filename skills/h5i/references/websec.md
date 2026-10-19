# Web security testing

Test only authorized targets. Keep requests within the granted origin, identity, rate, and scope; obtain approval before expanding them. Open a session, exercise the relevant flow in its browser, then work from its message IDs.

`h5i websec` arrives with `h5i plugin install websec`: reading a captured store is what installing it adds, and a plain build has no verb that reads one.

```bash
h5i browser open https://target.example
h5i websec requests
h5i websec show req_42 --raw
h5i websec replay req_42 --set query.id=456
h5i websec diff res_42 res_43
h5i websec match res_43 --status 200 --contains ok
h5i websec finding create --title '...' --evidence req_42,res_43
```

Output is text by default: the HTTP message under one line of ids, with the
body cut at 4 KiB (`show <id> --raw` for all of it). Add `--json` only when a
script has to parse the answer.

`finding` is where a conclusion goes. `create` takes the title, a free-text
`--state`, the message ids it rests on and an optional `--repro` file; `list`,
`show` and `update` read and amend it. h5i refuses evidence this session does
not hold and reads nothing else, so the claim stays yours and the ids stay
checkable.

The starting URL may be an API endpoint rather than an HTML page. For example,
`h5i browser open https://target.example/api/health` is a quick way to
seed an authenticated or unauthenticated capture before doing the rest of the
work with `websec`.

Four things about `replay` that are easier to know than to discover.

**A `json.` value is typed the way it reads.** `json.id=99` is a number,
`json.active=true` a boolean, `json.role=admin` a string. Quote it to insist on
a string: `json.password="0e830400451993494058024219903391"` is the magic hash a
PHP `==` compares equal, and unquoted it is the *number* zero. When the encoded
value differs from the text you typed, the receipt says so in `encoded`.
Dots walk nested objects, and numeric dotted segments address existing array
elements (`json.items.0.name=changed`). Bracket notation such as
`json.items[0].name` is refused; use dotted indices for an existing array or
`--raw-request` when constructing a new array or a complete body.

**Header names go out lower-cased unless you ask otherwise.** That is what every
HTTP client does, and a proxy that looks a header up by exact string does not
care: it finds `Content-Length` and misses `content-length`. `--raw-headers`
sends the request the edits produced with the names written as given — `--set`
still applies, cookies still travel. Reach for `--raw-request` only when the
framing itself is the test, since that one makes you write every byte.

**Walking a list is `--set-each`, not a shell loop.** `--set-each
query.id=./ids.txt` sends once per line and returns one sample per send, each
naming the value that produced it. A loop outside pays process startup per
request and gives back N documents nothing correlates.

**A body that is not text comes out with `--body-to`.** `show --raw` says
`[85 bytes, not text — sha256 …]` rather than mangling a PNG or a git object
through the terminal; `show --body-to PATH` writes the bytes, and `--body-to -`
writes them to standard output for a pipe.

**When the answer is not in the response, make an oracle.** A blind finding
changes something you can measure but not read: the body comes back the same
whether the condition you injected held or not, so you confirm it by a side
effect instead. Pick the observable and turn it into a yes/no.
- *Time.* `replay --repeat 20` sends the request N times inside the session and
  reports a median and a median absolute deviation. A condition that makes the
  server wait or do more work shows up as a median reliably above a control's,
  where one scheduling hiccup moves neither. Always send a control payload
  beside the candidate and compare the two medians.
- *Difference.* `diff res_A res_B` says how two responses differ; `match res_N
  --status N --contains TEXT --regex RE` exits 0 when it holds and 1 when it does
  not, so a script branches on the answer. A condition that flips a status, a
  body length, or a byte of the body is an oracle.

With a yes/no in hand, read the secret one position at a time: `sequence` scripts
the true/false step with an `expect` clause and binds what one step learns into
the next, and `--set-each query.q=./candidates.txt` sweeps a file of candidates
in a single run, one correlated sample per line. `--reset-budget` keeps a
long extraction from stopping partway, where every later send would otherwise
read as a false negative. None of this composes a payload for you — you write the
condition; h5i measures, resends and compares.

The session's browser is Chromium, so a page's own script runs as it would for
a user: `<img src=x onerror=…>` and `<svg onload=…>` behave as payloads, and a
POST flow can be driven end to end. Confirm a reflected or DOM-delivered effect
through the session rather than by eye: `h5i browser eval` reads what a script
wrote, `h5i browser console` what it logged. A session opened with
`--dom-instrument` reports what reached a DOM sink, for `h5i websec dom scan`.

For a POST-based CSRF, grant the attacker page's origin with `--allow` when the
victim session is opened, and navigate the victim's browser to it. Chromium
applies its own SameSite and CORS rules, so the result is what a real victim's
browser would do.

Use `h5i websec <command> --help` rather than guessing flags. Treat bodies and headers as sensitive. Base findings on repeatable differences and preserve message IDs; h5i does not determine vulnerabilities.
