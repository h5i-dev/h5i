// The console's whole view of the server. Every route is a GET — the console
// watches sessions and boxes and never drives them (see
// crates/h5i-core/src/server.rs).
//
// Authorization is ambient: the page was loaded with `?token=…`, which the
// server traded for a SameSite=Strict cookie, so `fetch` needs nothing but
// `credentials: "same-origin"`.

// ── boxes: mirrors of the Rust types ─────────────────────────────────────────

/** `h5i_core::env::EnvManifest`, flattened into every fleet row. */
export interface EnvManifest {
  id: string;
  agent: string;
  slug: string;
  base_commit: string;
  base_tree: string;
  parent_branch: string;
  branch: string;
  source: string;
  profile: string;
  policy_digest: string;
  isolation_claim: string;
  backend: string;
  created_at: string;
  updated_at: string;
  status: string;
  captures: string[];
  service_digest?: string;
  persona_digest?: string;
  pr?: number;
  pr_head_ref?: string;
}

export interface LiveSession {
  pid: number;
  kind: string;
  started_at: string;
  command?: string;
}

export interface EnvEvent {
  ts: string;
  env_id: string;
  agent: string;
  event: string;
  detail?: string;
  capture?: string;
}

export interface EgressHost {
  host: string;
  port: number;
  allowed: number;
  denied: number;
}

export interface EgressSummary {
  allowed: number;
  denied: number;
  hosts?: EgressHost[];
  hosts_truncated?: boolean;
  log?: string;
}

export interface BrowserEvidence {
  verb?: string;
  console?: string[];
  errors?: string[];
  failed_requests?: string[];
  truncated?: boolean;
  unavailable?: boolean;
}

/** What an ingress session was — `h5i_core::receipt::ShareEvidence`. */
export interface ShareEvidence {
  transport: string;
  port: number;
  peers: number;
  seconds: number;
  turned_away?: number;
}

/** A transport this console does not know is not a promise of end-to-end
 *  encryption, so it answers true. */
export function thirdPartyCanRead(share: ShareEvidence): boolean {
  return share.transport !== "p2p";
}

export interface Detection {
  rule: string;
  family: string;
  severity: "info" | "notice" | "alert";
  title: string;
  count: number;
  first_ns: number;
  last_ns: number;
  examples?: string[];
  examples_truncated?: boolean;
}

/** What the kernel-observed lane saw. An empty `detections` is only clean if
 *  the run was watched; {@link runtimeObserved} is that test. */
export interface RuntimeEvidence {
  lane: string;
  scope: string;
  coverage: "full" | "partial" | "none";
  coverage_reason?: string;
  events_seen?: number;
  events_lost?: number;
  events_filtered?: number;
  detections?: Detection[];
  unavailable?: string;
}

export function runtimeObserved(rt: RuntimeEvidence): boolean {
  return !rt.unavailable && rt.coverage !== "none";
}

/** One observed execution — `h5i_core::receipt::ExecRecord`. */
export interface ExecRecord {
  id: string;
  timestamp: string;
  env_id: string;
  policy_digest?: string;
  effective_digest?: string;
  fs_overlap?: string[];
  source: string;
  cmd?: string;
  cwd?: string;
  exit_code?: number;
  timed_out?: boolean;
  wall_ms?: number;
  cpu_ms?: number;
  max_rss_kb?: number;
  git_tree?: string;
  files?: string[];
  egress?: EgressSummary;
  browser?: BrowserEvidence;
  share?: ShareEvidence;
  runtime?: RuntimeEvidence;
  redactions?: string[];
  raw_oid: string;
  raw_size: number;
  raw_lines: number;
  raw_truncated?: boolean;
}

export type Verdict = "denial" | "attention" | "clean";

/** Arithmetic over one box's receipts. Nothing here is a score. */
export interface Signals {
  runs: number;
  failed: number;
  timed_out: number;
  egress_allowed: number;
  egress_denied: number;
  denied_hosts: string[];
  browser_issues: number;
  host_observed: number;
  box_claimed: number;
  last_run_ts?: string;
  verdict: Verdict;
  weak_isolation: boolean;
  box_claimed_only: boolean;
  kernel_watched?: number;
  kernel_unwatched?: number;
  kernel_alerts?: number;
  kernel_notices?: number;
  kernel_events_lost?: number;
  kernel_rules?: string[];
  shares: number;
  shares_third_party_readable: number;
  share_peers: number;
  fs_overlap: string[];
}

