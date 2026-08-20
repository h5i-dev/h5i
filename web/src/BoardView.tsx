// The board: what the agents said to each other, and what the host did about
// it.
//
// This surface has one job the console does not: making the trust boundary
// visible in the reading order. So it has one visual rule, and everything else
// follows from it.
//
//   Inside the fence is what an agent claimed. Outside it is what the host
//   observed.
//
// A post body sits in a dashed enclosure labelled "agent-claimed". Its sender,
// box, role and time sit outside, unfenced, because the host stamped them from
// the env directory the record came out of and no agent could have written
// them. A refusal — a denied post, a revoked sender — is a filled red band with
// no fence at all, because the host is speaking in its own voice.
//
// That is also why the palette is not the console's. The console is a mint
// instrument for watching one box; the board is the product's outward face, and
// it wears the site's drafting-sheet identity: near-black ground, greyscale
// doing the structural work, and red reserved so tightly that the only filled
// red on the page is a boundary the host refused to let anyone cross.
//
// Read-only, like everything else here (see crates/h5i-core/src/server.rs). The
// human actions — revoke, close, apply — are shown as the commands that perform
// them, to be run in a terminal. A browser tab that could post to the board
// would be a participant the host cannot name.

import React from "react";
import {
  boardApi,
  type BoardOverview,
  type BoardPost,
  type BoardStatus,
  type BoardThread,
  type BoardThreadSummary,
  type BoardRosterEntry,
} from "./api";

/** How often the thread list is re-read. Matches the fleet's cadence. */
const LIST_POLL_MS = 8000;
/** How often an open conversation is re-read — a conversation should feel live. */
const THREAD_POLL_MS = 2000;

type Filter = "all" | "open" | "claimed" | "review" | "refused";

const FILTERS: { key: Filter; label: string }[] = [
  { key: "all", label: "all" },
  { key: "open", label: "open" },
  { key: "claimed", label: "claimed" },
  { key: "review", label: "review" },
  { key: "refused", label: "refused" },
];

export function BoardView() {
  const [overview, setOverview] = React.useState<BoardOverview | null>(null);
  const [selected, setSelected] = React.useState<string | null>(null);
  const [thread, setThread] = React.useState<BoardThread | null>(null);
  const [filter, setFilter] = React.useState<Filter>("all");
  const [error, setError] = React.useState<string | null>(null);

  React.useEffect(() => {
    let live = true;
    const load = () =>
      boardApi
        .overview()
        .then((o) => {
          if (!live) return;
          setOverview(o);
          setError(null);
        })
        .catch((e: Error) => live && setError(e.message));
    load();
    const t = setInterval(load, LIST_POLL_MS);
    return () => {
      live = false;
      clearInterval(t);
    };
  }, []);

  React.useEffect(() => {
    if (!selected) {
      setThread(null);
      return;
    }
    let live = true;
    const load = () =>
      boardApi
        .thread(selected)
        .then((t) => live && setThread(t))
        .catch(() => live && setThread(null));
    load();
    const t = setInterval(load, THREAD_POLL_MS);
    return () => {
      live = false;
      clearInterval(t);
    };
  }, [selected]);

  // Keep a selection pointing at something that still exists: a thread closed
  // from a terminal while its conversation is open should fall back to the
  // list, not leave a stale pane the poller keeps failing to refresh.
  React.useEffect(() => {
    if (!overview || !selected) return;
    const known = [...overview.threads, ...overview.attic].some(
      (t) => t.header.id === selected,
    );
    if (!known) setSelected(null);
  }, [overview, selected]);

  const threads = overview?.threads ?? [];
  const shown = threads.filter((t) => matches(t, filter));

  return (
    <div className="brd">
      <div className="brd-body">
        <ThreadList
          threads={shown}
          attic={overview?.attic ?? []}
          filter={filter}
          onFilter={setFilter}
          selected={selected}
          onSelect={setSelected}
          error={error}
        />
        <Conversation thread={thread} empty={threads.length === 0} />
        <Participants
          roster={overview?.roster ?? []}
          influenced={overview?.influenced ?? []}
        />
      </div>
    </div>
  );
}

function matches(t: BoardThreadSummary, f: Filter): boolean {
  switch (f) {
    case "all":
      return true;
    case "refused":
      return t.denials > 0;
    default:
      return t.status === f;
  }
}

// ── the thread list ──────────────────────────────────────────────────────────

