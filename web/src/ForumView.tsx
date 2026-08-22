// The forum: what the agents said to each other, and what the host did about
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
// instrument for watching one box; the forum is the product's outward face, and
// it wears the site's drafting-sheet identity: near-black ground, greyscale
// doing the structural work, and red reserved so tightly that the only filled
// red on the page is a boundary the host refused to let anyone cross.
//
// Read-only, like everything else here (see crates/h5i-core/src/server.rs). The
// human actions — revoke, close, apply — are shown as the commands that perform
// them, to be run in a terminal. A browser tab that could post to the forum
// would be a participant the host cannot name.

import React from "react";
import {
  forumApi,
  type ForumOverview,
  type ForumPost,
  type ForumPreview,
  type ForumStatus,
  type ForumThread,
  type ForumThreadSummary,
  type ForumRosterEntry,
} from "./api";
import { Markdown, plainText } from "./markdown";

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

export function ForumView({
  initialThread = null,
}: {
  /** Thread named by `#forum/<id>`, opened on first render. */
  initialThread?: string | null;
}) {
  const [overview, setOverview] = React.useState<ForumOverview | null>(null);
  const [selected, setSelected] = React.useState<string | null>(initialThread);
  const [thread, setThread] = React.useState<ForumThread | null>(null);
  const [filter, setFilter] = React.useState<Filter>("all");
  const [error, setError] = React.useState<string | null>(null);

  // Keep the address bar in step with the selection, so the URL is always the
  // link to what is on screen rather than to where the reader started.
  React.useEffect(() => {
    const want = selected ? `#forum/${selected}` : "#forum";
    if (window.location.hash !== want) {
      window.history.replaceState({}, "", window.location.pathname + want);
    }
  }, [selected]);

  React.useEffect(() => {
    let live = true;
    const load = () =>
      forumApi
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
      forumApi
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
    const known = [...overview.threads, ...overview.closed].some(
      (t) => t.header.id === selected,
    );
    if (!known) setSelected(null);
  }, [overview, selected]);

  const threads = overview?.threads ?? [];
  const shown = threads.filter((t) => matches(t, filter));

  // The cohort is the roster plus whoever the open thread quotes, so a peer's
  // agent that this host has no row for still gets a face nobody else on this
  // screen is using.
  const faces = React.useMemo(
    () =>
      faceMap([
        ...(overview?.roster ?? []).map((e) => e.agent),
        ...(thread?.posts ?? []).map((p) => p.sender),
      ]),
    [overview?.roster, thread?.posts],
  );

  return (
    <Faces.Provider value={faces}>
      <div className="brd">
        <div className="brd-body">
          <ThreadList
            threads={shown}
            closed={overview?.closed ?? []}
            filter={filter}
            onFilter={setFilter}
            selected={selected}
            onSelect={setSelected}
            error={error}
          />
          {thread ? (
            <Conversation thread={thread} empty={false} />
          ) : (
            <Feed
              threads={threads}
              previews={overview?.previews ?? []}
              onSelect={setSelected}
            />
          )}
          <Participants
            roster={overview?.roster ?? []}
            influenced={overview?.influenced ?? []}
            hostOrigin={overview?.host_origin ?? ""}
          />
        </div>
      </div>
    </Faces.Provider>
  );
}

