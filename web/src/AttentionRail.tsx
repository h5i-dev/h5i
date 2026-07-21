/**
 * The attention rail — the workbench's persistent left column.
 *
 * Unresolved conditions stay in NEEDS YOU / ACTIVE / INFO until their backing
 * state resolves. Seen-ness is acknowledgement only: it suppresses unread
 * counts and notifications but never moves a live condition into "quiet".
 * Clicking a row opens the entity and records that acknowledgement in both
 * the web and `h5i status`. The footer is honest about freshness:
 * `live` means the SSE stream is connected, not that we hope it is.
 */
import { Button } from "@blueprintjs/core";

import {
  ago,
  markSeen,
  type AttentionItem,
  type AttentionReport,
  type EntityRef,
} from "./attention";
import { AuthorityBadge, distinctAuthorities, WhyDrawer } from "./AuthorityBadge";

const NEEDS_YOU = new Set(["critical", "decision", "communication"]);

function Row({
  item,
  onOpen,
}: {
  item: AttentionItem;
  onOpen: (entity: EntityRef, itemId: string) => void;
}) {
  return (
    <WhyDrawer item={item}>
      <button
        type="button"
        className={`att-row att-${item.priority}${item.seen_at ? " att-seen" : ""}`}
        onClick={() => {
          // Viewing acknowledges the condition; only source-state resolution
          // removes it from the rail.
          void markSeen([item.id]).catch(() => {});
          onOpen(item.entity, item.id);
        }}
      >
        <span className={`att-dot att-dot-${item.priority}`} aria-hidden />
        <span className="att-title">{item.title}</span>
        <span className="att-badges">
          {distinctAuthorities(item.evidence).map((a) => (
            <AuthorityBadge key={a} authority={a} />
          ))}
        </span>
        <span className="att-age">{ago(item.occurred_at)}</span>
      </button>
    </WhyDrawer>
  );
}

export function AttentionRail({
  report,
  connected,
  onOpen,
  onDrained,
}: {
  report: AttentionReport | null;
  connected: boolean;
  onOpen: (entity: EntityRef, itemId: string) => void;
  /** Called after acknowledge-all so the owner can refetch the projection. */
  onDrained: () => void;
}) {
  const items = report?.items ?? [];
  const needsYou = items.filter((i) => NEEDS_YOU.has(i.priority));
  const active = items.filter((i) => i.priority === "active");
  const info = items.filter((i) => i.priority === "info");
  const unseenNeedsYou = needsYou.filter((i) => !i.seen_at);

  return (
    <aside className="att-rail" aria-label="Attention">
      <div className="att-group">
        <div className="att-head att-head-urgent">
          needs you {needsYou.length ? `(${needsYou.length})` : ""}
          {unseenNeedsYou.length > 0 ? (
            <Button
              minimal
              small
              icon="tick"
              title={`Acknowledge ${unseenNeedsYou.length} new item(s)`}
              onClick={() =>
                void markSeen(unseenNeedsYou.map((item) => item.id))
                  .then(onDrained)
                  .catch(() => {})
              }
            />
          ) : null}
        </div>
        {needsYou.length === 0 ? (
          <div className="att-empty">nothing blocked on you</div>
        ) : (
          needsYou.map((i) => <Row key={i.id} item={i} onOpen={onOpen} />)
        )}
      </div>

      {active.length > 0 ? (
        <div className="att-group">
          <div className="att-head">active ({active.length})</div>
          {active.map((i) => (
            <Row key={i.id} item={i} onOpen={onOpen} />
          ))}
        </div>
      ) : null}

      {info.length > 0 ? (
        <div className="att-group">
          <div className="att-head">info ({info.length})</div>
          {info.map((i) => (
            <Row key={i.id} item={i} onOpen={onOpen} />
          ))}
        </div>
      ) : null}

      <div className="att-foot">
        <span
          className={`att-live ${connected ? "att-live-on" : "att-live-off"}`}
        />
        {connected ? "live · event feed connected" : "disconnected · data may be stale"}
        {report ? <span className="att-ident"> · {report.identity}</span> : null}
      </div>
    </aside>
  );
}
