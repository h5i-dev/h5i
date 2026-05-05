import { useEffect, useMemo, useState } from "react";
import {
  Callout,
  HTMLTable,
  Icon,
  NonIdealState,
  Spinner,
  Tag,
} from "@blueprintjs/core";

import {
  api,
  type ContextDag,
  type ContextPromotion,
  type ContextShow,
  type ContextSnapshotItem,
  type ContextStatus,
} from "./api";
import { DagViz, RecentActivity } from "./DagViz";
import { HSplit } from "./HSplit";

// Comprehensive Context dashboard. Loads four endpoints in parallel:
//   /api/context/status      — workspace summary + per-branch stats
//   /api/context/show        — milestones + mini_trace + todos + recent commits
//   /api/context/promotion   — promotion pipeline counts for the active branch
//   /api/context/dag         — OTA graph stats + recent nodes
//   /api/context/snapshots   — snapshot history linked to git commits
//
// This is the "killer feature" surface that the legacy single-line summary
// hid behind a tab. We render: hero, KPIs, promotion pipeline, OTA balance,
// recent milestones, recent trace, todos, branches, snapshots.

interface AllCtx {
  status: ContextStatus;
  show: ContextShow;
  promotion: ContextPromotion;
  dag: ContextDag;
  snapshots: ContextSnapshotItem[];
}

export function ContextView() {
  const [data, setData] = useState<AllCtx | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setData(null);
    setError(null);
    Promise.all([
      api.contextStatus(),
      api.contextShow(),
      api.contextPromotion(),
      api.contextDag(),
      api.contextSnapshots(),
    ])
      .then(([status, show, promotion, dag, snapshots]) => {
        setData({ status, show, promotion, dag, snapshots });
      })
      .catch((e) => setError(String(e)));
  }, []);

  if (error) {
    return (
      <NonIdealState icon="error" title="Failed to load context" description={error} />
    );
  }
  if (!data) {
    return <NonIdealState icon={<Spinner size={20} />} title="Loading context…" />;
  }
  if (!data.status.initialized) {
    return (
      <div style={{ padding: 24 }}>
        <Callout intent="none" icon="info-sign">
          No context workspace. Run <code>h5i context init --goal "&lt;summary&gt;"</code>{" "}
          to start one.
        </Callout>
      </div>
    );
  }

  const { status, show, promotion, dag, snapshots } = data;
  const cleanMilestones = stripCheckmarks(show.milestones).reverse(); // newest first

  return (
    <div className="ctx-view">
      <Hero status={status} show={show} promotion={promotion} />
      <PromotionFlow p={promotion} dag={dag} />
      <OtaBalance dag={dag} />

      {/* Three-pane workbench-style row, matches Explore's visual feel:
          edge-to-edge, no gap, 1px draggable dividers between panes,
          sticky pane headers, internal scrolling per pane. */}
      <div className="ctx-three-bleed">
        <HSplit
          storageKey="h5i.ctx.three"
          leftDefaultPx={360}
          rightDefaultPx={380}
          leftMinPx={240}
          rightMinPx={260}
        >
          <CtxPane title="Recent milestones" count={cleanMilestones.length}>
            {cleanMilestones.length === 0 ? (
              <EmptyHint>
                No milestones yet — <code>h5i context commit "&lt;summary&gt;"</code>
              </EmptyHint>
            ) : (
              <ol className="ctx-milestones ctx-milestones-clipped">
                {cleanMilestones.map((m, i) => (
                  <li key={i} title={m}>
                    {m}
                  </li>
                ))}
              </ol>
            )}
          </CtxPane>

          <CtxPane title="Reasoning DAG" count={dag.node_count}>
            <DagViz dag={dag} />
          </CtxPane>

          <CtxPane title="Recent activity" count={dag.node_count}>
            <RecentActivity dag={dag} clipped />
          </CtxPane>
        </HSplit>
      </div>

      {show.todo_items.length > 0 ? (
        <Section
          title="Open TODOs"
          count={show.todo_items.length}
          intent="warning"
        >
          <ul className="ctx-todos">
            {show.todo_items.map((t, i) => (
              <li key={i}>
                <Icon icon="dot" size={10} intent="warning" /> {t}
              </li>
            ))}
          </ul>
        </Section>
      ) : null}

      <Section title="Branches" count={status.branch_summaries.length}>
        <BranchesTable status={status} />
      </Section>

      {snapshots.length > 0 ? (
        <Section title="Snapshot history" count={snapshots.length}>
          <SnapshotsTable snapshots={snapshots} />
        </Section>
      ) : null}
    </div>
  );
}

