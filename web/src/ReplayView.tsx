import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Button,
  ButtonGroup,
  Code,
  NonIdealState,
  Spinner,
  Tag,
} from "@blueprintjs/core";

import {
  api,
  type EnvFleetItem,
  type Commit,
  type FileHeat,
  type ReplayEvent,
  type ReplayView as ReplayData,
} from "./api";

// ─────────────────────────────────────────────────────────────────────────────
// Replay — the flight recorder. "Review the run, not just the diff."
//
// Three panes: a chronological TIMELINE of what the agent did, a center
// WORKSPACE HEATMAP of which files were read / edited / tested / blocked, and a
// right EVIDENCE drawer for the selected event. The loudest element by design
// is a BLOCKED access — the proof of what the agent could not reach.
// ─────────────────────────────────────────────────────────────────────────────

type RunRef =
  | { kind: "env"; id: string; agent: string; slug: string; label: string; sub: string }
  | { kind: "commit"; id: string; label: string; sub: string };

const KIND_GLYPH: Record<string, string> = {
  PROMPT: "✦",
  THINK: "◇",
  READ: "○",
  RUN: "❯",
  TEST_PASS: "✓",
  TEST_FAIL: "✕",
  BLOCKED: "⛔",
  EDIT: "✎",
  NOTE: "!",
  DIFF: "±",
  CREATE: "+",
  PROPOSE: "▲",
  APPLY: "✓",
  ABORT: "×",
  MSG: "✉",
  EVENT: "·",
};

const LANE_LABEL: Record<string, string> = {
  intent: "INTENT",
  fs: "FS",
  net: "NET",
  proc: "PROC",
  test: "TEST",
  provenance: "PROV",
  lifecycle: "LIFE",
  msg: "MSG",
};

export function ReplayView({
  focusOid,
  branch,
}: {
  focusOid?: string | null;
  branch?: string | null;
}) {
  const [envs, setEnvs] = useState<EnvFleetItem[] | null>(null);
  const [commits, setCommits] = useState<Commit[] | null>(null);
  const [selected, setSelected] = useState<RunRef | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.envs().then(setEnvs).catch(() => setEnvs([]));
  }, []);

  // Commit-fallback runs are scoped to the picked branch's own commits
  // (base..branch), so the run list isn't flooded by the whole history.
  useEffect(() => {
    setCommits(null);
    api
      .commits({ limit: 60, branch, branchOnly: !!branch })
      .then(setCommits)
      .catch((e) => setError(String(e)));
  }, [branch]);

  // Deep-link: when a focus commit is requested (e.g. from the cockpit), select
  // it as a commit-anchored run.
  useEffect(() => {
    if (!focusOid) return;
    const c = commits?.find((x) => x.git_oid === focusOid);
    setSelected({
      kind: "commit",
      id: focusOid,
      label: c?.message.split("\n")[0] ?? focusOid.slice(0, 12),
      sub: c ? `${c.short_oid.slice(0, 7)} · ${c.ai_agent ?? "ai"}` : "commit",
    });
  }, [focusOid, commits]);

  const runs = useMemo<RunRef[]>(() => {
    const envRuns: RunRef[] =
      envs?.map((e) => ({
        kind: "env" as const,
        id: e.id,
        agent: e.agent,
        slug: e.slug,
        label: `${e.agent}/${e.slug}`,
        sub: `${e.isolation} · ${e.captures} cap${e.captures === 1 ? "" : "s"}${
          e.risk.score > 0 ? ` · pressure ${e.risk.score}` : ""
        }`,
      })) ?? [];
    // Prefer AI commits as fallback runs; cap the list.
    const commitRuns: RunRef[] =
      commits
        ?.filter((c) => c.ai_model || c.ai_agent)
        .slice(0, 30)
        .map((c) => ({
          kind: "commit" as const,
          id: c.git_oid,
          label: c.message.split("\n")[0],
          sub: `${c.short_oid.slice(0, 7)} · ${c.ai_agent ?? "ai"}`,
        })) ?? [];
    return [...envRuns, ...commitRuns];
  }, [envs, commits]);

  // Auto-select the most pressing run once data lands. Keep the current pick
  // when it's still valid (or is the focused commit), else fall to the top —
  // this also re-picks correctly when the branch filter changes the run list.
  useEffect(() => {
    if (runs.length === 0) return;
    setSelected((cur) => {
      if (cur && (cur.id === focusOid || runs.some((r) => r.id === cur.id))) return cur;
      return runs[0];
    });
  }, [runs, focusOid]);

  if (error && !commits) {
    return (
      <div className="wb-body wb-body-single">
        <div className="wb-pane">
          <NonIdealState icon="error" title="Failed to load runs" description={error} />
        </div>
      </div>
    );
  }

  return (
    <div className="rpl-shell">
      <RunBar runs={runs} selected={selected} onSelect={setSelected} loading={!envs || !commits} />
      {selected ? (
        <ReplayBoard run={selected} />
      ) : runs.length === 0 ? (
        <div className="rpl-empty">
          <NonIdealState
            icon="play"
            title="No runs to replay yet"
            description={
              <span>
                Run an agent in a sandbox with <Code>h5i env run &lt;name&gt; -- …</Code>, or make an
                AI-assisted commit. Replays appear here automatically.
              </span>
            }
          />
        </div>
      ) : (
        <div className="rpl-empty">
          <Spinner size={24} />
        </div>
      )}
    </div>
  );
}

