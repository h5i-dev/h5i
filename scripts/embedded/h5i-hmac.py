#!/usr/bin/env python3
"""Send one request signed for h5i_app_embedded::hmac::HmacAuth.

    scripts/embedded/h5i-hmac.py --key dev-key PUT http://127.0.0.1:8080/mode heat

Asks the device for its boot id (an unsigned request is refused with
`WWW-Authenticate: H5i-HMAC boot=...,last=...`), then signs with the time in
microseconds as the counter. Prints the response body; exits 1 unless 2xx.
"""
import argparse, hashlib, hmac, http.client, re, sys, time, urllib.parse


def authorization(key, boot, counter, method, target, body):
    msg = b"\n".join([b"h5i-hmac", boot, str(counter).encode(), method, target, body])
    mac = hmac.new(key, msg, hashlib.sha256).hexdigest()
    return b"H5i-HMAC %s:%d:%s" % (boot, counter, mac.encode())


def challenge(headers):
    m = re.search(r"boot=([0-9a-f]{8}),last=([0-9]+)", headers.get("WWW-Authenticate", ""))
    if not m:
        sys.exit("no H5i-HMAC challenge in the response")
    return m.group(1).encode(), int(m.group(2))


def request(url, method, body, headers=None):
    u = urllib.parse.urlsplit(url)
    c = http.client.HTTPConnection(u.hostname, u.port or 80, timeout=20)
    target = u.path + ("?" + u.query if u.query else "")
    c.request(method, target, body=body, headers=headers or {})
    r = c.getresponse()
    return r.status, dict(r.getheaders()), r.read(), target.encode()


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--key", required=True)
    p.add_argument("method")
    p.add_argument("url")
    p.add_argument("body", nargs="?", default="")
    a = p.parse_args()
    body = a.body.encode()
    status, headers, _, target = request(a.url, a.method, body)
    if status != 401:
        sys.exit("expected a 401 challenge, got %d" % status)
    boot, last = challenge(headers)
    counter = max(last + 1, time.time_ns() // 1000)
    auth = authorization(a.key.encode(), boot, counter, a.method.encode(), target, body)
    status, _, out, _ = request(a.url, a.method, body, {"Authorization": auth.decode()})
    sys.stdout.write(out.decode(errors="replace") + "\n")
    sys.exit(0 if 200 <= status < 300 else 1)


if __name__ == "__main__":
    main()
