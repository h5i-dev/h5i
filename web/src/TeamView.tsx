import { useEffect, useMemo, useState } from "react";
import { NonIdealState, Spinner } from "@blueprintjs/core";
import { api, TeamCompareRow, TeamRun, TeamStatus } from "./api";

// The team lifecycle is a typed sequence — the order carries information the
// reviewer needs, so it earns a numbered rail. Phase ids come straight from
// src/team.rs; `no_verdict` is the alternate terminal of the verify stage.
const RAIL = [
  { id: "draft", label: "Draft", hint: "roster created · agents working sealed" },
  { id: "sealed_submit", label: "Seal", hint: "submissions frozen, immutable" },
  { id: "discuss", label: "Review", hint: "permissioned peer review" },
  { id: "verified", label: "Verify", hint: "neutral verifier replays each candidate" },
  { id: "applied", label: "Apply", hint: "one winner committed to base" },
] as const;

function railIndex(phase: string): number {
  if (phase === "created") return 0;
  if (phase === "no_verdict") return 3; // reached verify, produced no winner
  const i = RAIL.findIndex((p) => p.id === phase);
  return i < 0 ? 0 : i;
}

export function TeamView() {
  const [teams, setTeams] = useState<TeamRun[] | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [detail, setDetail] = useState<TeamStatus | null>(null);
  const [compare, setCompare] = useState<TeamCompareRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .teams()
      .then((runs) => {
        setTeams(runs);
        setSelected((cur) => cur ?? runs[0]?.id ?? null);
      })
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    if (!selected) return;
    setDetail(null);
    setCompare(null);
    Promise.all([api.team(selected), api.teamCompare(selected)])
      .then(([d, c]) => {
        setDetail(d);
        setCompare(c);
      })
      .catch((e) => setError(String(e)));
  }, [selected]);

  const verBySubmission = useMemo(() => {
    const m = new Map<string, TeamStatus["run"]["verifications"][number]>();
    for (const v of detail?.run.verifications ?? []) m.set(v.submission_id, v);
    return m;
  }, [detail]);

  if (error)
    return (
      <div className="team-view team-view-empty">
        <NonIdealState
          icon="error"
          title="Team data unavailable"
          description={error}
        />
      </div>
    );
  if (!teams)
    return (
      <div className="team-view team-view-empty">
        <Spinner size={28} />
      </div>
    );
  if (teams.length === 0)
    return (
      <div className="team-view team-view-empty">
        <div className="team-zero">
          <div className="team-zero-mark" />
          <h2>No ensembles yet</h2>
          <p>
            Run the same task across several agents in sealed lanes, then merge
            the one result a neutral verifier clears.
          </p>
          <code className="team-zero-cmd">h5i team create &lt;name&gt;</code>
        </div>
      </div>
    );

  const run = detail?.run;
  const activeIdx = run ? railIndex(run.phase) : 0;
  const failed = run?.phase === "no_verdict";
  const winnerSub = run?.verdict?.selected_submission ?? null;

  return (
    <div className="team-view">
      <aside className="team-sidebar">
        <div className="team-sidebar-head">Ensembles</div>
        {teams.map((t) => {
          const idx = railIndex(t.phase);
          return (
            <button
              key={t.id}
              className={`team-run-btn ${selected === t.id ? "active" : ""}`}
              onClick={() => setSelected(t.id)}
            >
              <span className="team-run-id">{t.id}</span>
              <span className="team-run-phase">
                {RAIL[idx]?.label ?? t.phase}
              </span>
            </button>
          );
        })}
      </aside>

      <main className="team-main">
        {!run ? (
          <div className="team-loading">
            <Spinner size={24} />
          </div>
        ) : (
          <>
            <header className="team-head">
              <div className="team-head-id">
                <h2>{run.name}</h2>
                <div className="team-head-meta">
                  <span className="team-base">base {run.base_oid.slice(0, 10)}</span>
                  <span className="team-dot-sep">·</span>
                  <span>{run.agents.length} agents</span>
                  <span className="team-dot-sep">·</span>
                  <span>{run.submissions.length} sealed</span>
                </div>
              </div>
              <span className={`team-phase-badge ${failed ? "fail" : ""}`}>
                {run.phase}
              </span>
            </header>

            {/* Signature: the settlement rail — the run's heartbeat. */}
            <ol className="team-rail" aria-label="ensemble lifecycle">
              {RAIL.map((p, i) => {
                const state =
                  failed && i === 3
                    ? "fail"
                    : i < activeIdx
                      ? "done"
                      : i === activeIdx
                        ? "active"
                        : "future";
                return (
                  <li
                    key={p.id}
                    className={`team-rail-stage is-${state} ${i <= activeIdx ? "lit" : ""}`}
                    title={p.hint}
                  >
                    <span className="team-rail-dot">
                      <span className="team-rail-num">{i + 1}</span>
                    </span>
                    <span className="team-rail-label">{p.label}</span>
                    <span className="team-rail-hint">{p.hint}</span>
                  </li>
                );
              })}
            </ol>

            {/* The lanes — sealed, independent attempts, side by side. */}
            <section className="team-lanes">
              {run.agents.map((a) => {
                const sub = run.submissions.find(
                  (s) => s.id === a.latest_submission_id,
                );
                const ver = sub ? verBySubmission.get(sub.id) : undefined;
                const isWinner = !!winnerSub && sub?.id === winnerSub;
                const isSealed = activeIdx >= 1 && !!sub;
                return (
                  <article
                    className={`team-lane ${isWinner ? "winner" : ""} ${
                      winnerSub && !isWinner ? "dimmed" : ""
                    }`}
                    key={a.agent_id}
                  >
                    {isWinner ? <div className="team-lane-ribbon">verdict</div> : null}
                    <div className="team-lane-top">
                      <strong className="team-lane-name">
                        {a.display_label || a.agent_id}
                      </strong>
                      {/* The winner's ribbon already occupies the top-right; the
                          seal tag would collide with it, so show it only on
                          non-winning lanes. */}
                      {isWinner ? null : isSealed ? (
                        <span className="team-seal">sealed</span>
                      ) : (
                        <span className="team-seal open">{a.state}</span>
                      )}
                    </div>
                    <div className="team-lane-ident">
                      {a.model ? (
                        <span className="team-chip model">{shortModel(a.model)}</span>
                      ) : null}
                      {a.role ? <span className="team-chip">{a.role}</span> : null}
                      <span className="team-chip ghost">{a.isolation_claim}</span>
                    </div>

                    {sub ? (
                      <div className="team-lane-diff">
                        <span className="d-files">{sub.files_changed} files</span>
                        <span className="d-ins">+{sub.insertions}</span>
                        <span className="d-del">−{sub.deletions}</span>
                        <span
                          className={`d-indep ${sub.independent ? "" : "influenced"}`}
                        >
                          {sub.independent ? "independent" : "influenced"}
                        </span>
                      </div>
                    ) : (
                      <div className="team-lane-nosub">no submission</div>
                    )}

                    <div className="team-gates">
                      {ver ? (
                        <>
                          <Gate ok={ver.applies_cleanly} label="applies" />
                          <Gate ok={ver.tests_passed} label="tests" />
                          {ver.failure ? (
                            <span className="team-gate-fail" title={ver.failure}>
                              {ver.failure.slice(0, 28)}
                            </span>
                          ) : null}
                        </>
                      ) : (
                        <span className="team-gate-pending">
                          {activeIdx >= 3 ? "not verified" : "awaiting verify"}
                        </span>
                      )}
                    </div>
                  </article>
                );
              })}
            </section>

            {/* Verdict — delivered with the weight of a settlement. */}
            {run.verdict ? (
              <section className="team-verdict-card">
                <div className="team-verdict-rail" />
                <div className="team-verdict-body">
                  <div className="team-verdict-head">
                    <span className="team-verdict-eyebrow">neutral verdict</span>
                    <span
                      className={`team-verdict-pick ${
                        run.verdict.selected_submission ? "" : "none"
                      }`}
                    >
                      {run.verdict.selected_submission ?? "no verdict"}
                    </span>
                  </div>
                  <div className="team-verdict-meta">
                    {run.verdict.method}
                    <span className="team-dot-sep">·</span>
                    auto-apply {run.verdict.can_auto_apply ? "enabled" : "off"}
                  </div>
                  {run.verdict.reasons.length ? (
                    <ul className="team-verdict-reasons">
                      {run.verdict.reasons.map((r) => (
                        <li key={r}>{r}</li>
                      ))}
                    </ul>
                  ) : null}
                </div>
              </section>
            ) : null}

            {/* Compare — the dense readout. */}
            <section className="team-section">
              <h3 className="team-section-title">Compare</h3>
              <table className="team-table">
                <thead>
                  <tr>
                    <th>Agent</th>
                    <th>Submitted</th>
                    <th className="num">Files</th>
                    <th className="num">+</th>
                    <th className="num">−</th>
                    <th>Latest capture</th>
                  </tr>
                </thead>
                <tbody>
                  {(compare ?? []).map((r) => (
                    <tr key={r.agent_id}>
                      <td className="mono">{r.agent_id}</td>
                      <td>
                        {r.submitted ? (
                          <span className="mono dim">{r.submission_id}</span>
                        ) : (
                          <span className="team-no">no</span>
                        )}
                      </td>
                      <td className="num">{r.files_changed}</td>
                      <td className="num pos">{r.insertions}</td>
                      <td className="num neg">{r.deletions}</td>
                      <td className="mono dim">
                        {r.last_tool
                          ? `${r.last_tool} ${r.last_result ?? ""}`
                          : "—"}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </section>

            <section className="team-section">
              <h3 className="team-section-title">Recent events</h3>
              <ul className="team-events">
                {(detail?.events ?? [])
                  .slice(-14)
                  .reverse()
                  .map((e) => (
                    <li key={e.id} className="team-event">
                      <span className="team-event-kind">{e.kind}</span>
                      <span className="team-event-actor">{e.actor}</span>
                      <span className="team-event-ts">{e.ts}</span>
                    </li>
                  ))}
              </ul>
            </section>
          </>
        )}
      </main>
    </div>
  );
}

function Gate({ ok, label }: { ok: boolean; label: string }) {
  return (
    <span className={`team-gate ${ok ? "pass" : "fail"}`}>
      <span className="team-gate-mark">{ok ? "✓" : "✕"}</span>
      {label}
    </span>
  );
}

function shortModel(m: string): string {
  return m.replace(/^claude-/, "").replace(/-\d{8}$/, "");
}
