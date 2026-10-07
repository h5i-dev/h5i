import { useEffect, useMemo, useRef, useState } from "react";

import type { Fleet } from "./App";
import { api, type BoxRow, type SessionRow } from "./api";
import { shownState } from "./seen";
import { ATTENTION_LABEL, AttentionTag, Cmd, Count, ago, hostOf, plural } from "./ui";

/**
 * The landing board. A person running one agent reads its terminal; a person
 * running twelve needs to be told which one wants them, so that is the hero,
 * and everything else on the board is the answer to "and what else is going
 * on".
 */
export function Overview({ fleet, go }: { fleet: Fleet; go: (parts: string[]) => void }) {
  const sessions = fleet.sessions?.sessions ?? null;
  const boxes = fleet.boxes;

  const byState = useMemo(() => {
    const out: Record<string, SessionRow[]> = {};
    for (const s of sessions ?? []) {
      const st = shownState(s, fleet.seen);
      (out[st] ??= []).push(s);
    }
    return out;
  }, [sessions, fleet.seen]);

  const waiting = [...(byState.blocked ?? []), ...(byState.done ?? [])];
  const live = [...(byState.working ?? []), ...(byState.idle ?? []), ...(byState.unknown ?? [])];
  const pressing = (boxes ?? []).filter((b) => b.signals.verdict !== "clean");
  const refused = (sessions ?? []).reduce((n, s) => n + s.denied, 0);
  const findings = useFindings(fleet);
  const projectOpen = (fleet.projects ?? []).reduce((n, p) => n + p.open_findings, 0);
  const sessionFindings = (sessions ?? []).reduce((n, s) => n + s.findings, 0);
  const withFindings = projectOpen + sessionFindings;

  const open = (s: SessionRow) => {
    fleet.look(s);
    go(["sessions", s.id]);
  };

  return (
    <>
      <div className="topbar">
        <span className="topbar-title">Overview</span>
        <span className="topbar-scope">
          sessions are this machine's; boxes are this repository's
        </span>
        <div className="topbar-right">
          {fleet.error ? <span className="topbar-error">{fleet.error}</span> : null}
        </div>
      </div>
      <div className="page scroll">
        <div className="board">
          <div className="hero">
            <div className="hero-main">
              <h2>
                {sessions === null
                  ? "Reading the registry"
                  : waiting.length === 0
                    ? "Nothing is waiting on you."
                    : `${waiting.length} ${plural(waiting.length, "session")} ${waiting.length === 1 ? "wants" : "want"} a person.`}
              </h2>
              <p>
                {sessions === null
                  ? ""
                  : waiting.length === 0
                    ? `${live.length} live ${plural(live.length, "session")} and ${boxes?.length ?? 0} ${plural(boxes?.length ?? 0, "box", "boxes")} are running on their own. Every state here carries the clause that produced it; nothing is a score.`
                    : "Each one below says why. Opening it clears the unread mark in this browser and nowhere else."}
              </p>
            </div>
            <div className="hero-stats">
              <Stat n={fleet.sessions?.live ?? 0} label="live sessions" />
              <Stat n={fleet.sessions?.total ?? 0} label="recorded" />
              <Stat n={refused} label="refused fetches" tone={refused > 0 ? "bad" : undefined} />
              <Stat n={withFindings} label="findings" tone={withFindings > 0 ? "warn" : undefined} />
              <Stat n={boxes?.length ?? 0} label="boxes" />
              <Stat n={pressing.length} label="boxes under pressure" tone={pressing.length > 0 ? "bad" : undefined} />
            </div>
          </div>

          <Card title="Waiting on you" aside={waiting.length ? `${waiting.length}` : undefined}>
            {waiting.length === 0 ? (
              <div className="card-empty">
                No session is blocked, and every finished one has been read.
              </div>
            ) : (
              waiting.slice(0, 12).map((s) => (
                <button key={s.id} type="button" className="card-row" onClick={() => open(s)}>
                  <span className="card-row-name">{s.name ?? s.id}</span>
                  <AttentionTag state={shownState(s, fleet.seen)} why={s.attention.why} />
                  <span className="card-row-why">{s.attention.why}</span>
                </button>
              ))
            )}
            {waiting.length > 12 ? (
              <button type="button" className="card-row" onClick={() => go(["sessions"])}>
                <span className="card-row-why">and {waiting.length - 12} more in Sessions</span>
              </button>
            ) : null}
          </Card>

          <Card title="Live sessions" aside={live.length ? `${live.length} of ${fleet.sessions?.live ?? 0}` : undefined}>
            {live.length === 0 ? (
              <div className="card-empty">
                Nothing is running. <Cmd text="h5i browser open <url>" /> starts a session.
              </div>
            ) : (
              live.slice(0, 12).map((s) => (
                <button key={s.id} type="button" className="card-row" onClick={() => open(s)}>
                  <span className="card-row-name">
                    {s.name ?? s.id}{" "}
                    <span className="srow-age">{hostOf(s.url)}</span>
                  </span>
                  <span className="card-row-aside">
                    {s.last_request_at ? ago(s.last_request_at) : ATTENTION_LABEL[shownState(s, fleet.seen)]}
                  </span>
                  <span className="card-row-why">
                    <Count n={s.requests} label="fetches" />{" "}
                    {s.denied > 0 ? <Count n={s.denied} label="refused" tone="bad" /> : null}{" "}
                    {s.held_by_human ? "a human holds the wheel" : s.attention.why}
                  </span>
                </button>
              ))
            )}
          </Card>

          <Card
            title="Findings"
            aside={
              withFindings
                ? [
                    projectOpen ? `${projectOpen} open in projects` : "",
                    sessionFindings ? `${sessionFindings} in sessions` : "",
                  ]
                    .filter(Boolean)
                    .join(", ")
                : undefined
            }
          >
            {withFindings === 0 ? (
              <div className="card-empty">
                No open finding. An agent writes one with <Cmd text="h5i websec finding create" />.
              </div>
            ) : findings.length === 0 ? (
              <div className="card-empty">reading</div>
            ) : (
              findings.slice(0, 10).map((f) => (
                <button key={`${f.where}/${f.id}`} type="button" className="card-row" onClick={() => go(f.route)}>
                  <span className="card-row-name">{f.title || "(untitled)"}</span>
                  <span className="card-row-aside">{f.tag}</span>
                  <span className="card-row-why">
                    {f.kind === "project" ? "project" : "session"} {f.where}
                  </span>
                </button>
              ))
            )}
            {findings.length > 10 ? (
              <div className="card-row">
                <span className="card-row-why">and {findings.length - 10} more</span>
              </div>
            ) : null}
          </Card>

          <Card title="Boxes under pressure" aside={pressing.length ? `${pressing.length} of ${boxes?.length ?? 0}` : undefined}>
            {boxes === null ? (
              <div className="card-empty">reading</div>
            ) : boxes.length === 0 ? (
              <div className="card-empty">
                This repository has no boxes. <Cmd text="h5i box create <name>" /> makes one.
              </div>
            ) : pressing.length === 0 ? (
              <div className="card-empty">
                Every box ran clean: no refused egress and no failed run.
              </div>
            ) : (
              pressing.map((b) => <BoxLine key={b.id} b={b} onOpen={() => go(["boxes", b.agent, b.slug])} />)
            )}
          </Card>
        </div>
      </div>
    </>
  );
}

