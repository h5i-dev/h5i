import { useEffect, useState } from "react";
import { NonIdealState, Spinner, Tag } from "@blueprintjs/core";

import { api, type Commit, type IntentGraph, type IntentNode } from "./api";

// Refs tab: shows direct intent-graph neighbours of the selected commit
// (parents, children, and any explicit `caused_by` links).

export function RefsTab({
  commit,
  onSelect,
}: {
  commit: Commit;
  onSelect: (oid: string) => void;
}) {
  const [graph, setGraph] = useState<IntentGraph | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setGraph(null);
    setError(null);
    api
      .intentGraph(120, "prompt")
      .then(setGraph)
      .catch((e) => setError(String(e)));
  }, []); // load once — graph is repo-wide

  if (error) {
    return (
      <NonIdealState icon="error" title="Failed to load graph" description={error} />
    );
  }
  if (!graph) {
    return <NonIdealState icon={<Spinner size={20} />} title="Loading…" />;
  }

  const nodeByOid = new Map(graph.nodes.map((n) => [n.oid, n]));
  const parents: IntentNode[] = [];
  const children: IntentNode[] = [];
  const causal: IntentNode[] = [];

  for (const e of graph.edges) {
    if (e.from === commit.git_oid) {
      const target = nodeByOid.get(e.to);
      if (!target) continue;
      if (e.kind === "causal") causal.push(target);
      else parents.push(target);
    } else if (e.to === commit.git_oid) {
      const source = nodeByOid.get(e.from);
      if (!source) continue;
      if (e.kind === "causal") causal.push(source);
      else children.push(source);
    }
  }

  const total = parents.length + children.length + causal.length;
  if (total === 0) {
    return (
      <NonIdealState
        icon="link"
        title="No linked commits"
        description="This commit has no parent, child, or causal links in the loaded graph."
      />
    );
  }

  return (
    <div style={{ padding: 0 }}>
      <RefList title="Children" intent="primary" nodes={children} onSelect={onSelect} />
      <RefList title="Parents" intent="none" nodes={parents} onSelect={onSelect} />
      <RefList title="Caused by" intent="warning" nodes={causal} onSelect={onSelect} />
    </div>
  );
}

function RefList({
  title,
  intent,
  nodes,
  onSelect,
}: {
  title: string;
  intent: "primary" | "warning" | "none";
  nodes: IntentNode[];
  onSelect: (oid: string) => void;
}) {
  if (nodes.length === 0) return null;
  return (
    <div className="wb-detail-section">
      <div className="wb-detail-title">
        {title}{" "}
        <Tag minimal style={{ marginLeft: 6, fontFamily: "monospace", fontSize: 11 }}>
          {nodes.length}
        </Tag>
      </div>
      {nodes.map((n) => (
        <div
          key={n.oid}
          onClick={() => onSelect(n.oid)}
          style={{
            display: "flex",
            gap: 8,
            padding: "6px 0",
            borderTop: "1px solid var(--bp-border)",
            cursor: "pointer",
            alignItems: "baseline",
          }}
        >
          <Tag
            minimal
            intent={intent !== "none" ? intent : undefined}
            style={{ fontFamily: "monospace", flexShrink: 0 }}
          >
            {n.short_oid.slice(0, 7)}
          </Tag>
          <span
            style={{
              fontSize: 12,
              color: "var(--bp-text)",
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
              flex: 1,
              minWidth: 0,
            }}
            title={n.message}
          >
            {n.message.split("\n")[0]}
          </span>
        </div>
      ))}
    </div>
  );
}
