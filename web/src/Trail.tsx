import { useEffect, useRef, useState } from "react";

import type { Fleet } from "./App";
import type { SessionRow } from "./api";
import { hostOf, keep, plural, remember } from "./ui";

/**
 * The one moving thing on the board. Every mark it draws is a number the
 * registry reported: a node is a host a live session is on, a packet is a
 * fetch counted since the last poll, a flag is a finding, and the gate is
 * drawn only where the host refused a fetch. Nothing is invented to fill the
 * band, so glancing at it is glancing at the fleet rather than at a
 * decoration, and the line under it says the same thing in words.
 */

const MAX_NODES = 7;
const MAX_PACKETS = 6;

/** The trail in user units: the scout walks between these, the gate ends it. */
const LEFT = 56;
const RIGHT = 946;
const GATE_X = 938;
const BASE = 34;
/** The scout walks above the wire; packets travel on it. */
const WALK = BASE - 8;

interface Node {
  host: string;
  findings: number;
  x: number;
}

export type Scene = "quiet" | "idle" | "working" | "waiting" | "held";

export function Trail({ fleet }: { fleet: Fleet }) {
  const [shown, setShown] = useState(() => remember("h5i.console.trail", 1) === 1);
  const paused = usePageHidden();
  const sessions = fleet.sessions?.sessions ?? null;
  const live = (sessions ?? []).filter((s) => s.state === "live");

  // Fetches counted between two polls. The first read has nothing to subtract
  // from, so it draws no packets rather than the whole session's history.
  const fetched = useDelta(live.reduce((n, s) => n + s.requests, 0), fleet.tick, sessions !== null);

  const denied = live.reduce((n, s) => n + s.denied, 0);
  const findings = live.reduce((n, s) => n + s.findings, 0);
  const scene = sceneOf(live);
  const nodes = nodesOf(live, denied > 0);
  const walkTo = denied > 0 ? GATE_X - 26 : RIGHT;
  // A parked scout stands on the host of the session that parked it, so the
  // place it stops is a fact and not just the end of the band.
  const park = parkOf(live, scene, nodes, walkTo);
  const caption = captionOf(scene, nodes.length, fetched, denied, findings);

  if (!shown) {
    return (
      <div className="trail is-folded">
        <span className="trail-caption">{caption}</span>
        <button type="button" className="trail-toggle" onClick={() => setShown(keepTrail(true))}>
          show the trail
        </button>
      </div>
    );
  }

  return (
    <div className="trail">
      <svg
        className={`trail-svg is-${scene}${paused ? " is-paused" : ""}`}
        viewBox="0 0 1000 56"
        role="img"
        aria-label={caption}
      >
        <line className="trail-line" x1={LEFT} y1={BASE} x2={denied > 0 ? GATE_X : RIGHT} y2={BASE} />

        {nodes.map((n) => (
          <g key={n.host} className="trail-node">
            <circle cx={n.x} cy={BASE} r={5} />
            {n.findings > 0 ? (
              <g className="trail-flag">
                <line x1={n.x} y1={BASE - 8} x2={n.x} y2={BASE - 21} />
                <path d={`M${n.x} ${BASE - 21} l9 3.5 -9 3.5 z`} />
              </g>
            ) : null}
            <text x={n.x} y={BASE + 16}>
              {n.host}
            </text>
          </g>
        ))}

        {Array.from({ length: Math.min(fetched, MAX_PACKETS) }, (_, i) => (
          <circle
            key={i}
            className="trail-packet"
            cy={BASE}
            r={2.2}
            style={walk(LEFT, walkTo, `${3.2 + i * 0.35}s`, `${i * 0.6}s`)}
          />
        ))}

        {denied > 0 ? (
          <g className="trail-gate">
            <rect x={GATE_X} y={14} width={8} height={34} rx={1} />
            <text x={GATE_X - 10} y={11}>
              {denied} refused
            </text>
          </g>
        ) : null}

        <g className="trail-scout" style={walk(LEFT, walkTo, "15s", "0s", park)}>
          <g className="trail-bob">
            <line className="trail-tail" x1={-19} y1={WALK} x2={-8} y2={WALK} />
            <circle className="trail-body" cy={WALK} r={6.5} />
            <circle className="trail-eye" cx={2.4} cy={WALK - 1.8} r={1.7} />
            {scene === "held" ? (
              <g className="trail-key">
                <circle cy={WALK - 15} r={2.8} />
                <path d={`M0 ${WALK - 13} v6 M0 ${WALK - 8} h3`} />
              </g>
            ) : null}
          </g>
        </g>
      </svg>
      <div className="trail-foot">
        <span className="trail-caption">{caption}</span>
        <button type="button" className="trail-toggle" onClick={() => setShown(keepTrail(false))}>
          hide
        </button>
      </div>
    </div>
  );
}