// ── Branches table — sortable, active-pinned, freshness, sparkbars ────────

type BranchSortKey = "activity" | "branch" | "milestones" | "trace" | "todos" | "exclusive";

function BranchesTable({ status }: { status: ContextStatus }) {
  const [sortKey, setSortKey] = useState<BranchSortKey>("activity");
  const [sortDir, setSortDir] = useState<"asc" | "desc">("desc");

  const all = status.branch_summaries;
  const maxMilestones = Math.max(1, ...all.map((b) => b.milestone_count));
  const maxTrace = Math.max(1, ...all.map((b) => b.trace_lines));

  // Active branch is always pinned to the top regardless of sort.
  const sorted = useMemo(() => {
    const active = all.find((b) => b.branch === status.current_branch);
    const others = all.filter((b) => b.branch !== status.current_branch);
    const dir = sortDir === "asc" ? 1 : -1;
    others.sort((a, b) => {
      switch (sortKey) {
        case "branch":
          return dir * a.branch.localeCompare(b.branch);
        case "milestones":
          return dir * (a.milestone_count - b.milestone_count);
        case "trace":
          return dir * (a.trace_lines - b.trace_lines);
        case "todos":
          return dir * (a.todo_count - b.todo_count);
        case "exclusive":
          return (
            dir *
            (a.exclusive_milestones +
              a.exclusive_trace_lines -
              (b.exclusive_milestones + b.exclusive_trace_lines))
          );
        case "activity":
          return dir * a.last_activity.localeCompare(b.last_activity);
      }
    });
    return active ? [active, ...others] : others;
  }, [all, sortKey, sortDir, status.current_branch]);

  const headerProps = (key: BranchSortKey, width?: number) => ({
    onClick: () => {
      if (sortKey === key) {
        setSortDir((d) => (d === "asc" ? "desc" : "asc"));
      } else {
        setSortKey(key);
        setSortDir("desc");
      }
    },
    "data-sortable": "true",
    "data-sorted": sortKey === key ? sortDir : undefined,
    style: {
      cursor: "pointer",
      userSelect: "none" as const,
      ...(width != null ? { width } : null),
    },
  });

  return (
    <HTMLTable className="ctx-branches ctx-table" interactive compact>
      <thead>
        <tr>
          <th style={{ width: 28 }} />
          <th {...headerProps("branch")}>Branch</th>
          <th>Purpose</th>
          <th {...headerProps("activity", 130)}>Last activity</th>
          <th {...headerProps("milestones", 130)}>Milestones</th>
          <th {...headerProps("trace", 130)}>Trace</th>
          <th {...headerProps("todos", 70)}>TODOs</th>
          <th {...headerProps("exclusive", 110)}>Exclusive</th>
        </tr>
      </thead>
      <tbody>
        {sorted.map((b) => {
          const isActive = b.branch === status.current_branch;
          const fresh = freshnessClass(parseUtcStamp(b.last_activity));
          return (
            <tr
              key={b.branch}
              className={
                (isActive ? "active " : "") + (b.is_scope ? "scope" : "")
              }
            >
              <td className="ctx-fresh-cell">
                <span
                  className={"ctx-fresh-dot " + fresh}
                  title={`Last activity: ${b.last_activity}`}
                />
              </td>
              <td>
                <Tag minimal style={{ fontFamily: "monospace" }}>
                  {b.branch}
                </Tag>
                {isActive ? (
                  <Tag
                    intent="primary"
                    minimal
                    style={{
                      marginLeft: 6,
                      fontSize: 10,
                      fontWeight: 700,
                      letterSpacing: "0.06em",
                      textTransform: "uppercase",
                    }}
                  >
                    HEAD
                  </Tag>
                ) : null}
                {b.is_scope ? (
                  <Tag
                    minimal
                    style={{
                      marginLeft: 6,
                      fontSize: 10,
                      letterSpacing: "0.06em",
                      textTransform: "uppercase",
                      color: "var(--bp-text-dim)",
                    }}
                  >
                    scope
                  </Tag>
                ) : null}
              </td>
              <td className="ctx-cell-purpose" title={b.purpose}>
                {b.purpose}
              </td>
              <td className="ctx-cell-activity">
                <span style={{ color: "var(--bp-text-muted)", fontSize: 11 }}>
                  {b.last_activity}
                </span>
              </td>
              <td>
                <SparkCell value={b.milestone_count} max={maxMilestones} color="var(--bp-violet)" />
              </td>
              <td>
                <SparkCell value={b.trace_lines} max={maxTrace} color="var(--bp-blue)" />
              </td>
              <td className="mono">
                {b.todo_count > 0 ? (
                  <span style={{ color: "var(--bp-orange)", fontWeight: 600 }}>
                    {b.todo_count}
                  </span>
                ) : (
                  <span style={{ color: "var(--bp-text-dim)" }}>—</span>
                )}
              </td>
              <td
                className="mono ctx-exclusive"
                title={`${b.exclusive_milestones} milestones · ${b.exclusive_trace_lines} trace lines unique to this branch`}
              >
                {b.exclusive_milestones > 0 || b.exclusive_trace_lines > 0 ? (
                  <>
                    <span className="ctx-excl-num">{b.exclusive_milestones}</span>
                    <span className="ctx-excl-unit">m</span>
                    <span className="ctx-excl-sep">·</span>
                    <span className="ctx-excl-num">{b.exclusive_trace_lines}</span>
                    <span className="ctx-excl-unit">l</span>
                  </>
                ) : (
                  <span style={{ color: "var(--bp-text-dim)" }}>—</span>
                )}
              </td>
            </tr>
          );
        })}
      </tbody>
    </HTMLTable>
  );
}

