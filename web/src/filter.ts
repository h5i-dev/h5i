// The history filter: a small query language over one session's fetches.
//
// Shaped after the proxies people already know (Caido's HTTPQL, Burp's
// filter bar) but flattened to what fits in one input: space-separated terms
// are ANDed, `a|b` inside a value is OR, a leading `-` negates, and a bare
// word matches anywhere in the URL. Every term is checkable by eye, so a
// reader can always say why a row is or is not on the screen.

import type { ActionRow, RequestRecord } from "./api";

/** One fetch: both log phases folded, plus the verb that caused it. */
export interface HistoryRow {
  seq: number;
  at: string;
  method: string;
  url: string;
  host: string;
  path: string;
  query: string;
  initiator: string;
  allowed: boolean;
  denied_reason?: string;
  status?: number;
  bytes?: number;
  duration_ms?: number;
  ttfb_ms?: number;
  cookies_sent?: number;
  cookies_stored?: number;
  error?: string;
  /** The agent verb that spent this fetch, when the action log says so. */
  verb?: string;
  action_seq?: number;
  /** Whether the response phase has been written yet. */
  answered: boolean;
}

/** Two log lines are one fetch: fold the response onto its request. */
export function fold(records: RequestRecord[], actions: ActionRow[]): HistoryRow[] {
  const byAction = new Map<number, ActionRow>();
  for (const a of actions) for (const seq of a.requests) byAction.set(seq, a);
  const rows = new Map<number, HistoryRow>();
  for (const r of records) {
    const have = rows.get(r.seq);
    if (!have) {
      const parts = splitUrl(r.url);
      const action = byAction.get(r.seq);
      rows.set(r.seq, {
        seq: r.seq,
        at: r.at,
        method: r.method,
        url: r.url,
        ...parts,
        initiator: r.initiator,
        allowed: r.allowed,
        denied_reason: r.denied_reason,
        verb: action?.verb,
        action_seq: action?.seq,
        answered: r.phase === "response",
        ...(r.phase === "response" ? responseFields(r) : {}),
      });
    } else if (r.phase === "response") {
      Object.assign(have, responseFields(r), { answered: true });
      if (r.allowed === false) have.allowed = false;
      if (r.denied_reason) have.denied_reason = r.denied_reason;
    }
  }
  return [...rows.values()].sort((a, b) => a.seq - b.seq);
}

function responseFields(r: RequestRecord) {
  return {
    status: r.status,
    bytes: r.bytes,
    duration_ms: r.duration_ms,
    ttfb_ms: r.ttfb_ms,
    cookies_sent: r.cookies_sent,
    cookies_stored: r.cookies_stored,
    error: r.error,
  };
}

export function splitUrl(url: string): { host: string; path: string; query: string } {
  try {
    const u = new URL(url);
    return {
      host: u.host,
      path: u.pathname,
      query: u.search.startsWith("?") ? u.search.slice(1) : u.search,
    };
  } catch {
    return { host: "", path: url, query: "" };
  }
}

// ── the language ─────────────────────────────────────────────────────────────

export type Op = "=" | "~" | ">" | ">=" | "<" | "<=";

export interface Term {
  neg: boolean;
  /** `null` is a bare word: substring of the URL. */
  key: string | null;
  op: Op;
  /** Alternatives, ORed. */
  values: string[];
  /** What was typed, for showing the term back. */
  raw: string;
}

/** The keys the language knows. Anything else is refused at parse time so a
 *  typo does not silently match nothing. */
export const KEYS = [
  "method",
  "status",
  "host",
  "path",
  "url",
  "query",
  "initiator",
  "verb",
  "action",
  "ms",
  "ttfb",
  "bytes",
  "cookies",
  "seq",
  "is",
] as const;

const NUMERIC = new Set(["status", "ms", "ttfb", "bytes", "cookies", "seq", "action"]);

/** Bare words that read better than their `is:` spelling. */
const ALIASES: Record<string, string> = {
  refused: "is:refused",
  denied: "is:refused",
  allowed: "is:allowed",
  error: "is:error",
  errors: "is:error",
  navigation: "initiator:navigation",
  replay: "initiator:replay",
};

export interface Parsed {
  terms: Term[];
  /** Terms that could not be read, with why. Shown, not swallowed. */
  errors: string[];
}

