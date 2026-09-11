import { useEffect, useMemo, useRef, useState } from "react";

import type { SessionDetail } from "./api";
import { KEYS, fold, matches, parse, withTerm, type HistoryRow } from "./filter";
import {
  Chip,
  Cmd,
  Empty,
  Method,
  Note,
  Split,
  Status,
  clockMs,
  fmtBytes,
  fmtMs,
  plural,
} from "./ui";

// The HTTP history: every fetch this session made, with the one column no
// proxy can show. A proxy sees a GET; this table says which agent verb
// caused it, because the engine wrote the decision before the bytes moved.

const FILTER_KEY = "h5i.console.filter";

const PRESETS: { label: string; q: string; title: string }[] = [
  { label: "refused", q: "refused", title: "fetches policy stopped before the wire" },
  { label: "errors", q: "errors", title: "4xx, 5xx, or failed on the wire" },
  { label: "navigations", q: "navigation", title: "page loads, not what pages pulled in" },
  { label: "replays", q: "replay", title: "resent by the workbench, not by a page" },
  { label: "slow", q: "ms:>=1000", title: "a second or more" },
  { label: "with cookies", q: "is:cookies", title: "sent or stored a cookie" },
];

type SortKey = "seq" | "method" | "host" | "path" | "status" | "bytes" | "duration_ms" | "ttfb_ms" | "initiator" | "verb";

