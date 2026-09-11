import { useCallback, useEffect, useMemo, useState, type ReactElement } from "react";

import type { Fleet } from "./App";
import {
  api,
  type BoxDetail,
  type BoxRow,
  type EnforcedPolicy,
  type EnvEvent,
  type ExecRecord,
  type ServiceStatus,
  type ShareEvidence,
  type SharedNow,
  type Signals,
  runtimeObserved,
  thirdPartyCanRead,
} from "./api";
import { BrowserTerminal } from "./BrowserTerminal";
import {
  Chip,
  Cmd,
  Empty,
  Facts,
  Note,
  SectionHead,
  Spin,
  Split,
  clock,
  fmtBytes,
  fmtKb,
  fmtMs,
  fmtSecs,
  plural,
} from "./ui";

// Boxes: the repository's confined environments. The badge on a row is
// arithmetic over receipts, never a score; the detail is a flight recorder of
// one row per receipt across the lanes a policy can hold something to.

const LANES: { key: LaneKey; label: string; hint: string }[] = [
  { key: "fs", label: "files", hint: "filesystem reach" },
  { key: "net", label: "egress", hint: "network egress" },
  { key: "proc", label: "exit", hint: "process exit status" },
  { key: "res", label: "limits", hint: "resource limits" },
  { key: "browser", label: "page", hint: "what the in-box browser saw" },
  { key: "kernel", label: "kernel", hint: "what an eBPF collector saw from the kernel; the one lane a box cannot write" },
];

type LaneKey = "fs" | "net" | "proc" | "res" | "browser" | "kernel";

const FILTERS = ["all", "running", "proposed", "pressure", "drifted", "container", "supervised", "process", "workspace"];

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

export function BoxesPage({
  fleet,
  route,
  go,
}: {
  fleet: Fleet;
  route: string[];
  go: (parts: string[], replace?: boolean) => void;
}) {
  const boxes = fleet.boxes;
  const [filter, setFilter] = useState("all");
  const agent = route[0];
  const slug = route[1];
  const view: DetailView = route[2] === "browser" ? "browser" : "evidence";
  const selected = useMemo(
    () => boxes?.find((b) => b.agent === agent && b.slug === slug) ?? null,
    [boxes, agent, slug],
  );
  const filtered = useMemo(() => (boxes ? boxes.filter((b) => matchesFilter(b, filter)) : null), [boxes, filter]);

  const active = boxes?.filter((b) => ["running", "idle", "created"].includes(b.status)).length ?? 0;
  const denials = boxes?.filter((b) => b.signals.egress_denied > 0).length ?? 0;
  const runs = boxes?.reduce((n, b) => n + b.signals.runs, 0) ?? 0;

  return (
    <>
      <div className="topbar">
        <span className="topbar-title">Boxes</span>
        <span className="topbar-scope">this repository's boxes: what each is, what its policy allows, what ran, what pressed on a boundary</span>
        <div className="topbar-right">
          <span className="vital">
            <b>{boxes?.length ?? 0}</b> boxes
          </span>
          <span className="vital">
            <b>{active}</b> active
          </span>
          <span className={`vital${denials > 0 ? " is-loud" : ""}`}>
            <b>{denials}</b> refused egress
          </span>
          <span className="vital">
            <b>{runs}</b> runs
          </span>
          {fleet.probe ? (
            <span className="vital" title={`mechanism: ${fleet.probe.mechanism}`}>
              strongest <b>{fleet.probe.strongest_tier}</b>
            </span>
          ) : null}
        </div>
      </div>
      <div className="page">
        <Split
          id="boxes"
          initial={340}
          first={
            <div className="column">
              <div className="column-tools">
                <div className="chips">
                  {FILTERS.map((f) => (
                    <Chip key={f} tone="dim" on={filter === f} onClick={() => setFilter(f)}>
                      {f}
                    </Chip>
                  ))}
                </div>
              </div>
              <div className="column-body">
                {!filtered ? (
                  <Spin label="reading the fleet" />
                ) : filtered.length === 0 ? (
                  <Empty title={boxes?.length === 0 ? "No boxes" : "None match this filter"}>
                    {boxes?.length === 0 ? <Cmd text="h5i box create <name>" /> : null}
                  </Empty>
                ) : (
                  filtered.map((b) => (
                    <FleetRow key={b.id} b={b} on={b.id === selected?.id} onClick={() => go(["boxes", b.agent, b.slug])} />
                  ))
                )}
              </div>
            </div>
          }
          second={
            selected ? (
              <DetailPane box={selected} tick={fleet.tick} view={view} onView={(v) => go(["boxes", selected.agent, selected.slug, v], true)} />
            ) : (
              <Empty title="Pick a box">
                <p>The column is sorted most pressing first, which the server already did.</p>
              </Empty>
            )
          }
        />
      </div>
    </>
  );
}

