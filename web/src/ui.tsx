// The console's own small kit. No component library: the page is served over
// loopback, must render with no route out, and the few pieces it needs are
// cheaper to own than to restyle.

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";

// ── routing: the hash is the whole navigation state ──────────────────────────

/** `#/sessions/br_x/history` → `["sessions", "br_x", "history"]`. Deep links
 *  and the back button come for free, and a reload lands where it left. */
export function useHash(): [string[], (parts: string[], replace?: boolean) => void] {
  const [parts, setParts] = useState<string[]>(() => read());
  useEffect(() => {
    const on = () => setParts(read());
    window.addEventListener("hashchange", on);
    return () => window.removeEventListener("hashchange", on);
  }, []);
  const go = useCallback((next: string[], replace = false) => {
    const hash = "#/" + next.map(encodeURIComponent).join("/");
    if (replace) window.history.replaceState({}, "", hash);
    else window.location.hash = hash;
    setParts(read());
  }, []);
  return [parts, go];
}

function read(): string[] {
  const raw = window.location.hash.replace(/^#\/?/, "");
  return raw === "" ? [] : raw.split("/").map((p) => decodeURIComponent(p));
}

// ── remembered numbers ───────────────────────────────────────────────────────

/** `localStorage` can throw (private mode, disabled). A console that fails to
 *  render because it could not remember a pane width would be a bad trade. */
export function remember<T extends string | number>(key: string, fallback: T): T {
  try {
    const raw = window.localStorage.getItem(key);
    if (raw === null) return fallback;
    return (typeof fallback === "number" ? Number(raw) : raw) as T;
  } catch {
    return fallback;
  }
}

export function keep(key: string, value: string | number): void {
  try {
    window.localStorage.setItem(key, String(value));
  } catch {
    // Only the memory of it is lost.
  }
}

// ── the split ────────────────────────────────────────────────────────────────

/**
 * Two panes and a handle between them. Pointer events so a pen and a touch
 * screen work; a focusable `separator` so a keyboard can move it too.
 */
export function Split({
  id,
  dir = "row",
  min = 220,
  max = 900,
  initial = 360,
  first,
  second,
  className,
}: {
  id: string;
  dir?: "row" | "column";
  min?: number;
  max?: number;
  initial?: number;
  first: ReactNode;
  second: ReactNode;
  className?: string;
}) {
  const key = `h5i.console.split.${id}`;
  const clamp = useCallback(
    (n: number) => {
      const room =
        dir === "row" ? window.innerWidth - 320 : window.innerHeight - 200;
      return Math.min(Math.min(max, Math.max(min, room)), Math.max(min, n));
    },
    [dir, min, max],
  );
  const [size, setSize] = useState<number>(() => clamp(remember(key, initial)));
  const dragging = useRef(false);
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => keep(key, Math.round(size)), [key, size]);
  useEffect(() => {
    const on = () => setSize((s) => clamp(s));
    window.addEventListener("resize", on);
    return () => window.removeEventListener("resize", on);
  }, [clamp]);

  const onMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!dragging.current || !box.current) return;
    const rect = box.current.getBoundingClientRect();
    setSize(clamp(dir === "row" ? e.clientX - rect.left : e.clientY - rect.top));
  };
  const onKey = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? 64 : 16;
    const less = dir === "row" ? "ArrowLeft" : "ArrowUp";
    const more = dir === "row" ? "ArrowRight" : "ArrowDown";
    if (e.key === less) setSize((s) => clamp(s - step));
    else if (e.key === more) setSize((s) => clamp(s + step));
    else return;
    e.preventDefault();
  };

  const style: CSSProperties =
    dir === "row"
      ? { gridTemplateColumns: `${size}px 1px minmax(0, 1fr)` }
      : { gridTemplateRows: `${size}px 1px minmax(0, 1fr)` };

  return (
    <div ref={box} className={`split split-${dir}${className ? ` ${className}` : ""}`} style={style}>
      <div className="split-first">{first}</div>
      <div
        className="split-handle"
        role="separator"
        aria-orientation={dir === "row" ? "vertical" : "horizontal"}
        aria-valuenow={Math.round(size)}
        tabIndex={0}
        onPointerDown={(e) => {
          dragging.current = true;
          e.currentTarget.setPointerCapture(e.pointerId);
          document.body.classList.add("is-dragging");
        }}
        onPointerMove={onMove}
        onPointerUp={(e) => {
          dragging.current = false;
          e.currentTarget.releasePointerCapture(e.pointerId);
          document.body.classList.remove("is-dragging");
        }}
        onPointerCancel={() => {
          dragging.current = false;
          document.body.classList.remove("is-dragging");
        }}
        onKeyDown={onKey}
        onDoubleClick={() => setSize(clamp(initial))}
        title="Drag to resize. Double-click to reset."
      />
      <div className="split-second">{second}</div>
    </div>
  );
}