export function History({ detail }: { detail: SessionDetail }) {
  const name = detail.name ?? detail.id;
  const [query, setQuery] = useState<string>(() => sessionFilter(detail.id));
  const [sort, setSort] = useState<{ key: SortKey; desc: boolean }>({ key: "seq", desc: true });
  const [selected, setSelected] = useState<number | null>(null);
  const [help, setHelp] = useState(false);

  useEffect(() => {
    try {
      window.sessionStorage.setItem(`${FILTER_KEY}.${detail.id}`, query);
    } catch {
      // Only the memory of it is lost.
    }
  }, [query, detail.id]);

  const rows = useMemo(() => fold(detail.requests_log, detail.actions), [detail]);
  const parsed = useMemo(() => parse(query), [query]);
  const shown = useMemo(() => {
    const out = rows.filter((r) => matches(r, parsed.terms));
    const dir = sort.desc ? -1 : 1;
    out.sort((a, b) => dir * compare(a, b, sort.key));
    return out;
  }, [rows, parsed, sort]);

  const selectedRow = selected === null ? null : rows.find((r) => r.seq === selected) ?? null;
  const refused = shown.filter((r) => !r.allowed).length;
  const omitted = detail.requests_total - rows.length;

  // Keyboard: the arrows walk the visible rows, Escape closes the inspector.
  const tableRef = useRef<HTMLDivElement>(null);
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp" && e.key !== "Escape") return;
    if (e.key === "Escape") {
      setSelected(null);
      return;
    }
    e.preventDefault();
    const i = shown.findIndex((r) => r.seq === selected);
    const next = e.key === "ArrowDown" ? Math.min(shown.length - 1, i + 1) : Math.max(0, i - 1);
    if (shown[next]) setSelected(shown[next].seq);
  };

  const table = (
    <div className="hist" style={{ display: "flex", flexDirection: "column", minHeight: 0, flex: 1 }}>
      <div className="hist-bar">
        <div className="filter">
          <label className="search">
            <input
              type="text"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder='filter: host:api status:4xx -path:/static verb:click "free text"'
              spellCheck={false}
              aria-label="filter the history"
            />
            {query ? (
              <button type="button" className="search-n" onClick={() => setQuery("")} title="clear">
                clear
              </button>
            ) : null}
          </label>
          <div className="help">
            <Chip tone="dim" on={help} onClick={() => setHelp(!help)} title="the filter language">
              ?
            </Chip>
            {help ? <FilterHelp onClose={() => setHelp(false)} /> : null}
          </div>
        </div>
        {parsed.errors.length > 0 ? (
          <div className="filter-errors">{parsed.errors.join(". ")}</div>
        ) : null}
        <div className="presets">
          <span className="presets-label">show</span>
          {PRESETS.map((p) => (
            <Chip key={p.q} tone="dim" on={hasTerm(query, p.q)} onClick={() => setQuery(toggleTerm(query, p.q))} title={p.title}>
              {p.label}
            </Chip>
          ))}
          {detail.sitemap.length > 1 ? (
            <>
              <span className="presets-label" style={{ marginLeft: 8 }}>
                origin
              </span>
              {detail.sitemap.slice(0, 6).map((o) => {
                const host = o.origin.replace(/^https?:\/\//, "");
                return (
                  <Chip
                    key={o.origin}
                    tone="dim"
                    mono
                    on={hasTerm(query, `host:${host}`)}
                    onClick={() => setQuery(hasTerm(query, `host:${host}`) ? dropTerm(query, `host:${host}`) : withTerm(query, "host", host))}
                    title={`${o.hits} allowed, ${o.refused} refused`}
                  >
                    {host}
                  </Chip>
                );
              })}
            </>
          ) : null}
        </div>
      </div>

      {rows.length === 0 ? (
        <Empty title="This session has fetched nothing">
          <p>The request log is written before the wire, so an empty log means no bytes left.</p>
        </Empty>
      ) : (
        <div className="table-wrap" ref={tableRef} tabIndex={0} onKeyDown={onKey}>
          <table className="grid">
            <thead>
              <tr>
                <Th k="seq" sort={sort} setSort={setSort} className="num">#</Th>
                <Th k="verb" sort={sort} setSort={setSort} title="the agent verb that spent this fetch">verb</Th>
                <Th k="method" sort={sort} setSort={setSort}>method</Th>
                <Th k="host" sort={sort} setSort={setSort}>host</Th>
                <Th k="path" sort={sort} setSort={setSort}>path</Th>
                <Th k="status" sort={sort} setSort={setSort} className="num">status</Th>
                <Th k="bytes" sort={sort} setSort={setSort} className="num">size</Th>
                <Th k="duration_ms" sort={sort} setSort={setSort} className="num">time</Th>
                <Th k="initiator" sort={sort} setSort={setSort}>initiator</Th>
              </tr>
            </thead>
            <tbody>
              {shown.map((r) => (
                <tr
                  key={r.seq}
                  className={`${r.seq === selected ? "is-on" : ""}${!r.allowed ? " is-refused" : ""}`}
                  onClick={() => setSelected(r.seq === selected ? null : r.seq)}
                >
                  <td className="num dim">{r.seq}</td>
                  <td className="cell-verb">{r.verb ?? ""}</td>
                  <td>
                    <Method m={r.method} />
                  </td>
                  <td className="cell-host" title={r.host}>
                    {r.host}
                  </td>
                  <td className="cell-path" title={r.url}>
                    <Path path={r.path} query={r.query} />
                  </td>
                  <td className="num">
                    <Status status={r.status} allowed={r.allowed} error={r.error} reason={r.denied_reason} pending={!r.answered} />
                  </td>
                  <td className="num dim">{r.bytes !== undefined ? fmtBytes(r.bytes) : ""}</td>
                  <td className="num dim">{r.duration_ms !== undefined ? fmtMs(r.duration_ms) : ""}</td>
                  <td className="dim">{r.initiator}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <div className="table-foot">
        <span>
          {shown.length.toLocaleString()} of {rows.length.toLocaleString()} {plural(rows.length, "fetch", "fetches")}
          {parsed.terms.length > 0 ? " match" : ""}
        </span>
        {refused > 0 ? <span style={{ color: "var(--accent-hi)" }}>{refused} refused</span> : null}
        {omitted > 0 ? (
          <span>
            the oldest {omitted.toLocaleString()} are not shown; <code>h5i websec requests --session {name}</code> has them all
          </span>
        ) : null}
        <span style={{ marginLeft: "auto" }}>arrows move, Esc closes</span>
      </div>
    </div>
  );

  if (!selectedRow) return table;
  return (
    <Split
      id="history"
      initial={Math.max(360, Math.round(window.innerWidth * 0.42))}
      min={280}
      max={1400}
      first={table}
      second={<Inspector row={selectedRow} detail={detail} onClose={() => setSelected(null)} />}
    />
  );
}

function Th({
  k,
  sort,
  setSort,
  children,
  className,
  title,
}: {
  k: SortKey;
  sort: { key: SortKey; desc: boolean };
  setSort: (s: { key: SortKey; desc: boolean }) => void;
  children: React.ReactNode;
  className?: string;
  title?: string;
}) {
  const on = sort.key === k;
  return (
    <th
      className={`sortable${on ? " is-sorted" : ""}${className ? ` ${className}` : ""}`}
      title={title}
      onClick={() => setSort({ key: k, desc: on ? !sort.desc : k === "seq" })}
      aria-sort={on ? (sort.desc ? "descending" : "ascending") : undefined}
    >
      {children}
      {on ? (sort.desc ? " ↓" : " ↑") : ""}
    </th>
  );
}

/** The path with its last segment lit and the query dimmed: what a reader
 *  scans a proxy table for is the endpoint, not the directory. */
function Path({ path, query }: { path: string; query: string }) {
  const i = path.lastIndexOf("/");
  const dir = i >= 0 ? path.slice(0, i + 1) : "";
  const last = i >= 0 ? path.slice(i + 1) : path;
  return (
    <>
      {/* The root itself is lit, not dimmed: "/" is the endpoint there. */}
      {path === "/" ? <span className="path-last">/</span> : <span className="path-dir">{dir}</span>}
      <span className="path-last">{last}</span>
      {query ? (
        <span className="path-q">
          ?
          {query.split("&").map((kv, n) => {
            const eq = kv.indexOf("=");
            const k = eq >= 0 ? kv.slice(0, eq) : kv;
            const v = eq >= 0 ? kv.slice(eq + 1) : undefined;
            return (
              <span key={n}>
                {n > 0 ? "&" : ""}
                <b>{k}</b>
                {v !== undefined ? `=${v}` : ""}
              </span>
            );
          })}
        </span>
      ) : null}
    </>
  );
}

function compare(a: HistoryRow, b: HistoryRow, key: SortKey): number {
  const x = a[key];
  const y = b[key];
  if (x === undefined && y === undefined) return a.seq - b.seq;
  if (x === undefined) return -1;
  if (y === undefined) return 1;
  if (typeof x === "number" && typeof y === "number") return x - y || a.seq - b.seq;
  return String(x).localeCompare(String(y)) || a.seq - b.seq;
}

function sessionFilter(id: string): string {
  try {
    return window.sessionStorage.getItem(`${FILTER_KEY}.${id}`) ?? "";
  } catch {
    return "";
  }
}

function hasTerm(query: string, term: string): boolean {
  return query.split(/\s+/).includes(term);
}
function dropTerm(query: string, term: string): string {
  return query
    .split(/\s+/)
    .filter((t) => t !== term && t !== "")
    .join(" ");
}
function toggleTerm(query: string, term: string): string {
  return hasTerm(query, term) ? dropTerm(query, term) : `${query} ${term}`.trim();
}

function FilterHelp({ onClose }: { onClose: () => void }) {
  return (
    <div className="help-pop" role="dialog" aria-label="filter language">
      <p>
        Terms are ANDed. <code>a|b</code> is OR inside a value, a leading <code>-</code> negates, and a bare word
        matches anywhere in the URL. Numbers take <code>&gt;</code> <code>&gt;=</code> <code>&lt;</code> <code>&lt;=</code>;
        text takes <code>~</code> for a regular expression.
      </p>
      <dl>
        <dt>
          <code>method:GET|POST</code>
        </dt>
        <dd>whole word</dd>
        <dt>
          <code>host:api</code> <code>path:/v1/</code> <code>query:id=</code>
        </dt>
        <dd>fragments</dd>
        <dt>
          <code>status:4xx</code> <code>status:&gt;=500</code>
        </dt>
        <dd>class or number</dd>
        <dt>
          <code>verb:click</code> <code>action:12</code>
        </dt>
        <dd>the agent verb that spent it</dd>
        <dt>
          <code>ms:&gt;1000</code> <code>ttfb:&gt;200</code> <code>bytes:&lt;100</code>
        </dt>
        <dd>timing and size</dd>
        <dt>
          <code>refused</code> <code>errors</code> <code>navigation</code> <code>replay</code> <code>is:pending</code>
        </dt>
        <dd>shorthands</dd>
        <dt>
          <code>path:~"\.php$"</code>
        </dt>
        <dd>regular expression</dd>
      </dl>
      <p style={{ marginTop: 8, color: "var(--ink-3)" }}>
        fields: {KEYS.join(", ")}.{" "}
        <button type="button" onClick={onClose} style={{ color: "var(--accent-hi)" }}>
          close
        </button>
      </p>
    </div>
  );
}

// ── the inspector ────────────────────────────────────────────────────────────

/**
 * Everything the log knows about one fetch, and the command that reads the
 * bytes. The store holds bodies, cookies and `Authorization` in full, so the
 * console never renders it (design C2); what it renders instead is the chain
 * of decisions that produced the fetch, which is the part a proxy cannot show.
 */
function Inspector({ row, detail, onClose }: { row: HistoryRow; detail: SessionDetail; onClose: () => void }) {
  const name = detail.name ?? detail.id;
  const action = row.action_seq !== undefined ? detail.actions.find((a) => a.seq === row.action_seq) : undefined;
  const kept = detail.captured !== null;
  const total = row.duration_ms ?? 0;
  const ttfb = row.ttfb_ms ?? 0;

  return (
    <div className="inspector">
      <div className="insp-head">
        <div className="insp-line">
          <code>req_{row.seq}</code>
          <Method m={row.method} />
          <Status status={row.status} allowed={row.allowed} error={row.error} reason={row.denied_reason} pending={!row.answered} />
          {row.bytes !== undefined ? <span className="count">{fmtBytes(row.bytes)}</span> : null}
          {row.duration_ms !== undefined ? <span className="count">{fmtMs(row.duration_ms)}</span> : null}
          <button type="button" className="insp-close" onClick={onClose}>
            close
          </button>
        </div>
        <div className="insp-url">{row.url}</div>
      </div>
      <div className="insp-body">
        {!row.allowed ? (
          <Note tone="bad">
            Refused before the wire: {row.denied_reason ?? "policy declined it"}. Nothing was sent, so there is no
            response and no stored message.
          </Note>
        ) : null}
        {row.error !== undefined ? <Note tone="warn">Failed on the wire: {row.error}</Note> : null}

        <div>
          <div className="section-head">
            <h3>Why this fetch exists</h3>
          </div>
          <div className="trace">
            {action ? (
              <div className="trace-step is-info">
                <span className="trace-mark">
                  <i />
                </span>
                <span>
                  the agent asked for <b>{action.verb}</b>
                  {action.target ? <code> {action.target}</code> : null}
                  {action.url ? <code> {action.url}</code> : null}{" "}
                  <span className="trace-when">{clockMs(action.at)}</span>
                  <br />
                  <span className="count">
                    action #{action.seq}, which spent {action.requests.length} {plural(action.requests.length, "fetch", "fetches")}
                  </span>
                </span>
              </div>
            ) : (
              <div className="trace-step">
                <span className="trace-mark">
                  <i />
                </span>
                <span>
                  no verb claims this fetch{" "}
                  <span className="count">the page, a redirect, or a run outside the action log made it</span>
                </span>
              </div>
            )}
            <div className="trace-step">
              <span className="trace-mark">
                <i />
              </span>
              <span>
                the engine decided a <b>{row.initiator}</b> fetch of <code>{row.host}</code>{" "}
                <span className="trace-when">{clockMs(row.at)}</span>
              </span>
            </div>
            <div className={`trace-step ${row.allowed ? "is-good" : "is-bad"}`}>
              <span className="trace-mark">
                <i />
              </span>
              <span>
                {row.allowed ? (
                  <>
                    policy <b>allowed</b> it, and the receipt was written before the bytes moved
                  </>
                ) : (
                  <>
                    policy <b>refused</b> it: {row.denied_reason ?? "no reason recorded"}
                  </>
                )}
              </span>
            </div>
            {row.allowed ? (
              <div className={`trace-step ${row.error ? "is-bad" : row.answered ? "is-good" : ""}`}>
                <span className="trace-mark">
                  <i />
                </span>
                <span>
                  {row.error ? (
                    <>the wire failed: {row.error}</>
                  ) : row.answered ? (
                    <>
                      the server answered <b>{row.status}</b>
                      {row.bytes !== undefined ? <> with {fmtBytes(row.bytes)}</> : null}
                      {row.duration_ms !== undefined ? <> in {fmtMs(row.duration_ms)}</> : null}
                    </>
                  ) : (
                    <>no response yet</>
                  )}
                </span>
              </div>
            ) : null}
          </div>
        </div>

        {row.allowed && row.answered && row.duration_ms !== undefined ? (
          <div>
            <div className="section-head">
              <h3>Timing</h3>
            </div>
            <div className="timing">
              <div className="timing-bar" title={`${ttfb}ms to first byte, ${total}ms total`}>
                <span className="timing-ttfb" style={{ width: `${total ? Math.min(100, (ttfb / total) * 100) : 0}%` }} />
                <span
                  className="timing-body"
                  style={{
                    left: `${total ? Math.min(100, (ttfb / total) * 100) : 0}%`,
                    width: `${total ? Math.max(0, 100 - (ttfb / total) * 100) : 0}%`,
                  }}
                />
              </div>
              <div className="timing-legend">
                <span>
                  <i style={{ background: "var(--cyan)" }} />
                  first byte {ttfb}ms
                </span>
                <span>
                  <i style={{ background: "var(--green)" }} />
                  body {Math.max(0, total - ttfb)}ms
                </span>
                <span>
                  {row.cookies_sent ?? 0} {plural(row.cookies_sent ?? 0, "cookie")} sent, {row.cookies_stored ?? 0} stored
                </span>
              </div>
            </div>
          </div>
        ) : null}

        <div>
          <div className="section-head">
            <h3>Read the bytes</h3>
          </div>
          {kept && row.allowed ? (
            <div style={{ display: "flex", flexDirection: "column", gap: 6, alignItems: "flex-start" }}>
              <Cmd text={`h5i websec show req_${row.seq} --session ${name}`} hint="the request as it went out" />
              {row.answered ? <Cmd text={`h5i websec show res_${row.seq} --session ${name}`} hint="the response as it came back" /> : null}
              <Cmd text={`h5i websec replay req_${row.seq} --session ${name}`} hint="send it again, with edits named on the command line" />
              <p className="count" style={{ marginTop: 4 }}>
                Headers, cookies and bodies stay on disk, owner-only. The console shows the account of a fetch
                and teaches the command that reads it.
              </p>
            </div>
          ) : (
            <p className="count">
              {!row.allowed
                ? "Nothing was sent."
                : detail.reclaimed
                  ? `The store was reclaimed on ${detail.reclaimed.at.slice(0, 10)}; the decision record above is what remains.`
                  : "Opened without --capture, so the decision record above is all there is."}
            </p>
          )}
        </div>

        <div>
          <div className="section-head">
            <h3>Record</h3>
          </div>
          <dl className="facts">
            {(
              [
                ["seq", String(row.seq)],
                ["at", row.at],
                ["initiator", row.initiator],
                ["allowed", row.allowed ? "yes" : "no"],
                ["status", row.status !== undefined ? String(row.status) : "none"],
                ["bytes", row.bytes !== undefined ? String(row.bytes) : "none"],
                ["duration", row.duration_ms !== undefined ? `${row.duration_ms}ms` : "none"],
                ["first byte", row.ttfb_ms !== undefined ? `${row.ttfb_ms}ms` : "none"],
              ] as [string, string][]
            ).map(([k, v]) => (
              <div key={k} className="facts-row">
                <dt>{k}</dt>
                <dd>{v}</dd>
              </div>
            ))}
          </dl>
        </div>
      </div>
    </div>
  );
}