// Inline sparkbar cell: number + proportional bar fill.
function SparkCell({
  value,
  max,
  color,
}: {
  value: number;
  max: number;
  color: string;
}) {
  const pct = max > 0 ? (value / max) * 100 : 0;
  return (
    <div className="ctx-spark">
      <span className="ctx-spark-val">{value}</span>
      <div className="ctx-spark-track">
        <div
          className="ctx-spark-fill"
          style={{ width: `${pct}%`, background: color }}
        />
      </div>
    </div>
  );
}

// ── Snapshot history table — time-delta column, goal-evolution highlight ──

function SnapshotsTable({ snapshots }: { snapshots: ContextSnapshotItem[] }) {
  // API returns oldest-first; reverse so newest is on top.
  const items = useMemo(() => [...snapshots].reverse(), [snapshots]);
  return (
    <HTMLTable className="ctx-branches ctx-table" interactive compact>
      <thead>
        <tr>
          <th style={{ width: 28 }} />
          <th style={{ width: 100 }}>Commit</th>
          <th style={{ width: 100 }}>Branch</th>
          <th>Goal at the time</th>
          <th style={{ width: 90 }}>Δ</th>
          <th style={{ width: 160 }}>Timestamp</th>
        </tr>
      </thead>
      <tbody>
        {items.map((s, i) => {
          const next = items[i + 1]; // older snapshot (we reversed)
          const goalChanged = next && next.goal !== s.goal;
          const delta = next
            ? formatDelta(parseUtcStamp(s.timestamp), parseUtcStamp(next.timestamp))
            : null;
          const fresh = i === 0 ? freshnessClass(parseUtcStamp(s.timestamp)) : "";
          return (
            <tr key={s.context_oid} className={i === 0 ? "active" : ""}>
              <td className="ctx-fresh-cell">
                {i === 0 ? (
                  <span
                    className={"ctx-fresh-dot " + fresh}
                    title={`Latest snapshot · ${s.timestamp}`}
                  />
                ) : null}
              </td>
              <td>
                <span className="wb-oid">{s.sha_short.slice(0, 7)}</span>
              </td>
              <td>
                <Tag minimal style={{ fontFamily: "monospace", fontSize: 11 }}>
                  {s.branch}
                </Tag>
              </td>
              <td className="ctx-cell-purpose" title={s.goal}>
                {goalChanged ? (
                  <span
                    style={{ color: "var(--bp-violet)", fontWeight: 500 }}
                    title={`Changed from: ${next!.goal}`}
                  >
                    <Icon icon="changes" size={11} style={{ marginRight: 4 }} />
                    {s.goal}
                  </span>
                ) : (
                  s.goal
                )}
              </td>
              <td className="mono ctx-delta-cell">
                {delta ? delta : <span style={{ color: "var(--bp-text-dim)" }}>—</span>}
              </td>
              <td style={{ fontSize: 12, color: "var(--bp-text-muted)" }}>
                {s.timestamp}
              </td>
            </tr>
          );
        })}
      </tbody>
    </HTMLTable>
  );
}

