import type { Commit, Repo } from "./api";

// Right-pane placeholder that will, in subsequent iterations, fetch and display:
// - Sessions analyzed for this commit (via /api/session-log/list filtered)
// - Files this commit's session touched
// - Linked context milestones
// For the first slice we surface what's already available on the commit object
// to validate the linked-pane interaction model.

export function CrossRef({
  commit,
  repo,
}: {
  commit: Commit | null;
  repo: Repo | null;
}) {
  if (!commit) {
    return (
      <div className="wb-pane-empty">Select a commit to see cross-references.</div>
    );
  }

  return (
    <>
      <div className="wb-xref-stat">
        <span className="wb-xref-stat-label">AI</span>
        <span className="wb-xref-stat-val">
          {commit.ai_model ? "Yes" : "No"}
        </span>
      </div>
      <div className="wb-xref-stat">
        <span className="wb-xref-stat-label">Tokens</span>
        <span className="wb-xref-stat-val">
          {commit.ai_tokens?.toLocaleString() ?? "—"}
        </span>
      </div>
      <div className="wb-xref-stat">
        <span className="wb-xref-stat-label">Tests</span>
        <span className="wb-xref-stat-val">
          {commit.test_total ?? "—"}
        </span>
      </div>
      <div className="wb-xref-stat">
        <span className="wb-xref-stat-label">AST files</span>
        <span className="wb-xref-stat-val">
          {commit.ast_file_count ?? "—"}
        </span>
      </div>
      <div className="wb-xref-stat">
        <span className="wb-xref-stat-label">CRDT</span>
        <span className="wb-xref-stat-val">{commit.has_crdt ? "Yes" : "—"}</span>
      </div>

      <div style={{ padding: "16px", fontSize: 12, color: "var(--bp-text-dim)", lineHeight: 1.5 }}>
        Sessions, file footprints, and intent links land here in the next
        iteration. Repo total: <strong style={{ color: "var(--bp-text)" }}>{repo?.total_commits ?? "—"}</strong>{" "}
        commits, <strong style={{ color: "var(--bp-violet)" }}>{repo?.ai_commits ?? "—"}</strong>{" "}
        AI-assisted.
      </div>
    </>
  );
}