function matches(t: ForumThreadSummary, f: Filter): boolean {
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
  closed,
  filter,
  onFilter,
  selected,
  onSelect,
  error,
}: {
  threads: ForumThreadSummary[];
  closed: ForumThreadSummary[];
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
          <code>h5i forum create "…"</code>
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
      {closed.length > 0 && (
        <>
          <div className="brd-h brd-h-spaced">closed</div>
          {closed.map((t) => (
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
  t: ForumThreadSummary;
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
        {t.denials > 0 && (
          <span className="brd-pill is-denial">{t.denials} refused</span>
        )}
      </span>
    </button>
  );
}

function StatusPill({ status }: { status: ForumStatus }) {
  return <span className={`brd-pill is-${status}`}>{status}</span>;
}

// ── the conversation ─────────────────────────────────────────────────────────

function Conversation({
  thread,
  empty,
}: {
  thread: ForumThread | null;
  empty: boolean;
}) {
  if (!thread) {
    return (
      <div className="brd-col brd-col-mid">
        <div className="brd-blank">
          {empty ? (
            <>
              <p>Nothing is on the forum yet.</p>
              <p className="brd-dim">
                A human opens a thread and puts boxes on it. The agents inside
                them read, post and submit; they never gain a capability by
                doing so.
              </p>
              <pre>
                {`h5i forum create "fix the auth refresh race" --ceiling code-review
h5i forum attach claude-box --as claude-worker --role worker
h5i forum attach codex-box  --as codex-reviewer --role reviewer`}
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
          <div className="brd-dim brd-pad">
            No body and no replies yet — this thread is a title.
          </div>
        )}
        {(() => {
          const shown = thread.posts.filter(
            (p) => p.kind !== "UPVOTE" && p.kind !== "DOWNVOTE",
          );
          // The opening post is the thread's body, not its first comment, so it
          // sits above the rule and the replies sit below it — the shape every
          // discussion surface has, and the one a reader already knows.
          const opening = shown.find((p) => p.kind === "TASK");
          const replies = shown.filter((p) => p !== opening);
          return (
            <>
              {opening && (
                <div className="brd-opening">
                  <PostRow
                    p={opening}
                    me={thread.origin}
                    score={thread.scores[opening.id] ?? 0}
                  />
                </div>
              )}
              {opening && replies.length > 0 && (
                <div className="brd-replies-head">
                  {replies.length} {replies.length === 1 ? "reply" : "replies"}
                </div>
              )}
              {replies.map((p) => (
                <PostRow
                  key={p.id}
                  p={p}
                  me={thread.origin}
                  score={thread.scores[p.id] ?? 0}
                />
              ))}
            </>
          );
        })()}
      </div>

      <div className="brd-conv-foot">
        <span className="brd-dim">
          the console watches. act from a terminal:
        </span>
        <Cmd text={`h5i forum read ${thread.header.id}`} />
        <Cmd text={`h5i forum close ${thread.header.id}`} />
      </div>
    </div>
  );
}

function PostRow({
  p,
  me,
  score,
}: {
  p: ForumPost;
  me: string;
  score: number;
}) {
  const observed = !!p.origin && p.origin === me;
  return (
    <div className={`brd-post${observed ? "" : " is-peer"}`}>
      {/* Outside the fence: what *some* host stamped. For a post this host
          wrote, none of it came from the box's payload — the wire format has no
          field for any of it, so it is knowledge rather than claim. For a post
          fetched from a peer, this host observed none of it, and the lane below
          says so rather than letting the line keep looking like a fact. */}
      <div className="brd-post-meta">
        <Avatar name={p.sender} />
        <b>{p.sender}</b>
        <span className="brd-dim">{p.role}</span>
        {p.box_id && <span className="brd-dim">{p.box_id}</span>}
        <span className={`brd-kind is-${kindClass(p.kind)}`}>{p.kind}</span>
        <span className="brd-dim">{shortTime(p.ts)}</span>
        {score !== 0 && (
          <span className={`brd-score${score > 0 ? " is-up" : " is-down"}`}>
            {score > 0 ? `▲${score}` : `▼${-score}`}
          </span>
        )}
      </div>

      <div className={`brd-lane${observed ? " is-observed" : ""}`}>
        {observed ? (
          "host-observed"
        ) : p.origin ? (
          <>
            peer-claimed
            <span className="brd-dim">
              {" "}
              · {p.origin} says so; this host observed none of the line above
            </span>
          </>
        ) : (
          <>
            unattributed
            <span className="brd-dim"> · arrived naming no origin</span>
          </>
        )}
      </div>

      {/* Inside the fence: what the agent said, rendered as markdown by a
          renderer that emits React nodes and never an HTML string — see
          `markdown.tsx`. Formatting the text must not be the thing that lets it
          stop being text. */}
      <div className="brd-fence">
        <Markdown text={p.body} />
      </div>

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
/**
 * Faces for the participants, picked from the name.
 *
 * Initials do not work here. Agents get named `agent-1`, `agent-2`,
 * `claude-worker`, `codex-reviewer` — the first letter is the same for most of
 * a forum and a hashed hue is not a difference you can hold in your head while
 * scanning a thread. These are distinguishable at a glance and, being derived
 * from the name, are the same on every machine looking at the same forum.
 *
 * Chosen to have distinct silhouettes rather than to be decorative, and
 * deliberately free of anything that carries meaning elsewhere on this screen:
 * no ticks, no warnings, no red. A face that reads as a status is worse than no
 * face at all.
 */
const FACES = [
  "🦊",
  "🐢",
  "🦉",
  "🐙",
  "🦁",
  "🐝",
  "🦋",
  "🐧",
  "🦀",
  "🐳",
  "🦔",
  "🐿",
  "🦩",
  "🐊",
  "🦚",
  "🐺",
  "🌵",
  "🍄",
  "🌻",
  "🍁",
  "🌲",
  "🪴",
  "🌊",
  "🪐",
  "🎈",
  "🎩",
  "🧭",
  "🔭",
  "🪁",
  "🧩",
  "🎲",
  "🪗",
];

/** A stable index from a name: same participant, same face, everywhere. */
function faceOf(name: string): string {
  // FNV-1a, because a plain sum collides on anagrams — and `agent-1` through
  // `agent-9` differ by one character, which is exactly the case that has to
  // spread.
  let h = 0x811c9dc5;
  for (const ch of name) {
    h ^= ch.charCodeAt(0);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return FACES[h % FACES.length];
}

/**
 * Faces for one forum, with collisions resolved.
 *
 * Hashing alone gives two participants the same face often enough to matter —
 * eleven names into thirty-two faces collides about five times in six — and a
 * duplicate face is worse than the initials it replaced, because it looks like
 * a fact. So a name that lands on a taken face probes forward.
 *
 * Sorting first is what keeps this agreeing across machines: every host holding
 * the same participant set walks it in the same order and resolves the same
 * way, so a face is a thing two people can refer to out loud. It is not
 * pinned for all time — a new participant sorting earlier can displace a later
 * one — which is the price of never showing the same face twice, and the right
 * side of that trade for a forum that is read while it is being written.
 *
 * Past `FACES.length` participants the probe stops and faces repeat, because
 * there is nothing left to move to. A forum that large has outgrown telling
 * people apart by icon anyway, and the name is next to every one of them.
 */
function faceMap(names: string[]): Map<string, string> {
  const out = new Map<string, string>();
  const taken = new Set<string>();
  for (const name of [...new Set(names)].sort()) {
    let face = faceOf(name);
    if (taken.size < FACES.length) {
      let at = FACES.indexOf(face);
      while (taken.has(face)) {
        at = (at + 1) % FACES.length;
        face = FACES[at];
      }
    }
    taken.add(face);
    out.set(name, face);
  }
  return out;
}

/**
 * The forum's faces, so a post and the participants panel agree.
 *
 * The default is the bare hash rather than an empty map: a post can name a
 * sender this host has no roster row for — a peer's agent, or one revoked long
 * enough ago to have been pruned — and such a post still has to render.
 */
const Faces = React.createContext<Map<string, string> | null>(null);

function Avatar({ name }: { name: string }) {
  const faces = React.useContext(Faces);
  return (
    <span className="brd-avatar" title={name}>
      {faces?.get(name) ?? faceOf(name)}
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
  hostOrigin,
}: {
  roster: ForumRosterEntry[];
  influenced: string[];
  hostOrigin: string;
}) {
  return (
    <div className="brd-col brd-col-right">
      <div className="brd-h">participants</div>
      {roster.length === 0 && (
        <div className="brd-empty">
          nobody yet.
          <code>h5i forum attach &lt;box&gt; --as &lt;name&gt;</code>
        </div>
      )}
      {roster.map((e) => (
        <div
          key={e.agent}
          className={`brd-agent${e.revoked_at ? " is-revoked" : ""}`}
        >
          <div className="brd-agent-name">
            <Avatar name={e.agent} />
            {e.agent}
            <span
              className={`brd-dot ${e.revoked_at ? "is-off" : "is-live"}`}
            />
          </div>
          <Row k="role" v={e.role} />
          {/* A box id is a path on the machine that attached it. A peer's
              entry can share a local path exactly, so the origin is the only
              thing keeping the two apart on screen. */}
          {e.box_id && (
            <Row
              k="box"
              v={
                e.origin && e.origin !== hostOrigin
                  ? `${e.box_id}@${e.origin}`
                  : e.box_id
              }
            />
          )}
          {e.policy_digest && (
            <Row k="policy" v={e.policy_digest.slice(0, 12)} />
          )}
          {influenced.includes(e.agent) && (
            <Row k="influenced" v="read a peer" warn />
          )}
          {e.revoked_at ? (
            <Row k="revoked" v={shortTime(e.revoked_at)} warn />
          ) : (
            <div className="brd-agent-cmd">
              <Cmd text={`h5i forum revoke ${e.agent}`} />
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

// ── the landing feed ─────────────────────────────────────────────────────────

/**
 * What the forum shows before you have picked anything.
 *
 * "Pick a thread" is the wrong first screen: it asks the reader to choose
 * between titles when what they want to know is where the argument got to. So
 * the feed leads with the best-scored post in each thread — the one the room
 * agreed on — and ranks by that, with recency breaking ties. A forum with
 * nothing on it explains how to put something on it instead.
 */
function Feed({
  threads,
  previews,
  onSelect,
}: {
  threads: ForumThreadSummary[];
  previews: ForumPreview[];
  onSelect: (id: string) => void;
}) {
  const byId = new Map(previews.map((p) => [p.thread, p]));
  const ranked = [...threads].sort((a, b) => {
    const sa = byId.get(a.header.id)?.top_score ?? 0;
    const sb = byId.get(b.header.id)?.top_score ?? 0;
    if (sa !== sb) return sb - sa;
    return b.last_activity.localeCompare(a.last_activity);
  });

  if (ranked.length === 0) {
    return (
      <div className="brd-col brd-col-mid">
        <div className="brd-blank">
          <p>Nothing is on the forum yet.</p>
          <p className="brd-dim">
            A human opens a thread and puts boxes on it. The agents inside them
            read, post and submit; they never gain a capability by doing so.
          </p>
          <pre>
            {`h5i forum create "fix the auth refresh race" --ceiling code-review
h5i forum attach claude-box --as claude-worker --role worker
h5i forum attach codex-box  --as codex-reviewer --role reviewer`}
          </pre>
        </div>
      </div>
    );
  }

  return (
    <div className="brd-col brd-col-mid">
      <div className="brd-feed-head">
        <span className="brd-h">what the forum agreed on</span>
        <span className="brd-dim">
          ranked by the best-scored post in each thread
        </span>
      </div>
      <div className="brd-feed">
        {ranked.map((t) => {
          const p = byId.get(t.header.id);
          return (
            <button
              type="button"
              className="brd-card"
              key={t.header.id}
              onClick={() => onSelect(t.header.id)}
            >
              <div className="brd-card-score">
                <span className={p && p.top_score > 0 ? "is-up" : ""}>
                  {p && p.top_score > 0 ? `▲${p.top_score}` : "–"}
                </span>
              </div>
              <div className="brd-card-body">
                <div className="brd-card-title">{t.header.title}</div>
                <div className="brd-card-meta">
                  <StatusPill status={t.status} />
                  <span>{t.posts} posts</span>
                  {(p?.voices.length ?? 0) > 0 && (
                    <span>{p!.voices.join(", ")}</span>
                  )}
                  {t.denials > 0 && (
                    <span className="brd-pill is-denial">
                      {t.denials} refused
                    </span>
                  )}
                </div>
                {p?.opening && (
                  <div className="brd-card-quote">{plainText(p.opening)}</div>
                )}
                {p?.top_body && (
                  <div className="brd-card-reply">
                    {/* A non-breaking space, because a trailing ordinary one
                        at the end of an inline element is collapsed away and
                        the byline runs into the quote. */}
                    <span className="brd-dim">{`top reply · ${p.top_sender}:\u00a0`}</span>
                    {plainText(p.top_body)}
                  </div>
                )}
              </div>
            </button>
          );
        })}
      </div>
      <div className="brd-conv-foot">
        <span className="brd-dim">
          the console watches; agreeing happens in a terminal:
        </span>
        <Cmd text="h5i forum up <n>" />
      </div>
    </div>
  );
}
