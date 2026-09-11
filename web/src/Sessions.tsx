import { useEffect, useMemo, useRef, useState } from "react";

import type { Fleet } from "./App";
import { api, type SessionDetail, type SessionRow, type SessionStub } from "./api";
import { Actions } from "./Actions";
import { Findings } from "./Findings";
import { History } from "./History";
import { Recon } from "./Recon";
import { Sitemap } from "./Sitemap";
import { shownState } from "./seen";
import {
  ATTENTION_LABEL,
  ATTENTION_ORDER,
  AttentionTag,
  Chip,
  Cmd,
  Count,
  Empty,
  Facts,
  Note,
  Spin,
  Split,
  ago,
  day,
  hostOf,
  plural,
  span,
} from "./ui";

// Browser sessions: the column lists them, loudest first; the workspace is
// one session, read the way a proxy reader reads a target.

export type Tab = "history" | "sitemap" | "actions" | "findings" | "recon" | "about";

const TABS: { key: Tab; label: string }[] = [
  { key: "history", label: "History" },
  { key: "sitemap", label: "Sitemap" },
  { key: "actions", label: "Actions" },
  { key: "findings", label: "Findings" },
  { key: "recon", label: "Recon" },
  { key: "about", label: "About" },
];

export function SessionsPage({
  fleet,
  route,
  go,
}: {
  fleet: Fleet;
  route: string[];
  go: (parts: string[], replace?: boolean) => void;
}) {
  const id = route[0] ?? null;
  const tab: Tab = (TABS.find((t) => t.key === route[1])?.key ?? "history") as Tab;
  const sessions = fleet.sessions?.sessions ?? null;
  const row = sessions?.find((s) => s.id === id) ?? null;

  return (
    <>
      <div className="topbar">
        <span className="topbar-title">Sessions</span>
        <span className="topbar-scope">
          this machine's browser sessions: what each one fetched, reached, was refused, and concluded
        </span>
        <div className="topbar-right">
          {fleet.sessions ? (
            <>
              <span className="vital">
                <b>{fleet.sessions.live}</b> live
              </span>
              <span className="vital">
                <b>{fleet.sessions.total}</b> recorded
              </span>
            </>
          ) : null}
          {fleet.error ? <span className="topbar-error">{fleet.error}</span> : null}
        </div>
      </div>
      <div className="page">
        <Split
          id="sessions"
          initial={340}
          first={
            <SessionList
              fleet={fleet}
              selectedId={id}
              onSelect={(s) => {
                if (s) fleet.look(s);
                go(["sessions", s?.id ?? "", tab].filter(Boolean));
              }}
              onSelectStub={(stub) => go(["sessions", stub.id, tab])}
            />
          }
          second={
            id ? (
              <Workspace
                id={id}
                row={row}
                seen={fleet.seen}
                tab={tab}
                tick={fleet.tick}
                onTab={(t) => go(["sessions", id, t], true)}
              />
            ) : (
              <Empty title="Pick a session">
                <p>
                  The column is sorted loudest first: a session waiting on a person, then finished
                  ones nobody has read, then the live ones.
                </p>
              </Empty>
            )
          }
        />
      </div>
    </>
  );
}

// ── the column ───────────────────────────────────────────────────────────────

type Sort = "attention" | "newest" | "name" | "target";

