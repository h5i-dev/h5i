import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type ReactElement,
} from "react";
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
  type BoxDetail,
  type BoxRow,
  type CapabilitiesReport,
  type EnforcedPolicy,
  type EnvEvent,
  type ExecRecord,
  type ServiceStatus,
  type Signals,
} from "./api";

// The box console: a read-only operator view of the h5i fleet. It answers, at
// a glance: which boxes exist, what each one's policy actually allows, what ran
// inside it, and what pressed on a boundary.
//
// Honesty is the design constraint, and it is the same one the original
// dashboard had. Red means enforcement *fired* — the egress proxy refused a
// destination. Amber means something worth a look (a failed run, a wall-clock
// kill, a page throwing errors) with no claim that a boundary was tested. Grey
// means the evidence itself is weak: nothing was confined, or every record came
// from inside the box. None of the three is an accusation, and no number on
// this screen is computed from anything but the receipts.

const LANES: { key: LaneKey; label: string; hint: string }[] = [
  { key: "fs", label: "FS", hint: "filesystem reach" },
  { key: "net", label: "NET", hint: "network egress" },
  { key: "proc", label: "PROC", hint: "process / exit status" },
  { key: "res", label: "RES", hint: "resource limits" },
  { key: "browser", label: "PAGE", hint: "what the in-box browser saw" },
];

type LaneKey = "fs" | "net" | "proc" | "res" | "browser";

const POLL_MS = 8000;

export function SandboxView() {
  const [boxes, setBoxes] = useState<BoxRow[] | null>(null);
  const [probe, setProbe] = useState<CapabilitiesReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [filter, setFilter] = useState<string>("all");
  // Bumped on every successful fleet poll, so the detail pane refreshes with it.
  const [tick, setTick] = useState(0);

  const load = useCallback(() => {
    api
      .boxes()
      .then((b) => {
        setBoxes(b);
        setError(null);
        setTick((t) => t + 1);
      })
      .catch((e) => setError(String(e instanceof Error ? e.message : e)));
  }, []);

  useEffect(() => {
    load();
    const t = setInterval(load, POLL_MS);
    return () => clearInterval(t);
  }, [load]);

  // The probe shells out to `podman info`; it is host state, not fleet state,
  // so it is fetched once rather than on every poll.
  useEffect(() => {
    api.probe().then(setProbe).catch(() => setProbe(null));
  }, []);

  // Keep a selection valid as the fleet refreshes; default to the most
  // pressing box, which the server already sorted to the top.
  useEffect(() => {
    if (!boxes) return;
    setSelectedId((prev) => {
      if (prev && boxes.some((b) => b.id === prev)) return prev;
      return boxes[0]?.id ?? null;
    });
  }, [boxes]);

  const filtered = useMemo(
    () => (boxes ? boxes.filter((b) => matchesFilter(b, filter)) : null),
    [boxes, filter],
  );

  const selected = useMemo(
    () => boxes?.find((b) => b.id === selectedId) ?? null,
    [boxes, selectedId],
  );

  if (error) {
    return (
      <div className="sbx-shell">
        <NonIdealState
          icon="error"
          title="Could not read the fleet"
          description={error}
        />
      </div>
    );
  }

  return (
    <div className="sbx-shell">
      <TopStrip probe={probe} boxes={boxes} />
      <div className="sbx-body">
        <FleetPane
          boxes={filtered}
          total={boxes?.length ?? 0}
          filter={filter}
          onFilter={setFilter}
          selectedId={selectedId}
          onSelect={setSelectedId}
        />
        <DetailPane box={selected} tick={tick} />
      </div>
    </div>
  );
}

// ── top strip: host readiness + fleet vitals ─────────────────────────────────