export interface SharedNow {
  transport: string;
  port: number;
  grants: number;
}

export interface BoxRow extends EnvManifest {
  drift: string;
  drift_summary: string;
  live: LiveSession[];
  stale_running: boolean;
  has_workspace: boolean;
  files_changed: number;
  insertions: number;
  deletions: number;
  last_event?: EnvEvent;
  signals: Signals;
  shared_now?: SharedNow | null;
}

export interface EnforcedPolicy {
  isolation: string;
  net_mode: string;
  net_egress: string[];
  fs_read: string[];
  fs_write: string[];
  fs_deny: string[];
  tools: string[];
  env_pass: string[];
  image?: string;
  mem_bytes?: number;
  max_procs?: number;
  mem_enforced?: boolean;
  procs_enforced?: boolean;
  wall_secs: number;
  cpu_secs?: number;
  fsize_bytes?: number;
}

export interface ServiceStatus {
  name: string;
  pid: number;
  command: string;
  started_at: string;
  port?: number;
  dynamic_port?: number;
  log: string;
  alive: boolean;
}

export interface BoxDetail {
  item: BoxRow;
  policy?: EnforcedPolicy;
  events: EnvEvent[];
  receipts: ExecRecord[];
  receipts_folded: number;
  services: ServiceStatus[];
  diffstat?: string;
}

export interface ClaimSupport {
  claim: string;
  satisfiable: boolean;
  runnable?: boolean;
  note?: string;
}

/** `h5i box capabilities --json`, verbatim. */
export interface CapabilitiesReport {
  os: string;
  landlock_abi?: number | null;
  userns: boolean;
  seccomp: boolean;
  seatbelt: boolean;
  mechanism: string;
  syscall_filter: boolean;
  memory_limit: boolean;
  container_runtime?: string | null;
  egress_enforced: boolean;
  resource_limits: boolean;
  claims: ClaimSupport[];
  strongest_tier: string;
}

// ── the in-box browser stream ────────────────────────────────────────────────

export type Lane = "host-observed" | "box-claimed";
export type Grade = "fail-closed" | "best-effort";
export type ConsoleLevel = "warning" | "error" | "page-error";
export type Initiator = "navigation" | "subresource" | "redirect" | "other";

interface EventBase {
  id: number;
  observed_at: string;
  lane: Lane;
  grade: Grade;
  caused_by?: number;
}

export type ViewerEvent = EventBase &
  (
    | { kind: "navigated"; url: string }
    | {
        kind: "request";
        seq: number;
        method: string;
        url: string;
        initiator: Initiator;
        allowed: boolean;
        denied_reason?: string;
      }
    | {
        kind: "response";
        seq: number;
        status?: number;
        bytes?: number;
        duration_ms?: number;
        error?: string;
      }
    | { kind: "console"; level: ConsoleLevel; text: string }
    | { kind: "agent-action"; action: string; forwarded: boolean }
    | { kind: "policy-verdict"; subject: string; reason: string }
    | { kind: "session-reset"; source: string }
  );

export interface BrowserStream {
  events: ViewerEvent[];
  cursor: number;
  dropped: number;
  engine?: string;
  live_view: boolean;
  frame_seq?: number;
  frame_error?: string;
}

// ── browser sessions ─────────────────────────────────────────────────────────

/** How loudly a session is asking, and the evidence behind it. */
export interface Attention {
  /** `blocked`, `working`, `done`, `idle`, `unknown`. */
  state: string;
  why: string;
}

export interface LedgerCounts {
  candidate: number;
  observed: number;
  confirmed: number;
  refused: number;
  gone: number;
  cursor: number;
  unreadable: number;
  truncated: boolean;
}

export interface JobRow {
  id: string;
  verb: string;
  started_at: string;
  ended_at: string | null;
  requests: number;
  written: number;
  stopped: string | null;
}