// ── Run selector + trust strip ───────────────────────────────────────────────

function RunBar({
  runs,
  selected,
  onSelect,
  loading,
}: {
  runs: RunRef[];
  selected: RunRef | null;
  onSelect: (r: RunRef) => void;
  loading: boolean;
}) {
  return (
    <div className="rpl-runbar">
      <div className="rpl-runbar-label">
        Replay
        {loading ? <Spinner size={12} /> : null}
      </div>
      <div className="rpl-runbar-scroll">
        {runs.map((r) => (
          <button
            key={`${r.kind}:${r.id}`}
            className={
              "rpl-run-chip" +
              (selected && selected.id === r.id ? " active" : "") +
              (r.kind === "env" ? " env" : " commit")
            }
            onClick={() => onSelect(r)}
            title={r.label}
          >
            <span className="rpl-run-kind">{r.kind === "env" ? "▣" : "◆"}</span>
            <span className="rpl-run-main">
              <span className="rpl-run-name">{r.label}</span>
              <span className="rpl-run-sub">{r.sub}</span>
            </span>
          </button>
        ))}
      </div>
    </div>
  );
}

// ── The three-pane board ─────────────────────────────────────────────────────

function ReplayBoard({ run }: { run: RunRef }) {
  const [data, setData] = useState<ReplayData | null>(null);
  const [loading, setLoading] = useState(true);
  const [err, setErr] = useState<string | null>(null);
  const [selectedSeq, setSelectedSeq] = useState<number | null>(null);
  const [fileFilter, setFileFilter] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setLoading(true);
    setErr(null);
    setSelectedSeq(null);
    setFileFilter(null);
    const p =
      run.kind === "env" ? api.envReplay(run.agent, run.slug) : api.commitReplay(run.id);
    p.then((d) => {
      if (live) setData(d);
    })
      .catch((e) => {
        if (live) setErr(String(e));
      })
      .finally(() => {
        if (live) setLoading(false);
      });
    return () => {
      live = false;
    };
  }, [run]);

  const selectedEvent = useMemo(
    () => data?.timeline.find((e) => e.seq === selectedSeq) ?? null,
    [data, selectedSeq],
  );

  // Files highlighted in the heatmap = those touched by the selected event.
  const highlightedFiles = useMemo(
    () => new Set(selectedEvent?.files ?? []),
    [selectedEvent],
  );

  if (loading && !data) {
    return (
      <div className="rpl-empty">
        <Spinner size={24} />
      </div>
    );
  }
  if (err || !data) {
    return (
      <div className="rpl-empty">
        <NonIdealState icon="error" title="Failed to load replay" description={err ?? ""} />
      </div>
    );
  }

  return (
    <>
      <TrustStrip h={data.header} />
      <div className="rpl-body">
        <TimelinePane
          events={data.timeline}
          selectedSeq={selectedSeq}
          onSelect={setSelectedSeq}
          fileFilter={fileFilter}
          onClearFilter={() => setFileFilter(null)}
        />
        <HeatmapPane
          heat={data.heatmap}
          highlighted={highlightedFiles}
          active={fileFilter}
          onPick={(f) => setFileFilter((cur) => (cur === f ? null : f))}
        />
        <EvidencePane event={selectedEvent} run={run} header={data.header} />
      </div>
    </>
  );
}

