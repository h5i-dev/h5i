import { useCallback, useEffect, useMemo, useState } from "react";

import {
  api,
  type BoxRow,
  type CapabilitiesReport,
  type SessionFleet,
  type SessionRow,
} from "./api";
import { BoxesPage } from "./Boxes";
import { HostPage } from "./Host";
import { Overview } from "./Overview";
import { SessionsPage } from "./Sessions";
import { doneMark, loadSeen, markSeen, shownState } from "./seen";
import { Empty, useHash } from "./ui";

// One screen over everything h5i is doing on this machine. The console
// watches and never drives: every route it calls is a GET, and every next
// step it suggests is a command to type.

const POLL_MS = 8000;

export type Section = "overview" | "sessions" | "boxes" | "host";

const SECTIONS: { key: Section; label: string; hint: string }[] = [
  { key: "overview", label: "Overview", hint: "what wants a person, and what is running" },
  { key: "sessions", label: "Sessions", hint: "browser sessions on this machine" },
  { key: "boxes", label: "Boxes", hint: "boxes of the repository this console was started in" },
  { key: "host", label: "Host", hint: "what this machine can enforce" },
];

export interface Fleet {
  boxes: BoxRow[] | null;
  sessions: SessionFleet | null;
  probe: CapabilitiesReport | null;
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
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const [seen, setSeen] = useState<Record<string, string>>(loadSeen);

  const load = useCallback(() => {
    // The two registries fail independently: a machine that cannot read one
    // still shows the other.
    void Promise.allSettled([api.boxes(), api.sessions()]).then(([b, s]) => {
      if (b.status === "fulfilled") setBoxes(b.value);
      if (s.status === "fulfilled") setSessions(s.value);
      if (b.status === "rejected" && s.status === "rejected") {
        setError(String(b.reason instanceof Error ? b.reason.message : b.reason));
      } else {
        setError(null);
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
    () => ({ boxes, sessions, probe, tick, error, seen, look }),
    [boxes, sessions, probe, tick, error, seen, look],
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

  return (
    <div className="app">
      <nav className="rail" aria-label="sections">
        <div className="rail-brand" title="read-only, loopback only, lifecycle verbs stay in the CLI">
          <b>h5i</b>
          <span>console</span>
        </div>
        {SECTIONS.map((s) => {
          const badge =
            s.key === "sessions"
              ? waiting > 0
                ? { n: waiting, loud: true }
                : { n: sessions?.live ?? 0, loud: false }
              : s.key === "boxes"
                ? pressing > 0
                  ? { n: pressing, loud: true }
                  : { n: boxes?.length ?? 0, loud: false }
                : null;
          return (
            <button
              key={s.key}
              type="button"
              className={`rail-item${section === s.key ? " is-on" : ""}`}
              title={s.hint}
              onClick={() => go([s.key])}
              aria-current={section === s.key ? "page" : undefined}
            >
              <span>{s.label}</span>
              {badge && badge.n > 0 ? (
                <span className={`rail-badge${badge.loud ? " is-loud" : ""}`}>{badge.n}</span>
              ) : null}
            </button>
          );
        })}
        <div className="rail-foot">
          <b>watches, never drives.</b>
          <br />
          Every button here copies a command.
        </div>
      </nav>

      <div className="main">
        {error && !boxes && !sessions ? (
          <Empty title="Could not read the fleet">
            <p>{error}</p>
          </Empty>
        ) : section === "overview" ? (
          <Overview fleet={fleet} go={go} />
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
