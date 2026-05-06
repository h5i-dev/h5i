import { useEffect, useState } from "react";
import { Callout, NonIdealState, Spinner, Tag } from "@blueprintjs/core";

import { api, type ContextStatus } from "./api";

// Context tab: shows the h5i context-workspace snapshot (goal, branches,
// trace activity) as a per-repo view. Mirrors `h5i context status`.

export function ContextTab() {
  const [status, setStatus] = useState<ContextStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setStatus(null);
    setError(null);
    api
      .contextStatus()
      .then(setStatus)
      .catch((e) => setError(String(e)));
  }, []);

  if (error) {
    return (
      <NonIdealState icon="error" title="Failed to load context" description={error} />
    );
  }
  if (!status) {
    return <NonIdealState icon={<Spinner size={20} />} title="Loading…" />;
  }
  if (!status.initialized) {
    return (
      <div style={{ padding: 16 }}>
        <Callout intent="none" icon="info-sign">
          No context workspace. Run <code>h5i context init</code> to start one.
        </Callout>
      </div>
    );
  }

  return (
    <>
      <div className="wb-detail-section">
        <div className="wb-detail-title">Goal</div>
        <div
          style={{
            fontSize: 12,
            color: "var(--bp-text)",
            lineHeight: 1.5,
            whiteSpace: "pre-wrap",
          }}
        >
          {status.goal || "(no goal set)"}
        </div>
      </div>

      <div className="wb-detail-section">
        <div className="wb-detail-title">Activity</div>
        <div className="wb-detail-grid">
          <div className="wb-detail-key">Active branch</div>
          <div className="wb-detail-val mono">{status.current_branch}</div>
          <div className="wb-detail-key">Branches</div>
          <div className="wb-detail-val mono">{status.branch_count}</div>
          <div className="wb-detail-key">Milestones</div>
          <div className="wb-detail-val mono">{status.commit_count}</div>
          <div className="wb-detail-key">Trace lines</div>
          <div className="wb-detail-val mono">{status.trace_lines}</div>
          <div className="wb-detail-key">Snapshots</div>
          <div className="wb-detail-val mono">{status.snapshot_count}</div>
          {status.todo_count > 0 ? (
            <>
              <div className="wb-detail-key">TODOs</div>
              <div className="wb-detail-val">
                <Tag intent="warning" minimal>
                  {status.todo_count}
                </Tag>
              </div>
            </>
          ) : null}
        </div>
      </div>

      {status.branch_summaries.length > 0 ? (
        <div className="wb-detail-section">
          <div className="wb-detail-title">
            Branches{" "}
            <Tag minimal style={{ marginLeft: 6, fontFamily: "monospace", fontSize: 10 }}>
              {status.branch_summaries.length}
            </Tag>
          </div>
          {status.branch_summaries.map((b) => (
            <div
              key={b.branch}
              style={{
                padding: "8px 0",
                borderTop: "1px solid var(--bp-border)",
              }}
            >
              <div style={{ display: "flex", gap: 8, alignItems: "baseline" }}>
                <Tag minimal style={{ fontFamily: "monospace" }}>
                  {b.branch}
                </Tag>
                <span
                  style={{
                    fontSize: 11,
                    color: "var(--bp-text-dim)",
                    marginLeft: "auto",
                  }}
                >
                  {b.last_activity}
                </span>
              </div>
              <div
                style={{
                  fontSize: 11,
                  color: "var(--bp-text-muted)",
                  marginTop: 4,
                  lineHeight: 1.4,
                }}
              >
                {b.purpose}
              </div>
              <div
                style={{
                  fontSize: 11,
                  color: "var(--bp-text-dim)",
                  marginTop: 4,
                  fontFamily: "monospace",
                }}
              >
                {b.milestone_count} milestones · {b.trace_lines} trace lines
              </div>
            </div>
          ))}
        </div>
      ) : null}
    </>
  );
}