function TopStrip({
  probe,
  boxes,
}: {
  probe: CapabilitiesReport | null;
  boxes: BoxRow[] | null;
}) {
  const active =
    boxes?.filter((b) => ["running", "idle", "created"].includes(b.status))
      .length ?? 0;
  const proposed = boxes?.filter((b) => b.status === "proposed").length ?? 0;
  const denials =
    boxes?.reduce((n, b) => n + (b.signals.egress_denied > 0 ? 1 : 0), 0) ?? 0;
  const runs = boxes?.reduce((n, b) => n + b.signals.runs, 0) ?? 0;

  return (
    <div className="sbx-strip">
      <div className="sbx-strip-group">
        <span className="sbx-strip-label">host</span>
        {probe ? (
          <>
            {probe.claims.map((c) => (
              <Tag
                key={c.claim}
                minimal
                // `satisfiable` says the policy resolves; `runnable` is the
                // functional exec self-test. A tier that resolves but will not
                // run is not a green tick.
                intent={
                  c.satisfiable && c.runnable !== false ? "success" : "none"
                }
                title={
                  c.note ??
                  (c.runnable === false
                    ? "policy resolves here, but a confined exec fails"
                    : undefined)
                }
              >
                {c.claim} {c.satisfiable && c.runnable !== false ? "✓" : "✗"}
              </Tag>
            ))}
            <Tag
              minimal
              intent={probe.egress_enforced ? "success" : "none"}
              title={
                probe.egress_enforced
                  ? "a domain allowlist can be enforced here"
                  : "no allowlist enforcement here — the kernel tiers deny all or allow all"
              }
            >
              egress {probe.egress_enforced ? "✓" : "✗"}
            </Tag>
            <Tag
              minimal
              intent={probe.resource_limits ? "success" : "none"}
              title={
                probe.memory_limit
                  ? "cpu / procs / wall / memory limits enforceable"
                  : "no memory cap on this host (see the Seatbelt resource note)"
              }
            >
              limits {probe.resource_limits ? "✓" : "✗"}
            </Tag>
            <Tag minimal title={`mechanism: ${probe.mechanism}`}>
              strongest: {probe.strongest_tier}
            </Tag>
          </>
        ) : (
          <Tag minimal>probing…</Tag>
        )}
      </div>
      <div className="sbx-strip-vitals">
        <Vital label="boxes" value={boxes?.length ?? 0} />
        <Vital label="active" value={active} />
        <Vital
          label="proposed"
          value={proposed}
          intent={proposed > 0 ? "primary" : undefined}
        />
        <Vital
          label="denials"
          value={denials}
          intent={denials > 0 ? "danger" : undefined}
        />
        <Vital label="runs" value={runs} />
      </div>
    </div>
  );
}

function Vital({
  label,
  value,
  intent,
}: {
  label: string;
  value: number;
  intent?: "primary" | "danger";
}) {
  const color =
    intent === "danger"
      ? "var(--bp-red)"
      : intent === "primary"
        ? "var(--bp-blue-hi)"
        : "var(--bp-text)";
  return (
    <span className="sbx-vital">
      <span className="sbx-vital-num" style={{ color }}>
        {value}
      </span>
      <span className="sbx-vital-label">{label}</span>
    </span>
  );
}

// ── left: the fleet ──────────────────────────────────────────────────────────

const FILTERS = [
  "all",
  "running",
  "proposed",
  "pressure",
  "drifted",
  "container",
  "supervised",
  "process",
  "workspace",
];

function matchesFilter(b: BoxRow, f: string): boolean {
  switch (f) {
    case "all":
      return true;
    case "running":
      return b.status === "running";
    case "proposed":
      return b.status === "proposed";
    case "pressure":
      return b.signals.verdict !== "clean";
    case "drifted":
      return b.drift !== "up-to-date" && b.drift !== "detached";
    default:
      return b.isolation_claim === f;
  }
}