interface FindingLine {
  kind: "project" | "session";
  id: string;
  title: string;
  /** Severity for a project finding, the agent's state for a session one. */
  tag: string;
  where: string;
  updated: string;
  route: string[];
}

const OPEN = new Set(["", "open", "in-progress", "fix-claimed"]);
const SEVERITY = ["critical", "high", "medium", "low", "info"];

/**
 * Finding titles, which the fleet poll does not carry. Only projects with an
 * open finding and sessions with any are read, and each is read again only
 * when its own count or stamp moves.
 */
function useFindings(fleet: Fleet): FindingLine[] {
  const cache = useRef(new Map<string, FindingLine[]>());
  const [, bump] = useState(0);
  const wanted = useMemo(() => {
    const out: { key: string; load: () => Promise<FindingLine[]> }[] = [];
    for (const p of fleet.projects ?? []) {
      if (p.open_findings === 0) continue;
      out.push({
        key: `p|${p.name}|${p.updated ?? ""}|${p.open_findings}`,
        load: () =>
          api.project(p.name).then((d) =>
            d.findings
              .filter((f) => OPEN.has(f.status))
              .map((f) => ({
                kind: "project" as const,
                id: f.id,
                title: f.title,
                tag: f.severity || f.status || "open",
                where: p.title || p.name,
                updated: f.updated,
                route: ["projects", p.name, "findings"],
              })),
          ),
      });
    }
    const sessions = (fleet.sessions?.sessions ?? []).filter((s) => s.findings > 0).slice(0, 12);
    for (const s of sessions) {
      out.push({
        key: `s|${s.id}|${s.findings}|${s.last_request_at ?? ""}`,
        load: () =>
          api.session(s.id).then((d) =>
            d.findings_list.map((f) => ({
              kind: "session" as const,
              id: f.id,
              title: f.title,
              tag: f.state || "finding",
              where: s.name ?? s.id,
              updated: f.updated,
              route: ["sessions", s.id, "findings"],
            })),
          ),
      });
    }
    return out;
  }, [fleet.projects, fleet.sessions]);

  useEffect(() => {
    let alive = true;
    for (const w of wanted) {
      if (cache.current.has(w.key)) continue;
      cache.current.set(w.key, []);
      w.load()
        .then((lines) => {
          cache.current.set(w.key, lines);
          if (alive) bump((n) => n + 1);
        })
        .catch(() => cache.current.delete(w.key));
    }
    return () => {
      alive = false;
    };
  }, [wanted]);

  const rank = (f: FindingLine) => {
    const i = SEVERITY.indexOf(f.tag);
    return f.kind === "project" ? (i < 0 ? SEVERITY.length : i) : SEVERITY.length + 1;
  };
  return wanted
    .flatMap((w) => cache.current.get(w.key) ?? [])
    .sort((a, b) => rank(a) - rank(b) || b.updated.localeCompare(a.updated));
}

