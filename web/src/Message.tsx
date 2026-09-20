import { useEffect, useState } from "react";

import { api, type BodyView, type MessageHalf, type MessageView } from "./api";
import { Cmd } from "./ui";

// The bytes themselves: what went out and what came back. Credential headers
// arrive masked from the server, so the page holds no secret until someone
// asks for one, and asking is a second request rather than a client-side
// toggle over bytes that already crossed.

function Body({ body }: { body: BodyView }) {
  if (body.kind === "missing") {
    return <p className="count">{body.text}</p>;
  }
  return (
    <>
      {body.kind === "cut" ? (
        <p className="count">
          The head of {body.of_bytes?.toLocaleString()} bytes. `h5i websec show` has the rest.
        </p>
      ) : null}
      {body.kind === "binary" ? (
        <p className="count">
          {body.of_bytes?.toLocaleString()} bytes, not text. Preview only; sha256 {body.sha256?.slice(0, 12)}.
        </p>
      ) : null}
      <pre className="msg-body">{body.text || "(empty)"}</pre>
    </>
  );
}

function Half({ half, title }: { half: MessageHalf; title: string }) {
  return (
    <div className="msg-half">
      <div className="section-head">
        <h3>{title}</h3>
        <span className="count">
          {half.method ? `${half.method} ` : ""}
          {typeof half.status === "number" ? `${half.status} ` : ""}
          {half.wire_bytes ? `${half.wire_bytes.toLocaleString()} B on the wire` : ""}
        </span>
      </div>
      <table className="msg-headers">
        <tbody>
          {half.headers.map((h, i) => (
            <tr key={`${h.name}-${i}`} className={h.masked ? "is-masked" : undefined}>
              <th>{h.name}</th>
              <td>{h.value}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <Body body={half.body} />
    </div>
  );
}

export function Message({
  sessionId,
  sessionName,
  seq,
}: {
  sessionId: string;
  sessionName: string;
  seq: number;
}) {
  const [view, setView] = useState<MessageView | null>(null);
  const [reveal, setReveal] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Selecting another fetch is a different message: start masked again rather
  // than carrying one request's decision onto the next.
  useEffect(() => setReveal(false), [sessionId, seq]);

  useEffect(() => {
    let live = true;
    setError(null);
    api
      .message(sessionId, seq, reveal)
      .then((m) => live && setView(m))
      .catch(() => live && setError("not in this session's store"));
    return () => {
      live = false;
    };
  }, [sessionId, seq, reveal]);

  if (error) {
    return <p className="count">{error}</p>;
  }
  if (!view) {
    return <p className="count">reading…</p>;
  }

  return (
    <div className="msg">
      {view.has_secrets ? (
        <div className="msg-reveal">
          <button type="button" className="chip" onClick={() => setReveal((r) => !r)}>
            {reveal ? "hide credentials" : "show credentials"}
          </button>
          <span className="count">
            {reveal
              ? "Authorization, cookies and API keys are shown as they were sent."
              : "Authorization, cookies and API keys are masked."}
          </span>
        </div>
      ) : null}
      {view.request ? <Half half={view.request} title="Request" /> : null}
      {view.response ? (
        <Half half={view.response} title="Response" />
      ) : (
        <p className="count">No response was stored: the fetch was refused, or it never got one.</p>
      )}
      <Cmd
        text={`h5i websec show req_${seq} --session ${sessionName} --raw`}
        hint="the same message, exactly, in a terminal"
      />
    </div>
  );
}
