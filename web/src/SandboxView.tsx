import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Callout,
  Code,
  HTMLTable,
  NonIdealState,
  Spinner,
  Tag,
} from "@blueprintjs/core";

import {
  api,
  type EnforcedPolicy,
  type EnvCaptureView,
  type EnvDetail,
  type EnvEventView,
  type EnvFleetItem,
  type ProbeResponse,
  type RiskFinding,
  type RiskLane,
  type RiskSeverity,
} from "./api";

// The Sandbox "flight recorder": a read-only operator console over h5i
// environments. It answers, at a glance: which envs ran, what isolation was
// actually enforced, what each agent tried, and what pressed on the boundary.
//
// Honesty is the design constraint (see src/risk.rs): red means enforcement
// *fired* (a denial), amber means a probing *shape* with no observed denial,
// grey means a weak-isolation capability gap — never an accusation.

const LANES: { key: RiskLane; label: string; hint: string }[] = [
  { key: "fs", label: "FS", hint: "filesystem reach" },
  { key: "net", label: "NET", hint: "network egress" },
  { key: "proc", label: "PROC", hint: "process / privilege" },
  { key: "resource", label: "RES", hint: "resource limits" },
  { key: "provenance", label: "PROV", hint: "integrity / provenance" },
];

const POLL_MS = 8000;

export function SandboxView() {
  const [envs, setEnvs] = useState<EnvFleetItem[] | null>(null);
  const [probe, setProbe] = useState<ProbeResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [filter, setFilter] = useState<string>("all");

  const load = useCallback(() => {
    api.envs().then(setEnvs).catch((e) => setError(String(e)));
    api.envProbe().then(setProbe).catch(() => setProbe(null));
  }, []);

  useEffect(() => {
    load();
    const t = setInterval(load, POLL_MS);
    return () => clearInterval(t);
  }, [load]);

  // Keep a selection valid as the fleet refreshes; default to the most pressing.
  useEffect(() => {
    if (!envs) return;
    setSelectedId((prev) => {
      if (prev && envs.some((e) => e.id === prev)) return prev;
      return envs[0]?.id ?? null;
    });
  }, [envs]);

  const filtered = useMemo(() => {
    if (!envs) return null;
    return envs.filter((e) => matchesFilter(e, filter));
  }, [envs, filter]);

  const selected = useMemo(
    () => envs?.find((e) => e.id === selectedId) ?? null,
    [envs, selectedId],
  );

  if (error) {
    return (
      <div className="wb-body wb-body-single">
        <div className="wb-pane">
          <NonIdealState icon="error" title="Failed to load environments" description={error} />
        </div>
      </div>
    );
  }

  return (
    <div className="sbx-shell">
      <TopStrip probe={probe} envs={envs} />
      <div className="sbx-body">
        <FleetPane
          envs={filtered}
          total={envs?.length ?? 0}
          filter={filter}
          onFilter={setFilter}
          selectedId={selectedId}
          onSelect={setSelectedId}
        />
        <DetailPane env={selected} />
      </div>
    </div>
  );
}

// ── top strip: host readiness + fleet vitals ─────────────────────────────────

function TopStrip({ probe, envs }: { probe: ProbeResponse | null; envs: EnvFleetItem[] | null }) {
  const active = envs?.filter((e) => e.status === "running" || e.status === "idle" || e.status === "created").length ?? 0;
  const proposed = envs?.filter((e) => e.status === "proposed").length ?? 0;
  const trips = envs?.reduce((n, e) => n + (e.risk.last_denial_ts ? 1 : 0), 0) ?? 0;
  const captures = envs?.reduce((n, e) => n + e.captures, 0) ?? 0;

  return (
    <div className="sbx-strip">
      <div className="sbx-strip-group">
        <span className="sbx-strip-label">host</span>
        {probe ? (
          probe.tiers.map((t) => (
            <Tag
              key={t.claim}
              minimal
              intent={t.satisfiable ? "success" : "none"}
              title={t.note ?? undefined}
            >
              {t.claim} {t.satisfiable ? "✓" : "✗"}
            </Tag>
          ))
        ) : (
          <Tag minimal>probing…</Tag>
        )}
        {probe && !probe.process_runnable ? (
          <Tag minimal intent="warning" title={probe.process_runnable_detail ?? undefined}>
            process tier not runnable
          </Tag>
        ) : null}
      </div>
      <div className="sbx-strip-vitals">
        <Vital label="active" value={active} />
        <Vital label="proposed" value={proposed} intent={proposed > 0 ? "primary" : undefined} />
        <Vital label="boundary trips" value={trips} intent={trips > 0 ? "danger" : undefined} />
        <Vital label="captures" value={captures} />
      </div>
    </div>
  );
}

