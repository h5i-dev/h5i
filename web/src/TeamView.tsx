import { useEffect, useMemo, useState } from "react";
import { HTMLTable, NonIdealState, Spinner, Tag } from "@blueprintjs/core";
import { api, TeamCompareRow, TeamRun, TeamStatus } from "./api";

export function TeamView() {
  const [teams, setTeams] = useState<TeamRun[] | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [detail, setDetail] = useState<TeamStatus | null>(null);
  const [compare, setCompare] = useState<TeamCompareRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.teams().then((runs) => {
      setTeams(runs);
      setSelected((cur) => cur ?? runs[0]?.id ?? null);
    }).catch((e) => setError(String(e)));
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

  if (error) return <NonIdealState title="Team data unavailable" description={error} />;
  if (!teams) return <div className="team-loading"><Spinner size={24} /></div>;
  if (teams.length === 0) {
    return <NonIdealState title="No teams" description="Create one with h5i team create." />;
  }

  const run = detail?.run;

  return (
    <div className="team-view">
      <aside className="team-sidebar">
        {teams.map((t) => (
          <button
            key={t.id}
            className={`team-run-btn ${selected === t.id ? "active" : ""}`}
            onClick={() => setSelected(t.id)}
          >
            <span>{t.id}</span>
            <Tag minimal>{t.phase}</Tag>
          </button>
        ))}
      </aside>
      <main className="team-main">
        {!run ? (
          <div className="team-loading"><Spinner size={24} /></div>
        ) : (
          <>
            <header className="team-head">
              <div>
                <h2>{run.name}</h2>
                <div className="team-muted">base {run.base_oid.slice(0, 12)}</div>
              </div>
              <Tag intent={run.verdict?.selected_submission ? "success" : "none"}>{run.phase}</Tag>
            </header>

            <section className="team-lanes">
              {run.agents.map((a) => {
                const sub = run.submissions.find((s) => s.id === a.latest_submission_id);
                const ver = sub ? verBySubmission.get(sub.id) : undefined;
                return (
                  <article className="team-lane" key={a.agent_id}>
                    <div className="team-lane-top">
                      <strong>{a.display_label || a.agent_id}</strong>
                      <Tag minimal>{a.isolation_claim}</Tag>
                    </div>
                    <div className="team-muted">{a.agent_id} · {a.env_id}</div>
                    <div className="team-lane-stats">
                      <span>{sub ? `${sub.files_changed} files` : "no submission"}</span>
                      <span>{sub?.independent === false ? "influenced" : "independent"}</span>
                    </div>
                    {ver ? (
                      <div className="team-gates">
                        <Tag intent={ver.applies_cleanly ? "success" : "danger"}>apply</Tag>
                        <Tag intent={ver.tests_passed ? "success" : "danger"}>tests</Tag>
                      </div>
                    ) : <div className="team-muted">no verifier evidence</div>}
                  </article>
                );
              })}
            </section>

            <section className="team-section">
              <h3>Compare</h3>
              <HTMLTable compact striped className="team-table">
                <thead><tr><th>Agent</th><th>Submitted</th><th>Files</th><th>+</th><th>-</th><th>Latest</th></tr></thead>
                <tbody>
                  {(compare ?? []).map((r) => (
                    <tr key={r.agent_id}>
                      <td>{r.agent_id}</td>
                      <td>{r.submitted ? r.submission_id : "no"}</td>
                      <td>{r.files_changed}</td>
                      <td>{r.insertions}</td>
                      <td>{r.deletions}</td>
                      <td>{r.last_tool ? `${r.last_tool} ${r.last_result ?? ""}` : "no capture"}</td>
                    </tr>
                  ))}
                </tbody>
              </HTMLTable>
            </section>

            {run.verdict ? (
              <section className="team-section">
                <h3>Verdict</h3>
                <div className="team-verdict">{run.verdict.selected_submission ?? "no verdict"}</div>
                <div className="team-muted">{run.verdict.method} · auto apply {String(run.verdict.can_auto_apply)}</div>
                <ul>{run.verdict.reasons.map((r) => <li key={r}>{r}</li>)}</ul>
              </section>
            ) : null}

            <section className="team-section">
              <h3>Recent Events</h3>
              <HTMLTable compact className="team-table">
                <tbody>
                  {(detail?.events ?? []).slice(-12).reverse().map((e) => (
                    <tr key={e.id}><td>{e.ts}</td><td>{e.kind}</td><td>{e.actor}</td></tr>
                  ))}
                </tbody>
              </HTMLTable>
            </section>
          </>
        )}
      </main>
    </div>
  );
}