function BoxLine({ b, onOpen }: { b: BoxRow; onOpen: () => void }) {
  const s = b.signals;
  const bits = [
    s.egress_denied ? `${s.egress_denied} refused egress` : null,
    s.failed ? `${s.failed} failed` : null,
    s.timed_out ? `${s.timed_out} timed out` : null,
  ].filter(Boolean);
  return (
    <button type="button" className="card-row" onClick={onOpen}>
      <span className="card-row-name">
        {b.agent}/{b.slug}
      </span>
      <span className={`pressure ${s.verdict === "denial" ? "critical" : "warning"}`}>
        {s.verdict === "denial" ? "refused" : "attention"}
      </span>
      <span className="card-row-why">{bits.join(", ")}</span>
    </button>
  );
}

function Card({ title, aside, children }: { title: string; aside?: string; children: React.ReactNode }) {
  return (
    <section className="card">
      <div className="card-head">
        <h3>{title}</h3>
        {aside ? <span>{aside}</span> : null}
      </div>
      <div className="card-body">{children}</div>
    </section>
  );
}

function Stat({ n, label, tone }: { n: number; label: string; tone?: "bad" | "warn" | "good" }) {
  return (
    <div className={`stat${tone ? ` is-${tone}` : ""}`}>
      <b>{n.toLocaleString()}</b>
      <span>{label}</span>
    </div>
  );
}
