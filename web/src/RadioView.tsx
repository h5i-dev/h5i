import { useEffect, useMemo, useState } from "react";
import { NonIdealState, Spinner, Tag } from "@blueprintjs/core";

import { api, type RadioMessage, type RadioThread } from "./api";

// ─────────────────────────────────────────────────────────────────────────────
// Agent Radio — cross-agent messaging shown as a risk-resolution review graph,
// NOT a chat app (roadmap §7). Each thread is a vertical chain:
//   Claude proposed → Codex flagged risk → Claude patched → Codex re-reviewed →
//   Resolved. The structure is the message; the point is auditable review
//   evidence, not "agents chatting".
// ─────────────────────────────────────────────────────────────────────────────

const KIND_INTENT: Record<string, "primary" | "warning" | "danger" | "success" | "none"> = {
  ASK: "primary",
  REVIEW_REQUEST: "primary",
  REVIEW: "primary",
  RISK: "danger",
  HANDOFF: "warning",
  ACK: "none",
  DONE: "success",
  DECLINE: "warning",
};

export function RadioView() {
  const [threads, setThreads] = useState<RadioThread[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.radio().then(setThreads).catch((e) => setError(String(e)));
  }, []);

  if (error) {
    return <NonIdealState icon="error" title="Failed to load agent radio" description={error} />;
  }
  if (!threads) {
    return <NonIdealState icon={<Spinner size={20} />} title="Loading threads…" />;
  }
  if (threads.length === 0) {
    return (
      <div style={{ padding: 32, maxWidth: 720, margin: "0 auto" }}>
        <NonIdealState
          icon="chat"
          title="No agent threads yet"
          description="When agents review each other's work (h5i msg review / risk / handoff), the review-resolution graph appears here."
        />
      </div>
    );
  }

  const open = threads.filter((t) => !isResolved(t.status)).length;

  return (
    <div className="radio-view">
      <div className="radio-head">
        <span className="radio-head-title">Agent radio</span>
        <Tag minimal round>
          {threads.length} thread{threads.length === 1 ? "" : "s"}
        </Tag>
        {open > 0 ? (
          <Tag minimal intent="warning">
            {open} open
          </Tag>
        ) : (
          <Tag minimal intent="success">
            all resolved
          </Tag>
        )}
        <span className="radio-head-hint">review evidence, not chat</span>
      </div>
      <div className="radio-threads">
        {threads.map((t) => (
          <ThreadCard key={t.thread_id} thread={t} />
        ))}
      </div>
    </div>
  );
}

function ThreadCard({ thread }: { thread: RadioThread }) {
  const resolved = isResolved(thread.status);
  const participants = useMemo(() => {
    const s = new Set<string>();
    thread.messages.forEach((m) => {
      s.add(m.from);
      if (m.to !== "all") s.add(m.to);
    });
    return [...s];
  }, [thread.messages]);

  return (
    <div className={"radio-thread" + (resolved ? " resolved" : "")}>
      <div className="radio-thread-head">
        <span className={"radio-status " + statusClass(thread.status)}>{thread.status}</span>
        {thread.branch ? <Tag minimal icon="git-branch">{thread.branch}</Tag> : null}
        <span className="radio-thread-parties">{participants.join(" ↔ ")}</span>
        <span className="radio-thread-ts">{shortTime(thread.latest_ts)}</span>
      </div>
      <ol className="radio-chain">
        {thread.messages.map((m, i) => (
          <RadioNode key={m.id} m={m} last={i === thread.messages.length - 1} />
        ))}
      </ol>
    </div>
  );
}

function RadioNode({ m, last }: { m: RadioMessage; last: boolean }) {
  const intent = KIND_INTENT[m.kind] ?? "none";
  return (
    <li className={"radio-node" + (last ? " last" : "")}>
      <span className="radio-node-rail">
        <span className={"radio-node-dot intent-" + intent} />
        {!last ? <span className="radio-node-line" /> : null}
      </span>
      <span className="radio-node-body">
        <span className="radio-node-head">
          <span className="radio-node-from">{m.from}</span>
          <span className="radio-node-arrow">→</span>
          <span className="radio-node-to">{m.to}</span>
          <Tag minimal intent={intent} className="radio-node-kind">
            {m.kind}
          </Tag>
          {m.priority && m.priority !== "normal" ? (
            <Tag minimal intent={m.priority === "urgent" || m.priority === "high" ? "danger" : "none"}>
              {m.priority}
            </Tag>
          ) : null}
        </span>
        <span className="radio-node-text">{m.body}</span>
        {m.risk ? <span className="radio-node-risk">⚠ {m.risk}</span> : null}
        {m.focus && m.focus.length > 0 ? (
          <span className="radio-node-focus">
            {m.focus.map((f) => (
              <code key={f}>{f}</code>
            ))}
          </span>
        ) : null}
      </span>
    </li>
  );
}

function isResolved(status: string): boolean {
  return status === "done" || status === "declined";
}
function statusClass(status: string): string {
  if (status === "done") return "done";
  if (status === "declined") return "declined";
  if (status === "acked") return "acked";
  return "open";
}
function shortTime(ts: string): string {
  const m = ts.match(/(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2})/);
  return m ? `${m[1]} ${m[2]}` : ts;
}