// ── atoms ────────────────────────────────────────────────────────────────────

export type Tone = "plain" | "dim" | "good" | "warn" | "bad" | "info" | "accent" | "violet";

export function Chip({
  tone = "plain",
  children,
  title,
  onClick,
  on,
  mono,
}: {
  tone?: Tone;
  children: ReactNode;
  title?: string;
  onClick?: () => void;
  on?: boolean;
  mono?: boolean;
}) {
  const cls = `chip chip-${tone}${on ? " is-on" : ""}${mono ? " mono" : ""}`;
  if (onClick) {
    return (
      <button type="button" className={cls} title={title} onClick={onClick} aria-pressed={on}>
        {children}
      </button>
    );
  }
  return (
    <span className={cls} title={title}>
      {children}
    </span>
  );
}

/** A count with its noun. Numbers are set in tabular figures so a column of
 *  them lines up. */
export function Count({ n, label, tone }: { n: number; label: string; tone?: Tone }) {
  return (
    <span className={`count${tone ? ` count-${tone}` : ""}`}>
      <b>{n.toLocaleString()}</b> {label}
    </span>
  );
}

export function Note({ tone = "plain", children, icon }: { tone?: Tone; children: ReactNode; icon?: string }) {
  return (
    <div className={`note note-${tone}`} role={tone === "bad" ? "alert" : undefined}>
      {icon ? <span className="note-icon" aria-hidden>{icon}</span> : null}
      <div className="note-body">{children}</div>
    </div>
  );
}

export function Empty({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="empty">
      <p className="empty-title">{title}</p>
      {children ? <div className="empty-hint">{children}</div> : null}
    </div>
  );
}

export function Spin({ label = "reading" }: { label?: string }) {
  return (
    <span className="spin" role="status">
      <span className="spin-dot" aria-hidden />
      {label}
    </span>
  );
}

/** A command the reader can run. The console never acts, so the next step is
 *  always something to type; clicking copies it. */
export function Cmd({ text, hint, block }: { text: string; hint?: string; block?: boolean }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className={`cmd${block ? " cmd-block" : ""}${copied ? " is-copied" : ""}`}
      title={hint ? `${hint}\nClick to copy.` : "Click to copy."}
      onClick={(e) => {
        e.stopPropagation();
        void navigator.clipboard
          ?.writeText(text)
          .then(() => {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1200);
          })
          .catch(() => setCopied(false));
      }}
    >
      <span className="cmd-prompt" aria-hidden>$</span>
      <code>{copied ? "copied" : text}</code>
    </button>
  );
}

/** Key/value rows: the facts about one thing, read top to bottom. */
export function Facts({ rows }: { rows: [string, ReactNode][] }) {
  return (
    <dl className="facts">
      {rows.map(([k, v]) => (
        <div key={k} className="facts-row">
          <dt>{k}</dt>
          <dd>{v}</dd>
        </div>
      ))}
    </dl>
  );
}

export function SectionHead({ children, aside }: { children: ReactNode; aside?: ReactNode }) {
  return (
    <div className="section-head">
      <h3>{children}</h3>
      {aside ? <div className="section-aside">{aside}</div> : null}
    </div>
  );
}

