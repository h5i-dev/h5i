import { useEffect, useMemo, useState } from "react";

import type { FindingRow, SessionDetail } from "./api";
import { Chip, Cmd, Empty, Note, ago, clock, day } from "./ui";

// What the agent concluded, and the evidence each conclusion stands on. h5i
// asserts one thing about a finding: every message id it cites is a message
// this session holds. The title, the state and the notes are the agent's.

const READ_KEY = "h5i.console.findings-read";

function loadRead(): Record<string, string> {
  try {
    return JSON.parse(window.localStorage.getItem(READ_KEY) ?? "{}") as Record<string, string>;
  } catch {
    return {};
  }
}

export function Findings({ detail }: { detail: SessionDetail }) {
  const name = detail.name ?? detail.id;
  const list = detail.findings_list;
  const [read, setRead] = useState<Record<string, string>>(loadRead);
  const states = useMemo(() => [...new Set(list.map((f) => f.state).filter(Boolean))], [list]);
  const [state, setState] = useState<string>("all");

  // Looking at the tab is reading: a finding stays marked until this browser
  // has seen its newest change, the way a reporter's unseen count works.
  useEffect(() => {
    const next = { ...loadRead() };
    for (const f of list) next[`${detail.id}/${f.id}`] = f.updated;
    const t = window.setTimeout(() => {
      try {
        window.localStorage.setItem(READ_KEY, JSON.stringify(next));
      } catch {
        // Only the memory is lost.
      }
      setRead(next);
    }, 1500);
    return () => window.clearTimeout(t);
  }, [list, detail.id]);

  if (list.length === 0) {
    return (
      <Empty title="No finding written for this session">
        <p>
          A finding is the agent's claim, kept beside the store with the message ids it rests on. h5i refuses one
          that cites a message the session does not hold.
        </p>
        <Cmd text={`h5i websec finding create --session ${name} --title "…" --evidence req_N`} />
      </Empty>
    );
  }

  const shown = list.filter((f) => state === "all" || f.state === state).reverse();

  return (
    <div className="scroll">
      <div className="findings">
        {states.length > 1 ? (
          <div className="chips">
            <Chip tone="dim" on={state === "all"} onClick={() => setState("all")}>
              all <b>{list.length}</b>
            </Chip>
            {states.map((s) => (
              <Chip key={s} tone="dim" on={state === s} onClick={() => setState(state === s ? "all" : s)}>
                {s} <b>{list.filter((f) => f.state === s).length}</b>
              </Chip>
            ))}
          </div>
        ) : null}
        {shown.map((f) => (
          <FindingCard key={f.id} f={f} name={name} fresh={read[`${detail.id}/${f.id}`] !== f.updated} captured={detail.captured !== null} />
        ))}
        <Note>
          States are the agent's own words, on purpose: nothing reads them, so nothing restricts them. What h5i
          checked is that every evidence id below names a stored message.
        </Note>
      </div>
    </div>
  );
}

function FindingCard({ f, name, fresh, captured }: { f: FindingRow; name: string; fresh: boolean; captured: boolean }) {
  return (
    <article className={`finding${fresh ? " is-new" : ""}`}>
      <div className="finding-head">
        <span className="finding-id">{f.id}</span>
        <h3>{f.title || "(untitled)"}</h3>
        {f.state ? <Chip tone="warn">{f.state}</Chip> : null}
        {fresh ? <Chip tone="accent">changed</Chip> : null}
        <span className="finding-when" title={`written ${f.created}, last changed ${f.updated}`}>
          {ago(f.updated)}
        </span>
      </div>
      <div className="finding-evidence">
        <span className="label">rests on</span>
        {f.evidence.length === 0 ? (
          <span className="count">nothing cited</span>
        ) : (
          f.evidence.map((e) => (
            <Chip key={e} tone="info" mono title={captured ? `h5i websec show ${e} --session ${name}` : "the store is gone; the id still names what was there"}>
              {e}
            </Chip>
          ))
        )}
      </div>
      {f.notes.length > 0 ? (
        <ul className="finding-notes">
          {f.notes.map((n, i) => (
            <li key={i}>
              <time dateTime={n.at} title={n.at}>
                {day(n.at) === day(f.created) ? clock(n.at) : day(n.at)}
              </time>
              <p>{n.text}</p>
            </li>
          ))}
        </ul>
      ) : null}
      <div className="finding-foot">
        <Cmd text={`h5i websec finding show ${f.id} --session ${name}`} hint="the finding, with every note" />
        {f.repro ? <Cmd text={`h5i websec sequence ${f.repro} --session ${name}`} hint="the file that reproduces it" /> : null}
        {f.evidence[0] && captured ? <Cmd text={`h5i websec show ${f.evidence[f.evidence.length - 1]} --session ${name}`} hint="the newest message it cites" /> : null}
      </div>
    </article>
  );
}
