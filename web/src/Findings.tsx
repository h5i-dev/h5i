import { useEffect, useMemo, useRef, useState } from "react";

import type { FindingRow, SessionDetail } from "./api";
import { Chip, Cmd, Empty, Note, ReqLink, ago, clock, day, type SessionNav } from "./ui";

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

function saveRead(next: Record<string, string>) {
  try {
    window.localStorage.setItem(READ_KEY, JSON.stringify(next));
  } catch {
    // Only the memory is lost.
  }
}

export function Findings({ detail, nav }: { detail: SessionDetail; nav: SessionNav }) {
  const name = detail.name ?? detail.id;
  const list = detail.findings_list;
  // What was unread when the tab opened keeps its mark for this visit, so a
  // card does not change under the reader; storage records what was read.
  const [before] = useState<Record<string, string>>(loadRead);
  const [read, setRead] = useState<Record<string, string>>(loadRead);
  const states = useMemo(() => [...new Set(list.map((f) => f.state).filter(Boolean))], [list]);
  const [state, setState] = useState<string>("all");

  const mark = (key: string, updated: string | null) => {
    const next = { ...loadRead() };
    if (updated === null) delete next[key];
    else next[key] = updated;
    saveRead(next);
    setRead(next);
  };

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
        {shown.map((f) => {
          const key = `${detail.id}/${f.id}`;
          return (
            <FindingCard
              key={f.id}
              f={f}
              name={name}
              nav={nav}
              fresh={before[key] !== f.updated}
              read={read[key] === f.updated}
              onRead={(on) => mark(key, on ? f.updated : null)}
              captured={detail.captured !== null}
            />
          );
        })}
        <Note>
          States are the agent's own words, on purpose: nothing reads them, so nothing restricts them. What h5i
          checked is that every evidence id below names a stored message.
        </Note>
      </div>
    </div>
  );
}

/** Read means on screen for a moment, not merely on the tab. */
function useReadOnSight(ref: React.RefObject<HTMLElement>, enabled: boolean, onSeen: () => void) {
  const seen = useRef(onSeen);
  seen.current = onSeen;
  useEffect(() => {
    const el = ref.current;
    if (!enabled || !el || typeof IntersectionObserver === "undefined") return;
    let t: number | undefined;
    const io = new IntersectionObserver(
      ([e]) => {
        window.clearTimeout(t);
        if (e.isIntersecting) t = window.setTimeout(() => seen.current(), 1500);
      },
      { threshold: 0.6 },
    );
    io.observe(el);
    return () => {
      window.clearTimeout(t);
      io.disconnect();
    };
  }, [ref, enabled]);
}

function FindingCard({
  f,
  name,
  nav,
  fresh,
  read,
  onRead,
  captured,
}: {
  f: FindingRow;
  name: string;
  nav: SessionNav;
  fresh: boolean;
  read: boolean;
  onRead: (read: boolean) => void;
  captured: boolean;
}) {
  const ref = useRef<HTMLElement>(null);
  // Kept unread by hand: not marked again on this visit.
  const [kept, setKept] = useState(false);
  useReadOnSight(ref, fresh && !read && !kept, () => onRead(true));
  return (
    <article ref={ref} className={`finding${fresh ? " is-new" : ""}`}>
      <div className="finding-head">
        <span className="finding-id">{f.id}</span>
        <h3>{f.title || "(untitled)"}</h3>
        {f.state ? <Chip tone="warn">{f.state}</Chip> : null}
        {fresh ? <Chip tone="accent">changed</Chip> : null}
        {fresh && read ? (
          <button
            type="button"
            className="req-link is-text"
            onClick={() => {
              setKept(true);
              onRead(false);
            }}
          >
            keep unread
          </button>
        ) : null}
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
            <ReqLink
              key={e}
              id={e}
              nav={nav}
              title={
                captured
                  ? `open ${e} in History\nh5i websec show ${e} --session ${name}`
                  : `open ${e} in History; the store is gone, so only its decision record remains`
              }
            />
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