// ── HTTP vocabulary ──────────────────────────────────────────────────────────

export function Method({ m }: { m: string }) {
  return <span className={`method method-${m.toLowerCase()}`}>{m}</span>;
}

/** The status, coloured by class. A refusal is not a status: it never reached
 *  the wire, and it wears the one red fill this screen has. */
export function Status({
  status,
  allowed,
  error,
  reason,
  pending,
}: {
  status?: number;
  allowed: boolean;
  error?: string;
  reason?: string;
  pending?: boolean;
}) {
  if (!allowed) {
    return (
      <span className="status status-refused" title={reason ?? "refused by policy"}>
        refused
      </span>
    );
  }
  if (error !== undefined) {
    return (
      <span className="status status-error" title={error}>
        failed
      </span>
    );
  }
  if (status === undefined) {
    return <span className="status status-none">{pending ? "…" : "—"}</span>;
  }
  return <span className={`status status-${Math.floor(status / 100)}xx`}>{status}</span>;
}

// ── attention ────────────────────────────────────────────────────────────────

export const ATTENTION_ORDER = ["blocked", "done", "working", "idle", "unknown"];

export const ATTENTION_LABEL: Record<string, string> = {
  blocked: "waiting on you",
  done: "finished, unread",
  working: "working",
  idle: "idle",
  unknown: "unclassified",
};

export function AttentionTag({ state, why }: { state: string; why: string }) {
  return (
    <span className={`attn attn-${state}`} title={why}>
      <span className="attn-dot" aria-hidden />
      {ATTENTION_LABEL[state] ?? state}
    </span>
  );
}

// ── time and size ────────────────────────────────────────────────────────────

/** RFC3339 → HH:MM:SS, best-effort. */
export function clock(ts: string): string {
  const m = ts.match(/T(\d{2}:\d{2}:\d{2})/);
  return m ? m[1] : ts;
}

/** RFC3339 → `HH:MM:SS.mmm`, for a log where the second is not enough. */
export function clockMs(ts: string): string {
  const m = ts.match(/T(\d{2}:\d{2}:\d{2})(?:\.(\d{1,3}))?/);
  return m ? `${m[1]}.${(m[2] ?? "").padEnd(3, "0")}` : ts;
}

export function day(ts: string): string {
  return ts.slice(0, 10);
}

/** "3m ago", "2h ago", "4d ago". Nothing under a minute is called "now",
 *  because a log line written 40 seconds ago is not now. */
export function ago(ts: string | null | undefined, now = Date.now()): string {
  if (!ts) return "";
  const t = Date.parse(ts);
  if (!Number.isFinite(t)) return "";
  const s = Math.max(0, Math.round((now - t) / 1000));
  if (s < 60) return `${s}s ago`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 48) return `${h}h ago`;
  return `${Math.round(h / 24)}d ago`;
}

export function fmtMs(ms: number): string {
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms}ms`;
}

export function fmtSecs(s: number): string {
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m${s % 60 ? ` ${s % 60}s` : ""}`;
  return `${Math.floor(m / 60)}h${m % 60 ? ` ${m % 60}m` : ""}`;
}

export function fmtKb(kb: number): string {
  return kb >= 1024 ? `${(kb / 1024).toFixed(0)}M` : `${kb}K`;
}

export function fmtBytes(n: number): string {
  if (n >= 1 << 30) return `${(n / (1 << 30)).toFixed(1)}G`;
  if (n >= 1 << 20) return `${(n / (1 << 20)).toFixed(1)}M`;
  if (n >= 1 << 10) return `${(n / (1 << 10)).toFixed(1)}K`;
  return `${n}`;
}

/** How long a session ran, from its two timestamps. */
export function span(start: string, end: string | null): string {
  const a = Date.parse(start);
  const b = end ? Date.parse(end) : Date.now();
  if (!Number.isFinite(a) || !Number.isFinite(b)) return "";
  return fmtSecs(Math.max(0, Math.round((b - a) / 1000)));
}

export function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return n === 1 ? one : many;
}
