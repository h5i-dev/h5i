// The console's whole view of the server. Every route is a GET — the console
// watches boxes and never drives them (see crates/h5i-core/src/server.rs).
//
// Authorization is ambient: the page was loaded with `?token=…`, which the
// server traded for a SameSite=Strict cookie, so `fetch` needs nothing but
// `credentials: "same-origin"`.

// ── mirrors of the Rust types ────────────────────────────────────────────────

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

/** One observed execution — `h5i_core::receipt::ExecRecord`. */
export interface ExecRecord {
  id: string;
  timestamp: string;
  env_id: string;
  policy_digest?: string;
  /** Which lane observed this: `host-env-run`, `tee-shim`, `shell-egress`. */
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

// ── transport ────────────────────────────────────────────────────────────────

async function get<T>(path: string): Promise<T> {
  const res = await fetch(path, { credentials: "same-origin" });
  if (!res.ok) {
    // 401 here means the cookie is gone (or was never set): the page was
    // opened without the token h5i printed. Say that, rather than "500".
    if (res.status === 401) {
      throw new Error(
        "not authorized — reopen the URL `h5i ui` printed, token and all",
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
};
