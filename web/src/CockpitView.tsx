import { useEffect, useMemo, useState } from "react";
import { Button, Code, NonIdealState, Spinner, Tag } from "@blueprintjs/core";

import {
  api,
  type ConfidenceFactor,
  type PromptDimension,
  type PromptMaturity,
  type ReviewerCockpit,
  type ReviewPoint,
} from "./api";

// ─────────────────────────────────────────────────────────────────────────────
// Reviewer Cockpit — "Should I trust this PR?" (roadmap §4 + §6).
//
// Left: commits ranked by review-worthiness. Right: the compact cockpit card —
// merge confidence, provenance, sandbox proof, tests, and the files to review
// first, with the prompt-maturity coach (scores the delegation, not the dev).
// ─────────────────────────────────────────────────────────────────────────────

export function CockpitView({
  onOpenReplay,
  branch,
}: {
  onOpenReplay?: (oid: string) => void;
  branch?: string | null;
}) {
  const [points, setPoints] = useState<ReviewPoint[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);

  useEffect(() => {
    setPoints(null);
    setError(null);
    api
      .reviewPoints(200, 0.0, branch)
      .then((ps) => {
        setPoints(ps);
        // Keep the selection if it's still on this branch, else jump to the top.
        setSelected((cur) =>
          cur && ps.some((p) => p.commit_oid === cur) ? cur : ps[0]?.commit_oid ?? null,
        );
      })
      .catch((e) => setError(String(e)));
  }, [branch]);

  if (error) {
    return <NonIdealState icon="error" title="Failed to load" description={error} />;
  }
  if (!points) {
    return <NonIdealState icon={<Spinner size={20} />} title="Analysing commits…" />;
  }

  return (
    <div className="ckp-shell">
      <div className="ckp-list">
        <div className="rpl-pane-head">
          <span>Review priority</span>
          {branch ? (
            <Tag minimal icon="git-branch" intent="primary" title="Scoped to the picked branch">
              {branch}
            </Tag>
          ) : null}
          <Tag minimal round>
            {points.length}
          </Tag>
        </div>
        <div className="ckp-list-body">
          {points.length === 0 ? (
            <div className="rpl-empty-hint">
              {branch ? `No commits on ${branch} to review.` : "No commits analysed yet."}
            </div>
          ) : (
            points.map((p) => (
              <button
                key={p.commit_oid}
                className={"ckp-list-row" + (p.commit_oid === selected ? " active" : "")}
                onClick={() => setSelected(p.commit_oid)}
              >
                <span className="ckp-list-oid">{p.short_oid.slice(0, 7)}</span>
                <span className="ckp-list-msg" title={p.message}>
                  {p.message.split("\n")[0]}
                </span>
                <ScorePip score={p.score} />
              </button>
            ))
          )}
        </div>
      </div>
      <div className="ckp-detail">
        {selected ? (
          <CockpitCard oid={selected} onOpenReplay={onOpenReplay} />
        ) : (
          <div className="rpl-empty-hint" style={{ padding: 32 }}>
            Select a commit.
          </div>
        )}
      </div>
    </div>
  );
}

function ScorePip({ score }: { score: number }) {
  const pct = Math.min(100, Math.round(score * 100));
  const cls = score >= 0.6 ? "high" : score >= 0.4 ? "med" : "low";
  return <span className={"ckp-pip " + cls}>{pct}</span>;
}

// ── The cockpit card ─────────────────────────────────────────────────────────