function ThreadList({
  threads,
  attic,
  filter,
  onFilter,
  selected,
  onSelect,
  error,
}: {
  threads: BoardThreadSummary[];
  attic: BoardThreadSummary[];
  filter: Filter;
  onFilter: (f: Filter) => void;
  selected: string | null;
  onSelect: (id: string | null) => void;
  error: string | null;
}) {
  return (
    <div className="brd-col brd-col-left">
      <div className="brd-h">threads</div>
      <div className="brd-chips">
        {FILTERS.map((f) => (
          <button
            key={f.key}
            type="button"
            className={`brd-chip${filter === f.key ? " is-on" : ""}`}
            onClick={() => onFilter(f.key)}
          >
            {f.label}
          </button>
        ))}
      </div>
      {error && <div className="brd-error">{error}</div>}
      {threads.length === 0 && !error && (
        <div className="brd-empty">
          no threads here.
          <code>h5i board create "…"</code>
        </div>
      )}
      {threads.map((t) => (
        <ThreadRow
          key={t.header.id}
          t={t}
          selected={t.header.id === selected}
          onSelect={onSelect}
        />
      ))}
      {attic.length > 0 && (
        <>
          <div className="brd-h brd-h-spaced">closed</div>
          {attic.map((t) => (
            <ThreadRow
              key={t.header.id}
              t={t}
              closed
              selected={t.header.id === selected}
              onSelect={onSelect}
            />
          ))}
        </>
      )}
    </div>
  );
}

function ThreadRow({
  t,
  selected,
  closed,
  onSelect,
}: {
  t: BoardThreadSummary;
  selected: boolean;
  closed?: boolean;
  onSelect: (id: string) => void;
}) {
  return (
    <button
      type="button"
      className={`brd-thread${selected ? " is-sel" : ""}${closed ? " is-closed" : ""}`}
      onClick={() => onSelect(t.header.id)}
    >
      <span className="brd-thread-title">{t.header.title}</span>
      <span className="brd-thread-meta">
        <StatusPill status={t.status} />
        <span>{t.claimed_by ?? "unclaimed"}</span>
        <span>{t.posts} posts</span>
        {t.denials > 0 && <span className="brd-pill is-denial">{t.denials} refused</span>}
      </span>
    </button>
  );
}

function StatusPill({ status }: { status: BoardStatus }) {
  return <span className={`brd-pill is-${status}`}>{status}</span>;
}

// ── the conversation ─────────────────────────────────────────────────────────

function Conversation({
  thread,
  empty,
}: {
  thread: BoardThread | null;
  empty: boolean;
}) {
  if (!thread) {
    return (
      <div className="brd-col brd-col-mid">
        <div className="brd-blank">
          {empty ? (
            <>
              <p>Nothing is on the board yet.</p>
              <p className="brd-dim">
                A human opens a thread and puts boxes on it. The agents inside
                them read, post and submit; they never gain a capability by
                doing so.
              </p>
              <pre>
{`h5i board create "fix the auth refresh race" --ceiling code-review
h5i board attach claude-box --as claude-worker --role worker
h5i board attach codex-box  --as codex-reviewer --role reviewer`}
              </pre>
            </>
          ) : (
            <p className="brd-dim">Pick a thread.</p>
          )}
        </div>
      </div>
    );
  }

  return (
    <div className="brd-col brd-col-mid">
      <div className="brd-conv-head">
        <span className="brd-conv-title">{thread.header.title}</span>
        <StatusPill status={thread.status} />
        <span className="brd-mono">{thread.header.id}</span>
        {thread.header.ceiling ? (
          <span className="brd-mono">
            ceiling {thread.header.ceiling.profile}
            {thread.header.ceiling.digest
              ? ` · sha256:${thread.header.ceiling.digest.slice(0, 12)}`
              : ""}
          </span>
        ) : (
          <span className="brd-mono brd-warn">no ceiling</span>
        )}
      </div>

      <div className="brd-posts">
        {thread.posts.length === 0 && (
          <div className="brd-dim brd-pad">No posts yet.</div>
        )}
        {thread.posts.map((p) => (
          <PostRow key={p.id} p={p} />
        ))}
      </div>

      <div className="brd-conv-foot">
        <span className="brd-dim">the console watches. act from a terminal:</span>
        <Cmd text={`h5i board read ${thread.header.id}`} />
        <Cmd text={`h5i board close ${thread.header.id}`} />
      </div>
    </div>
  );
}

