import { useState } from "react";

import type { ActionRow, SessionDetail } from "./api";
import { Cmd, Empty, clockMs, fmtMs, plural } from "./ui";

// The verbs the agent asked for, in order, each with what it spent. This is
// `h5i browser audit` as a timeline: the account of what the agent did,
// against which the History tab is the account of what the wire saw.

export function Actions({ detail }: { detail: SessionDetail }) {
  const [open, setOpen] = useState<number | null>(null);
  const name = detail.name ?? detail.id;
  const rows = [...detail.actions].reverse();

  if (rows.length === 0) {
    return (
      <Empty title="No verb has been asked of this session">
        <p>
          The action log fills in as an agent drives: <code>open</code>, <code>click</code>, <code>fill</code>,{" "}
          <code>resend</code> and the rest.
        </p>
        <Cmd text={`h5i browser audit --session ${name}`} />
      </Empty>
    );
  }

  return (
    <div className="scroll">
      <div className="timeline">
        {rows.map((a) => (
          <ActionLine key={a.seq} a={a} on={open === a.seq} onClick={() => setOpen(open === a.seq ? null : a.seq)} />
        ))}
        <div style={{ padding: "14px 6px 0" }}>
          <Cmd text={`h5i browser audit --session ${name}`} hint="the same timeline with handovers and the ending, from the terminal" />
        </div>
      </div>
    </div>
  );
}

function ActionLine({ a, on, onClick }: { a: ActionRow; on: boolean; onClick: () => void }) {
  const state = a.ok === undefined ? "is-pending" : a.ok ? "is-ok" : "is-failed";
  const took =
    a.ended_at !== undefined ? Date.parse(a.ended_at) - Date.parse(a.at) : undefined;
  const n = a.requests.length;
  return (
    <button type="button" className={`tl-row ${state}${on ? " is-on" : ""}`} onClick={onClick}>
      <span className="tl-time">{clockMs(a.at)}</span>
      <span className="tl-mark">
        <i />
      </span>
      <span className="tl-body">
        <span className="tl-line">
          <span className="tl-verb">{a.verb}</span>
          {a.target ? <span className="tl-target">{a.target}</span> : null}
          {a.url ? <span className="tl-target">{a.url}</span> : null}
          {n > 0 ? (
            <span className="tl-reqs">
              <b>{n}</b> {plural(n, "fetch", "fetches")}
            </span>
          ) : null}
          {a.ok === undefined ? <span className="tl-reqs">in flight</span> : null}
          {took !== undefined && Number.isFinite(took) ? <span className="tl-took">{fmtMs(Math.max(0, took))}</span> : null}
        </span>
        {a.error ? <div className="tl-error">{a.error}</div> : null}
        {on && n > 0 ? (
          <div className="tl-reqs" style={{ marginTop: 4 }}>
            spent req_{a.requests[0]}
            {n > 1 ? ` through req_${a.requests[n - 1]}` : ""}. Filter the History tab with{" "}
            <code>action:{a.seq}</code> to see them.
          </div>
        ) : null}
      </span>
    </button>
  );
}