/** Where the scout and the packets travel, as the keyframes read them. */
function walk(from: number, to: number, dur: string, delay: string, park?: number) {
  return {
    "--trail-from": `${from}px`,
    "--trail-to": `${to}px`,
    "--trail-dur": dur,
    "--trail-delay": delay,
    ...(park === undefined ? {} : { "--trail-park": `${park}px` }),
  } as React.CSSProperties;
}

/** Where a waiting or held scout stands: the node of the session that put it
 *  there, a step to its left so both stay legible. */
function parkOf(live: SessionRow[], scene: Scene, nodes: Node[], fallback: number): number {
  const which =
    scene === "waiting"
      ? live.find((s) => s.attention.state === "blocked")
      : scene === "held"
        ? live.find((s) => s.held_by_human)
        : undefined;
  if (which) {
    const host = hostOf(which.url);
    const node = nodes.find((n) => host.startsWith(n.host.replace(/…$/, "")));
    if (node) return node.x - 11;
  }
  return nodes.length > 0 ? nodes[nodes.length - 1].x - 11 : fallback;
}

function keepTrail(next: boolean): boolean {
  keep("h5i.console.trail", next ? 1 : 0);
  return next;
}

/** One node per host a live session is on, newest first, the tail folded into
 *  a single node so the band never crowds. */
export function nodesOf(live: SessionRow[], gated: boolean): Node[] {
  const byHost = new Map<string, number>();
  for (const s of live) {
    const host = hostOf(s.url);
    byHost.set(host, (byHost.get(host) ?? 0) + s.findings);
  }
  const hosts = [...byHost];
  const shown: [string, number][] =
    hosts.length > MAX_NODES
      ? [
          ...hosts.slice(0, MAX_NODES - 1),
          [
            `+${hosts.length - MAX_NODES + 1} more`,
            hosts.slice(MAX_NODES - 1).reduce((n, [, f]) => n + f, 0),
          ],
        ]
      : hosts;
  const last = gated ? GATE_X - 90 : RIGHT - 50;
  const first = LEFT + 50;
  return shown.map(([host, findings], i) => ({
    host: host.length > 22 ? `${host.slice(0, 21)}…` : host,
    findings,
    x: shown.length === 1 ? (first + last) / 2 : first + ((last - first) * i) / (shown.length - 1),
  }));
}

/** The coarse state the band moves in. It is the attention state the server
 *  already computed, folded over the live sessions. */
function sceneOf(live: SessionRow[]): Scene {
  if (live.length === 0) return "quiet";
  const states = live.map((s) => s.attention.state);
  if (states.includes("blocked")) return "waiting";
  if (live.some((s) => s.held_by_human)) return "held";
  if (states.includes("working")) return "working";
  return "idle";
}

export function captionOf(
  scene: Scene,
  hosts: number,
  fetched: number,
  denied: number,
  findings: number,
): string {
  if (scene === "quiet") return "Nothing is running.";
  const bits = [`${hosts} ${plural(hosts, "host")}`];
  if (fetched > 0) bits.push(`${fetched} ${plural(fetched, "fetch", "fetches")} since the last poll`);
  if (denied > 0) bits.push(`${denied} refused`);
  if (findings > 0) bits.push(`${findings} ${plural(findings, "finding")}`);
  if (scene === "waiting") bits.push("one session wants a person");
  if (scene === "held") bits.push("a human holds the wheel");
  return `${bits.join(", ")}.`;
}

/** How much a counter grew between two polls. */
function useDelta(total: number, tick: number, ready: boolean): number {
  const prev = useRef<{ total: number; tick: number } | null>(null);
  const [delta, setDelta] = useState(0);
  useEffect(() => {
    if (!ready) return;
    const last = prev.current;
    prev.current = { total, tick };
    if (last === null || last.tick === tick) return;
    setDelta(Math.max(0, total - last.total));
  }, [total, tick, ready]);
  return delta;
}

/** A console is left open for days; a band animating in a background tab is
 *  just a battery cost. */
function usePageHidden(): boolean {
  const [hidden, setHidden] = useState(() => document.visibilityState === "hidden");
  useEffect(() => {
    const on = () => setHidden(document.visibilityState === "hidden");
    document.addEventListener("visibilitychange", on);
    return () => document.removeEventListener("visibilitychange", on);
  }, []);
  return hidden;
}