export interface SessionRow {
  id: string;
  name: string | null;
  state: string;
  placement: string;
  lane: string;
  identity: string;
  url: string;
  started_at: string;
  ended_at: string | null;
  end_reason: string | null;
  engine: string;
  confinement: string;
  enclosing_box: string | null;
  permissive_cors: boolean;
  policy_digest: string;
  restored_from: string | null;
  expires_at: string | null;
  held_by_human: boolean;
  requests: number;
  denied: number;
  origins: string[];
  last_request_at: string | null;
  captured: number | null;
  reclaimed: { at: string; messages: number; bytes: number } | null;
  ledger: LedgerCounts | null;
  jobs: JobRow[];
  findings: number;
  verbs: number;
  attention: Attention;
}

/** A record the poll did not fold: enough to find it and ask for it. */
export interface SessionStub {
  id: string;
  name: string | null;
  state: string;
  url: string;
  started_at: string;
  ended_at: string | null;
}

export interface SessionFleet {
  sessions: SessionRow[];
  total: number;
  live: number;
  index: SessionStub[];
}

/** One endpoint in the recon ledger. */
export interface EndpointRow {
  id: string;
  origin: string;
  path: string;
  method: string;
  identity: string;
  state: string;
  sources: { from: string }[];
  evidence: string[];
  params: { name: string; at: string }[];
  status: number | null;
  cluster: string | null;
  reason: string | null;
  first_seen: string;
  last_seen: string;
  line: number;
}

/** One line of the request log. Counts and names, never values. */
export interface RequestRecord {
  seq: number;
  at: string;
  phase: string;
  initiator: string;
  method: string;
  url: string;
  allowed: boolean;
  denied_reason?: string;
  status?: number;
  bytes?: number;
  duration_ms?: number;
  ttfb_ms?: number;
  cookies_sent?: number;
  cookies_stored?: number;
  error?: string;
}

/** One verb the agent asked for, with the receipts it spent. */
export interface ActionRow {
  seq: number;
  at: string;
  verb: string;
  target?: string;
  url?: string;
  ok?: boolean;
  error?: string;
  ended_at?: string;
  requests: number[];
}

export interface FindingRow {
  id: string;
  title: string;
  state: string;
  evidence: string[];
  repro?: string;
  notes: { at: string; text: string }[];
  created: string;
  updated: string;
}

export interface SiteEndpoint {
  path: string;
  methods: string[];
  statuses: number[];
  params: string[];
  hits: number;
  refused: number;
  navigated: boolean;
  last_seq: number;
}

export interface SiteOrigin {
  origin: string;
  hits: number;
  refused: number;
  endpoints: SiteEndpoint[];
}

export interface SessionDetail extends SessionRow {
  requests_log: RequestRecord[];
  requests_total: number;
  endpoints: EndpointRow[];
  actions: ActionRow[];
  findings_list: FindingRow[];
  sitemap: SiteOrigin[];
}

// ── transport ────────────────────────────────────────────────────────────────

async function get<T>(path: string): Promise<T> {
  const res = await fetch(path, { credentials: "same-origin" });
  if (!res.ok) {
    if (res.status === 401) {
      throw new Error(
        "not authorized: reopen the URL `h5i ui` printed, token and all",
      );
    }
    throw new Error(`${res.status} ${res.statusText}`);
  }
  return (await res.json()) as T;
}

export const api = {
  boxes: () => get<BoxRow[]>("/api/boxes"),
  box: (agent: string, slug: string) =>
    get<BoxDetail>(`/api/box/${encodeURIComponent(agent)}/${encodeURIComponent(slug)}`),
  receipt: (agent: string, slug: string, id: string) =>
    get<{ render: string }>(
      `/api/box/${encodeURIComponent(agent)}/${encodeURIComponent(slug)}/receipts/${encodeURIComponent(id)}`,
    ),
  probe: () => get<CapabilitiesReport>("/api/probe"),
  sessions: () => get<SessionFleet>("/api/sessions"),
  session: (id: string) =>
    get<SessionDetail>(`/api/session/${encodeURIComponent(id)}`),
  browser: (agent: string, slug: string, since: number) =>
    get<BrowserStream>(
      `/api/box/${encodeURIComponent(agent)}/${encodeURIComponent(slug)}/browser?since=${since}`,
    ),
  browserFrameUrl: (agent: string, slug: string, seq: number) =>
    `/api/box/${encodeURIComponent(agent)}/${encodeURIComponent(slug)}/browser/frame?seq=${seq}`,
};
