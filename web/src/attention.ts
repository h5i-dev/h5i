/**
 * The attention data contract — mirrors `h5i-core`'s `attention` module.
 *
 * `/api/attention` is byte-identical to `h5i status --json` (both are the
 * same Rust projection), so these types are the one place the web names
 * that contract. `/api/events` streams `update` frames over SSE whenever
 * the projection changes, so nothing here polls.
 */

export type Authority =
  | "enforced"
  | "verified"
  | "observed"
  | "reported"
  | "inferred"
  | "unknown";

export type Priority =
  | "critical"
  | "decision"
  | "communication"
  | "active"
  | "info";

export interface EvidenceRef {
  kind: string;
  id: string;
  authority: Authority;
  note?: string | null;
}

export interface EntityRef {
  kind: string; // env | team | msg
  id: string;
}

export interface AttentionItem {
  id: string;
  priority: Priority;
  state: string;
  entity: EntityRef;
  title: string;
  reasons: string[];
  evidence: EvidenceRef[];
  commands: string[];
  occurred_at: string;
  seen_at?: string | null;
}

export interface SeatView {
  agent: string;
  env_id: string;
  status: string;
}

export interface WorkItem {
  id: string;
  kind: string; // env | team
  title: string;
  lifecycle: string;
  seats: SeatView[];
  updated_at: string;
  unseen: number;
}

export interface AttentionReport {
  generated_at: string;
  identity: string;
  items: AttentionItem[];
  work_items: WorkItem[];
}

/** A domain fact from the env event log, pushed over SSE. */
export interface RuntimeFact {
  ts: string;
  env_id: string;
  agent: string;
  event: string;
  detail?: string | null;
  capture?: string | null;
}

export interface EventsUpdate {
  attention?: AttentionReport;
  facts: RuntimeFact[];
}

async function getJSON<T>(url: string): Promise<T> {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`${url}: ${res.status}`);
  return res.json() as Promise<T>;
}

export function getAttention(): Promise<AttentionReport> {
  return getJSON<AttentionReport>("/api/attention");
}

/**
 * The one mutation the assurance plane allows: record the seen-cursor.
 * The custom header is the CSRF guard — a cross-origin page can't attach
 * it without a preflight this server never grants.
 */
export async function markSeen(ids?: string[]): Promise<{ marked: number }> {
  const res = await fetch("/api/attention/seen", {
    method: "POST",
    headers: { "content-type": "application/json", "x-h5i-attention": "1" },
    body: JSON.stringify(ids ? { ids } : {}),
  });
  if (!res.ok) throw new Error(`mark seen: ${res.status}`);
  return res.json() as Promise<{ marked: number }>;
}

/**
 * Subscribe to the live update stream. Returns an unsubscribe function.
 * `onState` reports transport liveness so the UI can be honest about
 * freshness (live vs disconnected) instead of pretending.
 */
export function subscribeUpdates(
  onUpdate: (u: EventsUpdate) => void,
  onState?: (connected: boolean) => void,
): () => void {
  const source = new EventSource("/api/events");
  source.addEventListener("update", (ev) => {
    try {
      onUpdate(JSON.parse((ev as MessageEvent).data) as EventsUpdate);
    } catch {
      // A malformed frame is dropped; the next frame resyncs.
    }
  });
  source.onopen = () => onState?.(true);
  source.onerror = () => onState?.(false);
  return () => source.close();
}

/** Compact "2m" / "3h" / "5d" age for rail rows. */
export function ago(ts: string): string {
  const then = Date.parse(ts);
  if (Number.isNaN(then)) return "";
  const s = Math.max(0, (Date.now() - then) / 1000);
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}