function FleetRow({ b, on, onClick }: { b: BoxRow; on: boolean; onClick: () => void }) {
  return (
    <button type="button" className={`fleet-row${on ? " is-on" : ""}`} onClick={onClick}>
      <div className="fleet-top">
        <span className="fleet-name">
          {b.agent}/{b.slug}
        </span>
        <SignalBadge signals={b.signals} />
      </div>
      <div className="fleet-sub">
        <span>
          {b.signals.runs} {plural(b.signals.runs, "run")}
        </span>
        {b.files_changed > 0 ? (
          <span>
            {b.files_changed}f +{b.insertions}/−{b.deletions}
          </span>
        ) : null}
        {b.drift !== "up-to-date" && b.drift !== "detached" ? (
          <span className="drift" title={b.drift_summary}>
            {b.drift}
          </span>
        ) : null}
        {b.shared_now ? <SharedNowChip shared={b.shared_now} /> : null}
        {b.stale_running ? <span className="drift" title="status says running, but no live session holds it: a crash leftover">stale</span> : null}
        {!b.has_workspace ? <span>pulled</span> : null}
        {b.live.length > 0 ? <span title={b.live.map((s) => `${s.kind} pid ${s.pid}`).join("\n")}>● {b.live.length} live</span> : null}
      </div>
      <div className="fleet-bottom">
        <IsolationTag isolation={b.isolation_claim} weak={b.signals.weak_isolation} />
        <Chip tone="dim">{b.status}</Chip>
        <Chip tone="dim">{b.profile}</Chip>
      </div>
    </button>
  );
}

function IsolationTag({ isolation, weak }: { isolation: string; weak: boolean }) {
  const tone = weak ? "dim" : isolation === "container" || isolation === "supervised" ? "good" : "info";
  return (
    <Chip tone={tone} title={weak ? "workspace tier: nothing was confined" : undefined}>
      {isolation}
    </Chip>
  );
}

/** Not red: the fleet's red means enforcement fired, and nothing was refused
 *  here. An operator opened a door on purpose, and it is standing open. */
function SharedNowChip({ shared }: { shared: SharedNow }) {
  const relayed = shared.transport !== "p2p";
  return (
    <span
      className={`shared${relayed ? " relayed" : ""}`}
      title={
        `somebody outside can reach port ${shared.port} inside this box right now, over ${shared.transport}, on ${shared.grants} live ticket(s).` +
        (relayed
          ? "\nThis transport is relayed: a third party terminates TLS."
          : "\nDirect peer to peer: no relay carries the application bytes.") +
        "\nThe receipt lands when the share ends."
      }
    >
      ⇄ shared :{shared.port} {shared.transport}
    </span>
  );
}