function FleetPane(props: {
  boxes: BoxRow[] | null;
  total: number;
  filter: string;
  onFilter: (f: string) => void;
  selectedId: string | null;
  onSelect: (id: string) => void;
}) {
  const { boxes, total, filter, onFilter, selectedId, onSelect } = props;
  return (
    <div className="sbx-fleet">
      <div className="sbx-fleet-header">
        <span>Boxes</span>
        <Tag minimal round>
          {total}
        </Tag>
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
        {!boxes ? (
          <NonIdealState icon={<Spinner size={20} />} title="Loading…" />
        ) : boxes.length === 0 ? (
          <NonIdealState
            icon="shield"
            title="No boxes"
            description={
              total === 0
                ? "Make one with `h5i box .`"
                : "None match this filter."
            }
          />
        ) : (
          <HTMLTable className="sbx-fleet-table" interactive compact>
            <thead>
              <tr>
                <th>Box</th>
                <th style={{ width: 90 }}>Isolation</th>
                <th style={{ width: 70 }}>Status</th>
                <th style={{ width: 96 }}>Signal</th>
              </tr>
            </thead>
            <tbody>
              {boxes.map((b) => (
                <tr
                  key={b.id}
                  className={b.id === selectedId ? "selected" : ""}
                  onClick={() => onSelect(b.id)}
                >
                  <td>
                    <div className="sbx-env-id">
                      {b.agent}/{b.slug}
                    </div>
                    <div className="sbx-env-sub">
                      {b.signals.runs} run{b.signals.runs === 1 ? "" : "s"}
                      {b.files_changed > 0 ? (
                        <span>
                          {" · "}
                          {b.files_changed}f +{b.insertions}/−{b.deletions}
                        </span>
                      ) : null}
                      {b.drift !== "up-to-date" && b.drift !== "detached" ? (
                        <span className="sbx-drift" title={b.drift_summary}>
                          {" · "}
                          {b.drift}
                        </span>
                      ) : null}
                      {b.stale_running ? (
                        <span
                          className="sbx-drift"
                          title="status says running, but no live session holds it — a crash leftover"
                        >
                          {" · stale"}
                        </span>
                      ) : null}
                      {!b.has_workspace ? (
                        <span className="sbx-env-sub-dim"> · pulled</span>
                      ) : null}
                    </div>
                  </td>
                  <td>
                    <IsolationTag
                      isolation={b.isolation_claim}
                      weak={b.signals.weak_isolation}
                    />
                  </td>
                  <td>
                    <span className="sbx-status">{b.status}</span>
                    {b.live.length > 0 ? (
                      <div
                        className="sbx-env-sub"
                        title={b.live
                          .map((s) => `${s.kind} pid ${s.pid}`)
                          .join("\n")}
                      >
                        ● {b.live.length} live
                      </div>
                    ) : null}
                  </td>
                  <td>
                    <SignalBadge signals={b.signals} />
                  </td>
                </tr>
              ))}
            </tbody>
          </HTMLTable>
        )}
      </div>
    </div>
  );
}

function IsolationTag({
  isolation,
  weak,
}: {
  isolation: string;
  weak: boolean;
}) {
  // container / supervised are the confined tiers (green); process is
  // kernel-confined (blue); workspace confines nothing, and says so.
  const intent = weak
    ? "none"
    : isolation === "container" || isolation === "supervised"
      ? "success"
      : "primary";
  return (
    <Tag
      minimal
      intent={intent}
      title={weak ? "workspace tier — nothing was confined" : undefined}
    >
      {isolation}
    </Tag>
  );
}

/** The one badge in the fleet table. Never a score — only what was recorded. */
function SignalBadge({ signals }: { signals: Signals }) {
  if (signals.verdict === "denial") {
    return (
      <span
        className="sbx-pressure critical"
        title={
          `${signals.egress_denied} egress request(s) refused` +
          (signals.denied_hosts.length
            ? `:\n${signals.denied_hosts.join("\n")}`
            : "")
        }
      >
        ⛔ blocked
      </span>
    );
  }
  if (signals.verdict === "attention") {
    const parts = [
      signals.failed ? `${signals.failed} failed` : null,
      signals.timed_out ? `${signals.timed_out} timed out` : null,
      signals.browser_issues ? `${signals.browser_issues} page issue(s)` : null,
    ].filter(Boolean);
    return (
      <span className="sbx-pressure warning" title={parts.join(", ")}>
        ⚠ {parts.length} kind{parts.length === 1 ? "" : "s"}
      </span>
    );
  }
  if (signals.runs === 0) {
    return <span className="sbx-pressure none">no runs</span>;
  }
  if (signals.box_claimed_only) {
    return (
      <span
        className="sbx-pressure weak"
        title="every record came from inside the box — nothing here was observed from the host"
      >
        ◌ box-claimed
      </span>
    );
  }
  return <span className="sbx-pressure clean">clean</span>;
}

// ── right: one box ───────────────────────────────────────────────────────────