function Vital({ label, value, intent }: { label: string; value: number; intent?: "primary" | "danger" }) {
  const color =
    intent === "danger" ? "var(--bp-red)" : intent === "primary" ? "var(--bp-blue-hi)" : "var(--bp-text)";
  return (
    <span className="sbx-vital">
      <span className="sbx-vital-num" style={{ color }}>{value}</span>
      <span className="sbx-vital-label">{label}</span>
    </span>
  );
}

// ── left: fleet table ─────────────────────────────────────────────────────────

const FILTERS = ["all", "running", "proposed", "pressure", "drifted", "container", "process", "workspace"];

function matchesFilter(e: EnvFleetItem, f: string): boolean {
  switch (f) {
    case "all": return true;
    case "running": return e.status === "running";
    case "proposed": return e.status === "proposed";
    case "pressure": return e.risk.score > 0;
    case "drifted": return e.drift !== "up-to-date";
    case "container": return e.isolation === "container";
    case "process": return e.isolation === "process";
    case "workspace": return e.isolation === "workspace";
    default: return true;
  }
}

function FleetPane(props: {
  envs: EnvFleetItem[] | null;
  total: number;
  filter: string;
  onFilter: (f: string) => void;
  selectedId: string | null;
  onSelect: (id: string) => void;
}) {
  const { envs, total, filter, onFilter, selectedId, onSelect } = props;
  return (
    <div className="sbx-fleet">
      <div className="wb-pane-header sbx-fleet-header">
        <span>Environments</span>
        <Tag minimal round>{total}</Tag>
      </div>
      <div className="sbx-filters">
        {FILTERS.map((f) => (
          <button
            key={f}
            className={"sbx-chip" + (filter === f ? " active" : "")}
            onClick={() => onFilter(f)}
          >
            {f}
          </button>
        ))}
      </div>
      <div className="sbx-fleet-body">
        {!envs ? (
          <NonIdealState icon={<Spinner size={20} />} title="Loading…" />
        ) : envs.length === 0 ? (
          <NonIdealState
            icon="shield"
            title="No environments"
            description={
              total === 0
                ? "Create one with `h5i env create <name>`."
                : "None match this filter."
            }
          />
        ) : (
          <HTMLTable className="sbx-fleet-table" interactive compact>
            <thead>
              <tr>
                <th>Env</th>
                <th style={{ width: 78 }}>Isolation</th>
                <th style={{ width: 64 }}>Status</th>
                <th style={{ width: 110 }}>Pressure</th>
              </tr>
            </thead>
            <tbody>
              {envs.map((e) => (
                <tr
                  key={e.id}
                  className={e.id === selectedId ? "selected" : ""}
                  onClick={() => onSelect(e.id)}
                >
                  <td>
                    <div className="sbx-env-id">{e.agent}/{e.slug}</div>
                    <div className="sbx-env-sub">
                      {e.captures} cap{e.captures === 1 ? "" : "s"}
                      {e.drift !== "up-to-date" ? (
                        <span className="sbx-drift" title={e.drift_summary}> · {e.drift}</span>
                      ) : null}
                      {!e.has_workspace ? <span className="sbx-env-sub-dim"> · pulled</span> : null}
                    </div>
                  </td>
                  <td><IsolationTag isolation={e.isolation} /></td>
                  <td><span className="sbx-status">{e.status}</span></td>
                  <td><PressureBadge score={e.risk.score} level={e.risk.level} /></td>
                </tr>
              ))}
            </tbody>
          </HTMLTable>
        )}
      </div>
    </div>
  );
}

function IsolationTag({ isolation }: { isolation: string }) {
  // workspace = weakest (grey), process = kernel-confined (blue), container =
  // strongest available here (green).
  const intent = isolation === "container" ? "success" : isolation === "process" ? "primary" : "none";
  return <Tag minimal intent={intent}>{isolation}</Tag>;
}

function PressureBadge({ score, level }: { score: number; level: RiskSeverity }) {
  if (score === 0) {
    return <span className="sbx-pressure clean">clean</span>;
  }
  return (
    <span className={"sbx-pressure " + level}>
      <span className="sbx-pressure-score">{score}</span>
      <span className="sbx-pressure-bar">
        <span className="sbx-pressure-fill" style={{ width: `${score}%` }} />
      </span>
    </span>
  );
}

// ── right: per-env detail (the flight recorder) ───────────────────────────────

