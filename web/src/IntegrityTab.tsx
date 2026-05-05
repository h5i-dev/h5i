import { useEffect, useState } from "react";
import { Callout, NonIdealState, Spinner, Tag } from "@blueprintjs/core";

import { api, type Commit, type IntegrityReport } from "./api";

// Integrity tab: runs the same per-commit audit that the legacy UI exposes
// inline. Result auto-loads when a commit is selected so the user doesn't
// have to click a button — the right-pane is meant to feel reactive.

export function IntegrityTab({ commit }: { commit: Commit }) {
  const [report, setReport] = useState<IntegrityReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setReport(null);
    setError(null);
    api
      .integrityCommit(commit.git_oid)
      .then(setReport)
      .catch((e) => setError(String(e)));
  }, [commit.git_oid]);

  if (error) {
    return (
      <NonIdealState icon="error" title="Audit failed" description={error} />
    );
  }
  if (!report) {
    return <NonIdealState icon={<Spinner size={20} />} title="Running rules…" />;
  }

  const intent =
    report.level === "Valid"
      ? "success"
      : report.level === "Warning"
        ? "warning"
        : "danger";

  return (
    <>
      <div className="wb-detail-section" style={{ display: "flex", alignItems: "center", gap: 14 }}>
        <div>
          <div
            style={{
              fontSize: 28,
              fontWeight: 600,
              fontVariantNumeric: "tabular-nums",
              letterSpacing: "-0.02em",
              color: "var(--bp-text)",
              lineHeight: 1,
            }}
          >
            {(report.score * 100).toFixed(0)}
            <span style={{ fontSize: 14, color: "var(--bp-text-dim)" }}>%</span>
          </div>
          <div className="wb-detail-title" style={{ marginTop: 4, marginBottom: 0 }}>
            Integrity score
          </div>
        </div>
        <Tag intent={intent} large>
          {report.level.toUpperCase()}
        </Tag>
      </div>

      {report.findings.length === 0 ? (
        <div style={{ padding: 16 }}>
          <Callout intent="success" icon="tick" compact>
            No findings — all rules passed.
          </Callout>
        </div>
      ) : (
        <div className="wb-detail-section">
          <div className="wb-detail-title">Findings ({report.findings.length})</div>
          {report.findings.map((f, i) => {
            const fIntent =
              f.severity === "Violation"
                ? "danger"
                : f.severity === "Warning"
                  ? "warning"
                  : "primary";
            return (
              <Callout
                key={i}
                intent={fIntent}
                title={f.rule_id}
                compact
                style={{ marginTop: i === 0 ? 0 : 8 }}
              >
                {f.detail}
              </Callout>
            );
          })}
        </div>
      )}
    </>
  );
}
