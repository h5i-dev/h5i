import { useMemo, useState } from "react";
import { Button, Tag } from "@blueprintjs/core";
import type { ContextDag, ContextDagNode } from "./api";

// Reasoning DAG visualization.
//
// Layout: five vertical lanes (one per node kind), with the Y axis as
// chronological index (newest at top). Nodes are positioned absolutely; an
// SVG layer behind them draws cubic-bezier edges from parent to child.
//
// We render the last N nodes by default (default 40) — typical context DAGs
// have hundreds of OBSERVE entries and showing them all at once is noise.
// A "Show all" toggle expands the limit when the user wants the full picture.

const LANES = ["OBSERVE", "THINK", "ACT", "NOTE", "MERGE"] as const;
type Lane = (typeof LANES)[number];

const LANE_X: Record<string, number> = {
  OBSERVE: 0,
  THINK: 1,
  ACT: 2,
  NOTE: 3,
  MERGE: 4,
};

const LANE_COLOR: Record<string, string> = {
  OBSERVE: "var(--bp-blue-hi, #4c90f0)",
  THINK: "var(--bp-violet, #8f5fbf)",
  ACT: "var(--bp-green-hi, #72ca9b)",
  NOTE: "var(--bp-orange, #c87619)",
  MERGE: "#ff7bff",
};

const NODE_WIDTH = 220;
const LANE_GAP = 24;
const ROW_HEIGHT = 38;
const NODE_HEIGHT = 30;
const TOP_PAD = 36;
const SIDE_PAD = 12;

export function DagViz({ dag }: { dag: ContextDag }) {
  const [expanded, setExpanded] = useState(false);
  const [hoveredId, setHoveredId] = useState<string | null>(null);

  const limit = expanded ? dag.nodes.length : Math.min(40, dag.nodes.length);
  // The API returns nodes oldest-first; we render newest at the top so the
  // user lands on the most recent activity without scrolling.
  const slice = useMemo(
    () => dag.nodes.slice(-limit).slice().reverse(),
    [dag.nodes, limit],
  );

  const indexById = useMemo(() => {
    const m = new Map<string, number>();
    slice.forEach((n, i) => m.set(n.id, i));
    return m;
  }, [slice]);

  if (dag.nodes.length === 0) {
    return (
      <div className="ctx-empty-hint">
        No DAG nodes yet — <code>h5i context trace --kind OBSERVE "&hellip;"</code>{" "}
        records the first one.
      </div>
    );
  }

  const totalWidth =
    LANES.length * NODE_WIDTH + (LANES.length - 1) * LANE_GAP + SIDE_PAD * 2;
  const totalHeight = TOP_PAD + slice.length * ROW_HEIGHT + 16;

  const xForLane = (kind: string) => {
    const lane = LANE_X[kind] ?? LANE_X.OBSERVE;
    return SIDE_PAD + lane * (NODE_WIDTH + LANE_GAP);
  };
  const yForIndex = (i: number) => TOP_PAD + i * ROW_HEIGHT;

  // Build edges: for each child, point at every parent that's in the slice.
  type EdgeSpec = {
    key: string;
    x1: number;
    y1: number;
    x2: number;
    y2: number;
    kind: string;
    parent: string;
    child: string;
  };
  const edges: EdgeSpec[] = [];
  for (const child of slice) {
    const childIdx = indexById.get(child.id);
    if (childIdx === undefined) continue;
    const cx = xForLane(child.kind) + NODE_WIDTH / 2;
    const cy = yForIndex(childIdx);
    for (const pid of child.parent_ids) {
      const parentIdx = indexById.get(pid);
      if (parentIdx === undefined) continue;
      const parent = slice[parentIdx];
      const px = xForLane(parent.kind) + NODE_WIDTH / 2;
      const py = yForIndex(parentIdx) + NODE_HEIGHT;
      edges.push({
        key: `${pid}->${child.id}`,
        x1: px,
        y1: py,
        x2: cx,
        y2: cy,
        kind: child.kind,
        parent: pid,
        child: child.id,
      });
    }
  }

  const isHighlit = (e: EdgeSpec) =>
    hoveredId !== null && (e.parent === hoveredId || e.child === hoveredId);

  return (
    <div className="ctx-dag-shell">
      <div
        className="ctx-dag-canvas"
        style={{
          width: totalWidth,
          height: totalHeight,
          position: "relative",
        }}
      >
        {/* Lane headers */}
        {LANES.map((lane) => (
          <div
            key={lane}
            className="ctx-dag-lane-header"
            style={{
              left: xForLane(lane),
              width: NODE_WIDTH,
              color: LANE_COLOR[lane],
            }}
          >
            <span className="ctx-dag-lane-label">{lane}</span>
            <span className="ctx-dag-lane-count">
              {countByKind(slice, lane)}
            </span>
          </div>
        ))}

        {/* Lane background guides */}
        <svg
          className="ctx-dag-edges"
          width={totalWidth}
          height={totalHeight}
          style={{ position: "absolute", inset: 0, pointerEvents: "none" }}
        >
          {LANES.map((lane) => (
            <line
              key={lane}
              x1={xForLane(lane) + NODE_WIDTH / 2}
              y1={TOP_PAD}
              x2={xForLane(lane) + NODE_WIDTH / 2}
              y2={totalHeight - 8}
              stroke="var(--bp-border, #404854)"
              strokeWidth={1}
              strokeDasharray="2 4"
              opacity={0.5}
            />
          ))}
          {/* Edges */}
          {edges.map((e) => {
            const dy = Math.max(20, Math.abs(e.y2 - e.y1) / 2);
            const d = `M ${e.x1} ${e.y1} C ${e.x1} ${e.y1 + dy}, ${e.x2} ${
              e.y2 - dy
            }, ${e.x2} ${e.y2}`;
            const hi = isHighlit(e);
            return (
              <path
                key={e.key}
                d={d}
                fill="none"
                stroke={hi ? LANE_COLOR[e.kind] : "var(--bp-border-strong, #5c7080)"}
                strokeWidth={hi ? 2 : 1}
                opacity={hi ? 1 : 0.55}
              />
            );
          })}
        </svg>

        {/* Nodes (HTML for native interaction + text rendering) */}
        {slice.map((n, i) => (
          <DagNode
            key={n.id}
            node={n}
            x={xForLane(n.kind)}
            y={yForIndex(i)}
            color={LANE_COLOR[n.kind] ?? LANE_COLOR.OBSERVE}
            highlight={hoveredId === n.id}
            onEnter={() => setHoveredId(n.id)}
            onLeave={() => setHoveredId(null)}
          />
        ))}
      </div>

      {dag.nodes.length > 40 ? (
        <div className="ctx-dag-controls">
          <Button
            minimal
            small
            icon={expanded ? "minimize" : "maximize"}
            onClick={() => setExpanded((v) => !v)}
          >
            {expanded
              ? `Showing all ${dag.nodes.length}`
              : `Showing last 40 of ${dag.nodes.length}`}
          </Button>
        </div>
      ) : null}
    </div>
  );
}