// ── Time helpers ──────────────────────────────────────────────────────────
function parseUtcStamp(s: string): Date | null {
  // Expects "YYYY-MM-DD HH:MM UTC".
  const m = s?.match(/^(\d{4}-\d{2}-\d{2})\s+(\d{2}:\d{2})(?::\d{2})?\s*UTC?$/);
  if (!m) return null;
  return new Date(`${m[1]}T${m[2]}:00Z`);
}

function freshnessClass(d: Date | null): "fresh" | "recent" | "stale" | "unknown" {
  if (!d) return "unknown";
  const ageMs = Date.now() - d.getTime();
  if (ageMs < 60 * 60 * 1000) return "fresh"; // < 1h
  if (ageMs < 24 * 60 * 60 * 1000) return "recent"; // < 1d
  return "stale";
}

function formatDelta(later: Date | null, earlier: Date | null): string | null {
  if (!later || !earlier) return null;
  const ms = later.getTime() - earlier.getTime();
  if (ms < 60 * 1000) return "<1m";
  if (ms < 60 * 60 * 1000) return `${Math.round(ms / 60000)}m`;
  if (ms < 24 * 60 * 60 * 1000) return `${Math.round(ms / 3_600_000)}h`;
  return `${Math.round(ms / 86_400_000)}d`;
}

function Hero({
  status,
  show,
  promotion,
}: {
  status: ContextStatus;
  show: ContextShow;
  promotion: ContextPromotion;
}) {
  return (
    <div className="ctx-hero">
      <div className="ctx-hero-goal">
        <div className="ctx-eyebrow">Goal · {status.current_branch}</div>
        <div className="ctx-hero-text">
          {show.project_goal || "(no goal recorded)"}
        </div>
        {promotion.purpose ? (
          <div className="ctx-hero-purpose">{promotion.purpose}</div>
        ) : null}
      </div>
      <div className="ctx-kpis">
        <Kpi value={show.milestones.length} label="milestones" />
        <Kpi value={status.trace_lines} label="trace lines" />
        <Kpi value={status.snapshot_count} label="snapshots" />
        <Kpi
          value={show.todo_items.length}
          label="todos"
          intent={show.todo_items.length > 0 ? "warning" : undefined}
        />
        <Kpi value={status.branch_count} label="branches" />
      </div>
    </div>
  );
}

function Kpi({
  value,
  label,
  intent,
}: {
  value: number | string;
  label: string;
  intent?: "warning";
}) {
  return (
    <div className="ctx-kpi">
      <div
        className="ctx-kpi-val"
        style={intent === "warning" ? { color: "var(--bp-orange)" } : undefined}
      >
        {value}
      </div>
      <div className="ctx-kpi-label">{label}</div>
    </div>
  );
}