/** The one badge in the fleet. Never a score, only what was recorded. */
function SignalBadge({ signals }: { signals: Signals }) {
  if (signals.verdict === "denial") {
    return (
      <span
        className="pressure critical"
        title={`${signals.egress_denied} egress request(s) refused` + (signals.denied_hosts.length ? `:\n${signals.denied_hosts.join("\n")}` : "")}
      >
        refused
      </span>
    );
  }
  if (signals.verdict === "attention") {
    const parts = [
      signals.failed ? `${signals.failed} failed` : null,
      signals.timed_out ? `${signals.timed_out} timed out` : null,
      signals.browser_issues ? `${signals.browser_issues} page issue(s)` : null,
      signals.kernel_alerts ? `${signals.kernel_alerts} kernel alert(s): ${(signals.kernel_rules ?? []).join(", ")}` : null,
    ].filter(Boolean);
    return (
      <span className="pressure warning" title={parts.join(", ")}>
        {parts.length} {plural(parts.length, "kind")}
      </span>
    );
  }
  if (signals.runs === 0) return <span className="pressure none">no runs</span>;
  if (signals.box_claimed_only) {
    return (
      <span className="pressure weak" title="every record came from inside the box; nothing here was observed from the host">
        box-claimed
      </span>
    );
  }
  return <span className="pressure clean">clean</span>;
}

// ── one box ──────────────────────────────────────────────────────────────────

type DetailView = "evidence" | "browser";

