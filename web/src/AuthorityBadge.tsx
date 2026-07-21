/**
 * The authority vocabulary — h5i's epistemic visual language.
 *
 * Every claim is badged with *how it is known*: `enforced` (the kernel or
 * proxy acted), `verified` (neutral re-execution), `observed` (live pid /
 * host-side record), `reported` (a hook or the agent said so), `inferred`
 * (deterministic classifier), `unknown` (no evidence — shown as absent,
 * never dressed as success). Semantic color belongs to authority, not
 * decoration, and every badge can open a Why? drawer with its evidence.
 */
import { Popover } from "@blueprintjs/core";
import type { AttentionItem, Authority, EvidenceRef } from "./attention";

const TITLES: Record<Authority, string> = {
  enforced: "The kernel or egress proxy acted — a denial actually fired",
  verified: "Neutrally re-executed in a fresh sandboxed worktree",
  observed: "Seen by the host: live pid, lock, spool, or record",
  reported: "Claimed by a hook or the agent itself — honest but unaudited",
  inferred: "Produced by a deterministic classifier over evidence",
  unknown: "No evidence exists, or the host cannot produce it",
};

export function AuthorityBadge({ authority }: { authority: Authority }) {
  return (
    <span className={`auth-badge auth-${authority}`} title={TITLES[authority]}>
      {authority}
    </span>
  );
}

/** The distinct authorities present in a set of evidence refs, trust order. */
export function distinctAuthorities(evidence: EvidenceRef[]): Authority[] {
  const order: Authority[] = [
    "enforced",
    "verified",
    "observed",
    "reported",
    "inferred",
    "unknown",
  ];
  const present = new Set(evidence.map((e) => e.authority));
  return order.filter((a) => present.has(a));
}

/**
 * The Why? drawer: reasons, evidence references (each with its authority),
 * and the exact terminal commands that act on the item. The UI never shows
 * a claim without this one click away.
 */
export function WhyDrawer({
  item,
  children,
}: {
  item: AttentionItem;
  children: React.ReactNode;
}) {
  return (
    <Popover
      minimal
      placement="right-start"
      content={
        <div className="why-drawer">
          <div className="why-title">{item.title}</div>
          <div className="why-section">Why</div>
          {item.reasons.map((r, i) => (
            <div className="why-reason" key={i}>
              {r}
            </div>
          ))}
          <div className="why-section">Evidence</div>
          {item.evidence.length === 0 ? (
            <div className="why-reason">
              <AuthorityBadge authority="unknown" /> none recorded
            </div>
          ) : (
            item.evidence.map((e, i) => (
              <div className="why-evidence" key={i}>
                <AuthorityBadge authority={e.authority} />
                <code>
                  {e.kind}:{e.id}
                </code>
                {e.note ? <span className="why-note">{e.note}</span> : null}
              </div>
            ))
          )}
          <div className="why-section">Act in your terminal</div>
          {item.commands.map((cmd) => (
            <button
              key={cmd}
              type="button"
              className="why-cmd"
              title="Copy command"
              onClick={() => navigator.clipboard?.writeText(cmd)}
            >
              $ {cmd}
            </button>
          ))}
        </div>
      }
    >
      {children}
    </Popover>
  );
}