function SessionList({
  fleet,
  selectedId,
  onSelect,
  onSelectStub,
}: {
  fleet: Fleet;
  selectedId: string | null;
  onSelect: (s: SessionRow | null) => void;
  onSelectStub: (s: SessionStub) => void;
}) {
  const [q, setQ] = useState("");
  const [state, setState] = useState<string>("all");
  const [sort, setSort] = useState<Sort>("attention");
  const rows = fleet.sessions?.sessions ?? null;
  const index = fleet.sessions?.index ?? [];

  const counts = useMemo(() => {
    const out: Record<string, number> = {};
    for (const s of rows ?? []) {
      const st = shownState(s, fleet.seen);
      out[st] = (out[st] ?? 0) + 1;
    }
    return out;
  }, [rows, fleet.seen]);

  const needle = q.trim().toLowerCase();
  const hit = (name: string | null, id: string, url: string) =>
    needle === "" ||
    (name ?? "").toLowerCase().includes(needle) ||
    id.toLowerCase().includes(needle) ||
    url.toLowerCase().includes(needle);

  const shown = useMemo(() => {
    const list = (rows ?? []).filter(
      (s) => (state === "all" || shownState(s, fleet.seen) === state) && hit(s.name, s.id, s.url),
    );
    const rank = (s: SessionRow) => ATTENTION_ORDER.indexOf(shownState(s, fleet.seen));
    switch (sort) {
      case "newest":
        return list.sort((a, b) => b.started_at.localeCompare(a.started_at));
      case "name":
        return list.sort((a, b) => (a.name ?? a.id).localeCompare(b.name ?? b.id));
      case "target":
        return list.sort((a, b) => hostOf(a.url).localeCompare(hostOf(b.url)) || b.started_at.localeCompare(a.started_at));
      default:
        return list.sort((a, b) => rank(a) - rank(b) || b.started_at.localeCompare(a.started_at));
    }
  }, [rows, state, sort, needle, fleet.seen]);

  // The registry past what the poll folded: found by name or target only when
  // the reader is searching, so the column is not 1,900 rows long by default.
  const folded = useMemo(() => new Set((rows ?? []).map((s) => s.id)), [rows]);
  const stubs = useMemo(
    () =>
      needle === "" || state !== "all"
        ? []
        : index.filter((s) => !folded.has(s.id) && hit(s.name, s.id, s.url)).slice(0, 200),
    [index, folded, needle, state],
  );

  const unread = (fleet.sessions?.total ?? 0) - (rows?.length ?? 0);

  return (
    <div className="column">
      <div className="column-tools">
        <label className="search">
          <input
            type="search"
            placeholder="find by name, id or target"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            spellCheck={false}
          />
          <span className="search-n">
            {needle ? `${shown.length + stubs.length}` : `${rows?.length ?? 0}`}
          </span>
        </label>
        <div className="chips">
          {ATTENTION_ORDER.map((st) =>
            counts[st] ? (
              <Chip
                key={st}
                on={state === st}
                tone={state === st ? "plain" : st === "blocked" ? "accent" : st === "done" ? "warn" : "plain"}
                onClick={() => setState(state === st ? "all" : st)}
                title={`only sessions that are ${ATTENTION_LABEL[st]}`}
              >
                <b>{counts[st]}</b> {ATTENTION_LABEL[st]}
              </Chip>
            ) : null,
          )}
        </div>
        <div className="chips" role="group" aria-label="order">
          {(["attention", "newest", "name", "target"] as Sort[]).map((s) => (
            <Chip key={s} tone="dim" on={sort === s} onClick={() => setSort(s)}>
              {s}
            </Chip>
          ))}
        </div>
      </div>
      {unread > 0 && needle === "" ? (
        <div className="column-note">
          The newest {rows?.length ?? 0} of {fleet.sessions?.total ?? 0} recorded sessions are read.
          Search finds the other {unread} by name or target.
        </div>
      ) : null}
      <div className="column-body">
        {rows === null ? (
          <Spin label="reading the registry" />
        ) : shown.length === 0 && stubs.length === 0 ? (
          <Empty title={rows.length === 0 ? "No browser sessions on this machine" : "No session matches"}>
            {rows.length === 0 ? <Cmd text="h5i browser open <url> --capture" hint="starts one, keeping every message" /> : null}
          </Empty>
        ) : (
          <>
            {shown.map((s) => (
              <SessionRowView
                key={s.id}
                s={s}
                state={shownState(s, fleet.seen)}
                on={s.id === selectedId}
                onClick={() => onSelect(s)}
              />
            ))}
            {stubs.length > 0 ? (
              <>
                <div className="group-head">not read by this poll: open one to fold it</div>
                {stubs.map((s) => (
                  <button
                    key={s.id}
                    type="button"
                    className={`srow is-stub${s.id === selectedId ? " is-on" : ""}`}
                    onClick={() => onSelectStub(s)}
                  >
                    <div className="srow-top">
                      <span className="srow-name">
                        {s.name ?? s.id}
                        {s.name ? <code>{s.id}</code> : null}
                      </span>
                      <span className="srow-age">{day(s.started_at)}</span>
                    </div>
                    <div className="srow-target">{s.url}</div>
                    <div className="srow-bottom">
                      <span className="count">{s.state}</span>
                    </div>
                  </button>
                ))}
              </>
            ) : null}
          </>
        )}
      </div>
    </div>
  );
}