function CockpitCard({
  oid,
  onOpenReplay,
}: {
  oid: string;
  onOpenReplay?: (oid: string) => void;
}) {
  const [c, setC] = useState<ReviewerCockpit | null>(null);
  const [coach, setCoach] = useState<PromptMaturity | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let live = true;
    setLoading(true);
    setC(null);
    setCoach(null);
    api
      .cockpit(oid)
      .then((d) => live && setC(d))
      .catch(() => live && setC(null))
      .finally(() => live && setLoading(false));
    api
      .promptScore({ oid })
      .then((d) => live && setCoach(d))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [oid]);

  if (loading && !c) {
    return <NonIdealState icon={<Spinner size={20} />} title="Building cockpit…" />;
  }
  if (!c) {
    return <NonIdealState icon="error" title="No cockpit data" />;
  }

  return (
    <div className="ckp-card">
      <div className="ckp-card-head">
        <div>
          <div className="ckp-card-msg">{c.message}</div>
          <div className="ckp-card-sub">
            <Code>{c.short_oid}</Code> · {c.author}
          </div>
        </div>
        {onOpenReplay ? (
          <Button icon="play" text="Replay run" onClick={() => onOpenReplay(c.oid)} />
        ) : null}
      </div>

      <div className="ckp-confidence">
        <ConfidenceGauge value={c.merge_confidence} risk={c.risk} breakdown={c.confidence_breakdown} />
        <div className="ckp-confidence-meta">
          <Row k="Provenance" v={c.provenance} />
          {c.model ? <Row k="Model" v={c.model} /> : null}
          <Row
            k="Sandbox"
            v={c.sandbox ? `${c.sandbox} proof` : "no sandbox record"}
            dim={!c.sandbox}
          />
          <Row
            k="Network"
            v={`${c.net_blocked} blocked · ${c.net_allowed} allowed`}
            dim={c.net_blocked === 0 && c.net_allowed === 0}
          />
          <Row
            k="Tests"
            v={
              c.tests_passed == null && c.tests_failed == null
                ? "no test metrics"
                : `${c.tests_passed ?? 0} passed, ${c.tests_failed ?? 0} failed`
            }
            intent={(c.tests_failed ?? 0) > 0 ? "danger" : undefined}
            dim={c.tests_passed == null && c.tests_failed == null}
          />
          <Row
            k="Integrity"
            v={`${c.integrity_level} (${c.integrity_score.toFixed(2)})`}
            intent={
              c.integrity_level === "Violation"
                ? "danger"
                : c.integrity_level === "Warning"
                  ? "warning"
                  : undefined
            }
          />
        </div>
      </div>

      <div className="ckp-section">
        <div className="ckp-section-head">Review first</div>
        {c.review_first.length === 0 ? (
          <div className="rpl-empty-hint">
            No high-signal files surfaced — nothing edited without context.
          </div>
        ) : (
          <ol className="ckp-review-first">
            {c.review_first.map((f, i) => (
              <li key={f.path} className={"sev-" + f.severity}>
                <span className="ckp-rf-n">{i + 1}</span>
                <span className="ckp-rf-path">{f.path}</span>
                <span className="ckp-rf-reason">{f.reason}</span>
              </li>
            ))}
          </ol>
        )}
      </div>

      <PromptCoach coach={coach} maturity={c.prompt_maturity} />
    </div>
  );
}

function Row({
  k,
  v,
  intent,
  dim,
}: {
  k: string;
  v: string;
  intent?: "danger" | "warning";
  dim?: boolean;
}) {
  const color = intent === "danger" ? "var(--bp-red)" : intent === "warning" ? "var(--bp-orange)" : undefined;
  return (
    <div className="ckp-row">
      <span className="ckp-row-k">{k}</span>
      <span className="ckp-row-v" style={{ color, opacity: dim ? 0.55 : 1 }}>
        {v}
      </span>
    </div>
  );
}

