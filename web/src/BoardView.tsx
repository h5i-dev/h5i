import { useCallback, useEffect, useMemo, useState } from "react";
import { Button, NonIdealState, Spinner, Tag } from "@blueprintjs/core";

import { api, type EnvFleetItem, type LiveSession } from "./api";

// The Board: environments as cards flowing through the env lifecycle. One
// glance answers "who is working on what, right now, and what is waiting on
// me?" — the live column (pid-verified sessions), the review queue (proposed),
// and the settled work (applied/aborted).
//
// Deliberately READ-ONLY, like every h5i serve surface (no mutating HTTP
// routes without a CSRF story — see SECURITY.md). Each card instead offers
// the exact next CLI command, one click to copy.

const POLL_MS = 8000;

type ColumnKey = "created" | "working" | "proposed" | "applied" | "aborted";

const COLUMNS: {
  key: ColumnKey;
  label: string;
  hint: string;
  statuses: string[];
}[] = [
  { key: "created", label: "Created", hint: "fresh worktrees, nothing run yet", statuses: ["created"] },
  { key: "working", label: "Working", hint: "runs/sessions happening (or idle between them)", statuses: ["running", "idle"] },
  { key: "proposed", label: "Proposed", hint: "mediated commit ready — waiting on a reviewer", statuses: ["proposed"] },
  { key: "applied", label: "Applied", hint: "merged onto the parent branch", statuses: ["applied"] },
  { key: "aborted", label: "Aborted", hint: "discarded (manifest kept for forensics)", statuses: ["aborted"] },
];

// The one obvious next step per column, offered as a copyable command.
function nextCommand(col: ColumnKey, e: EnvFleetItem): string {
  switch (col) {
    case "created":
      return `h5i env shell ${e.slug}`;
    case "working":
      return `h5i env propose ${e.slug}`;
    case "proposed":
      return `h5i env apply ${e.slug}`;
    case "applied":
    case "aborted":
      return `h5i env gc`;
  }
}

function ago(ts: string): string {
  const t = Date.parse(ts);
  if (Number.isNaN(t)) return ts;
  const s = Math.max(0, (Date.now() - t) / 1000);
  if (s < 90) return `${Math.round(s)}s ago`;
  if (s < 90 * 60) return `${Math.round(s / 60)}m ago`;
  if (s < 36 * 3600) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

function liveWriter(e: EnvFleetItem): LiveSession | undefined {
  return e.live.find((s) => s.kind === "run" || s.kind === "shell");
}

export function BoardView() {
  const [envs, setEnvs] = useState<EnvFleetItem[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    api.envs().then(setEnvs).catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    load();
    const t = setInterval(load, POLL_MS);
    return () => clearInterval(t);
  }, [load]);

  if (error) {
    return (
      <div className="wb-body wb-body-single">
        <div className="wb-pane">
          <NonIdealState icon="error" title="Failed to load environments" description={error} />
        </div>
      </div>
    );
  }
  if (!envs) {
    return (
      <div className="wb-body wb-body-single">
        <div className="wb-pane brd-loading">
          <Spinner />
        </div>
      </div>
    );
  }

  return (
    <div className="brd-shell">
      <div className="brd-columns">
        {COLUMNS.map((col) => (
          <BoardColumn key={col.key} col={col} envs={envs} />
        ))}
      </div>
    </div>
  );
}

function BoardColumn({
  col,
  envs,
}: {
  col: (typeof COLUMNS)[number];
  envs: EnvFleetItem[];
}) {
  const cards = useMemo(
    () =>
      envs
        .filter((e) => col.statuses.includes(e.status))
        .sort((a, b) => b.updated_at.localeCompare(a.updated_at)),
    [envs, col],
  );
  return (
    <section className="brd-col" aria-label={col.label}>
      <header className="brd-col-header" title={col.hint}>
        <span className="brd-col-title">{col.label}</span>
        <span className="brd-col-count">{cards.length}</span>
      </header>
      <div className="brd-col-body">
        {cards.length === 0 ? (
          <div className="brd-col-empty">—</div>
        ) : (
          cards.map((e) => <EnvCard key={e.id} env={e} col={col.key} />)
        )}
      </div>
    </section>
  );
}

function EnvCard({ env: e, col }: { env: EnvFleetItem; col: ColumnKey }) {
  const writer = liveWriter(e);
  const observers = e.live.filter((s) => s.kind === "observe").length;
  const riskIntent =
    e.risk.level === "critical" ? "danger" : e.risk.level === "warning" ? "warning" : undefined;

  return (
    <article className={`brd-card${writer ? " brd-card-live" : ""}`}>
      <div className="brd-card-head">
        <span className="brd-card-slug" title={e.id}>
          {e.slug}
        </span>
        <span className="brd-card-agent">{e.agent}</span>
      </div>
      <div className="brd-card-tags">
        <Tag minimal>{e.isolation}</Tag>
        {writer ? (
          <Tag minimal intent="success" title={writer.command ?? undefined}>
            ● {writer.kind} pid {writer.pid}
          </Tag>
        ) : null}
        {observers > 0 ? <Tag minimal>◦ {observers} observer{observers > 1 ? "s" : ""}</Tag> : null}
        {e.stale_running ? (
          <Tag minimal intent="warning" title="status says running but no live session holds this env (writer likely crashed)">
            stale
          </Tag>
        ) : null}
        {e.pr ? <Tag minimal intent="primary">PR #{e.pr}</Tag> : null}
        {e.drift !== "up-to-date" ? (
          <Tag minimal intent="warning" title={e.drift_summary}>
            {e.drift}
          </Tag>
        ) : null}
        {e.risk.score > 0 ? (
          <Tag minimal intent={riskIntent} title={e.risk.findings[0]?.title}>
            risk {e.risk.score}
          </Tag>
        ) : null}
      </div>
      <div className="brd-card-meta">
        <span title="files changed vs the pinned base">
          {e.files_changed} file{e.files_changed === 1 ? "" : "s"}
        </span>
        <span className="brd-ins">+{e.insertions}</span>
        <span className="brd-del">−{e.deletions}</span>
        <span title="evidence captures">{e.captures} cap</span>
        <span className="brd-card-when" title={e.updated_at}>
          {ago(e.updated_at)}
        </span>
      </div>
      <div className="brd-card-foot">
        <span className="brd-card-parent" title={`proposes/applies onto ${e.parent_branch}`}>
          → {e.parent_branch}
        </span>
        <CopyCommand cmd={nextCommand(col, e)} />
      </div>
    </article>
  );
}

function CopyCommand({ cmd }: { cmd: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <Button
      minimal
      small
      icon={copied ? "tick" : "clipboard"}
      text={cmd}
      className="brd-copy"
      title="Copy the next-step command"
      onClick={() => {
        navigator.clipboard?.writeText(cmd).then(() => {
          setCopied(true);
          setTimeout(() => setCopied(false), 1200);
        });
      }}
    />
  );
}