function SessionRowView({
  s,
  state,
  on,
  onClick,
}: {
  s: SessionRow;
  state: string;
  on: boolean;
  onClick: () => void;
}) {
  return (
    <button type="button" className={`srow${on ? " is-on" : ""}`} onClick={onClick}>
      <div className="srow-top">
        <span className="srow-name">
          {s.name ?? s.id}
          {s.name ? <code>{s.id}</code> : null}
        </span>
        <span className="srow-age" title={s.last_request_at ?? s.started_at}>
          {ago(s.last_request_at ?? s.started_at)}
        </span>
      </div>
      <div className="srow-target" title={s.url}>
        {s.origins[0] ?? s.url}
        {s.origins.length > 1 ? ` +${s.origins.length - 1}` : ""}
      </div>
      <div className="srow-bottom">
        <div className="srow-counts">
          <Count n={s.requests} label="fetches" />
          {s.denied > 0 ? <Count n={s.denied} label="refused" tone="bad" /> : null}
          {s.captured !== null ? <Count n={s.captured} label="kept" /> : null}
          {s.ledger && s.ledger.confirmed > 0 ? <Count n={s.ledger.confirmed} label="confirmed" tone="good" /> : null}
          {s.findings > 0 ? <Count n={s.findings} label={plural(s.findings, "finding")} tone="warn" /> : null}
        </div>
        <AttentionTag state={state} why={s.attention.why} />
      </div>
    </button>
  );
}

// ── the workspace ────────────────────────────────────────────────────────────

function Workspace({
  id,
  row,
  seen,
  tab,
  tick,
  onTab,
}: {
  id: string;
  row: SessionRow | null;
  seen: Record<string, string>;
  tab: Tab;
  tick: number;
  onTab: (t: Tab) => void;
}) {
  const [detail, setDetail] = useState<SessionDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The detail is re-read only when something the row is made of moved. An
  // ended session is read once; a live one refetches exactly when the fleet
  // poll says its log grew.
  const stamp = row
    ? `${row.requests}|${row.last_request_at}|${row.ended_at}|${row.findings}|${row.verbs}|${row.jobs.length}|${row.ledger?.cursor}`
    : `unfolded|${tick}`;
  const last = useRef<string>("");

  useEffect(() => {
    setDetail(null);
    setError(null);
    last.current = "";
  }, [id]);

  useEffect(() => {
    if (last.current === `${id}|${stamp}`) return;
    last.current = `${id}|${stamp}`;
    let alive = true;
    api
      .session(id)
      .then((d) => alive && setDetail(d))
      .catch((e: Error) => alive && setError(e.message));
    return () => {
      alive = false;
    };
  }, [id, stamp]);

  if (error) {
    return (
      <Empty title="Could not read this session">
        <p>{error}</p>
      </Empty>
    );
  }
  if (!detail) return <Spin label="reading the session" />;

  const name = detail.name ?? detail.id;
  const state = row ? shownState(row, seen) : detail.attention.state;
  const counts: Record<Tab, number | null> = {
    history: detail.requests_total,
    sitemap: detail.sitemap.length,
    actions: detail.actions.length,
    findings: detail.findings_list.length,
    recon: detail.endpoints.length + detail.jobs.length,
    about: null,
  };

  return (
    <div className="work">
      <div className="work-head">
        <div className="work-title">
          <h2>{name}</h2>
          {detail.name ? <code>{detail.id}</code> : null}
          <AttentionTag state={state} why={detail.attention.why} />
          {detail.held_by_human ? <Chip tone="accent">a human holds the wheel</Chip> : null}
          {detail.denied > 0 ? (
            <Chip tone="bad" title="fetches policy refused before the wire">
              {detail.denied} refused
            </Chip>
          ) : null}
          {detail.permissive_cors ? <Chip tone="warn" title="pages here may send credentials cross-origin">permissive CORS</Chip> : null}
        </div>
        <div className="work-sub">
          <span className="mono" title={detail.url}>
            {detail.url}
          </span>
          <span>{detail.engine}</span>
          <span>{detail.lane}</span>
          <span>{detail.identity}</span>
          <span>
            {detail.state === "live" ? `live for ${span(detail.started_at, null)}` : `${detail.state}, ran ${span(detail.started_at, detail.ended_at)}`}
          </span>
        </div>
        <div className="tabs" role="tablist">
          {TABS.map((t) => (
            <button
              key={t.key}
              type="button"
              role="tab"
              aria-selected={tab === t.key}
              className={`tab${tab === t.key ? " is-on" : ""}`}
              onClick={() => onTab(t.key)}
            >
              {t.label}
              {counts[t.key] !== null ? (
                <span className={`tab-n${t.key === "findings" && counts[t.key] ? " is-loud" : ""}`}>
                  {counts[t.key]}
                </span>
              ) : null}
            </button>
          ))}
        </div>
      </div>
      <div className="work-body">
        {tab === "history" ? (
          <History detail={detail} />
        ) : tab === "sitemap" ? (
          <Sitemap detail={detail} />
        ) : tab === "actions" ? (
          <Actions detail={detail} />
        ) : tab === "findings" ? (
          <Findings detail={detail} />
        ) : tab === "recon" ? (
          <Recon detail={detail} />
        ) : (
          <About detail={detail} />
        )}
      </div>
    </div>
  );
}

