import { useEffect, useState } from "react";
import { Button, Tag } from "@blueprintjs/core";

import { api, type ContextStatus } from "./api";

// Always-visible strip below the navbar that surfaces the h5i context
// workspace at a glance: the active goal, current branch, and milestone /
// trace counts. Clicking "Open" jumps to the full Context mode.
//
// This is the "promote context" move — the legacy UI buried this behind a
// tab; here it's first-class real estate that's visible regardless of mode.

export function ContextStrip({
  repoBranch,
  onOpen,
}: {
  repoBranch: string | null;
  onOpen: () => void;
}) {
  const [status, setStatus] = useState<ContextStatus | null>(null);

  useEffect(() => {
    api
      .contextStatus()
      .then(setStatus)
      .catch(() => setStatus(null));
  }, [repoBranch]);

  if (!status || !status.initialized) {
    const initCommand = `h5i context init --goal "<summary>"`;
    return (
      <div className="wb-ctx-strip wb-ctx-strip-empty">
        <span className="wb-ctx-label">Context</span>
        <span className="wb-ctx-goal-empty">
          No context workspace.
          <code>{initCommand}</code>
        </span>
        <Button
          minimal
          small
          icon="clipboard"
          onClick={() => void navigator.clipboard?.writeText(initCommand)}
          title="Copy init command"
        >
          Copy
        </Button>
      </div>
    );
  }

  return (
    <div className="wb-ctx-strip" onClick={onOpen} role="button" tabIndex={0}>
      <span className="wb-ctx-label">Context</span>
      <span className="wb-ctx-goal" title={status.goal}>
        {status.goal || "(no goal recorded)"}
      </span>
      <span className="wb-ctx-meta">
        <Tag minimal style={{ fontFamily: "monospace", fontSize: 11 }}>
          {status.current_branch}
        </Tag>
        <Stat label="milestones" value={status.commit_count} />
        <Stat label="trace" value={status.trace_lines} />
        {status.todo_count > 0 ? (
          <Stat label="todo" value={status.todo_count} intent="warning" />
        ) : null}
      </span>
      <Button minimal small icon="arrow-right" onClick={onOpen}>
        Open
      </Button>
    </div>
  );
}

function Stat({
  label,
  value,
  intent,
}: {
  label: string;
  value: number;
  intent?: "warning";
}) {
  return (
    <span className="wb-ctx-stat">
      <span
        className="wb-ctx-stat-val"
        style={intent === "warning" ? { color: "var(--bp-orange)" } : undefined}
      >
        {value}
      </span>
      <span className="wb-ctx-stat-label">{label}</span>
    </span>
  );
}