function DetailPane({ env }: { env: EnvFleetItem | null }) {
  const [detail, setDetail] = useState<EnvDetail | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!env) {
      setDetail(null);
      return;
    }
    let live = true;
    setLoading(true);
    api
      .envDetail(env.agent, env.slug)
      .then((d) => { if (live) setDetail(d); })
      .catch(() => { if (live) setDetail(null); })
      .finally(() => { if (live) setLoading(false); });
    return () => { live = false; };
  }, [env]);

  if (!env) {
    return (
      <div className="sbx-detail">
        <div className="wb-pane-empty">Select an environment to inspect its boundary activity.</div>
      </div>
    );
  }

  return (
    <div className="sbx-detail">
      <div className="sbx-detail-head">
        <div>
          <span className="sbx-detail-title">{env.agent}/{env.slug}</span>
          <IsolationTag isolation={env.isolation} />
          <span className="sbx-detail-digest" title="enforced policy digest">
            {env.policy_digest.slice(0, 12)}
          </span>
        </div>
        <PressureBadge score={env.risk.score} level={env.risk.level} />
      </div>

      {loading && !detail ? (
        <NonIdealState icon={<Spinner size={20} />} title="Loading evidence…" />
      ) : !detail ? (
        <div className="wb-pane-empty">No detail available.</div>
      ) : (
        <div className="sbx-detail-body">
          <FindingsSummary findings={env.risk.findings} />
          <Timeline events={detail.events} captures={detail.captures} policy={detail.policy ?? null} findings={env.risk.findings} />
          <PolicyPanel policy={detail.policy ?? null} />
        </div>
      )}
    </div>
  );
}

function FindingsSummary({ findings }: { findings: RiskFinding[] }) {
  if (findings.length === 0) {
    return (
      <Callout intent="success" icon="tick-circle" className="sbx-callout">
        No boundary pressure detected across this environment's runs.
      </Callout>
    );
  }
  // Lead with the most severe.
  const ordered = [...findings].sort((a, b) => sevRank(b.severity) - sevRank(a.severity));
  return (
    <div className="sbx-findings">
      {ordered.map((f, i) => (
        <div key={i} className={"sbx-finding " + f.severity}>
          <span className={"sbx-lane-chip lane-" + f.lane}>{laneLabel(f.lane)}</span>
          <span className="sbx-finding-title">{f.title}</span>
          <span className="sbx-finding-detail">{f.detail}</span>
          {f.capture_id ? <Code className="sbx-finding-cap">{f.capture_id}</Code> : null}
        </div>
      ))}
    </div>
  );
}