function PromotionFlow({ p, dag }: { p: ContextPromotion; dag: ContextDag }) {
  // Pipeline: ephemeral → durable trace → milestone → snapshot → goal
  // Stable / dynamic line counts contextualise the trace volume.
  const steps = [
    {
      label: "Ephemeral",
      value: p.ephemeral_count,
      hint: "live working notes",
    },
    {
      label: "Durable trace",
      value: p.durable_trace_count,
      hint: "OBSERVE / THINK / ACT entries",
    },
    {
      label: "Milestones",
      value: p.milestone_count,
      hint: "promoted reasoning",
      emphasis: true,
    },
    {
      label: "Snapshots",
      value: p.snapshot_count,
      hint: "tied to git commits",
    },
  ];
  return (
    <div className="ctx-section">
      <div className="ctx-section-head">
        <span className="ctx-section-title">Promotion pipeline</span>
        <span className="ctx-section-sub">
          {p.stable_line_count}/{p.dynamic_line_count} stable / live ·{" "}
          {dag.node_count} OTA nodes · last snapshot {p.last_snapshot_timestamp}
        </span>
      </div>
      <div className="ctx-flow">
        {steps.map((s, i) => (
          <div key={s.label} className="ctx-flow-row">
            <div className={"ctx-flow-step " + (s.emphasis ? "emphasis" : "")}>
              <div className="ctx-flow-val">{s.value}</div>
              <div className="ctx-flow-label">{s.label}</div>
              <div className="ctx-flow-hint">{s.hint}</div>
            </div>
            {i < steps.length - 1 ? <div className="ctx-flow-arrow">→</div> : null}
          </div>
        ))}
      </div>
    </div>
  );
}

function OtaBalance({ dag }: { dag: ContextDag }) {
  const total = Math.max(
    1,
    dag.observe_count + dag.think_count + dag.act_count + dag.note_count + dag.merge_count,
  );
  const segments = [
    { label: "OBSERVE", count: dag.observe_count, color: "var(--bp-blue)" },
    { label: "THINK", count: dag.think_count, color: "var(--bp-violet)" },
    { label: "ACT", count: dag.act_count, color: "var(--bp-green-hi)" },
    { label: "NOTE", count: dag.note_count, color: "var(--bp-orange)" },
    { label: "MERGE", count: dag.merge_count, color: "#ff7bff" },
  ];
  return (
    <div className="ctx-section">
      <div className="ctx-section-head">
        <span className="ctx-section-title">OTA balance</span>
        <span className="ctx-section-sub">
          observe / think / act / note / merge across {dag.node_count} entries
        </span>
      </div>
      <div className="ctx-ota-bar">
        {segments.map((s) =>
          s.count > 0 ? (
            <div
              key={s.label}
              className="ctx-ota-segment"
              style={{
                width: `${(s.count / total) * 100}%`,
                background: s.color,
              }}
              title={`${s.label}: ${s.count}`}
            />
          ) : null,
        )}
      </div>
      <div className="ctx-ota-legend">
        {segments.map((s) => (
          <span key={s.label} className="ctx-ota-legend-item">
            <span
              className="ctx-ota-swatch"
              style={{ background: s.color }}
            />
            <span className="ctx-ota-legend-label">{s.label}</span>
            <span className="ctx-ota-legend-val">{s.count}</span>
          </span>
        ))}
      </div>
    </div>
  );
}

// Workbench-style pane (no card chrome, sticky header, internal scroll) —
// used inside the resizable 3-pane row to mirror Explore's visual feel.
function CtxPane({
  title,
  count,
  children,
}: {
  title: string;
  count?: number;
  children: React.ReactNode;
}) {
  return (
    <div className="ctx-pane">
      <div className="ctx-pane-header">
        <span>{title}</span>
        {count != null ? (
          <Tag minimal style={{ fontFamily: "monospace", fontSize: 10 }}>
            {count}
          </Tag>
        ) : null}
      </div>
      <div className="ctx-pane-body">{children}</div>
    </div>
  );
}

function Section({
  title,
  count,
  intent,
  children,
}: {
  title: string;
  count?: number;
  intent?: "warning";
  children: React.ReactNode;
}) {
  return (
    <div className="ctx-section">
      <div className="ctx-section-head">
        <span
          className="ctx-section-title"
          style={intent === "warning" ? { color: "var(--bp-orange)" } : undefined}
        >
          {title}
        </span>
        {count != null ? (
          <Tag minimal style={{ fontFamily: "monospace", fontSize: 11 }}>
            {count}
          </Tag>
        ) : null}
      </div>
      {children}
    </div>
  );
}

function EmptyHint({ children }: { children: React.ReactNode }) {
  return <div className="ctx-empty-hint">{children}</div>;
}

function stripCheckmarks(items: string[]): string[] {
  // `h5i context show` prefixes each milestone with "[x] " — strip for display.
  return items.map((m) => m.replace(/^\[.\]\s*/, ""));
}
