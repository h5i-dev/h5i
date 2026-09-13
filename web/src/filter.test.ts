import { describe, expect, it } from "vitest";

import type { ActionRow, RequestRecord } from "./api";
import { fold, matches, parse, withTerm, type HistoryRow } from "./filter";

function row(over: Partial<HistoryRow>): HistoryRow {
  return {
    seq: 1,
    at: "2026-09-11T00:00:00Z",
    method: "GET",
    url: "https://api.example.com/v1/users?id=7",
    host: "api.example.com",
    path: "/v1/users",
    query: "id=7",
    initiator: "subresource",
    allowed: true,
    status: 200,
    bytes: 1200,
    duration_ms: 40,
    answered: true,
    ...over,
  };
}

describe("parse", () => {
  it("reads bare words, keys, operators, negation and alternatives", () => {
    const p = parse('users method:GET|POST status:>=400 -host:cdn path:~"^/v[0-9]"');
    expect(p.errors).toEqual([]);
    expect(p.terms.map((t) => [t.neg, t.key, t.op, t.values])).toEqual([
      [false, null, "=", ["users"]],
      [false, "method", "=", ["GET", "POST"]],
      [false, "status", ">=", ["400"]],
      [true, "host", "=", ["cdn"]],
      [false, "path", "~", ["^/v[0-9]"]],
    ]);
  });

  it("refuses a field it does not know rather than matching nothing", () => {
    const p = parse("hots:example");
    expect(p.terms).toEqual([]);
    expect(p.errors[0]).toContain("`hots` is not a field");
  });

  it("refuses an operator that does not fit the field", () => {
    expect(parse("host:>3").errors[0]).toContain("text");
    expect(parse("ms:~3").errors[0]).toContain("number");
    expect(parse("status:").errors[0]).toContain("needs a value");
    expect(parse("path:~[").errors[0]).toContain("regular expression");
  });

  it("spells the common words as their is: form", () => {
    expect(parse("refused").terms[0]).toMatchObject({ key: "is", values: ["refused"] });
    expect(parse("-errors").terms[0]).toMatchObject({ neg: true, key: "is", values: ["error"] });
    expect(parse("replay").terms[0]).toMatchObject({ key: "initiator", values: ["replay"] });
  });
});

describe("matches", () => {
  const r = row({});
  const ok = (q: string, x: HistoryRow = r) => matches(x, parse(q).terms);

  it("bare words search the URL", () => {
    expect(ok("USERS")).toBe(true);
    expect(ok("orders")).toBe(false);
  });

  it("methods are whole words, hosts and paths are fragments", () => {
    expect(ok("method:GE")).toBe(false);
    expect(ok("method:get")).toBe(true);
    expect(ok("host:example")).toBe(true);
    expect(ok("path:/v1")).toBe(true);
  });

  it("status classes and comparisons", () => {
    expect(ok("status:2xx")).toBe(true);
    expect(ok("status:4xx")).toBe(false);
    expect(ok("status:200|404")).toBe(true);
    expect(ok("status:>199 status:<300")).toBe(true);
    expect(ok("status:>=400")).toBe(false);
  });

  it("refusals and errors", () => {
    const refused = row({ allowed: false, status: undefined, denied_reason: "off scope" });
    expect(ok("refused", refused)).toBe(true);
    expect(ok("refused")).toBe(false);
    expect(ok("-refused", refused)).toBe(false);
    expect(ok("error", row({ status: 500 }))).toBe(true);
    expect(ok("error", row({ error: "reset" }))).toBe(true);
    expect(ok("is:pending", row({ answered: false, status: undefined }))).toBe(true);
  });

  it("a row with no number never satisfies a numeric term", () => {
    expect(ok("ms:<100", row({ duration_ms: undefined }))).toBe(false);
    expect(ok("ms:<100")).toBe(true);
  });

  it("the verb and action join", () => {
    const clicked = row({ verb: "click", action_seq: 4 });
    expect(ok("verb:click", clicked)).toBe(true);
    expect(ok("action:4", clicked)).toBe(true);
    expect(ok("verb:click")).toBe(false);
  });

  it("regular expressions are case-insensitive", () => {
    expect(ok("path:~^/V1/USERS$")).toBe(true);
  });
});

describe("fold", () => {
  it("joins the two phases and the verb that spent them", () => {
    const records: RequestRecord[] = [
      { seq: 0, at: "t0", phase: "request", initiator: "navigation", method: "GET", url: "https://a.test/", allowed: true },
      { seq: 1, at: "t1", phase: "request", initiator: "subresource", method: "GET", url: "https://cdn.test/x.js", allowed: false, denied_reason: "off scope" },
      { seq: 0, at: "t2", phase: "response", initiator: "navigation", method: "GET", url: "https://a.test/", allowed: true, status: 200, bytes: 10, duration_ms: 5, ttfb_ms: 2, cookies_sent: 1, cookies_stored: 0 },
    ];
    const actions: ActionRow[] = [{ seq: 0, at: "t0", verb: "open", ok: true, requests: [0, 1] }];
    const rows = fold(records, actions);
    expect(rows.map((r) => r.seq)).toEqual([0, 1]);
    expect(rows[0]).toMatchObject({ status: 200, ttfb_ms: 2, cookies_sent: 1, verb: "open", action_seq: 0, answered: true, host: "a.test", path: "/" });
    expect(rows[1]).toMatchObject({ allowed: false, denied_reason: "off scope", answered: false, verb: "open" });
  });
});

describe("withTerm", () => {
  it("replaces a term on the same key and keeps the rest", () => {
    expect(withTerm("status:4xx host:old -host:x", "host", "new.test")).toBe("status:4xx host:new.test");
    expect(withTerm("", "path", "/a b")).toBe('path:"/a b"');
  });
});
