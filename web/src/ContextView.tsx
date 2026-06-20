import React, { useEffect, useMemo, useState } from "react";
import {
  Button,
  Callout,
  HTMLTable,
  Icon,
  NonIdealState,
  Spinner,
  Tag,
} from "@blueprintjs/core";
import type { IconName } from "@blueprintjs/icons";

import {
  api,
  type BranchInfo,
  type Commit,
  type ContextDag,
  type ContextDiff,
  type ContextMilestoneEntry,
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
  branches: BranchInfo[];
  milestones: ContextMilestoneEntry[];
  /** The picked branch's own commits (base..branch), for snapshot scoping. */
  branchCommits: Commit[];
}

export function ContextView({ branch }: { branch?: string | null }) {
  const [data, setData] = useState<AllCtx | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setData(null);
    setError(null);
    let cancelled = false;
    (async () => {
      try {
        // The header picks a *git* branch, but the context dashboard is keyed to
        // *context* branches (refs/h5i/context/*). Only scope when the picked git
        // branch has a matching context shadow — otherwise the views would be
        // empty, so we keep the active context branch and flag the fallback.
        const branches = await api.branches();
        const picked = branch ? branches.find((b) => b.name === branch) : null;
        const scope = picked?.has_context_branch ? branch ?? undefined : undefined;
        const [status, show, promotion, dag, snapshots, milestones, branchCommits] =
          await Promise.all([
            api.contextStatus(),
            api.contextShow(scope),
            api.contextPromotion(scope),
            api.contextDag(scope),
            api.contextSnapshots(),
            api.contextMilestones(scope),
            // Snapshots are git-commit-linked, so scope them by the picked git
            // branch's own commits (base..branch), not by context branch.
            branch
              ? api.commits({ limit: 500, branch, branchOnly: true })
              : Promise.resolve([] as Commit[]),
          ]);
        if (!cancelled) {
          setData({ status, show, promotion, dag, snapshots, branches, milestones, branchCommits });
        }
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [branch]);

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

  const { status, show, promotion, dag, snapshots, branches, milestones, branchCommits } = data;
  // Newest milestones at the top. The structured /api/context/milestones is
  // the preferred source — it has SHA + timestamp per row. We fall back to
  // the bare-string list from /api/context/show if the structured call
  // returns nothing (e.g. context not initialised, or main.md branch).
  const milestoneRows: ContextMilestoneEntry[] =
    milestones.length > 0
      ? [...milestones].reverse()
      : stripCheckmarks(show.milestones)
          .map((s) => ({ sha_short: "", timestamp: "", contribution: s }))
          .reverse();
  const activeBranch =
    (branch ? branches.find((b) => b.name === branch) : null) ??
    branches.find((b) => b.is_head) ??
    null;

  // When the picked git branch has no context shadow, we couldn't scope to it —
  // tell the user we're showing the active context branch instead.
  const pickedGit = branch ? branches.find((b) => b.name === branch) : null;
  const noContextForPicked = !!pickedGit && !pickedGit.has_context_branch;

  // Snapshots are linked to git commits (`sha`), so scope the history to the
  // selected git branch's own commits — not the context-branch tag (a snapshot
  // for an improve-ui commit is tagged with whatever context branch was active
  // at commit time, e.g. prompt-score). Default branch (no base) → its full
  // history; no branch → all snapshots.
  // Plain const (not useMemo) — this runs after the early-return guards above,
  // so a hook here would violate the Rules of Hooks. The set build over ≤500
  // commits is negligible per render.
  const branchShas = new Set<string>();
  for (const c of branchCommits) {
    branchShas.add(c.git_oid);
    branchShas.add(c.short_oid);
  }
  const branchSnapshots = branch
    ? snapshots.filter((s) => branchShas.has(s.sha) || branchShas.has(s.sha_short))
    : snapshots;

  return (
    <div className="ctx-view">
      {noContextForPicked ? (
        <Callout intent="none" icon="git-branch" className="ctx-scope-note">
          Branch <code>{branch}</code> has no context workspace — showing the active
          context branch <code>{status.current_branch}</code>. Create one with{" "}
          <code>h5i context branch {branch} --purpose "&lt;intent&gt;"</code>.
        </Callout>
      ) : null}
      <Hero
        status={status}
        show={show}
        promotion={promotion}
        activeBranch={activeBranch}
      />

      <NextActions
        status={status}
        show={show}
        activeBranch={activeBranch}
      />

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
          <CtxPane title="Recent milestones" count={milestoneRows.length}>
            {milestoneRows.length === 0 ? (
              <EmptyHint>
                No milestones yet — <code>h5i context commit "&lt;summary&gt;"</code>
              </EmptyHint>
            ) : (
              <ol className="ctx-milestones ctx-milestones-clipped">
                {milestoneRows.map((m, i) => (
                  <li key={`${m.sha_short}-${i}`} title={m.contribution}>
                    {m.sha_short ? (
                      <span
                        className="ctx-milestone-sha"
                        title={`Context commit ${m.sha_short}${m.timestamp ? " · " + m.timestamp : ""}`}
                      >
                        {m.sha_short.slice(0, 7)}
                      </span>
                    ) : null}
                    <span className="ctx-milestone-text">{m.contribution}</span>
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
                <Icon icon="dot" size={10} intent="warning" />
                <span>{t}</span>
                <CopyButton
                  text={`h5i context trace --kind NOTE "TODO: ${escapeCommandText(t)}"`}
                  label="Copy note"
                />
              </li>
            ))}
          </ul>
        </Section>
      ) : null}

      <Section title="Branches" count={branches.filter((b) => !b.is_remote).length}>
        <BranchesTable branches={branches} />
      </Section>

      {branchSnapshots.length > 0 ? (
        <Section
          title={branch ? `Snapshot history · ${branch}` : "Snapshot history"}
          count={branchSnapshots.length}
        >
          <SnapshotsTable snapshots={branchSnapshots} />
        </Section>
      ) : null}

      {/* Diagnostics live at the bottom — useful for debugging the context
          pipeline and OTA balance, but not the primary content. */}
      <PromotionFlow p={promotion} dag={dag} />
      <OtaBalance dag={dag} />
    </div>
  );
}

function NextActions({
  status,
  show,
  activeBranch,
}: {
  status: ContextStatus;
  show: ContextShow;
  activeBranch: BranchInfo | null;
}) {
  const gitBranch = status.git_branch || activeBranch?.name || "(unknown git branch)";
  const contextBranch = status.current_branch;
  const contextSummary = status.branch_summaries.find(
    (b) => b.branch === contextBranch,
  );
  const needsGitGoal = !status.git_branch_goal;
  const needsContextPurpose = !contextSummary?.purpose;
  const staleBranches = status.stale_branch_count;
  type Action = {
    icon: IconName;
    title: string;
    detail: string;
    command: string;
  };

  const rawActions: Array<Action | null> = [
    needsGitGoal
      ? {
          icon: "flag",
          title: "Set git branch goal",
          detail: `Define the objective for ${gitBranch}.`,
          command: `h5i context init --goal "<goal>"`,
        }
      : null,
    needsContextPurpose
      ? {
          icon: "git-branch",
          title: "Create context branch",
          detail: "Start an exploration path with a purpose.",
          command: `h5i context branch <name> --purpose "<intent>"`,
        }
      : null,
    show.todo_items.length > 0
      ? {
          icon: "warning-sign",
          title: "Resolve open TODOs",
          detail: `${show.todo_items.length} context TODO${show.todo_items.length === 1 ? "" : "s"} need attention.`,
          command: `h5i context show`,
        }
      : null,
    {
      icon: "endorsed",
      title: "Record milestone",
      detail: "Promote the current outcome into durable context.",
      command: `h5i context commit "<milestone summary>"`,
    },
    {
      icon: "git-commit",
      title: "Commit with provenance",
      detail: "Tie the current context state to git history.",
      command: `h5i commit -m "<message>" --agent codex --prompt "<prompt>"`,
    },
    staleBranches > 0
      ? {
          icon: "time",
          title: "Review stale branches",
          detail: `${staleBranches} context branch${staleBranches === 1 ? "" : "es"} have no recent activity.`,
          command: `h5i context show --depth 2`,
        }
      : null,
  ];
  const actions = rawActions.filter((a): a is Action => a != null);

  return (
    <div className="ctx-actions">
      <div className="ctx-actions-head">
        <span className="ctx-section-title">Next actions</span>
        <span className="ctx-section-sub">
          {gitBranch} / {contextBranch}
        </span>
      </div>
      <div className="ctx-action-grid">
        {actions.map((action) => (
          <div className="ctx-action" key={action.title}>
            <Icon icon={action.icon} size={14} />
            <div className="ctx-action-main">
              <div className="ctx-action-title">{action.title}</div>
              <div className="ctx-action-detail">{action.detail}</div>
              <code>{action.command}</code>
            </div>
            <CopyButton text={action.command} label="Copy" />
          </div>
        ))}
      </div>
    </div>
  );
}

function CopyButton({ text, label }: { text: string; label: string }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard?.writeText(text).then(
      () => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1200);
      },
      () => undefined,
    );
  };
  return (
    <Button
      minimal
      small
      icon={copied ? "tick" : "clipboard"}
      onClick={copy}
      title={text}
    >
      {copied ? "Copied" : label}
    </Button>
  );
}