function PostRow({ p }: { p: BoardPost }) {
  return (
    <div className="brd-post">
      {/* Outside the fence: what the host stamped. None of this came from the
          box's payload — the wire format has no field for any of it. */}
      <div className="brd-post-meta">
        <Avatar name={p.sender} />
        <b>{p.sender}</b>
        <span className="brd-dim">{p.role}</span>
        {p.box_id && <span className="brd-dim">{p.box_id}</span>}
        <span className={`brd-kind is-${kindClass(p.kind)}`}>{p.kind}</span>
        <span className="brd-dim">{shortTime(p.ts)}</span>
      </div>

      {/* Inside the fence: what the agent said. */}
      <div className="brd-fence">{p.body}</div>

      {(p.attachments ?? []).map((a) => (
        <div key={a.digest} className="brd-attach">
          <span>{a.kind}</span>
          <span>{a.name ?? "(unnamed)"}</span>
          <span className="brd-dim">
            {a.size} bytes · {a.digest.slice(0, 12)}
          </span>
        </div>
      ))}

      {(p.redactions ?? []).length > 0 && (
        <div className="brd-redacted">
          <span className="brd-redacted-who">redacted</span>
          <span>
            a credential was scrubbed before this post was stored (
            {(p.redactions ?? []).join(", ")})
          </span>
        </div>
      )}

      {p.denied && (
        <div className="brd-hostline">
          <span className="brd-hostline-who">host</span>
          <span>refused: {p.denied}</span>
        </div>
      )}
    </div>
  );
}

/** A square initial, coloured by name so two agents stay distinguishable. */
function Avatar({ name }: { name: string }) {
  const hue = [...name].reduce((a, c) => (a * 31 + c.charCodeAt(0)) % 360, 7);
  return (
    <span className="brd-avatar" style={{ color: `hsl(${hue} 45% 68%)` }}>
      {name.slice(0, 1).toUpperCase()}
    </span>
  );
}

function kindClass(kind: string): string {
  switch (kind) {
    case "RISK":
    case "BLOCKED":
    case "REVIEW_REQUEST":
      return "warn";
    case "DONE":
    case "ACK":
      return "good";
    case "TASK":
      return "task";
    default:
      return "plain";
  }
}

/** A command to copy, not a button that does it. */
function Cmd({ text }: { text: string }) {
  const [copied, setCopied] = React.useState(false);
  return (
    <button
      type="button"
      className="brd-cmd"
      title="copy"
      onClick={() => {
        void navigator.clipboard?.writeText(text);
        setCopied(true);
        setTimeout(() => setCopied(false), 1200);
      }}
    >
      {copied ? "copied" : text}
    </button>
  );
}

// ── participants ─────────────────────────────────────────────────────────────

function Participants({
  roster,
  influenced,
}: {
  roster: BoardRosterEntry[];
  influenced: string[];
}) {
  return (
    <div className="brd-col brd-col-right">
      <div className="brd-h">participants</div>
      {roster.length === 0 && (
        <div className="brd-empty">
          nobody yet.
          <code>h5i board attach &lt;box&gt; --as &lt;name&gt;</code>
        </div>
      )}
      {roster.map((e) => (
        <div key={e.agent} className={`brd-agent${e.revoked_at ? " is-revoked" : ""}`}>
          <div className="brd-agent-name">
            <Avatar name={e.agent} />
            {e.agent}
            <span className={`brd-dot ${e.revoked_at ? "is-off" : "is-live"}`} />
          </div>
          <Row k="role" v={e.role} />
          {e.box_id && <Row k="box" v={e.box_id} />}
          {e.policy_digest && <Row k="policy" v={e.policy_digest.slice(0, 12)} />}
          {influenced.includes(e.agent) && (
            <Row k="influenced" v="read a peer" warn />
          )}
          {e.revoked_at ? (
            <Row k="revoked" v={shortTime(e.revoked_at)} warn />
          ) : (
            <div className="brd-agent-cmd">
              <Cmd text={`h5i board revoke ${e.agent}`} />
            </div>
          )}
        </div>
      ))}
      <div className="brd-note">
        A post can change what an agent decides. It cannot change what that
        agent&rsquo;s box is able to do: no credential and no capability travels
        this path.
      </div>
    </div>
  );
}

function Row({ k, v, warn }: { k: string; v: string; warn?: boolean }) {
  return (
    <div className={`brd-agent-row${warn ? " is-warn" : ""}`}>
      <span>{k}</span>
      <span>{v}</span>
    </div>
  );
}

// ── formatting ───────────────────────────────────────────────────────────────

/** `2026-08-20T14:02:11.123456Z` → `08-20 14:02`. */
function shortTime(ts: string): string {
  return ts.length >= 16 ? `${ts.slice(5, 10)} ${ts.slice(11, 16)}` : ts;
}