function DetailPane({ box, tick, view, onView }: { box: BoxRow; tick: number; view: DetailView; onView: (v: DetailView) => void }) {
  const [detail, setDetail] = useState<BoxDetail | null>(null);
  const { agent, slug } = box;

  useEffect(() => {
    let live = true;
    api
      .box(agent, slug)
      .then((d) => live && setDetail(d))
      .catch(() => live && setDetail(null));
    return () => {
      live = false;
    };
  }, [agent, slug, tick]);

  // Only a browser box has a browser terminal. Off the profile, not off
  // whether events have arrived: a box that has not browsed yet still has
  // the tab, and its panes say so.
  const browserCapable = box.profile === "browser";

  return (
    <div className="work">
      <div className="work-head">
        <div className="work-title">
          <h2>
            {box.agent}/{box.slug}
          </h2>
          <IsolationTag isolation={box.isolation_claim} weak={box.signals.weak_isolation} />
          <Chip tone="dim">{box.profile}</Chip>
          {box.pr ? <Chip tone="info">PR #{box.pr}</Chip> : null}
          <SignalBadge signals={box.signals} />
        </div>
        <div className="work-sub">
          <span className="mono" title="sha256 of the policy that was actually enforced">
            policy {box.policy_digest.slice(0, 12)}
          </span>
          <span>{box.status}</span>
          <span>{box.branch}</span>
          <span>{box.drift_summary}</span>
        </div>
        {browserCapable ? (
          <div className="tabs" role="tablist">
            {(["evidence", "browser"] as DetailView[]).map((v) => (
              <button key={v} type="button" role="tab" aria-selected={view === v} className={`tab${view === v ? " is-on" : ""}`} onClick={() => onView(v)}>
                {v === "evidence" ? "Evidence" : "Browser"}
              </button>
            ))}
          </div>
        ) : (
          <div className="tabs" />
        )}
      </div>
      <div className="work-body">
        {!detail ? (
          <Spin label="reading the evidence" />
        ) : view === "browser" && browserCapable ? (
          <BrowserTerminal agent={box.agent} slug={box.slug} />
        ) : (
          <div className="scroll pad">
            <SignalSummary box={box} />
            <Services services={detail.services} />
            <Timeline agent={box.agent} slug={box.slug} receipts={detail.receipts} folded={detail.receipts_folded} events={detail.events} policy={detail.policy ?? null} />
            <PolicyPanel policy={detail.policy ?? null} />
            <Diffstat text={detail.diffstat} drift={box.drift_summary} />
          </div>
        )}
      </div>
    </div>
  );
}

function SignalSummary({ box }: { box: BoxRow }) {
  const s = box.signals;
  const notes: ReactElement[] = [];
  const live: ReactElement[] = [];
  const history: ReactElement[] = [];

  if (s.egress_denied > 0) {
    notes.push(
      <Note key="denial" tone="bad">
        The egress allowlist refused {s.egress_denied} {plural(s.egress_denied, "request")}
        {s.denied_hosts.length ? (
          <>
            {" "}
            to <code>{s.denied_hosts.join(", ")}</code>
          </>
        ) : null}
        . This is host-observed: the proxy recorded it, not the box.
      </Note>,
    );
  }
  if (s.failed > 0 || s.timed_out > 0) {
    notes.push(
      <Note key="fail" tone="warn">
        {s.failed} {plural(s.failed, "run")} exited non-zero{s.timed_out > 0 ? `, ${s.timed_out} killed by the wall-clock limit` : ""}. A
        failing command is not a boundary trip; it is a failing command.
      </Note>,
    );
  }
  if (box.shared_now) {
    const sn = box.shared_now;
    const relayed = sn.transport !== "p2p";
    live.push(
      <Note key="shared-now" tone="warn">
        Somebody outside can reach port <code>{sn.port}</code> inside this box right now, over <code>{sn.transport}</code>, on {sn.grants} live{" "}
        {plural(sn.grants, "ticket")}.{" "}
        {relayed
          ? "This transport is relayed: a third party terminates TLS, so it is not end-to-end encrypted."
          : "Direct peer to peer: no relay carries the application bytes."}{" "}
        The receipt for it lands when the share ends.
      </Note>,
    );
  }
  if (s.shares > 0) {
    history.push(
      <Note key="shares">
        {s.shares} share {plural(s.shares, "session")} admitted {s.share_peers} {plural(s.share_peers, "peer")} into this box.{" "}
        {s.shares_third_party_readable > 0
          ? `${s.shares_third_party_readable} of them ran over a relayed transport, so a third party could read that traffic.`
          : "All of them ran peer to peer."}{" "}
        Each one is a receipt below, host-observed.
      </Note>,
    );
  }
  if (s.weak_isolation) {
    notes.push(
      <Note key="weak">
        Isolation tier is <code>workspace</code>: nothing was confined. The receipts below are a record of what ran, not evidence that anything
        was stopped.
      </Note>,
    );
  }
  if (s.fs_overlap.length > 0) {
    notes.push(
      <Note key="overlap">
        The last run recorded writable-path overlap with {s.fs_overlap.length} other {plural(s.fs_overlap.length, "box", "boxes")}:{" "}
        <code>{s.fs_overlap.join("; ")}</code>. Cross-box influence is possible through a shared path. Boxes whose recorded grants are disjoint carry
        a machine-checked noninterference guarantee instead; this pair does not.
      </Note>,
    );
  }
  if (s.box_claimed_only) {
    notes.push(
      <Note key="claimed">
        Every receipt here came from the in-box tee shim, the box's own account. Nothing on this screen was observed from the host.
      </Note>,
    );
  }
  if (s.kernel_alerts) {
    notes.push(
      <Note key="kernel" tone="warn">
        An eBPF collector in the kernel recorded {s.kernel_alerts} alert-level {plural(s.kernel_alerts, "match", "matches")}
        {s.kernel_rules?.length ? (
          <>
            {" "}
            (<code>{s.kernel_rules.join(", ")}</code>)
          </>
        ) : null}
        . Nothing was blocked: this lane observes and never denies. The receipts below carry what tripped each rule.
      </Note>,
    );
  }
  if (s.kernel_unwatched) {
    notes.push(
      <Note key="kernel-off">
        {s.kernel_unwatched} {plural(s.kernel_unwatched, "run")} asked to be watched from the kernel and {s.kernel_unwatched === 1 ? "was" : "were"} not.
        Open the run below for the reason. An unwatched run is not a quiet one.
      </Note>,
    );
  }
  if (s.kernel_events_lost) {
    notes.push(
      <Note key="kernel-lost" tone="warn">
        {s.kernel_events_lost} kernel {plural(s.kernel_events_lost, "event")} were dropped before anything examined them, so every count from that
        lane is a lower bound.
      </Note>,
    );
  }
  if (notes.length === 0) {
    notes.push(
      <Note key="clean" tone="good">
        {s.runs === 0 ? "No runs recorded yet." : `${s.runs} ${plural(s.runs, "run")}, all host-observed, no refused egress and no failures.`}
      </Note>,
    );
  }
  return <div className="detail-notes">{[...live, ...notes, ...history]}</div>;
}

function Services({ services }: { services: ServiceStatus[] }) {
  if (services.length === 0) return null;
  return (
    <div>
      <SectionHead aside="declared in .h5i/env.toml">Services</SectionHead>
      <Facts
        rows={services.map((s) => [
          `${s.alive ? "●" : "○"} ${s.name}`,
          `pid ${s.pid}${s.dynamic_port ? `, port ${s.dynamic_port}` : ""}${s.alive ? "" : ", not running"}`,
        ])}
      />
    </div>
  );
}

// ── the flight recorder ──────────────────────────────────────────────────────

type Row = { kind: "run"; ts: string; receipt: ExecRecord } | { kind: "violation"; ts: string; event: EnvEvent };

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
    const out: Row[] = receipts.map((r) => ({ kind: "run", ts: r.timestamp, receipt: r }));
    // The output gate refusing a commit is boundary activity with no receipt
    // of its own, so it is folded in by timestamp rather than left out.
    for (const e of events) if (e.event === "violation") out.push({ kind: "violation", ts: e.ts, event: e });
    return out.sort((a, b) => a.ts.localeCompare(b.ts));
  }, [receipts, events]);

  const toggle = useCallback(
    (id: string) => {
      setOpenId((prev) => (prev === id ? null : id));
      if (!renders.has(id)) {
        api
          .receipt(agent, slug, id)
          .then(({ render }) => setRenders((m) => new Map(m).set(id, render)))
          .catch(() => setRenders((m) => new Map(m).set(id, "(failed to load this receipt)")));
      }
    },
    [renders, agent, slug],
  );

  return (
    <div>
      <SectionHead aside={`${rows.length} ${plural(rows.length, "record")}${folded > 0 ? `, ${folded} older folded` : ""}`}>Flight recorder</SectionHead>
      <div className="lanes">
        <div className="lane-row is-head">
          <div className="run-cell">policy allows</div>
          {LANES.map((l) => (
            <div key={l.key} className="lane-cell" title={l.hint}>
              <div className="lane-name">{l.label}</div>
              <div className="lane-allow">{laneAllowance(l.key, policy)}</div>
            </div>
          ))}
        </div>
        {rows.length === 0 ? (
          <div className="lane-empty">
            No runs yet. <code>h5i box run &lt;name&gt; -- …</code>
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

function TimelineRow({ row, open, render, onToggle }: { row: Row; open: boolean; render: string | undefined; onToggle?: () => void }) {
  if (row.kind === "violation") {
    return (
      <div className="lane-row">
        <div className="run-cell" title={row.event.detail ?? ""}>
          <div className="run-cmd">mediated commit refused</div>
          <div className="run-meta">output gate, {clock(row.ts)}</div>
        </div>
        <div className="lane-cell">
          <span className="verdict critical" title={row.event.detail ?? ""}>
            refused
          </span>
        </div>
        {LANES.slice(1).map((l) => (
          <div key={l.key} className="lane-cell">
            <span className="verdict none">·</span>
          </div>
        ))}
      </div>
    );
  }
  const r = row.receipt;
  return (
    <>
      <div className={`lane-row clickable${open ? " is-on" : ""}`} onClick={onToggle}>
        <div className="run-cell" title={r.cmd ?? ""}>
          <div className="run-cmd">{r.cmd ?? "(no command recorded)"}</div>
          <div className="run-meta">
            <SourceChip source={r.source} />
            {r.exit_code != null ? <span>exit {r.exit_code}</span> : null}
            {r.wall_ms != null ? <span>{fmtMs(r.wall_ms)}</span> : null}
            <span>{clock(r.timestamp)}</span>
            {r.redactions && r.redactions.length > 0 ? <span>redacted</span> : null}
          </div>
          {r.share ? <ShareLine share={r.share} /> : null}
        </div>
        {LANES.map((l) => (
          <div key={l.key} className="lane-cell">
            <LaneVerdict lane={l.key} receipt={r} />
          </div>
        ))}
      </div>
      {open ? <div className="render-row">{render === undefined ? <Spin /> : <pre className="render">{render}</pre>}</div> : null}
    </>
  );
}

function ShareLine({ share }: { share: ShareEvidence }) {
  const relayed = thirdPartyCanRead(share);
  return (
    <div className={`run-share${relayed ? " relayed" : ""}`}>
      ⇄ inbound, {share.transport} :{share.port}, {share.peers} {plural(share.peers, "peer")}, {fmtSecs(share.seconds)}
      {share.turned_away ? `, ${share.turned_away} turned away` : ""}.{" "}
      {relayed ? "A third party terminated TLS: not end-to-end encrypted." : "Direct peer to peer."}
    </div>
  );
}

/** Who observed this run. The single most load-bearing label on the screen. */
function SourceChip({ source }: { source: string }) {
  const boxClaimed = source === "tee-shim";
  return (
    <span className={`source ${boxClaimed ? "claimed" : "observed"}`} title={boxClaimed ? "recorded by the shim inside the box: the box's own account" : "observed by h5i from the host"}>
      {boxClaimed ? "box-claimed" : "host-observed"}
    </span>
  );
}

function LaneVerdict({ lane, receipt }: { lane: LaneKey; receipt: ExecRecord }) {
  switch (lane) {
    case "fs": {
      const n = receipt.files?.length ?? 0;
      return n > 0 ? (
        <span className="verdict info" title={(receipt.files ?? []).join("\n")}>
          {n} {plural(n, "file")}
        </span>
      ) : (
        <span className="verdict none">·</span>
      );
    }
    case "net": {
      const sh = receipt.share;
      if (sh) {
        const relayed = thirdPartyCanRead(sh);
        return (
          <span
            className={`verdict ${relayed ? "warning" : "info"}`}
            title={
              `inbound: ${sh.peers} peer(s) admitted to port ${sh.port} over ${sh.transport}, ${fmtSecs(sh.seconds)}` +
              (sh.turned_away ? `, ${sh.turned_away} connection(s) turned away` : "") +
              (relayed ? "\nRelayed transport: a third party terminated TLS." : "\nDirect peer to peer.")
            }
          >
            ⇄ {sh.peers} in
          </span>
        );
      }
      const e = receipt.egress;
      if (!e) return <span className="verdict none">·</span>;
      if (e.denied > 0) {
        return (
          <span
            className="verdict critical"
            title={(e.hosts ?? [])
              .filter((h) => h.denied > 0)
              .map((h) => `refused ${h.denied}× ${h.host}:${h.port}`)
              .join("\n")}
          >
            {e.denied} refused
          </span>
        );
      }
      return e.allowed > 0 ? (
        <span className="verdict ok" title={`${e.allowed} request(s) allowed`}>
          ✓ {e.allowed}
        </span>
      ) : (
        <span className="verdict none">·</span>
      );
    }
    case "proc": {
      if (receipt.exit_code == null) return <span className="verdict none">·</span>;
      return receipt.exit_code === 0 ? <span className="verdict ok">✓</span> : <span className="verdict warning">exit {receipt.exit_code}</span>;
    }
    case "res": {
      if (receipt.timed_out)
        return (
          <span className="verdict critical" title="the wall-clock limit killed the process group">
            wall limit
          </span>
        );
      if (receipt.max_rss_kb != null)
        return (
          <span className="verdict info" title="peak resident set size">
            {fmtKb(receipt.max_rss_kb)}
          </span>
        );
      return <span className="verdict none">·</span>;
    }
    case "browser": {
      const b = receipt.browser;
      if (!b) return <span className="verdict none">·</span>;
      if (b.unavailable)
        return (
          <span className="verdict weak" title="the drain could not reach a browser: nothing was looked at">
            none
          </span>
        );
      const issues = (b.console?.length ?? 0) + (b.errors?.length ?? 0) + (b.failed_requests?.length ?? 0);
      if (issues === 0)
        return (
          <span className="verdict ok" title={b.verb ? `${b.verb}: clean` : "clean"}>
            ✓
          </span>
        );
      return (
        <span className="verdict warning" title={[...(b.errors ?? []), ...(b.console ?? []), ...(b.failed_requests ?? [])].join("\n")}>
          {issues}
        </span>
      );
    }
    case "kernel": {
      const rt = receipt.runtime;
      if (!rt) return <span className="verdict none">·</span>;
      if (!runtimeObserved(rt))
        return (
          <span className="verdict weak" title={rt.unavailable ?? rt.coverage_reason ?? "the collector observed nothing for this run"}>
            none
          </span>
        );
      const dets = rt.detections ?? [];
      const alerts = dets.filter((d) => d.severity === "alert");
      const lost = rt.events_lost ? `\n${rt.events_lost} event(s) lost: this is a lower bound` : "";
      const partial = rt.coverage === "partial" ? `\npartial coverage: ${rt.coverage_reason ?? "some of the run was out of scope"}` : "";
      if (dets.length === 0)
        return (
          <span className="verdict ok" title={`${rt.events_seen ?? 0} kernel event(s), no signature fired${partial}${lost}`}>
            ✓
          </span>
        );
      const detail = dets.map((d) => `[${d.severity}] ${d.rule} ×${d.count}: ${d.title}`).join("\n");
      return (
        <span className={alerts.length ? "verdict critical" : "verdict warning"} title={`${detail}${partial}${lost}`}>
          {dets.length}
        </span>
      );
    }
  }
}

function PolicyPanel({ policy }: { policy: EnforcedPolicy | null }) {
  if (!policy) {
    return (
      <div>
        <SectionHead>Enforced policy</SectionHead>
        <p className="count">policy.resolved.toml unavailable (a pulled or gc'd box).</p>
      </div>
    );
  }
  const rows: [string, string][] = [
    ["isolation", policy.isolation],
    ["net.mode", policy.net_mode],
    ["net.egress", policy.net_egress.length ? policy.net_egress.join(", ") : "none"],
    ["fs.write", policy.fs_write.length ? policy.fs_write.join(", ") : "$WORK"],
    ["fs.read", policy.fs_read.length ? policy.fs_read.join(", ") : "none"],
    ["fs.deny", policy.fs_deny.length ? policy.fs_deny.join(", ") : "none"],
    ["tools", policy.tools.length ? policy.tools.join(", ") : "(unrestricted)"],
    ["env.pass", policy.env_pass.length ? policy.env_pass.join(", ") : "none"],
    ["image", policy.image ?? "none"],
    ["wall", `${policy.wall_secs}s`],
    ["mem", policy.mem_bytes ? `${fmtBytes(policy.mem_bytes)}${policy.mem_enforced === false ? " (declared, not enforced here)" : ""}` : "none"],
    ["procs", policy.max_procs != null ? `${policy.max_procs}${policy.procs_enforced === false ? " (declared, not enforced here)" : ""}` : "none"],
    ["cpu", policy.cpu_secs != null ? `${policy.cpu_secs}s` : "none"],
    ["fsize", policy.fsize_bytes ? fmtBytes(policy.fsize_bytes) : "none"],
  ];
  return (
    <div>
      <SectionHead aside="what was actually allowed">Enforced policy</SectionHead>
      <Facts rows={rows} />
    </div>
  );
}

function Diffstat({ text, drift }: { text?: string; drift: string }) {
  if (!text || !text.trim()) return null;
  return (
    <div>
      <SectionHead aside={drift}>Work against the pinned base</SectionHead>
      <pre className="render">{text}</pre>
    </div>
  );
}

function laneAllowance(lane: LaneKey, p: EnforcedPolicy | null): string {
  if (!p) return "";
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
    case "kernel":
      return "observe only";
  }
}