// ── Trust strip ──────────────────────────────────────────────────────────────

function TrustStrip({ h }: { h: ReplayData["header"] }) {
  const testsFail = h.tests_failed ?? 0;
  const testsPass = h.tests_passed ?? 0;
  const hasTests = h.tests_passed != null || h.tests_failed != null;
  return (
    <div className="rpl-trust">
      <div className="rpl-trust-id">
        <span className="rpl-trust-anchor">{h.anchor}</span>
        <span className="rpl-trust-title" title={h.title}>
          {h.title}
        </span>
        {h.isolation ? <Tag minimal intent={isoIntent(h.isolation)}>{h.isolation}</Tag> : null}
        {h.agent ? <span className="rpl-trust-agent">{h.agent}</span> : null}
        {h.model ? <span className="rpl-trust-model">{shortModel(h.model)}</span> : null}
      </div>
      <div className="rpl-trust-stats">
        <Stat
          label="blocked"
          value={h.blocked_count}
          intent={h.blocked_count > 0 ? "danger" : "none"}
          loud
        />
        <Stat label="net ok" value={h.allowed_count} intent="none" />
        {hasTests ? (
          <Stat
            label="tests"
            value={testsFail > 0 ? `${testsFail}✕ / ${testsPass}✓` : `${testsPass}✓`}
            intent={testsFail > 0 ? "danger" : "success"}
          />
        ) : null}
        {h.anchor === "env" ? (
          <Stat
            label="pressure"
            value={h.risk_score}
            intent={h.risk_score > 50 ? "danger" : h.risk_score > 0 ? "warning" : "success"}
          />
        ) : null}
        <Stat label="runs" value={h.run_count} intent="none" />
      </div>
    </div>
  );
}

function Stat({
  label,
  value,
  intent,
  loud,
}: {
  label: string;
  value: number | string;
  intent: "danger" | "warning" | "success" | "none" | "primary";
  loud?: boolean;
}) {
  const color =
    intent === "danger"
      ? "var(--bp-red)"
      : intent === "warning"
        ? "var(--bp-orange)"
        : intent === "success"
          ? "var(--bp-green-hi)"
          : "var(--bp-text)";
  return (
    <span className={"rpl-stat" + (loud && intent === "danger" ? " loud" : "")}>
      <span className="rpl-stat-val" style={{ color }}>
        {value}
      </span>
      <span className="rpl-stat-label">{label}</span>
    </span>
  );
}

// ── Timeline pane (left) ─────────────────────────────────────────────────────