export function parse(input: string): Parsed {
  const terms: Term[] = [];
  const errors: string[] = [];
  for (const tok of tokens(input)) {
    let raw = tok;
    let neg = false;
    if (raw.startsWith("-") && raw.length > 1) {
      neg = true;
      raw = raw.slice(1);
    }
    const aliased = ALIASES[raw.toLowerCase()];
    if (aliased) raw = aliased;
    const colon = raw.indexOf(":");
    if (colon <= 0) {
      terms.push({ neg, key: null, op: "=", values: [unquote(raw)], raw: tok });
      continue;
    }
    const key = raw.slice(0, colon).toLowerCase();
    let rest = raw.slice(colon + 1);
    if (!(KEYS as readonly string[]).includes(key)) {
      errors.push(`\`${key}\` is not a field. Fields: ${KEYS.join(", ")}`);
      continue;
    }
    let op: Op = "=";
    for (const candidate of [">=", "<=", ">", "<", "~"] as const) {
      if (rest.startsWith(candidate)) {
        op = candidate;
        rest = rest.slice(candidate.length);
        break;
      }
    }
    if (op !== "=" && op !== "~" && !NUMERIC.has(key)) {
      errors.push(`\`${key}\` is text, so \`${op}\` does not apply to it`);
      continue;
    }
    if (op === "~" && NUMERIC.has(key)) {
      errors.push(`\`${key}\` is a number, so \`~\` does not apply to it`);
      continue;
    }
    if (rest === "") {
      errors.push(`\`${key}:\` needs a value`);
      continue;
    }
    const values = op === "~" ? [unquote(rest)] : unquote(rest).split("|").filter(Boolean);
    if (op === "~") {
      try {
        new RegExp(values[0], "i");
      } catch {
        errors.push(`\`${values[0]}\` is not a regular expression`);
        continue;
      }
    }
    terms.push({ neg, key, op, values, raw: tok });
  }
  return { terms, errors };
}

/** Split on spaces, keeping quoted strings whole. */
function tokens(input: string): string[] {
  const out: string[] = [];
  const re = /(?:[^\s"]+|"[^"]*")+/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(input)) !== null) out.push(m[0]);
  return out;
}

function unquote(s: string): string {
  return s.replace(/"([^"]*)"/g, "$1");
}

/** Whether one row satisfies every term. */
export function matches(row: HistoryRow, terms: Term[]): boolean {
  for (const t of terms) {
    const hit = one(row, t);
    if (t.neg ? hit : !hit) return false;
  }
  return true;
}

function one(row: HistoryRow, t: Term): boolean {
  if (t.key === null) {
    return t.values.some((v) => row.url.toLowerCase().includes(v.toLowerCase()));
  }
  switch (t.key) {
    case "method":
      return text(row.method, t);
    case "host":
      return text(row.host, t);
    case "path":
      return text(row.path, t);
    case "url":
      return text(row.url, t);
    case "query":
      return text(row.query, t);
    case "initiator":
      return text(row.initiator, t);
    case "verb":
      return text(row.verb ?? "", t);
    case "status":
      return status(row.status, t);
    case "ms":
      return number(row.duration_ms, t);
    case "ttfb":
      return number(row.ttfb_ms, t);
    case "bytes":
      return number(row.bytes, t);
    case "cookies":
      return number((row.cookies_sent ?? 0) + (row.cookies_stored ?? 0), t);
    case "seq":
      return number(row.seq, t);
    case "action":
      return number(row.action_seq, t);
    case "is":
      return t.values.some((v) => is(row, v.toLowerCase()));
  }
  return false;
}

function is(row: HistoryRow, what: string): boolean {
  switch (what) {
    case "refused":
    case "denied":
      return !row.allowed;
    case "allowed":
      return row.allowed;
    case "error":
      return row.error !== undefined || (row.status !== undefined && row.status >= 400);
    case "pending":
      return row.allowed && !row.answered;
    case "cookies":
      return (row.cookies_sent ?? 0) + (row.cookies_stored ?? 0) > 0;
    case "navigation":
      return row.initiator === "navigation";
    case "replay":
      return row.initiator === "replay";
    default:
      return false;
  }
}

function text(have: string, t: Term): boolean {
  if (t.op === "~") return new RegExp(t.values[0], "i").test(have);
  const h = have.toLowerCase();
  return t.values.some((v) => {
    const want = v.toLowerCase();
    // A method is a whole word; everywhere else a fragment is what people type.
    return t.key === "method" || t.key === "initiator" || t.key === "verb"
      ? h === want
      : h.includes(want);
  });
}

function number(have: number | undefined, t: Term): boolean {
  if (have === undefined) return false;
  return t.values.some((v) => {
    const n = Number(v);
    if (!Number.isFinite(n)) return false;
    switch (t.op) {
      case ">":
        return have > n;
      case ">=":
        return have >= n;
      case "<":
        return have < n;
      case "<=":
        return have <= n;
      default:
        return have === n;
    }
  });
}

/** `status:4xx` is the class; `status:>=400` is the number. */
function status(have: number | undefined, t: Term): boolean {
  if (have === undefined) return false;
  if (t.op === "=") {
    for (const v of t.values) {
      const m = /^([1-5])xx$/i.exec(v);
      if (m) {
        if (Math.floor(have / 100) === Number(m[1])) return true;
        continue;
      }
      if (Number(v) === have) return true;
    }
    return false;
  }
  return number(have, t);
}

/** Add a term to a query, replacing any existing term on the same key so a
 *  click on the sitemap does not pile up `host:` terms. */
export function withTerm(query: string, key: string, value: string): string {
  const kept = tokens(query).filter((tok) => {
    const bare = tok.startsWith("-") ? tok.slice(1) : tok;
    return !bare.toLowerCase().startsWith(`${key}:`);
  });
  const quoted = /\s/.test(value) ? `"${value}"` : value;
  return [...kept, `${key}:${quoted}`].join(" ");
}