function escapeCommandText(s: string): string {
  return s.replace(/["\\]/g, "\\$&");
}

// ── Branches table — git + context joined, sortable, active-pinned ─────────

type BranchSortKey =
  | "branch"
  | "activity"
  | "ahead"
  | "ai"
  | "milestones"
  | "trace"
  | "todos";

function BranchesTable({ branches }: { branches: BranchInfo[] }) {
  const [sortKey, setSortKey] = useState<BranchSortKey>("activity");
  const [sortDir, setSortDir] = useState<"asc" | "desc">("desc");

  // Local branches only — remote tracking refs duplicate the data and clutter
  // the unified view. The branch picker still shows them when explicitly
  // browsing.
  const local = useMemo(() => branches.filter((b) => !b.is_remote), [branches]);

  const maxMilestones = Math.max(
    1,
    ...local.map((b) => b.context?.milestone_count ?? 0),
  );
  const maxTrace = Math.max(
    1,
    ...local.map((b) => b.context?.trace_lines ?? 0),
  );
  const maxWalked = Math.max(1, ...local.map((b) => b.walked_commit_count ?? 0));

  // Active branch (HEAD) is always pinned to the top regardless of sort.
  const sorted = useMemo(() => {
    const active = local.find((b) => b.is_head);
    const others = local.filter((b) => !b.is_head);
    const dir = sortDir === "asc" ? 1 : -1;
    others.sort((a, b) => {
      switch (sortKey) {
        case "branch":
          return dir * a.name.localeCompare(b.name);
        case "activity": {
          const ta = a.context?.last_activity ?? a.last_commit?.timestamp ?? "";
          const tb = b.context?.last_activity ?? b.last_commit?.timestamp ?? "";
          return dir * ta.localeCompare(tb);
        }
        case "ahead":
          return dir * ((a.ahead ?? 0) - (b.ahead ?? 0));
        case "ai":
          return dir * ((a.ai_commit_count ?? 0) - (b.ai_commit_count ?? 0));
        case "milestones":
          return (
            dir *
            ((a.context?.milestone_count ?? 0) -
              (b.context?.milestone_count ?? 0))
          );
        case "trace":
          return (
            dir * ((a.context?.trace_lines ?? 0) - (b.context?.trace_lines ?? 0))
          );
        case "todos":
          return (
            dir * ((a.context?.todo_count ?? 0) - (b.context?.todo_count ?? 0))
          );
      }
    });
    return active ? [active, ...others] : others;
  }, [local, sortKey, sortDir]);

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
          <th {...headerProps("ahead", 100)}>Ahead/Behind</th>
          <th {...headerProps("ai", 110)}>AI commits</th>
          <th {...headerProps("milestones", 130)}>Milestones</th>
          <th {...headerProps("trace", 130)}>Trace</th>
          <th {...headerProps("todos", 70)}>TODOs</th>
        </tr>
      </thead>
      <tbody>
        {sorted.map((b) => {
          const ctxInfo = b.context;
          const lastActivityStr =
            ctxInfo?.last_activity ?? formatIsoToUtc(b.last_commit?.timestamp);
          const fresh = freshnessClass(parseUtcStamp(lastActivityStr));
          const purpose = ctxInfo?.purpose ?? "";
          const lastCommitMsg = b.last_commit?.message?.split("\n")[0] ?? "";
          return (
            <tr
              key={b.name}
              className={b.is_head ? "active" : ""}
              title={
                b.last_commit
                  ? `Tip: ${b.last_commit.short_oid} — ${lastCommitMsg}`
                  : undefined
              }
            >
              <td className="ctx-fresh-cell">
                <span
                  className={"ctx-fresh-dot " + fresh}
                  title={`Last activity: ${lastActivityStr || "—"}`}
                />
              </td>
              <td>
                <Tag minimal style={{ fontFamily: "monospace" }}>
                  {b.name}
                </Tag>
                {b.is_head ? (
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
                {!b.has_context_branch ? (
                  <Tag
                    minimal
                    style={{
                      marginLeft: 6,
                      fontSize: 10,
                      letterSpacing: "0.06em",
                      textTransform: "uppercase",
                      color: "var(--bp-text-dim)",
                    }}
                    title="No matching context branch — run `h5i context branch <name>` to create one"
                  >
                    no ctx
                  </Tag>
                ) : null}
              </td>
              <td className="ctx-cell-purpose" title={purpose || lastCommitMsg}>
                {purpose ? (
                  purpose
                ) : (
                  <span style={{ color: "var(--bp-text-dim)" }}>
                    {lastCommitMsg || "—"}
                  </span>
                )}
              </td>
              <td className="ctx-cell-activity">
                <span style={{ color: "var(--bp-text-muted)", fontSize: 11 }}>
                  {lastActivityStr || "—"}
                </span>
              </td>
              <td className="mono">
                <AheadBehindCell ahead={b.ahead} behind={b.behind} />
              </td>
              <td>
                {b.walked_commit_count != null ? (
                  <SparkCell
                    value={b.ai_commit_count ?? 0}
                    max={maxWalked}
                    color="var(--bp-violet)"
                  />
                ) : (
                  <span style={{ color: "var(--bp-text-dim)" }}>—</span>
                )}
              </td>
              <td>
                {ctxInfo ? (
                  <SparkCell
                    value={ctxInfo.milestone_count}
                    max={maxMilestones}
                    color="var(--bp-violet)"
                  />
                ) : (
                  <span style={{ color: "var(--bp-text-dim)" }}>—</span>
                )}
              </td>
              <td>
                {ctxInfo ? (
                  <SparkCell
                    value={ctxInfo.trace_lines}
                    max={maxTrace}
                    color="var(--bp-blue)"
                  />
                ) : (
                  <span style={{ color: "var(--bp-text-dim)" }}>—</span>
                )}
              </td>
              <td className="mono">
                {ctxInfo == null ? (
                  <span style={{ color: "var(--bp-text-dim)" }}>—</span>
                ) : ctxInfo.todo_count > 0 ? (
                  <span style={{ color: "var(--bp-orange)", fontWeight: 600 }}>
                    {ctxInfo.todo_count}
                  </span>
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

// ── Snapshot history table — time-delta + click-to-expand-diff ────────────

function SnapshotsTable({ snapshots }: { snapshots: ContextSnapshotItem[] }) {
  // API returns oldest-first; reverse so newest is on top.
  const items = useMemo(() => [...snapshots].reverse(), [snapshots]);
  const [expandedOid, setExpandedOid] = useState<string | null>(null);

  const toggleExpand = (oid: string) => {
    setExpandedOid((prev) => (prev === oid ? null : oid));
  };

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
          <th style={{ width: 28 }} />
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
          const isExpanded = expandedOid === s.context_oid;
          const canDiff = next != null;
          return (
            <React.Fragment key={s.context_oid}>
              <tr
                className={
                  (i === 0 ? "active " : "") +
                  (isExpanded ? "expanded " : "") +
                  (canDiff ? "expandable" : "")
                }
                onClick={() => canDiff && toggleExpand(s.context_oid)}
              >
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
                <td className="ctx-expand-cell">
                  {canDiff ? (
                    <Icon
                      icon={isExpanded ? "chevron-up" : "chevron-down"}
                      size={11}
                      color="var(--bp-text-dim)"
                    />
                  ) : null}
                </td>
              </tr>
              {isExpanded && next ? (
                <tr className="ctx-snapshot-diff-row">
                  <td colSpan={7} style={{ padding: 0 }}>
                    <SnapshotDiff fromSha={next.sha} toSha={s.sha} />
                  </td>
                </tr>
              ) : null}
            </React.Fragment>
          );
        })}
      </tbody>
    </HTMLTable>
  );
}

// Loads /api/context/diff for a single (older → newer) snapshot pair and
// shows the milestones + trace deltas. Cached implicitly per-mount; remounts
// when the user collapses & re-expands.
function SnapshotDiff({ fromSha, toSha }: { fromSha: string; toSha: string }) {
  const [diff, setDiff] = useState<ContextDiff | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setDiff(null);
    setError(null);
    api
      .contextDiff(fromSha, toSha)
      .then(setDiff)
      .catch((e) => setError(String(e)));
  }, [fromSha, toSha]);

  if (error) {
    return (
      <div className="ctx-snapshot-diff" style={{ color: "var(--bp-red)" }}>
        Failed to load diff: {error}
      </div>
    );
  }
  if (!diff) {
    return (
      <div className="ctx-snapshot-diff">
        <Spinner size={14} /> Loading diff…
      </div>
    );
  }

  const empty =
    !diff.goal_changed &&
    diff.added_milestones.length === 0 &&
    diff.removed_milestones.length === 0 &&
    diff.added_trace_lines.length === 0 &&
    diff.removed_trace_lines.length === 0;

  return (
    <div className="ctx-snapshot-diff">
      <div className="ctx-diff-meta">
        <span className="ctx-diff-arrow">
          <span className="wb-oid">{diff.from.slice(0, 7)}</span>
          {" → "}
          <span className="wb-oid">{diff.to.slice(0, 7)}</span>
        </span>
        {diff.cross_branch ? (
          <Tag minimal intent="warning" style={{ marginLeft: 8, fontSize: 10 }}>
            cross-branch · {diff.from_branch} → {diff.to_branch}
          </Tag>
        ) : null}
      </div>

      {empty ? (
        <div className="ctx-diff-empty">No context changes between these snapshots.</div>
      ) : null}

      {diff.goal_changed ? (
        <div className="ctx-diff-section">
          <div className="ctx-diff-label">Goal change</div>
          <div className="ctx-diff-goal">
            <div className="ctx-diff-goal-from">
              <span className="ctx-diff-marker minus">−</span>
              {diff.from_goal || "(empty)"}
            </div>
            <div className="ctx-diff-goal-to">
              <span className="ctx-diff-marker plus">+</span>
              {diff.to_goal || "(empty)"}
            </div>
          </div>
        </div>
      ) : null}

      {diff.added_milestones.length > 0 ? (
        <div className="ctx-diff-section">
          <div className="ctx-diff-label">
            Milestones added{" "}
            <span className="ctx-diff-count">{diff.added_milestones.length}</span>
          </div>
          <ul className="ctx-diff-list">
            {diff.added_milestones.map((m, i) => (
              <li key={i} className="ctx-diff-add">
                <span className="ctx-diff-marker plus">+</span>
                {stripCheckmark(m)}
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {diff.removed_milestones.length > 0 ? (
        <div className="ctx-diff-section">
          <div className="ctx-diff-label">
            Milestones removed{" "}
            <span className="ctx-diff-count">{diff.removed_milestones.length}</span>
          </div>
          <ul className="ctx-diff-list">
            {diff.removed_milestones.map((m, i) => (
              <li key={i} className="ctx-diff-remove">
                <span className="ctx-diff-marker minus">−</span>
                {stripCheckmark(m)}
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {diff.added_trace_lines.length > 0 || diff.removed_trace_lines.length > 0 ? (
        <div className="ctx-diff-section">
          <div className="ctx-diff-label">
            Trace delta{" "}
            <span className="ctx-diff-count">
              +{diff.added_trace_lines.length} / −{diff.removed_trace_lines.length}
            </span>
          </div>
          <ul className="ctx-diff-list ctx-diff-trace">
            {diff.added_trace_lines.slice(0, 8).map((t, i) => (
              <li key={`a${i}`} className="ctx-diff-add">
                <span className="ctx-diff-marker plus">+</span>
                {t}
              </li>
            ))}
            {diff.removed_trace_lines.slice(0, 8).map((t, i) => (
              <li key={`r${i}`} className="ctx-diff-remove">
                <span className="ctx-diff-marker minus">−</span>
                {t}
              </li>
            ))}
            {diff.added_trace_lines.length + diff.removed_trace_lines.length > 16 ? (
              <li
                style={{
                  color: "var(--bp-text-dim)",
                  fontStyle: "italic",
                  fontSize: 11,
                }}
              >
                + more trace lines elided
              </li>
            ) : null}
          </ul>
        </div>
      ) : null}
    </div>
  );
}

function stripCheckmark(s: string): string {
  return s.replace(/^\[.\]\s*/, "");
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

// Convert RFC-3339 timestamp from /api/branches.last_commit.timestamp into the
// "YYYY-MM-DD HH:MM UTC" shape that parseUtcStamp / freshnessClass expect.
function formatIsoToUtc(iso: string | null | undefined): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (isNaN(d.getTime())) return "";
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())} UTC`;
}

// Ahead/behind with arrows, dim when both zero, hidden when no upstream.
function AheadBehindCell({
  ahead,
  behind,
}: {
  ahead: number | null;
  behind: number | null;
}) {
  if (ahead == null && behind == null) {
    return <span style={{ color: "var(--bp-text-dim)" }}>—</span>;
  }
  const a = ahead ?? 0;
  const b = behind ?? 0;
  if (a === 0 && b === 0) {
    return <span style={{ color: "var(--bp-text-dim)" }}>even</span>;
  }
  return (
    <span style={{ display: "inline-flex", gap: 6, alignItems: "baseline" }}>
      {a > 0 ? (
        <span style={{ color: "var(--bp-green-hi)" }}>↑{a}</span>
      ) : null}
      {b > 0 ? (
        <span style={{ color: "var(--bp-orange)" }}>↓{b}</span>
      ) : null}
    </span>
  );
}

function Hero({
  status,
  show,
  promotion,
  activeBranch,
}: {
  status: ContextStatus;
  show: ContextShow;
  promotion: ContextPromotion;
  activeBranch: BranchInfo | null;
}) {
  const gitBranch = status.git_branch || activeBranch?.name || "(unknown git branch)";
  const gitGoal = status.git_branch_goal || show.git_branch_goal;
  const projectGoal = show.project_goal;
  const contextBranch = status.current_branch;
  const contextPurpose =
    status.branch_summaries.find((b) => b.branch === contextBranch)?.purpose ??
    promotion.purpose;

  return (
    <div className="ctx-hero">
      <div className="ctx-hero-goal">
        <div className="ctx-eyebrow">
          Git branch goal · {gitBranch}
        </div>
        <div className="ctx-hero-text">
          {gitGoal || projectGoal || "(no goal recorded for this git branch)"}
        </div>
        <div className="ctx-hero-purpose">
          <span style={{ color: "var(--bp-text-dim)", fontSize: 11, marginRight: 6, textTransform: "uppercase", letterSpacing: "0.08em", fontWeight: 600 }}>
            Context
          </span>
          <Tag minimal style={{ fontFamily: "monospace", marginRight: 8 }}>
            {contextBranch}
          </Tag>
          {contextPurpose || "(no purpose recorded for this context branch)"}
        </div>
        {!gitGoal ? (
          <div className="ctx-hero-cta">
            <Icon icon="info-sign" size={11} style={{ marginRight: 4 }} />
            No goal yet for git branch <code>{gitBranch}</code>. Set one with{" "}
            <code>h5i context init --goal "&lt;goal&gt;"</code>.
          </div>
        ) : null}
        {!contextPurpose ? (
          <div className="ctx-hero-cta">
            <Icon icon="info-sign" size={11} style={{ marginRight: 4 }} />
            No purpose yet for h5i context branch <code>{contextBranch}</code>. Create or switch with{" "}
            <code>h5i context branch &lt;name&gt; --purpose "&lt;intent&gt;"</code>.
          </div>
        ) : null}
      </div>
      <div className="ctx-kpis">
        <Kpi
          value={show.milestones.length}
          label="milestones"
          sub={
            status.branch_summaries.length > 1
              ? `${status.branch_summaries.length} branches`
              : undefined
          }
        />
        <Kpi
          value={status.trace_lines}
          label="trace lines"
          sub={`${status.stable_line_count} stable · ${status.dynamic_line_count} live`}
        />
        <Kpi
          value={status.snapshot_count}
          label="snapshots"
          sub={
            status.latest_snapshot_timestamp
              ? `last ${formatRelative(parseUtcStamp(status.latest_snapshot_timestamp))}`
              : undefined
          }
        />
        <Kpi
          value={show.todo_items.length}
          label="todos"
          intent={show.todo_items.length > 0 ? "warning" : undefined}
          sub={
            show.todo_items.length > 0
              ? `${status.branch_summaries.filter((b) => b.todo_count > 0).length} branches`
              : undefined
          }
        />
        <Kpi
          value={status.branch_count}
          label="branches"
          sub={
            status.stale_branch_count > 0
              ? `${status.stale_branch_count} stale`
              : undefined
          }
        />
      </div>
    </div>
  );
}

// "3h ago", "2d ago", "just now" — used under the snapshots KPI.
function formatRelative(d: Date | null): string {
  if (!d) return "—";
  const ms = Date.now() - d.getTime();
  if (ms < 60 * 1000) return "just now";
  if (ms < 60 * 60 * 1000) return `${Math.round(ms / 60_000)}m ago`;
  if (ms < 24 * 60 * 60 * 1000) return `${Math.round(ms / 3_600_000)}h ago`;
  return `${Math.round(ms / 86_400_000)}d ago`;
}

function Kpi({
  value,
  label,
  intent,
  sub,
}: {
  value: number | string;
  label: string;
  intent?: "warning";
  /** Optional secondary line — tiny, dimmer; e.g. "+2 today", "1 active". */
  sub?: string;
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
      {sub ? <div className="ctx-kpi-sub">{sub}</div> : null}
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
        {segments.map((s) => {
          const pct = total > 0 ? (s.count / total) * 100 : 0;
          return (
            <span key={s.label} className="ctx-ota-legend-item">
              <span
                className="ctx-ota-swatch"
                style={{ background: s.color }}
              />
              <span className="ctx-ota-legend-label">{s.label}</span>
              <span className="ctx-ota-legend-val">{s.count}</span>
              <span className="ctx-ota-legend-pct">
                {pct >= 0.5 ? `${pct.toFixed(0)}%` : "—"}
              </span>
            </span>
          );
        })}
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
