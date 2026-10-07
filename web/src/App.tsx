import { useCallback, useEffect, useMemo, useState } from "react";

import {
  api,
  type BoxRow,
  type CapabilitiesReport,
  type ProjectSummary,
  type SessionFleet,
  type SessionRow,
} from "./api";
import { BoxesPage } from "./Boxes";
import { AppsPage } from "./Apps";
import { HostPage } from "./Host";
import { Overview } from "./Overview";
import { ProjectsPage } from "./Projects";
import { SessionsPage } from "./Sessions";
import { doneMark, loadSeen, markSeen, shownState } from "./seen";
import { Empty, localClock, useHash } from "./ui";

// One screen over everything h5i is doing on this machine. The console
// watches and never drives: every route it calls is a GET, and every next
// step it suggests is a command to type.

const POLL_MS = 8000;

export type Section = "overview" | "projects" | "apps" | "sessions" | "boxes" | "host";

const SECTIONS: { key: Section; label: string; hint: string }[] = [
  { key: "overview", label: "Overview", hint: "what wants a person, and what is running" },
  { key: "projects", label: "Projects", hint: "engagements, and every session under one" },
  { key: "apps", label: "Apps", hint: "guarantees, changed conditions and mutation evidence" },
  { key: "sessions", label: "Sessions", hint: "browser sessions on this machine" },
  { key: "boxes", label: "Boxes", hint: "boxes of the repository this console was started in" },
  { key: "host", label: "Host", hint: "what this machine can enforce" },
];

export interface Fleet {
  boxes: BoxRow[] | null;
  sessions: SessionFleet | null;
  probe: CapabilitiesReport | null;
  projects: ProjectSummary[] | null;
  /** When the last poll read anything, in this browser's clock. */
  polledAt: number | null;
  /** Bumped on every successful poll so detail panes refresh with the list. */
  tick: number;
  error: string | null;
  seen: Record<string, string>;
  /** Looking at a session is what clears its `done`; the memory is this
   *  browser's, never sent back. */
  look: (s: SessionRow) => void;
}

export function App() {
  const [route, go] = useHash();
  const [boxes, setBoxes] = useState<BoxRow[] | null>(null);
  const [sessions, setSessions] = useState<SessionFleet | null>(null);
  const [probe, setProbe] = useState<CapabilitiesReport | null>(null);
  const [projects, setProjects] = useState<ProjectSummary[] | null>(null);
  const [polledAt, setPolledAt] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const [seen, setSeen] = useState<Record<string, string>>(loadSeen);

  const load = useCallback(() => {
    // The two registries fail independently: a machine that cannot read one
    // still shows the other.
    void Promise.allSettled([api.boxes(), api.sessions(), api.projects()]).then(([b, s, p]) => {
      if (b.status === "fulfilled") setBoxes(b.value);
      if (s.status === "fulfilled") setSessions(s.value);
      if (p.status === "fulfilled") setProjects(p.value);
      if (b.status === "rejected" && s.status === "rejected") {
        setError(String(b.reason instanceof Error ? b.reason.message : b.reason));
      } else {
        setError(null);
        setPolledAt(Date.now());
        setTick((t) => t + 1);
      }
    });
  }, []);

  useEffect(() => {
    load();
    const t = window.setInterval(load, POLL_MS);
    return () => window.clearInterval(t);
  }, [load]);

  // Host state, not fleet state: the probe shells out, so it is read once.
  useEffect(() => {
    api.probe().then(setProbe).catch(() => setProbe(null));
  }, []);

  const look = useCallback((s: SessionRow) => {
    if (shownState(s, loadSeen()) === "done") setSeen(markSeen(s));
  }, []);

  const fleet: Fleet = useMemo(
    () => ({ boxes, sessions, probe, projects, polledAt, tick, error, seen, look }),
    [boxes, sessions, probe, projects, polledAt, tick, error, seen, look],
  );

  const section: Section = (SECTIONS.find((s) => s.key === route[0])?.key ?? "overview") as Section;

  // Rail badges: what is loud, not what is many.
  const waiting = useMemo(() => {
    let n = 0;
    for (const s of sessions?.sessions ?? []) {
      const st = shownState(s, seen);
      if (st === "blocked" || st === "done") n += 1;
    }
    return n;
  }, [sessions, seen]);
  const pressing = useMemo(
    () => (boxes ?? []).filter((b) => b.signals.verdict !== "clean").length,
    [boxes],
  );
  const openFindings = useMemo(
    () => (projects ?? []).reduce((n, p) => n + p.open_findings, 0),
    [projects],
  );

  return (
    <div className="app">
      <nav className="rail" aria-label="sections">
        <div className="rail-brand" title="read-only, loopback only, lifecycle verbs stay in the CLI">
          <b>h5i</b>
          <span>console</span>
        </div>
        {SECTIONS.map((s) => {
          // Each badge counts one thing that wants a person, and is gone at zero.
          const badge =
            s.key === "sessions"
              ? { n: waiting, tone: "loud", hint: "waiting on you" }
              : s.key === "boxes"
                ? { n: pressing, tone: "loud", hint: "under pressure" }
                : s.key === "projects"
                  ? { n: openFindings, tone: "warn", hint: "open findings" }
                  : null;
          return (
            <button
              key={s.key}
              type="button"
              className={`rail-item${section === s.key ? " is-on" : ""}`}
              title={badge && badge.n > 0 ? `${s.hint}\n${badge.n} ${badge.hint}` : s.hint}
              onClick={() => go([s.key])}
              aria-current={section === s.key ? "page" : undefined}
            >
              <span>{s.label}</span>
              {badge && badge.n > 0 ? (
                <span className={`rail-badge is-${badge.tone}`}>{badge.n}</span>
              ) : null}
            </button>
          );
        })}
        <div className="rail-foot">
          <b>watches, never drives.</b>
          <br />
          {error ? (
            <span className="rail-foot-bad">last read failed</span>
          ) : polledAt ? (
            <>read at {localClock(polledAt)}, every {POLL_MS / 1000}s</>
          ) : (
            "reading"
          )}
        </div>
      </nav>

      <div className="main">
        {section === "apps" ? (
          <AppsPage route={route.slice(1)} go={go} />
        ) : error && !boxes && !sessions ? (
          <Empty title="Could not read the fleet">
            <p>{error}</p>
          </Empty>
        ) : section === "overview" ? (
          <Overview fleet={fleet} go={go} />
        ) : section === "projects" ? (
          <ProjectsPage fleet={fleet} route={route.slice(1)} go={go} />
        ) : section === "sessions" ? (
          <SessionsPage fleet={fleet} route={route.slice(1)} go={go} />
        ) : section === "boxes" ? (
          <BoxesPage fleet={fleet} route={route.slice(1)} go={go} />
        ) : (
          <HostPage fleet={fleet} />
        )}
      </div>
    </div>
  );
}

export { doneMark };
