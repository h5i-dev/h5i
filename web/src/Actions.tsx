import { useEffect, useRef } from "react";

import type { ActionRow, SessionDetail } from "./api";
import { Cmd, Empty, ReqLink, clockMs, fmtMs, plural, type SessionNav } from "./ui";

// The verbs the agent asked for, in order, each with what it spent. This is
// `h5i browser audit` as a timeline: the account of what the agent did,
// against which the History tab is the account of what the wire saw.

export function Actions({
  detail,
  open,
  onOpen,
  nav,
}: {
  detail: SessionDetail;
  open: number | null;
  onOpen: (seq: number | null) => void;
  nav: SessionNav;
}) {
  const name = detail.name ?? detail.id;
  const rows = [...detail.actions].reverse();
  const listRef = useRef<HTMLDivElement>(null);

  // A verb opened from a History row is brought into view.
  useEffect(() => {
    if (open === null) return;
    listRef.current?.querySelector(`[data-seq="${open}"]`)?.scrollIntoView({ block: "nearest" });
  }, [open]);

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
      <div className="timeline" ref={listRef}>
        {rows.map((a) => (
          <ActionLine key={a.seq} a={a} on={open === a.seq} nav={nav} onClick={() => onOpen(open === a.seq ? null : a.seq)} />
        ))}
        <div style={{ padding: "14px 6px 0" }}>
          <Cmd text={`h5i browser audit --session ${name}`} hint="the same timeline with handovers and the ending, from the terminal" />
        </div>
      </div>
    </div>
  );
}

function ActionLine({ a, on, nav, onClick }: { a: ActionRow; on: boolean; nav: SessionNav; onClick: () => void }) {
  const state = a.ok === undefined ? "is-pending" : a.ok ? "is-ok" : "is-failed";
  const took =
    a.ended_at !== undefined ? Date.parse(a.ended_at) - Date.parse(a.at) : undefined;
  const n = a.requests.length;
  return (
    <div
      role="button"
      tabIndex={0}
      data-seq={a.seq}
      className={`tl-row ${state}${on ? " is-on" : ""}`}
      onClick={onClick}
      onKeyDown={(e) => {
        if (e.target === e.currentTarget && (e.key === "Enter" || e.key === " ")) {
          e.preventDefault();
          onClick();
        }
      }}
    >
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
          <div className="tl-reqs tl-spent" style={{ marginTop: 4 }}>
            spent <ReqLink id={`req_${a.requests[0]}`} nav={nav} />
            {n > 1 ? (
              <>
                {" "}
                through <ReqLink id={`req_${a.requests[n - 1]}`} nav={nav} />
              </>
            ) : null}
            <button
              type="button"
              className="req-link is-text"
              onClick={(e) => {
                e.stopPropagation();
                nav.filter(`action:${a.seq}`);
              }}
            >
              show {n === 1 ? "it" : `all ${n}`} in History
            </button>
          </div>
        ) : null}
      </span>
    </div>
  );
}