function About({ detail }: { detail: SessionDetail }) {
  const name = detail.name ?? detail.id;
  const capture =
    detail.captured !== null
      ? `on: ${detail.captured} ${plural(detail.captured, "message")} stored, owner-only`
      : detail.reclaimed
        ? `reclaimed on ${day(detail.reclaimed.at)}: ${detail.reclaimed.messages} messages were kept and then removed`
        : "off: the log records decisions, not bytes";
  return (
    <div className="scroll pad">
      <div className="detail-notes prose">
        <Note tone={detail.lane === "host-observed" ? "good" : "plain"}>
          {detail.lane === "host-observed"
            ? "Host-observed: h5i watched this session from outside the box, and the box could not edit the record."
            : "Engine-claimed: the request log is the engine's own account of what it fetched. It is fail-closed, so a fetch that is not in it did not happen, but nothing outside the engine vouches for it."}
        </Note>
        {detail.end_reason ? <Note>{detail.end_reason}</Note> : null}
      </div>
      <div style={{ marginTop: 18 }}>
        <Facts
          rows={[
            ["state", detail.state],
            ["placement", detail.placement],
            ["confinement", detail.confinement],
            ["engine", detail.engine],
            ["identity", detail.identity],
            ["policy", <code key="p">{detail.policy_digest}</code>],
            ["capture", capture],
            ["cross-origin credentials", detail.permissive_cors ? "allowed (--permissive-cors)" : "refused"],
            ["started", detail.started_at],
            ["ended", detail.ended_at ?? "still running"],
            ["expires", detail.expires_at ?? "never"],
            ["restored from", detail.restored_from ?? "nothing"],
            ["enclosing box", detail.enclosing_box ?? "none"],
            ["control", detail.held_by_human ? "a human holds the lock" : "the agent"],
          ]}
        />
      </div>
      <div style={{ marginTop: 22, display: "flex", flexDirection: "column", gap: 8, alignItems: "flex-start" }}>
        <Cmd text={`h5i browser status --session ${name}`} hint="the record, from the terminal" />
        <Cmd text={`h5i browser audit --session ${name}`} hint="the whole timeline: verbs, fetches, handovers, ending" />
        {detail.state === "live" ? (
          <Cmd text={`h5i browser take --session ${name}`} hint="take the control lock and drive it yourself" />
        ) : null}
      </div>
    </div>
  );
}