function DetailPane({ box, tick }: { box: BoxRow | null; tick: number }) {
  const [detail, setDetail] = useState<BoxDetail | null>(null);
  const [loading, setLoading] = useState(false);

  const agent = box?.agent;
  const slug = box?.slug;

  useEffect(() => {
    if (!agent || !slug) {
      setDetail(null);
      return;
    }
    let live = true;
    setLoading(true);
    api
      .box(agent, slug)
      .then((d) => {
        if (live) setDetail(d);
      })
      .catch(() => {
        if (live) setDetail(null);
      })
      .finally(() => {
        if (live) setLoading(false);
      });
    return () => {
      live = false;
    };
    // `tick` advances with the fleet poll: without it the detail pane fetched
    // once per selection while the row beside it kept updating, so an open box
    // drifted behind its own signal badge.
  }, [agent, slug, tick]);

  if (!box) {
    return (
      <div className="sbx-detail">
        <div className="sbx-pane-empty">
          Select a box to see what ran inside it.
        </div>
      </div>
    );
  }

  return (
    <div className="sbx-detail">
      <div className="sbx-detail-head">
        <div>
          <span className="sbx-detail-title">
            {box.agent}/{box.slug}
          </span>
          <IsolationTag
            isolation={box.isolation_claim}
            weak={box.signals.weak_isolation}
          />
          <Tag minimal>{box.profile}</Tag>
          {box.pr ? <Tag minimal intent="primary">PR #{box.pr}</Tag> : null}
          <span
            className="sbx-detail-digest"
            title="sha256 of the policy that was actually enforced"
          >
            {box.policy_digest.slice(0, 12)}
          </span>
        </div>
        <SignalBadge signals={box.signals} />
      </div>

      {loading && !detail ? (
        <NonIdealState icon={<Spinner size={20} />} title="Loading evidence…" />
      ) : !detail ? (
        <div className="sbx-pane-empty">No detail available.</div>
      ) : (
        <div className="sbx-detail-body">
          <SignalSummary box={box} />
          <Services services={detail.services} />
          <Timeline
            agent={box.agent}
            slug={box.slug}
            receipts={detail.receipts}
            folded={detail.receipts_folded}
            events={detail.events}
            policy={detail.policy ?? null}
          />
          <PolicyPanel policy={detail.policy ?? null} />
          <Diffstat text={detail.diffstat} drift={box.drift_summary} />
        </div>
      )}
    </div>
  );
}

function SignalSummary({ box }: { box: BoxRow }) {
  const s = box.signals;
  const notes: ReactElement[] = [];

  if (s.egress_denied > 0) {
    notes.push(
      <Callout
        key="denial"
        intent="danger"
        icon="ban-circle"
        className="sbx-callout"
      >
        The egress allowlist refused {s.egress_denied} request
        {s.egress_denied === 1 ? "" : "s"}
        {s.denied_hosts.length ? (
          <>
            {" "}
            to <Code>{s.denied_hosts.join(", ")}</Code>
          </>
        ) : null}
        . This is host-observed: the proxy recorded it, not the box.
      </Callout>,
    );
  }
  if (s.failed > 0 || s.timed_out > 0) {
    notes.push(
      <Callout key="fail" intent="warning" icon="warning-sign" className="sbx-callout">
        {s.failed} run{s.failed === 1 ? "" : "s"} exited non-zero
        {s.timed_out > 0
          ? `, ${s.timed_out} killed by the wall-clock limit`
          : ""}
        . A failing command is not a boundary trip — it is just a failing
        command.
      </Callout>,
    );
  }
  if (s.weak_isolation) {
    notes.push(
      <Callout key="weak" icon="unlock" className="sbx-callout">
        Isolation tier is <Code>workspace</Code>: nothing was confined. The
        receipts below are a record of what ran, not evidence that anything was
        stopped.
      </Callout>,
    );
  }
  if (s.box_claimed_only) {
    notes.push(
      <Callout key="claimed" icon="eye-off" className="sbx-callout">
        Every receipt here came from the in-box tee shim — the box's own
        account. Nothing on this screen was observed from the host.
      </Callout>,
    );
  }
  if (notes.length === 0) {
    notes.push(
      <Callout
        key="clean"
        intent="success"
        icon="tick-circle"
        className="sbx-callout"
      >
        {s.runs === 0
          ? "No runs recorded yet."
          : `${s.runs} run${s.runs === 1 ? "" : "s"}, all host-observed, no refused egress and no failures.`}
      </Callout>,
    );
  }
  return <div className="sbx-findings">{notes}</div>;
}

function Services({ services }: { services: ServiceStatus[] }) {
  if (services.length === 0) return null;
  return (
    <div className="sbx-policy">
      <div className="sbx-policy-head">Services · declared in .h5i/env.toml</div>
      <div className="sbx-policy-grid">
        {services.map((s) => (
          <div key={s.name} className="sbx-policy-row">
            <span className="sbx-policy-key">
              {s.alive ? "●" : "○"} {s.name}
            </span>
            <span className="sbx-policy-val">
              pid {s.pid}
              {s.dynamic_port ? ` · :${s.dynamic_port}` : ""}
              {s.alive ? "" : " · not running"}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

// ── the flight recorder ──────────────────────────────────────────────────────

/** One row: a receipt, or a mediated-commit refusal from the event log. */
type Row =
  | { kind: "run"; ts: string; receipt: ExecRecord }
  | { kind: "violation"; ts: string; event: EnvEvent };

function Timeline({
  agent,
  slug,
  receipts,
  folded,
  events,
  policy,
}: {
  agent: string;
  slug: string;
  receipts: ExecRecord[];
  folded: number;
  events: EnvEvent[];
  policy: EnforcedPolicy | null;
}) {
  const [openId, setOpenId] = useState<string | null>(null);
  const [renders, setRenders] = useState<Map<string, string>>(new Map());

  const rows = useMemo<Row[]>(() => {
    const out: Row[] = receipts.map((r) => ({
      kind: "run",
      ts: r.timestamp,
      receipt: r,
    }));
    // The output gate refusing a commit is boundary activity with no receipt
    // of its own, so it is folded in by timestamp rather than left out.
    for (const e of events) {
      if (e.event === "violation") out.push({ kind: "violation", ts: e.ts, event: e });
    }
    // RFC3339 UTC sorts lexically.
    return out.sort((a, b) => a.ts.localeCompare(b.ts));
  }, [receipts, events]);

  const toggle = useCallback(
    (id: string) => {
      setOpenId((prev) => (prev === id ? null : id));
      if (!renders.has(id)) {
        api
          .receipt(agent, slug, id)
          .then(({ render }) => setRenders((m) => new Map(m).set(id, render)))
          .catch(() =>
            setRenders((m) => new Map(m).set(id, "(failed to load this receipt)")),
          );
      }
    },
    [renders, agent, slug],
  );

  return (
    <div className="sbx-timeline">
      <div className="sbx-timeline-head">
        Flight recorder · {rows.length} record{rows.length === 1 ? "" : "s"}
        {folded > 0 ? ` · ${folded} older folded` : ""}
      </div>
      <div className="sbx-lane-grid">
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
          <div className="sbx-lane-empty">
            No runs yet — {"`h5i box run <name> -- …`"}
          </div>
        ) : (
          rows.map((row, i) => (
            <TimelineRow
              key={row.kind === "run" ? row.receipt.id : `v${i}`}
              row={row}
              open={row.kind === "run" && openId === row.receipt.id}
              render={row.kind === "run" ? renders.get(row.receipt.id) : undefined}
              onToggle={row.kind === "run" ? () => toggle(row.receipt.id) : undefined}
            />
          ))
        )}
      </div>
    </div>
  );
}

function TimelineRow({
  row,
  open,
  render,
  onToggle,
}: {
  row: Row;
  open: boolean;
  render: string | undefined;
  onToggle?: () => void;
}) {
  if (row.kind === "violation") {
    return (
      <div className="sbx-lane-row">
        <div className="sbx-run-cell" title={row.event.detail ?? ""}>
          <div className="sbx-run-cmd">mediated commit refused</div>
          <div className="sbx-run-meta">
            output gate · {shortTime(row.ts)}
          </div>
        </div>
        <div className="sbx-lane-cell">
          <span className="sbx-verdict critical" title={row.event.detail ?? ""}>
            ⛔ refused
          </span>
        </div>
        {LANES.slice(1).map((l) => (
          <div key={l.key} className="sbx-lane-cell">
            <span className="sbx-verdict none">·</span>
          </div>
        ))}
      </div>
    );
  }

  const r = row.receipt;
  return (
    <>
      <div
        className={"sbx-lane-row" + (open ? " selected" : "")}
        onClick={onToggle}
      >
        <div className="sbx-run-cell" title={r.cmd ?? ""}>
          <div className="sbx-run-cmd">{r.cmd ?? "(no command recorded)"}</div>
          <div className="sbx-run-meta">
            <SourceChip source={r.source} />
            {r.exit_code != null ? ` exit ${r.exit_code}` : ""}
            {r.wall_ms != null ? ` · ${fmtMs(r.wall_ms)}` : ""}
            {" · "}
            {shortTime(r.timestamp)}
            {r.redactions && r.redactions.length > 0 ? " · redacted" : ""}
          </div>
        </div>
        {LANES.map((l) => (
          <div key={l.key} className="sbx-lane-cell">
            <LaneVerdict lane={l.key} receipt={r} />
          </div>
        ))}
      </div>
      {open ? (
        <div className="sbx-render-row">
          {render === undefined ? (
            <Spinner size={16} />
          ) : (
            <pre className="sbx-render">{render}</pre>
          )}
        </div>
      ) : null}
    </>
  );
}

/** Who observed this run. The single most load-bearing label on the screen. */
function SourceChip({ source }: { source: string }) {
  const boxClaimed = source === "tee-shim";
  return (
    <span
      className={"sbx-source" + (boxClaimed ? " claimed" : " observed")}
      title={
        boxClaimed
          ? "recorded by the shim inside the box — the box's own account"
          : "observed by h5i from the host"
      }
    >
      {boxClaimed ? "box-claimed" : "host-observed"}
    </span>
  );
}

function LaneVerdict({ lane, receipt }: { lane: LaneKey; receipt: ExecRecord }) {
  switch (lane) {
    case "fs": {
      const n = receipt.files?.length ?? 0;
      return n > 0 ? (
        <span
          className="sbx-verdict info"
          title={(receipt.files ?? []).join("\n")}
        >
          {n} file{n === 1 ? "" : "s"}
        </span>
      ) : (
        <span className="sbx-verdict none">·</span>
      );
    }
    case "net": {
      const e = receipt.egress;
      if (!e) return <span className="sbx-verdict none">·</span>;
      if (e.denied > 0) {
        return (
          <span
            className="sbx-verdict critical"
            title={(e.hosts ?? [])
              .filter((h) => h.denied > 0)
              .map((h) => `refused ${h.denied}× ${h.host}:${h.port}`)
              .join("\n")}
          >
            ⛔ {e.denied} refused
          </span>
        );
      }
      return e.allowed > 0 ? (
        <span className="sbx-verdict ok" title={`${e.allowed} request(s) allowed`}>
          ✓ {e.allowed}
        </span>
      ) : (
        <span className="sbx-verdict none">·</span>
      );
    }
    case "proc": {
      if (receipt.exit_code == null)
        return <span className="sbx-verdict none">·</span>;
      return receipt.exit_code === 0 ? (
        <span className="sbx-verdict ok">✓</span>
      ) : (
        <span className="sbx-verdict warning">⚠ exit {receipt.exit_code}</span>
      );
    }
    case "res": {
      if (receipt.timed_out)
        return (
          <span
            className="sbx-verdict critical"
            title="the wall-clock limit killed the process group"
          >
            ⛔ wall limit
          </span>
        );
      if (receipt.max_rss_kb != null)
        return (
          <span className="sbx-verdict info" title="peak resident set size">
            {fmtKb(receipt.max_rss_kb)}
          </span>
        );
      return <span className="sbx-verdict none">·</span>;
    }
    case "browser": {
      const b = receipt.browser;
      if (!b) return <span className="sbx-verdict none">·</span>;
      if (b.unavailable)
        return (
          <span
            className="sbx-verdict weak"
            title="the drain could not reach a browser — nothing was looked at"
          >
            ◌ none
          </span>
        );
      const issues =
        (b.console?.length ?? 0) +
        (b.errors?.length ?? 0) +
        (b.failed_requests?.length ?? 0);
      if (issues === 0)
        return (
          <span className="sbx-verdict ok" title={b.verb ? `${b.verb}: clean` : "clean"}>
            ✓
          </span>
        );
      return (
        <span
          className="sbx-verdict warning"
          title={[...(b.errors ?? []), ...(b.console ?? []), ...(b.failed_requests ?? [])].join(
            "\n",
          )}
        >
          ⚠ {issues}
        </span>
      );
    }
  }
}

function PolicyPanel({ policy }: { policy: EnforcedPolicy | null }) {
  if (!policy) {
    return (
      <div className="sbx-policy">
        <div className="sbx-policy-head">Enforced policy</div>
        <div className="sbx-pane-empty">
          policy.resolved.toml unavailable (a pulled or gc'd box).
        </div>
      </div>
    );
  }
  const rows: [string, string][] = [
    ["isolation", policy.isolation],
    ["net.mode", policy.net_mode],
    ["net.egress", policy.net_egress.length ? policy.net_egress.join(", ") : "—"],
    ["fs.write", policy.fs_write.length ? policy.fs_write.join(", ") : "$WORK"],
    ["fs.read", policy.fs_read.length ? policy.fs_read.join(", ") : "—"],
    ["fs.deny", policy.fs_deny.length ? policy.fs_deny.join(", ") : "—"],
    ["tools", policy.tools.length ? policy.tools.join(", ") : "(unrestricted)"],
    ["env.pass", policy.env_pass.length ? policy.env_pass.join(", ") : "—"],
    ["image", policy.image ?? "—"],
    ["wall", `${policy.wall_secs}s`],
    // A cap this host cannot apply is marked rather than shown alongside the
    // real ones — the panel's whole claim is "what was actually allowed", and
    // Darwin's kernel tiers impose neither of these.
    [
      "mem",
      policy.mem_bytes
        ? `${fmtBytes(policy.mem_bytes)}${policy.mem_enforced === false ? " (declared, not enforced here)" : ""}`
        : "—",
    ],
    [
      "procs",
      policy.max_procs != null
        ? `${policy.max_procs}${policy.procs_enforced === false ? " (declared, not enforced here)" : ""}`
        : "—",
    ],
    ["cpu", policy.cpu_secs != null ? `${policy.cpu_secs}s` : "—"],
    ["fsize", policy.fsize_bytes ? fmtBytes(policy.fsize_bytes) : "—"],
  ];
  return (
    <div className="sbx-policy">
      <div className="sbx-policy-head">
        Enforced policy · what was actually allowed
      </div>
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

function Diffstat({ text, drift }: { text?: string; drift: string }) {
  if (!text || !text.trim()) return null;
  return (
    <div className="sbx-policy">
      <div className="sbx-policy-head">Work vs. pinned base · {drift}</div>
      <pre className="sbx-render">{text}</pre>
    </div>
  );
}

// ── helpers ──────────────────────────────────────────────────────────────────

function laneAllowance(lane: LaneKey, p: EnforcedPolicy | null): string {
  if (!p) return "—";
  switch (lane) {
    case "fs":
      return p.fs_write.length ? p.fs_write.join(",") : "$WORK rw";
    case "net":
      return p.net_egress.length ? `allow ${p.net_egress.length}` : p.net_mode;
    case "proc":
      return p.tools.length ? p.tools.join(",") : "any tool";
    case "res":
      return `wall ${p.wall_secs}s`;
    case "browser":
      return "in box";
  }
}

function shortTime(ts: string): string {
  // RFC3339 → HH:MM:SS (UTC), best-effort.
  const m = ts.match(/T(\d{2}:\d{2}:\d{2})/);
  return m ? m[1] : ts;
}

function fmtMs(ms: number): string {
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms}ms`;
}

function fmtKb(kb: number): string {
  return kb >= 1024 ? `${(kb / 1024).toFixed(0)}M` : `${kb}K`;
}

function fmtBytes(n: number): string {
  if (n >= 1 << 30) return `${(n / (1 << 30)).toFixed(1)}G`;
  if (n >= 1 << 20) return `${(n / (1 << 20)).toFixed(0)}M`;
  if (n >= 1 << 10) return `${(n / (1 << 10)).toFixed(0)}K`;
  return String(n);
}