function ConfidenceGauge({
  value,
  risk,
  breakdown,
}: {
  value: number;
  risk: string;
  breakdown: ConfidenceFactor[];
}) {
  const color =
    risk === "low" ? "var(--bp-green-hi)" : risk === "medium" ? "var(--bp-orange)" : "var(--bp-red)";
  const penalties = breakdown.filter((f) => f.delta < 0);
  return (
    <div className="ckp-gauge">
      <div className="ckp-gauge-num" style={{ color }}>
        {value}
        <span className="ckp-gauge-den">/100</span>
      </div>
      <div className="ckp-gauge-label">merge confidence</div>
      <div className="ckp-gauge-track">
        <div className="ckp-gauge-fill" style={{ width: `${value}%`, background: color }} />
      </div>
      <Tag
        minimal
        intent={risk === "low" ? "success" : risk === "medium" ? "warning" : "danger"}
        className="ckp-gauge-risk"
      >
        {risk} risk
      </Tag>
      <ul className="ckp-breakdown">
        <li className="ckp-bd-base">
          <span className="ckp-bd-label">baseline</span>
          <span className="ckp-bd-detail" />
          <span className="ckp-bd-delta">100</span>
        </li>
        {breakdown.map((f) => (
          <li key={f.label} className={"ckp-bd-row st-" + f.status}>
            <span className="ckp-bd-label">{f.label}</span>
            <span className="ckp-bd-detail">{f.detail}</span>
            <span className="ckp-bd-delta">
              {f.delta < 0 ? f.delta : f.status === "unmeasured" ? "–" : "✓"}
            </span>
          </li>
        ))}
        {penalties.length === 0 ? (
          <li className="ckp-bd-clean">no deductions — all checks clean</li>
        ) : null}
      </ul>
    </div>
  );
}

// ── Prompt-maturity coach ────────────────────────────────────────────────────

export function PromptCoach({
  coach,
  maturity,
}: {
  coach: PromptMaturity | null;
  maturity?: number | null;
}) {
  const [showUpgrade, setShowUpgrade] = useState(false);
  const score = coach?.score ?? maturity ?? null;
  const cls = useMemo(() => {
    if (score == null) return "none";
    return score >= 70 ? "high" : score >= 45 ? "med" : "low";
  }, [score]);

  if (score == null && (!coach || coach.words === 0)) {
    return (
      <div className="ckp-section">
        <div className="ckp-section-head">Prompt maturity</div>
        <div className="rpl-empty-hint">No captured prompt for this commit.</div>
      </div>
    );
  }

  return (
    <div className="ckp-section ckp-coach">
      <div className="ckp-section-head">
        Prompt maturity
        <span className="ckp-coach-disclaimer">scores the task delegation, not the developer</span>
      </div>
      <div className="ckp-coach-row">
        <span className={"ckp-coach-score " + cls}>
          {score != null ? Math.round(score) : "—"}
          <span className="ckp-coach-den">/100</span>
        </span>
        {coach ? <span className="ckp-coach-level">{coach.level}</span> : null}
        {coach && coach.flags.length > 0 ? (
          <span className="ckp-coach-flags">
            {coach.flags.map((f) => (
              <Tag key={f} minimal intent="warning">
                {f}
              </Tag>
            ))}
          </span>
        ) : null}
      </div>
      {coach && coach.dimensions.length > 0 ? (
        <PromptDimensions dims={coach.dimensions} />
      ) : null}
      {coach?.suggested_upgrade ? (
        <div className="ckp-coach-upgrade">
          <Button
            minimal
            small
            icon={showUpgrade ? "chevron-down" : "chevron-right"}
            text="Suggested upgrade"
            onClick={() => setShowUpgrade((v) => !v)}
          />
          {showUpgrade ? <pre className="rpl-pre">{coach.suggested_upgrade}</pre> : null}
        </div>
      ) : null}
    </div>
  );
}

// Per-dimension composition of the prompt score (7 weighted sub-signals). Each
// row shows the signal strength (fill) and its weighted points of the max.
function PromptDimensions({ dims }: { dims: PromptDimension[] }) {
  return (
    <div className="ckp-pdims">
      {dims.map((d) => {
        const ratio = d.max_points > 0 ? d.points / d.max_points : 0;
        const cls = ratio >= 0.66 ? "high" : ratio >= 0.33 ? "med" : "low";
        return (
          <div key={d.label} className="ckp-pdim" title={`${d.label}: ${d.signal.toFixed(0)}% strength`}>
            <span className="ckp-pdim-label">{d.label}</span>
            <span className="ckp-pdim-track">
              <span className={"ckp-pdim-fill " + cls} style={{ width: `${d.signal}%` }} />
            </span>
            <span className="ckp-pdim-pts">
              {d.points.toFixed(1)}
              <span className="ckp-pdim-max">/{d.max_points.toFixed(0)}</span>
            </span>
          </div>
        );
      })}
    </div>
  );
}