function DagNode({
  node,
  x,
  y,
  color,
  highlight,
  onEnter,
  onLeave,
}: {
  node: ContextDagNode;
  x: number;
  y: number;
  color: string;
  highlight: boolean;
  onEnter: () => void;
  onLeave: () => void;
}) {
  return (
    <div
      className={"ctx-dag-node" + (highlight ? " hi" : "")}
      style={{
        left: x,
        top: y,
        width: NODE_WIDTH,
        height: NODE_HEIGHT,
        borderColor: highlight ? color : "var(--bp-border)",
      }}
      onMouseEnter={onEnter}
      onMouseLeave={onLeave}
      title={`${node.kind} · ${node.timestamp}\n\n${node.content}`}
    >
      <span className="ctx-dag-node-time">{node.timestamp}</span>
      <span className="ctx-dag-node-content">{node.content}</span>
    </div>
  );
}

function countByKind(nodes: ContextDagNode[], kind: Lane): number {
  return nodes.filter((n) => n.kind === kind).length;
}

// ── Recent activity section (full content of OBSERVE / THINK / ACT) ────────
//
// Replaces the old `mini_trace` summary list. Pulls real content from the DAG
// nodes — the actual reasoning the agent recorded, not just `[ts] ACT: edited
// X`. This is the answer to "the context tab does not show observe/think
// content".

export function RecentActivity({
  dag,
  clipped = false,
}: {
  dag: ContextDag;
  /** When true, each entry is rendered as a single horizontally-scrollable
   * line so the section fits narrowly inside a column. Click an entry to
   * expand it inline and show the full multi-line content. */
  clipped?: boolean;
}) {
  const [showAll, setShowAll] = useState(false);
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());

  const toggleExpanded = (id: string) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  if (dag.nodes.length === 0) {
    return (
      <div className="ctx-empty-hint">
        No trace entries yet — record one with{" "}
        <code>h5i context trace --kind OBSERVE "&hellip;"</code>
      </div>
    );
  }

  const limit = showAll ? dag.nodes.length : 25;
  const items = dag.nodes.slice(-limit).slice().reverse();

  return (
    <>
      <ol className={"ctx-activity" + (clipped ? " ctx-activity-clipped" : "")}>
        {items.map((n) => {
          const intent =
            n.kind === "OBSERVE"
              ? "primary"
              : n.kind === "ACT"
                ? "success"
                : n.kind === "NOTE"
                  ? "warning"
                  : undefined;
          const isExpanded = expandedIds.has(n.id);
          return (
            <li
              key={n.id}
              className={isExpanded ? "expanded" : ""}
              title={clipped && !isExpanded ? n.content : undefined}
              onClick={() => toggleExpanded(n.id)}
            >
              <div className="ctx-activity-head">
                <Tag
                  intent={intent}
                  minimal
                  style={{
                    fontSize: 10,
                    fontWeight: 700,
                    letterSpacing: "0.04em",
                  }}
                >
                  {n.kind}
                </Tag>
                <span className="ctx-activity-ts">{n.timestamp}</span>
                <span className="ctx-activity-id">{n.id.slice(0, 7)}</span>
                <span className="ctx-activity-chevron" aria-hidden>
                  {isExpanded ? "▾" : "▸"}
                </span>
              </div>
              <div className="ctx-activity-body">{n.content}</div>
            </li>
          );
        })}
      </ol>
      {dag.nodes.length > 25 ? (
        <div className="ctx-dag-controls">
          <Button
            minimal
            small
            icon={showAll ? "minimize" : "maximize"}
            onClick={() => setShowAll((v) => !v)}
          >
            {showAll
              ? `Showing all ${dag.nodes.length}`
              : `Showing 25 of ${dag.nodes.length}`}
          </Button>
        </div>
      ) : null}
    </>
  );
}
