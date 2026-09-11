// `done` is the one attention state that depends on who is looking, so the
// server reports what is true and each client remembers what it has read.
// Nothing is written back: a passive view with a side effect is not one.

import type { SessionRow } from "./api";

const SEEN_KEY = "h5i.console.seen";

export function loadSeen(): Record<string, string> {
  try {
    return JSON.parse(window.localStorage.getItem(SEEN_KEY) ?? "{}") as Record<string, string>;
  } catch {
    return {};
  }
}

/** What makes a `done` distinct: the ending, or the newest job that finished.
 *  Looking at one ending does not clear the next one. */
export function doneMark(s: SessionRow): string {
  const job = s.jobs.length > 0 ? s.jobs[s.jobs.length - 1] : null;
  return `${s.ended_at ?? ""}|${job?.id ?? ""}|${job?.ended_at ?? ""}`;
}

/** The state to show, after this client's own seen set. */
export function shownState(s: SessionRow, seen: Record<string, string>): string {
  if (s.attention.state === "done" && seen[s.id] === doneMark(s)) return "idle";
  return s.attention.state;
}

export function markSeen(s: SessionRow): Record<string, string> {
  const seen = loadSeen();
  seen[s.id] = doneMark(s);
  try {
    window.localStorage.setItem(SEEN_KEY, JSON.stringify(seen));
  } catch {
    // A browser with storage blocked keeps its badges; nothing else breaks.
  }
  return seen;
}