function TimelinePane({
  events,
  selectedSeq,
  onSelect,
  fileFilter,
  onClearFilter,
}: {
  events: ReplayEvent[];
  selectedSeq: number | null;
  onSelect: (seq: number) => void;
  fileFilter: string | null;
  onClearFilter: () => void;
}) {
  const [laneFilter, setLaneFilter] = useState<"all" | "blocked">("all");

  const shown = useMemo(() => {
    let list = events;
    if (laneFilter === "blocked") list = list.filter((e) => e.kind === "BLOCKED");
    if (fileFilter) list = list.filter((e) => (e.files ?? []).includes(fileFilter));
    return list;
  }, [events, laneFilter, fileFilter]);

  const blockedCount = events.filter((e) => e.kind === "BLOCKED").length;

  return (
    <div className="rpl-pane rpl-timeline">
      <div className="rpl-pane-head">
        <span>Timeline</span>
        <Tag minimal round>
          {shown.length}
        </Tag>
        <ButtonGroup minimal className="rpl-tl-filters">
          <Button
            small
            active={laneFilter === "all"}
            onClick={() => setLaneFilter("all")}
            text="all"
          />
          <Button
            small
            active={laneFilter === "blocked"}
            onClick={() => setLaneFilter("blocked")}
            text={`blocked ${blockedCount}`}
            intent={blockedCount > 0 ? "danger" : "none"}
            disabled={blockedCount === 0}
          />
        </ButtonGroup>
      </div>
      {fileFilter ? (
        <div className="rpl-tl-activefilter">
          <span>filtered to</span> <Code>{fileFilter}</Code>
          <Button minimal small icon="cross" onClick={onClearFilter} />
        </div>
      ) : null}
      <div className="rpl-pane-body">
        {shown.length === 0 ? (
          <div className="rpl-empty-hint">No events match.</div>
        ) : (
          <ol className="rpl-events">
            {shown.map((e) => (
              <TimelineRow
                key={e.seq}
                e={e}
                selected={e.seq === selectedSeq}
                onSelect={() => onSelect(e.seq)}
              />
            ))}
          </ol>
        )}
      </div>
    </div>
  );
}

function TimelineRow({
  e,
  selected,
  onSelect,
}: {
  e: ReplayEvent;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <li
      className={
        "rpl-ev sev-" + e.severity + (selected ? " selected" : "") + (e.kind === "BLOCKED" ? " blocked" : "")
      }
      onClick={onSelect}
    >
      <span className="rpl-ev-rail">
        <span className={"rpl-ev-glyph kind-" + e.kind}>{KIND_GLYPH[e.kind] ?? "·"}</span>
      </span>
      <span className="rpl-ev-main">
        <span className="rpl-ev-head">
          <span className={"rpl-ev-kind kind-" + e.kind}>{e.kind}</span>
          <span className={"rpl-ev-lane lane-" + e.lane}>{LANE_LABEL[e.lane] ?? e.lane}</span>
          {e.exit_code != null && e.exit_code !== 0 ? (
            <span className="rpl-ev-exit">exit {e.exit_code}</span>
          ) : null}
          <span className="rpl-ev-ts">{shortTime(e.ts)}</span>
        </span>
        <span className="rpl-ev-title">{e.title}</span>
      </span>
    </li>
  );
}

// ── Workspace heatmap (center) ───────────────────────────────────────────────

interface TreeNode {
  name: string;
  path: string;
  children: Map<string, TreeNode>;
  heat?: FileHeat;
}

function buildTree(heat: FileHeat[]): TreeNode {
  const root: TreeNode = { name: "", path: "", children: new Map() };
  for (const h of heat) {
    const parts = h.path.split("/");
    let cur = root;
    let acc = "";
    parts.forEach((part, i) => {
      acc = acc ? `${acc}/${part}` : part;
      let child = cur.children.get(part);
      if (!child) {
        child = { name: part, path: acc, children: new Map() };
        cur.children.set(part, child);
      }
      if (i === parts.length - 1) child.heat = h;
      cur = child;
    });
  }
  return root;
}

