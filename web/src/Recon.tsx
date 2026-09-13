import { useMemo, useState } from "react";

import type { EndpointRow, SessionDetail } from "./api";
import { Chip, Cmd, Empty, Method, Note, SectionHead, clock, day, plural } from "./ui";

// The recon ledger and the runs that filled it. Five states are the whole
// discipline, and `confirmed` is the only one that means "this exists".

const STATES = ["confirmed", "observed", "candidate", "refused", "gone"];

/** Rows recon asked for on purpose, to learn what "not there" looks like. */
function isCalibration(e: EndpointRow): boolean {
  return e.sources.length > 0 && e.sources.every((s) => s.from === "calibration");
}

export function Recon({ detail }: { detail: SessionDetail }) {
  const name = detail.name ?? detail.id;
  const all = detail.endpoints;
  const [state, setState] = useState<string>("all");
  const [probes, setProbes] = useState(false);
  const [q, setQ] = useState("");

  const counts = useMemo(() => {
    const out: Record<string, number> = {};
    for (const e of all) if (!isCalibration(e)) out[e.state] = (out[e.state] ?? 0) + 1;
    return out;
  }, [all]);
  const nProbes = all.filter(isCalibration).length;
  const needle = q.trim().toLowerCase();
  const shown = all.filter(
    (e) =>
      (probes || !isCalibration(e)) &&
      (state === "all" || e.state === state) &&
      (needle === "" || `${e.origin}${e.path}`.toLowerCase().includes(needle)),
  );

  return (
    <div className="scroll pad">
      <SectionHead
        aside={
          detail.ledger?.truncated ? <span style={{ color: "var(--amber)" }}>the ledger is larger than one poll folds</span> : undefined
        }
      >
        Inventory
      </SectionHead>
      {all.length === 0 ? (
        <Empty title="No ledger yet">
          <p>
            <code>h5i recon extract</code> reads what this session already fetched and sends nothing;{" "}
            <code>crawl</code>, <code>paths</code> and <code>known</code> spend requests to learn more.
          </p>
          <Cmd text={`h5i recon extract --session ${name}`} />
        </Empty>
      ) : (
        <>
          <div className="chips" style={{ marginBottom: 8 }}>
            <Chip tone="dim" on={state === "all"} onClick={() => setState("all")}>
              all
            </Chip>
            {STATES.map((s) =>
              counts[s] ? (
                <Chip
                  key={s}
                  on={state === s}
                  tone={state === s ? "plain" : s === "confirmed" ? "good" : s === "refused" ? "accent" : s === "gone" ? "dim" : "plain"}
                  onClick={() => setState(state === s ? "all" : s)}
                >
                  {s} <b>{counts[s]}</b>
                </Chip>
              ) : null,
            )}
            {nProbes > 0 ? (
              <Chip tone="dim" on={probes} onClick={() => setProbes(!probes)} title="paths recon asked for because they should not exist, which is how triage learns what missing looks like">
                {probes ? "hide" : "show"} {nProbes} calibration {plural(nProbes, "probe")}
              </Chip>
            ) : null}
            <label className="search" style={{ height: 24, marginLeft: "auto" }}>
              <input type="search" placeholder="path" value={q} onChange={(e) => setQ(e.target.value)} />
            </label>
          </div>
          <div className="table-wrap" style={{ flex: "none", maxHeight: "55vh", border: "1px solid var(--line)", borderRadius: 4 }}>
            <table className="grid">
              <thead>
                <tr>
                  <th>state</th>
                  <th>method</th>
                  <th>endpoint</th>
                  <th>identity</th>
                  <th className="num">status</th>
                  <th>learned from</th>
                  <th>evidence</th>
                </tr>
              </thead>
              <tbody>
                {shown.map((e) => (
                  <tr key={e.id} className={e.state === "gone" || isCalibration(e) ? "is-dim" : ""}>
                    <td>
                      <Chip tone={e.state === "confirmed" ? "good" : e.state === "refused" ? "bad" : e.state === "gone" ? "dim" : "plain"}>{e.state}</Chip>
                    </td>
                    <td>
                      <Method m={e.method} />
                    </td>
                    <td className="cell-path" title={`${e.origin}${e.path}${e.reason ? `\n${e.reason}` : ""}`}>
                      <span className="path-dir">{e.origin.replace(/^https?:\/\//, "")}</span>
                      <span className="path-last">{e.path}</span>
                      {e.params.length > 0 ? <span className="path-q">?{e.params.map((p) => p.name).join("&")}</span> : null}
                    </td>
                    <td className="dim">{e.identity}</td>
                    <td className="num">{e.status ?? ""}</td>
                    <td className="dim">{[...new Set(e.sources.map((s) => s.from))].join(", ")}</td>
                    <td>
                      {e.evidence.length > 0 ? (
                        <Cmd text={`h5i websec show ${e.evidence[e.evidence.length - 1]} --session ${name}`} hint="the message that answered for this endpoint" />
                      ) : (
                        <span className="dim">never sent</span>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="table-foot" style={{ border: 0, padding: "6px 0" }}>
            <span>
              {shown.length} of {all.length} rows
            </span>
            <span>a row is (origin, path, method, identity): the same path under two identities is two rows</span>
          </div>
        </>
      )}

      <SectionHead>Runs that spent requests</SectionHead>
      {detail.jobs.length === 0 ? (
        <p className="count">No run has spent a request here.</p>
      ) : (
        <div className="table-wrap" style={{ flex: "none", border: "1px solid var(--line)", borderRadius: 4 }}>
          <table className="grid">
            <thead>
              <tr>
                <th>run</th>
                <th>started</th>
                <th className="num">requests</th>
                <th className="num">rows written</th>
                <th>ended</th>
                <th>resume</th>
              </tr>
            </thead>
            <tbody>
              {[...detail.jobs].reverse().map((j) => (
                <tr key={j.id}>
                  <td>
                    <b>{j.verb}</b> <span className="dim">{j.id}</span>
                  </td>
                  <td className="dim" title={j.started_at}>
                    {day(j.started_at)} {clock(j.started_at)}
                  </td>
                  <td className="num">{j.requests}</td>
                  <td className="num">{j.written}</td>
                  <td className="wide" style={{ maxWidth: 420 }}>
                    {j.ended_at === null ? (
                      <span className="dim">in flight</span>
                    ) : j.stopped ? (
                      <span style={{ color: "var(--amber)" }}>{j.stopped}</span>
                    ) : (
                      "finished"
                    )}
                  </td>
                  <td>
                    <Cmd text={`h5i recon jobs resume ${j.id} --session ${name}`} hint="the same parameters again, skipping what the ledger already answered" />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {detail.ledger ? (
        <div style={{ marginTop: 16 }}>
          <Note>
            <code>h5i recon endpoints --session {name} --state confirmed</code> prints the inventory with the message
            that proves each row.
          </Note>
        </div>
      ) : null}
    </div>
  );
}