// The five-lane timeline: one row per run (exec/violation event), each lane
// showing the verdict for that run derived from the deterministic findings.
function Timeline({
  events,
  captures,
  policy,
  findings,
}: {
  events: EnvEventView[];
  captures: EnvCaptureView[];
  policy: EnforcedPolicy | null;
  findings: RiskFinding[];
}) {
  const capById = useMemo(() => {
    const m = new Map<string, EnvCaptureView>();
    for (const c of captures) m.set(c.id, c);
    return m;
  }, [captures]);

  // Rows: exec + violation events, oldest→newest, that correspond to activity.
  const rows = events.filter((e) => e.event === "exec" || e.event === "violation");

  return (
    <div className="sbx-timeline">
      <div className="sbx-timeline-head">Flight recorder · {rows.length} run(s)</div>
      <div className="sbx-lane-grid">
        {/* policy allowance header */}
        <div className="sbx-lane-row sbx-lane-policy">
          <div className="sbx-run-cell sbx-run-policy">policy allows →</div>
          {LANES.map((l) => (
            <div key={l.key} className="sbx-lane-cell" title={l.hint}>
              <div className="sbx-lane-name">{l.label}</div>
              <div className="sbx-lane-allow">{laneAllowance(l.key, policy)}</div>
            </div>
          ))}
        </div>
        {rows.length === 0 ? (
          <div className="sbx-lane-empty">No runs yet — `h5i env run {`<name>`} -- …`</div>
        ) : (
          rows.map((ev, i) => {
            const cap = ev.capture ? capById.get(ev.capture) : undefined;
            const rowFindings = findings.filter((f) =>
              (ev.capture && f.capture_id === ev.capture) ||
              (ev.event === "violation" && f.kind === "commit-violation" && f.event_ts === ev.ts),
            );
            return (
              <div key={i} className="sbx-lane-row">
                <div className="sbx-run-cell" title={ev.detail ?? ""}>
                  <div className="sbx-run-cmd">{runLabel(ev, cap)}</div>
                  <div className="sbx-run-meta">
                    {cap?.exit_code != null ? `exit ${cap.exit_code}` : ev.event}
                    {" · "}{shortTime(ev.ts)}
                  </div>
                </div>
                {LANES.map((l) => (
                  <div key={l.key} className="sbx-lane-cell">
                    <LaneVerdict
                      lane={l.key}
                      findings={rowFindings.filter((f) => f.lane === l.key)}
                      egress={l.key === "net" ? cap?.egress ?? null : null}
                    />
                  </div>
                ))}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}

function LaneVerdict({
  lane,
  findings,
  egress,
}: {
  lane: RiskLane;
  findings: RiskFinding[];
  egress: import("./api").EgressSummary | null;
}) {
  // Net lane also folds in egress allow tallies even when no finding fired.
  if (findings.length === 0) {
    if (lane === "net" && egress && egress.allowed > 0 && egress.denied === 0) {
      return <span className="sbx-verdict ok" title={`${egress.allowed} egress request(s) allowed`}>✓</span>;
    }
    return <span className="sbx-verdict none">·</span>;
  }
  // Worst severity wins; weak-isolation renders grey.
  const worst = findings.reduce((acc, f) => (sevRank(f.severity) > sevRank(acc.severity) ? f : acc));
  const weak = findings.every((f) => f.kind === "weak-isolation");
  const cls = weak ? "weak" : worst.severity;
  const glyph = weak ? "◌" : worst.severity === "critical" ? "⛔" : "⚠";
  const label = weak ? "weak" : worst.severity === "critical" ? "blocked" : "pressure";
  const tip = findings.map((f) => `${f.title}: ${f.detail}`).join("\n");
  return (
    <span className={"sbx-verdict " + cls} title={tip}>
      {glyph} {label}
    </span>
  );
}

function PolicyPanel({ policy }: { policy: EnforcedPolicy | null }) {
  if (!policy) {
    return (
      <div className="sbx-policy">
        <div className="sbx-policy-head">Enforced policy</div>
        <div className="wb-pane-empty">policy.resolved.toml unavailable (pulled or gc'd env).</div>
      </div>
    );
  }
  const rows: [string, string][] = [
    ["isolation", policy.isolation],
    ["net.mode", policy.net_mode],
    ["net.egress", policy.net_egress.length ? policy.net_egress.join(", ") : "—"],
    ["fs.write", policy.fs_write.length ? policy.fs_write.join(", ") : "$WORK"],
    ["fs.read", policy.fs_read.length ? policy.fs_read.join(", ") : "—"],
    ["tools", policy.tools.length ? policy.tools.join(", ") : "(unrestricted)"],
    ["env.pass", policy.env_pass.length ? policy.env_pass.join(", ") : "—"],
    ["image", policy.image ?? "—"],
    ["wall", `${policy.wall_secs}s`],
    ["mem", policy.mem_bytes ? bytes(policy.mem_bytes) : "—"],
    ["procs", policy.max_procs != null ? String(policy.max_procs) : "—"],
    ["cpu", policy.cpu_secs != null ? `${policy.cpu_secs}s` : "—"],
  ];
  return (
    <div className="sbx-policy">
      <div className="sbx-policy-head">Enforced policy · what was actually allowed</div>
      <div className="sbx-policy-grid">
        {rows.map(([k, v]) => (
          <div key={k} className="sbx-policy-row">
            <span className="sbx-policy-key">{k}</span>
            <span className="sbx-policy-val">{v}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

// ── helpers ───────────────────────────────────────────────────────────────────

function laneAllowance(lane: RiskLane, p: EnforcedPolicy | null): string {
  if (!p) return "—";
  switch (lane) {
    case "fs": return p.fs_write.length ? p.fs_write.join(",") : "$WORK rw";
    case "net": return p.net_egress.length ? `allow ${p.net_egress.length}` : p.net_mode;
    case "proc": return p.tools.length ? p.tools.join(",") : "any tool";
    case "resource": return `wall ${p.wall_secs}s`;
    case "provenance": return "mediated";
  }
}

function runLabel(ev: EnvEventView, cap?: EnvCaptureView): string {
  if (ev.event === "violation") return "mediated commit refused";
  if (cap?.cmd) return cap.cmd;
  // Fall back to the `cmd=\`…\`` slice in the event detail.
  const m = ev.detail?.match(/cmd=`([^`]*)`/);
  return m ? m[1] : ev.detail ?? ev.event;
}

function laneLabel(lane: RiskLane): string {
  return LANES.find((l) => l.key === lane)?.label ?? lane.toUpperCase();
}

function sevRank(s: RiskSeverity): number {
  return s === "critical" ? 3 : s === "warning" ? 2 : 1;
}

function shortTime(ts: string): string {
  // RFC3339 → HH:MM:SS (UTC), best-effort.
  const m = ts.match(/T(\d{2}:\d{2}:\d{2})/);
  return m ? m[1] : ts;
}

function bytes(n: number): string {
  if (n >= 1 << 30) return `${(n / (1 << 30)).toFixed(1)}G`;
  if (n >= 1 << 20) return `${(n / (1 << 20)).toFixed(0)}M`;
  if (n >= 1 << 10) return `${(n / (1 << 10)).toFixed(0)}K`;
  return String(n);
}