function HeatmapPane({
  heat,
  highlighted,
  active,
  onPick,
}: {
  heat: FileHeat[];
  highlighted: Set<string>;
  active: string | null;
  onPick: (f: string) => void;
}) {
  const tree = useMemo(() => buildTree(heat), [heat]);
  const counts = useMemo(() => {
    const c = { read: 0, edited: 0, tested: 0, blocked: 0, risky: 0 };
    for (const h of heat) {
      if (h.read) c.read++;
      if (h.edited) c.edited++;
      if (h.tested) c.tested++;
      if (h.blocked) c.blocked++;
      if (h.risky) c.risky++;
    }
    return c;
  }, [heat]);

  return (
    <div className="rpl-pane rpl-heatmap">
      <div className="rpl-pane-head">
        <span>Workspace map</span>
        <Tag minimal round>
          {heat.length}
        </Tag>
        <span className="rpl-heat-legend">
          <Lg cls="edited" n={counts.edited} label="edited" />
          <Lg cls="read" n={counts.read} label="read" />
          <Lg cls="tested" n={counts.tested} label="tested" />
          {counts.risky > 0 ? <Lg cls="risky" n={counts.risky} label="risky" /> : null}
          {counts.blocked > 0 ? <Lg cls="blocked" n={counts.blocked} label="blocked" /> : null}
        </span>
      </div>
      <div className="rpl-pane-body">
        {heat.length === 0 ? (
          <div className="rpl-empty-hint">No files touched in this run.</div>
        ) : (
          <div className="rpl-tree">
            {[...tree.children.values()].map((n) => (
              <TreeRow
                key={n.path}
                node={n}
                depth={0}
                highlighted={highlighted}
                active={active}
                onPick={onPick}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function Lg({ cls, n, label }: { cls: string; n: number; label: string }) {
  return (
    <span className="rpl-lg" title={`${n} ${label}`}>
      <span className={"rpl-lg-dot heat-" + cls} />
      {n}
    </span>
  );
}

function TreeRow({
  node,
  depth,
  highlighted,
  active,
  onPick,
}: {
  node: TreeNode;
  depth: number;
  highlighted: Set<string>;
  active: string | null;
  onPick: (f: string) => void;
}) {
  const isFile = !!node.heat;
  const kids = [...node.children.values()];
  if (isFile) {
    const h = node.heat!;
    const cls = heatClass(h);
    return (
      <div
        className={
          "rpl-tree-row file " +
          cls +
          (highlighted.has(h.path) ? " hi" : "") +
          (active === h.path ? " active" : "")
        }
        style={{ paddingLeft: 8 + depth * 14 }}
        onClick={() => onPick(h.path)}
        title={statusText(h)}
      >
        <span className={"rpl-heat-swatch heat-" + cls} />
        <span className="rpl-tree-name">{node.name}</span>
        <span className="rpl-tree-badges">
          {h.blocked ? <b className="hb blocked">blocked</b> : null}
          {h.risky ? <b className="hb risky">risky</b> : null}
          {h.edited ? <b className="hb edited">edit</b> : null}
          {h.tested ? <b className="hb tested">test</b> : null}
          {h.read && !h.edited ? <b className="hb read">read</b> : null}
        </span>
      </div>
    );
  }
  return (
    <>
      <div className="rpl-tree-row dir" style={{ paddingLeft: 8 + depth * 14 }}>
        <span className="rpl-tree-name dir">{node.name}/</span>
      </div>
      {kids.map((k) => (
        <TreeRow
          key={k.path}
          node={k}
          depth={depth + 1}
          highlighted={highlighted}
          active={active}
          onPick={onPick}
        />
      ))}
    </>
  );
}

// ── Evidence drawer (right) ──────────────────────────────────────────────────

function EvidencePane({
  event,
  run,
  header,
}: {
  event: ReplayEvent | null;
  run: RunRef;
  header: ReplayData["header"];
}) {
  const [render, setRender] = useState<string | null>(null);
  const [loadingCap, setLoadingCap] = useState(false);

  const loadCapture = useCallback(
    (id: string) => {
      if (run.kind !== "env") return;
      setLoadingCap(true);
      setRender(null);
      api
        .envCapture(run.agent, run.slug, id)
        .then((r) => setRender(r.render))
        .catch(() => setRender(null))
        .finally(() => setLoadingCap(false));
    },
    [run],
  );

  useEffect(() => {
    setRender(null);
    if (event?.capture_id && run.kind === "env") loadCapture(event.capture_id);
  }, [event, run, loadCapture]);

  if (!event) {
    return (
      <div className="rpl-pane rpl-evidence">
        <div className="rpl-pane-head">Evidence</div>
        <div className="rpl-pane-body">
          <div className="rpl-evidence-intro">
            <p>Select a timeline event to see the evidence behind it.</p>
            {header.prompt ? (
              <div className="rpl-evidence-prompt">
                <div className="rpl-ev-label">Prompt</div>
                <div className="rpl-prompt-text">{header.prompt}</div>
              </div>
            ) : null}
            {header.policy_digest ? (
              <div className="rpl-kv">
                <span className="k">policy</span>
                <Code>{header.policy_digest.slice(0, 16)}</Code>
              </div>
            ) : null}
            {header.diffstat ? (
              <div className="rpl-evidence-diff">
                <div className="rpl-ev-label">Proposed diff</div>
                <pre className="rpl-pre">{header.diffstat}</pre>
              </div>
            ) : null}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="rpl-pane rpl-evidence">
      <div className="rpl-pane-head">
        Evidence
        <span className={"rpl-ev-kind kind-" + event.kind}>{event.kind}</span>
      </div>
      <div className="rpl-pane-body">
        <div className={"rpl-evidence-title sev-" + event.severity}>{event.title}</div>
        <div className="rpl-kv-grid">
          <span className="k">lane</span>
          <span className="v">{LANE_LABEL[event.lane] ?? event.lane}</span>
          <span className="k">when</span>
          <span className="v mono">{event.ts || "—"}</span>
          {event.exit_code != null ? (
            <>
              <span className="k">exit</span>
              <span className="v mono">{event.exit_code}</span>
            </>
          ) : null}
          {event.capture_id ? (
            <>
              <span className="k">capture</span>
              <span className="v mono">{event.capture_id}</span>
            </>
          ) : null}
        </div>

        {event.files && event.files.length > 0 ? (
          <div className="rpl-evidence-section">
            <div className="rpl-ev-label">Files ({event.files.length})</div>
            <ul className="rpl-file-list">
              {event.files.map((f) => (
                <li key={f}>{f}</li>
              ))}
            </ul>
          </div>
        ) : null}

        {event.detail ? (
          <div className="rpl-evidence-section">
            <div className="rpl-ev-label">Detail</div>
            <pre className="rpl-pre">{event.detail}</pre>
          </div>
        ) : null}

        {event.capture_id && run.kind === "env" ? (
          <div className="rpl-evidence-section">
            <div className="rpl-ev-label">Capture (raw evidence)</div>
            {loadingCap ? (
              <Spinner size={16} />
            ) : render ? (
              <pre className="rpl-pre rpl-pre-raw">{render}</pre>
            ) : (
              <div className="rpl-empty-hint">No capture render available.</div>
            )}
          </div>
        ) : null}
      </div>
    </div>
  );
}

// ── helpers ──────────────────────────────────────────────────────────────────

function heatClass(h: FileHeat): string {
  if (h.blocked) return "blocked";
  if (h.risky) return "risky";
  if (h.edited) return "edited";
  if (h.tested) return "tested";
  if (h.read) return "read";
  return "untouched";
}

function statusText(h: FileHeat): string {
  const s: string[] = [];
  if (h.read) s.push("read");
  if (h.edited) s.push("edited");
  if (h.tested) s.push("tested");
  if (h.blocked) s.push("blocked");
  if (h.risky) s.push("edited without reading first");
  return `${h.path} — ${s.join(", ") || "untouched"}`;
}

function isoIntent(iso: string): "success" | "primary" | "none" {
  return iso === "container" ? "success" : iso === "process" || iso === "supervised" ? "primary" : "none";
}

function shortModel(m: string): string {
  return m.replace(/^claude-/, "").replace(/-\d{8}$/, "");
}

function shortTime(ts: string): string {
  const m = ts.match(/T(\d{2}:\d{2}:\d{2})/);
  return m ? m[1] : "";
}
