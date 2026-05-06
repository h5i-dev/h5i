import { useEffect, useState } from "react";
import { Callout, NonIdealState, Spinner, Tag } from "@blueprintjs/core";

import { api, type Commit, type SessionLogMeta } from "./api";

// Sessions tab: shows the Claude Code session(s) analysed for this commit.
// Currently `/api/session-log/list` returns all analyses — we filter by
// commit_oid client-side. If empty for this commit, surface the "run
// `h5i notes analyze`" hint that the legacy UI also shows.

export function SessionsTab({ commit }: { commit: Commit }) {
  const [list, setList] = useState<SessionLogMeta[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setList(null);
    setError(null);
    api
      .sessionList()
      .then(setList)
      .catch((e) => setError(String(e)));
  }, []);

  if (error) {
    return (
      <NonIdealState icon="error" title="Failed to load sessions" description={error} />
    );
  }
  if (!list) {
    return <NonIdealState icon={<Spinner size={20} />} title="Loading…" />;
  }

  const forCommit = list.filter((s) => s.commit_oid === commit.git_oid);

  if (forCommit.length === 0) {
    return (
      <div style={{ padding: 16 }}>
        <Callout intent="none" icon="info-sign" compact>
          No session analysis linked to this commit. Run{" "}
          <code>h5i notes analyze</code> after a Claude Code session to attach
          one.
        </Callout>
        {list.length > 0 ? (
          <div style={{ marginTop: 16 }}>
            <div className="wb-detail-title">Other analysed commits</div>
            {list.slice(0, 5).map((s) => (
              <div
                key={s.commit_oid + s.session_id}
                style={{
                  display: "flex",
                  gap: 8,
                  padding: "6px 0",
                  borderTop: "1px solid var(--bp-border)",
                  fontSize: 12,
                }}
              >
                <Tag minimal style={{ fontFamily: "monospace" }}>
                  {s.commit_oid.slice(0, 7)}
                </Tag>
                <span style={{ color: "var(--bp-text-muted)" }}>
                  {s.message_count} msgs · {s.tool_call_count} tools
                </span>
              </div>
            ))}
          </div>
        ) : null}
      </div>
    );
  }

  return (
    <>
      {forCommit.map((s) => (
        <div className="wb-detail-section" key={s.session_id}>
          <div className="wb-detail-title">Session</div>
          <div className="wb-detail-grid">
            <div className="wb-detail-key">Analysed</div>
            <div className="wb-detail-val mono">{s.analyzed_at}</div>
            <div className="wb-detail-key">Session</div>
            <div className="wb-detail-val mono">{s.session_id}</div>
            <div className="wb-detail-key">Messages</div>
            <div className="wb-detail-val mono">{s.message_count}</div>
            <div className="wb-detail-key">Tool calls</div>
            <div className="wb-detail-val mono">{s.tool_call_count}</div>
            <div className="wb-detail-key">Files edited</div>
            <div className="wb-detail-val mono">{s.edited_count}</div>
            <div className="wb-detail-key">Files read</div>
            <div className="wb-detail-val mono">{s.consulted_count}</div>
            {s.uncertainty_count > 0 ? (
              <>
                <div className="wb-detail-key">Uncertainty</div>
                <div className="wb-detail-val">
                  <Tag intent="warning" minimal>
                    {s.uncertainty_count} signal
                    {s.uncertainty_count === 1 ? "" : "s"}
                  </Tag>
                </div>
              </>
            ) : null}
          </div>
        </div>
      ))}
    </>
  );
}
