import { useEffect, useState } from "react";
import {
  Callout,
  HTMLTable,
  NonIdealState,
  Spinner,
  Tag,
} from "@blueprintjs/core";

import { api, type ReviewPoint } from "./api";

// Review mode — flagged commits sorted by review-score, mirrors `h5i notes review`.
// The user can click a row to jump into Workbench mode at that commit.

export function ReviewView({
  onSelect,
}: {
  onSelect: (oid: string) => void;
}) {
  const [points, setPoints] = useState<ReviewPoint[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .reviewPoints(200, 0.25)
      .then(setPoints)
      .catch((e) => setError(String(e)));
  }, []);

  if (error) {
    return (
      <NonIdealState icon="error" title="Failed to load review points" description={error} />
    );
  }
  if (!points) {
    return <NonIdealState icon={<Spinner size={20} />} title="Analysing…" />;
  }
  if (points.length === 0) {
    return (
      <div style={{ padding: 24 }}>
        <Callout intent="success" icon="tick">
          No commits crossed the review threshold.
        </Callout>
      </div>
    );
  }

  return (
    <div style={{ padding: "0 0 32px" }}>
      <div className="wb-pane-header" style={{ position: "sticky", top: 0 }}>
        Suggested review points
        <Tag minimal round style={{ marginLeft: 8 }}>
          {points.length}
        </Tag>
      </div>
      <HTMLTable className="wb-commits-table" interactive compact>
        <thead>
          <tr>
            <th style={{ width: 80 }}>Commit</th>
            <th>Message</th>
            <th style={{ width: 80 }}>Score</th>
            <th style={{ width: 200 }}>Triggers</th>
            <th style={{ width: 100 }}>Author</th>
          </tr>
        </thead>
        <tbody>
          {points.map((p) => (
            <tr key={p.commit_oid} onClick={() => onSelect(p.commit_oid)}>
              <td>
                <span className="wb-oid">{p.short_oid.slice(0, 7)}</span>
              </td>
              <td className="wb-msg" title={p.message}>
                {p.message.split("\n")[0]}
              </td>
              <td>
                <ScoreBar score={p.score} />
              </td>
              <td style={{ fontSize: 12 }}>
                <div
                  style={{
                    display: "flex",
                    flexWrap: "wrap",
                    gap: 3,
                  }}
                >
                  {p.triggers.slice(0, 4).map((t, i) => (
                    <Tag key={i} minimal style={{ fontFamily: "monospace", fontSize: 11 }}>
                      {t.rule_id.toLowerCase()}
                    </Tag>
                  ))}
                </div>
              </td>
              <td
                style={{
                  fontSize: 12,
                  color: "var(--bp-text-muted)",
                  whiteSpace: "nowrap",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  maxWidth: 100,
                }}
              >
                {p.author.split(" ")[0]}
              </td>
            </tr>
          ))}
        </tbody>
      </HTMLTable>
    </div>
  );
}

function ScoreBar({ score }: { score: number }) {
  const pct = Math.round(score * 100);
  const intent = score >= 0.6 ? "danger" : score >= 0.4 ? "warning" : "primary";
  const color =
    intent === "danger"
      ? "var(--bp-red)"
      : intent === "warning"
        ? "var(--bp-orange)"
        : "var(--bp-blue)";
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
      <span
        className="mono"
        style={{
          fontVariantNumeric: "tabular-nums",
          fontSize: 12,
          width: 28,
          textAlign: "right",
        }}
      >
        {pct}
      </span>
      <div
        style={{
          flex: 1,
          height: 4,
          background: "var(--bp-elev)",
          borderRadius: 2,
          overflow: "hidden",
          minWidth: 40,
        }}
      >
        <div
          style={{
            width: `${pct}%`,
            height: "100%",
            background: color,
          }}
        />
      </div>
    </div>
  );
}
