import { useEffect, useState } from "react";
import {
  Callout,
  HTMLTable,
  NonIdealState,
  Spinner,
  Tag,
} from "@blueprintjs/core";

import { api, type MemorySnapshot } from "./api";

// Memory mode — full-width view of `h5i memory log` snapshots.
// Mirrors the legacy "Memory" tab. A snapshot diff viewer can be added in a
// follow-up; for now we surface the list which is what `h5i memory log` shows.

export function MemoryView() {
  const [snapshots, setSnapshots] = useState<MemorySnapshot[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .memorySnapshots()
      .then(setSnapshots)
      .catch((e) => setError(String(e)));
  }, []);

  if (error) {
    return (
      <NonIdealState icon="error" title="Failed to load snapshots" description={error} />
    );
  }
  if (!snapshots) {
    return <NonIdealState icon={<Spinner size={20} />} title="Loading…" />;
  }
  if (snapshots.length === 0) {
    return (
      <div style={{ padding: 24 }}>
        <Callout intent="none" icon="database">
          No memory snapshots. Run <code>h5i memory snapshot</code> after a
          Claude Code session to record one.
        </Callout>
      </div>
    );
  }

  return (
    <div style={{ padding: "0 0 32px" }}>
      <div className="wb-pane-header" style={{ position: "sticky", top: 0 }}>
        Memory snapshots
        <Tag minimal round style={{ marginLeft: 8 }}>
          {snapshots.length}
        </Tag>
      </div>
      <HTMLTable className="wb-commits-table" interactive compact>
        <thead>
          <tr>
            <th style={{ width: 80 }}>Commit</th>
            <th>Message</th>
            <th style={{ width: 80 }}>Files</th>
            <th style={{ width: 100 }}>Bytes</th>
            <th style={{ width: 160 }}>Timestamp</th>
          </tr>
        </thead>
        <tbody>
          {snapshots.map((s) => (
            <tr key={s.oid}>
              <td>
                <span className="wb-oid">{s.short_oid.slice(0, 7)}</span>
              </td>
              <td className="wb-msg" title={s.message}>
                {s.message.split("\n")[0]}
              </td>
              <td className="mono">{s.file_count}</td>
              <td className="mono">{formatBytes(s.total_bytes)}</td>
              <td style={{ fontSize: 12, color: "var(--bp-text-muted)" }}>
                {s.timestamp}
              </td>
            </tr>
          ))}
        </tbody>
      </HTMLTable>
    </div>
  );
}

function formatBytes(b: number): string {
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  return `${(b / 1024 / 1024).toFixed(1)} MB`;
}
